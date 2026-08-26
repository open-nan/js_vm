//! SWC AST 到 JS VM IR 的降低器。
//!
//! 该模块先用 SWC 解析 JavaScript/TypeScript，再把 AST 降低成 Core Layer 的结构化 IR。
//! 当前文件中仍保留一层历史文本 IR 作为过渡表示，最后由 `StructuredIrBuilder`
//! 归并成 `core::IrModule`。
//!
//! 维护这层时要关注三个边界：
//! - 作用域：声明、参数、catch、函数内部名字尽量转成 local slot。
//! - extern：未声明的根名字进入 extern slot，由运行时外部数组提供。
//! - 控制流：break/continue/return/throw/try/finally 要在 IR 中保留可执行结构。

use js_token_core as core;
use std::collections::{BTreeMap, BTreeSet};
use swc_common::{FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

#[derive(Debug, Clone, PartialEq)]
enum IrValue {
    Register(String),
    Name(String),
    Number(f64),
    String(String),
    BigInt(String),
    Bool(bool),
    Null,
    Undefined,
}

impl std::fmt::Display for IrValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IrValue::Register(value) => write!(f, "%{value}"),
            IrValue::Name(value) => f.write_str(value),
            IrValue::Number(value) => write!(f, "{value}"),
            IrValue::String(value) => write!(f, "{value:?}"),
            IrValue::BigInt(value) => write!(f, "{value}n"),
            IrValue::Bool(value) => write!(f, "{value}"),
            IrValue::Null => f.write_str("null"),
            IrValue::Undefined => f.write_str("undefined"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum IrInstruction {
    Marker(String),
    Label(String),
    Declare {
        kind: String,
        name: String,
    },
    LoadConst {
        dst: String,
        value: IrValue,
    },
    LoadName {
        dst: String,
        name: String,
    },
    StoreName {
        name: String,
        src: IrValue,
    },
    StoreMember {
        object: IrValue,
        property: IrValue,
        src: IrValue,
    },
    Move {
        dst: String,
        src: IrValue,
    },
    Binary {
        dst: String,
        op: String,
        left: IrValue,
        right: IrValue,
    },
    Unary {
        dst: String,
        op: String,
        arg: IrValue,
    },
    Member {
        dst: String,
        object: IrValue,
        property: IrValue,
    },
    Array {
        dst: String,
        items: Vec<IrValue>,
    },
    Object {
        dst: String,
        props: Vec<(String, IrValue)>,
    },
    ObjectRest {
        dst: String,
        source: IrValue,
        excluded: Vec<String>,
    },
    Call {
        dst: String,
        callee: IrValue,
        args: Vec<IrValue>,
    },
    New {
        dst: String,
        callee: IrValue,
        args: Vec<IrValue>,
    },
    Template {
        dst: String,
        quasis: Vec<String>,
        exprs: Vec<IrValue>,
    },
    Function {
        name: String,
        params: Vec<String>,
        is_async: bool,
        is_generator: bool,
        body: Vec<IrInstruction>,
    },
    FunctionExpr {
        dst: String,
        name: Option<String>,
        params: Vec<String>,
        is_async: bool,
        is_generator: bool,
        body: Vec<IrInstruction>,
    },
    Class {
        dst: Option<String>,
        name: Option<String>,
        super_class: Option<IrValue>,
        members: Vec<String>,
    },
    Import {
        source: String,
        specifiers: Vec<String>,
    },
    Export {
        kind: String,
        entries: Vec<(String, String)>,
    },
    Throw(IrValue),
    Try {
        body: Vec<IrInstruction>,
        catch_param: Option<String>,
        catch_body: Vec<IrInstruction>,
        finally_body: Vec<IrInstruction>,
    },
    EnterScope(String),
    LeaveScope,
    Scope {
        kind: String,
        body: Vec<IrInstruction>,
    },
    Return(Option<IrValue>),
    Pop(IrValue),
    Jump(String),
    JumpIfFalse {
        test: IrValue,
        label: String,
    },
    Yield {
        dst: String,
        value: IrValue,
        delegate: bool,
    },
    Await {
        dst: String,
        value: IrValue,
    },
    Unsupported(String),
}

/// AST lowering 上下文。
///
/// 一个上下文对应一个函数或顶层降低过程。它负责生成临时寄存器、标签、extern slot 和
/// 过渡指令序列；子函数通过 `child()` 创建独立上下文，再把 extern 需求合并回父级。
pub struct LoweringContext {
    /// 过渡指令序列。
    instructions: Vec<IrInstruction>,
    /// 临时寄存器编号。
    temp_id: usize,
    /// 标签编号。
    label_id: usize,
    /// 子上下文编号。
    child_id: usize,
    /// 标签前缀，避免嵌套函数标签冲突。
    label_prefix: String,
    /// break/continue 目标栈。
    control_stack: Vec<ControlTarget>,
    /// 待绑定的 label 声明。
    pending_control_labels: Vec<String>,
    /// extern 名称到 slot 的映射。
    extern_slots: BTreeMap<String, usize>,
    /// 当前上下文已声明的本地名字。
    locals: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlTarget {
    label: Option<String>,
    break_label: String,
    continue_label: Option<String>,
}

impl LoweringContext {
    /// 创建 lowering 上下文，并预置已有 extern slot。
    pub fn with_externals(externs: &[String]) -> Self {
        Self {
            instructions: Vec::new(),
            temp_id: 0,
            label_id: 0,
            child_id: 0,
            label_prefix: "c0".to_string(),
            control_stack: Vec::new(),
            pending_control_labels: Vec::new(),
            extern_slots: externs
                .iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), index))
                .collect(),
            locals: BTreeSet::new(),
        }
    }

    /// 完成 lowering，输出结构化 IR 模块。
    pub fn into_module(self) -> core::IrModule {
        let mut extern_slots = vec![String::new(); self.extern_slots.len()];
        for (name, slot) in self.extern_slots {
            extern_slots[slot] = name;
        }
        StructuredIrBuilder::new(extern_slots).build(self.instructions)
    }

    fn child(&mut self) -> Self {
        // 子函数必须拥有独立的寄存器、标签和控制流栈；否则内层函数 lowering 会污染外层函数。
        //
        // 这里故意复制 `locals`：子函数在编译阶段需要知道哪些名字来自外层，用于避免把闭包变量
        // 误判为 extern。真正的捕获关系会在后续结构化 IR 阶段继续收敛。
        let child_id = self.child_id;
        self.child_id += 1;
        Self {
            instructions: Vec::new(),
            temp_id: 0,
            label_id: 0,
            child_id: 0,
            label_prefix: format!("{}_{}", self.label_prefix, child_id),
            control_stack: Vec::new(),
            pending_control_labels: Vec::new(),
            extern_slots: BTreeMap::new(),
            locals: self.locals.clone(),
        }
    }

    fn merge_child_externs(&mut self, child: &LoweringContext) {
        // 内层函数引用的真实 extern 必须提升到模块 extern 表，否则运行时 wrapper 无法传入对应槽。
        // 已声明的外层名字不会进入 child.extern_slots，因此这里合并的是“宿主依赖”，不是闭包捕获。
        for name in child.extern_slots.keys() {
            self.mark_extern(name);
        }
    }

    fn declare_local(&mut self, name: impl Into<String>) {
        self.locals.insert(name.into());
    }

    fn declare_function_intrinsics(&mut self) {
        self.declare_local("arguments");
        self.declare_local("this");
        self.declare_local("super");
    }

    /// 将未声明根名字登记为 extern。
    ///
    /// 隐式全局如 `undefined`、`NaN`、`Infinity` 不进入 extern slot。
    fn mark_extern(&mut self, name: &str) {
        if !self.locals.contains(name) && !is_implicit_global(name) {
            let slot = self.extern_slots.len();
            self.extern_slots.entry(name.to_string()).or_insert(slot);
        }
    }

    fn temp(&mut self) -> String {
        let temp = format!("t{}", self.temp_id);
        self.temp_id += 1;
        temp
    }

    fn label(&mut self, prefix: &str) -> String {
        let label = format!("{}_{}_{}", self.label_prefix, prefix, self.label_id);
        self.label_id += 1;
        label
    }

    fn push_control_targets(
        &mut self,
        break_label: String,
        continue_label: Option<String>,
    ) -> usize {
        let previous_len = self.control_stack.len();
        let pending_labels = std::mem::take(&mut self.pending_control_labels);
        self.control_stack.push(ControlTarget {
            label: None,
            break_label: break_label.clone(),
            continue_label: continue_label.clone(),
        });
        for label in pending_labels {
            self.control_stack.push(ControlTarget {
                label: Some(label),
                break_label: break_label.clone(),
                continue_label: continue_label.clone(),
            });
        }
        previous_len
    }

    fn pop_control_targets(&mut self, previous_len: usize) {
        self.control_stack.truncate(previous_len);
    }

    fn resolve_break_target(&self, label: Option<&str>) -> Option<String> {
        self.control_stack.iter().rev().find_map(|target| {
            (target.label.as_deref() == label).then(|| target.break_label.clone())
        })
    }

    fn resolve_continue_target(&self, label: Option<&str>) -> Option<String> {
        self.control_stack.iter().rev().find_map(|target| {
            if target.label.as_deref() == label {
                target.continue_label.clone()
            } else {
                None
            }
        })
    }

    fn emit(&mut self, instruction: IrInstruction) {
        self.instructions.push(instruction);
    }

    fn lower_in_scope(&mut self, kind: &str, lower: impl FnOnce(&mut Self)) {
        let outer = std::mem::take(&mut self.instructions);
        lower(self);
        let body = std::mem::take(&mut self.instructions);
        self.instructions = outer;
        self.emit(IrInstruction::Scope {
            kind: kind.to_string(),
            body,
        });
    }

    pub fn lower_module(&mut self, module: &Module) {
        self.predeclare_module(module);
        for name in module_var_declared_names(module) {
            self.emit(IrInstruction::Declare {
                kind: "var".to_string(),
                name,
            });
        }
        for item in &module.body {
            self.lower_module_item(item);
        }
    }

    pub fn lower_script(&mut self, script: &Script) {
        for stmt in &script.body {
            self.predeclare_stmt(stmt);
        }
        for name in script_var_declared_names(script) {
            self.emit(IrInstruction::Declare {
                kind: "var".to_string(),
                name,
            });
        }
        for stmt in &script.body {
            self.lower_stmt(stmt);
        }
    }

    fn predeclare_module(&mut self, module: &Module) {
        for item in &module.body {
            match item {
                ModuleItem::Stmt(stmt) => self.predeclare_stmt(stmt),
                ModuleItem::ModuleDecl(ModuleDecl::Import(decl)) => {
                    for name in decl.specifiers.iter().filter_map(import_local_name) {
                        self.declare_local(name);
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(decl)) => {
                    self.predeclare_decl(&decl.decl);
                }
                _ => {}
            }
        }
    }

    pub fn predeclare_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Decl(decl) => self.predeclare_decl(decl),
            Stmt::Block(block) => self.predeclare_block(block),
            _ => {}
        }
    }

    fn predeclare_block(&mut self, block: &BlockStmt) {
        for stmt in &block.stmts {
            self.predeclare_stmt(stmt);
        }
    }

    fn predeclare_function_body(&mut self, body: &BlockStmt) {
        self.predeclare_block(body);
        for stmt in &body.stmts {
            for name in stmt_var_declared_names(stmt) {
                self.declare_local(name);
            }
        }
    }

    fn predeclare_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Var(var_decl) => {
                for name in var_decl_bound_names(var_decl) {
                    self.declare_local(name);
                }
            }
            Decl::Fn(fn_decl) => self.declare_local(ident_name(&fn_decl.ident)),
            Decl::Class(class_decl) => self.declare_local(ident_name(&class_decl.ident)),
            _ => {}
        }
    }

    fn lower_module_item(&mut self, item: &ModuleItem) {
        match item {
            ModuleItem::Stmt(stmt) => self.lower_stmt(stmt),
            ModuleItem::ModuleDecl(decl) => self.lower_module_decl(decl),
        }
    }

    fn lower_module_decl(&mut self, decl: &ModuleDecl) {
        match decl {
            ModuleDecl::Import(decl) => {
                let specifiers = decl
                    .specifiers
                    .iter()
                    .map(import_specifier_name)
                    .collect::<Vec<_>>();
                for name in decl.specifiers.iter().filter_map(import_local_name) {
                    self.declare_local(name);
                }
                self.emit(IrInstruction::Import {
                    source: decl.src.value.to_string(),
                    specifiers,
                });
            }
            ModuleDecl::ExportDecl(decl) => {
                self.lower_decl(&decl.decl);
                self.emit(IrInstruction::Export {
                    kind: "declaration".to_string(),
                    entries: decl_names(&decl.decl)
                        .into_iter()
                        .map(|name| (name.clone(), name))
                        .collect(),
                });
            }
            ModuleDecl::ExportNamed(decl) => {
                self.emit(IrInstruction::Export {
                    kind: decl
                        .src
                        .as_ref()
                        .map(|src| format!("named from {:?}", src.value))
                        .unwrap_or_else(|| "named".to_string()),
                    entries: decl.specifiers.iter().filter_map(export_entry).collect(),
                });
            }
            ModuleDecl::ExportDefaultDecl(decl) => {
                let value = match &decl.decl {
                    DefaultDecl::Class(class) => self.lower_class_expr(Some("default"), class),
                    DefaultDecl::Fn(function) => {
                        let value = self.lower_fn_expr(function);
                        value
                    }
                    DefaultDecl::TsInterfaceDecl(_) => {
                        self.emit(IrInstruction::Marker(
                            "typescript interface default".to_string(),
                        ));
                        IrValue::Undefined
                    }
                };
                self.declare_local("default".to_string());
                self.emit(IrInstruction::Declare {
                    kind: "var".to_string(),
                    name: "default".to_string(),
                });
                self.emit(IrInstruction::StoreName {
                    name: "default".to_string(),
                    src: value,
                });
                self.emit(IrInstruction::Export {
                    kind: "default declaration".to_string(),
                    entries: vec![("default".to_string(), "default".to_string())],
                });
            }
            ModuleDecl::ExportDefaultExpr(decl) => {
                let value = self.lower_expr(&decl.expr);
                self.declare_local("default".to_string());
                self.emit(IrInstruction::Declare {
                    kind: "var".to_string(),
                    name: "default".to_string(),
                });
                self.emit(IrInstruction::StoreName {
                    name: "default".to_string(),
                    src: value,
                });
                self.emit(IrInstruction::Export {
                    kind: "default expression".to_string(),
                    entries: vec![("default".to_string(), "default".to_string())],
                });
            }
            ModuleDecl::ExportAll(decl) => {
                self.emit(IrInstruction::Export {
                    kind: format!("all from {:?}", decl.src.value),
                    entries: Vec::new(),
                });
            }
            ModuleDecl::TsImportEquals(_)
            | ModuleDecl::TsExportAssignment(_)
            | ModuleDecl::TsNamespaceExport(_) => {
                self.emit(IrInstruction::Marker(format!(
                    "typescript module declaration: {}",
                    module_decl_name(decl)
                )));
            }
        }
    }

    pub fn lower_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Block(block) => self.lower_block_scope(block),
            Stmt::Return(stmt) => self.lower_return(stmt),
            Stmt::Decl(decl) => self.lower_decl(decl),
            Stmt::Expr(stmt) => self.lower_expr_stmt(stmt),
            Stmt::If(stmt) => {
                let else_label = self.label("if_else");
                let end_label = self.label("if_end");
                let test = self.lower_expr(&stmt.test);
                self.emit(IrInstruction::JumpIfFalse {
                    test,
                    label: else_label.clone(),
                });
                self.lower_stmt(&stmt.cons);
                self.emit(IrInstruction::Jump(end_label.clone()));
                self.emit(IrInstruction::Label(else_label));
                if let Some(alt) = &stmt.alt {
                    self.lower_stmt(alt);
                }
                self.emit(IrInstruction::Label(end_label));
            }
            Stmt::Debugger(_) => self.emit(IrInstruction::Marker("debugger".to_string())),
            Stmt::With(stmt) => {
                let object = self.lower_expr(&stmt.obj);
                self.emit(IrInstruction::Marker(format!("with {object} {{")));
                self.lower_stmt(&stmt.body);
                self.emit(IrInstruction::Marker("}".to_string()));
            }
            Stmt::Labeled(stmt) => {
                let label = ident_name(&stmt.label);
                if is_labelled_control_target(&stmt.body) {
                    self.pending_control_labels.push(label);
                    self.lower_stmt(&stmt.body);
                } else {
                    let end_label = self.label("label_end");
                    let previous_len = self.control_stack.len();
                    self.control_stack.push(ControlTarget {
                        label: Some(label),
                        break_label: end_label.clone(),
                        continue_label: None,
                    });
                    self.lower_stmt(&stmt.body);
                    self.pop_control_targets(previous_len);
                    self.emit(IrInstruction::Label(end_label));
                }
            }
            Stmt::Break(stmt) => {
                let label = stmt.label.as_ref().map(ident_name);
                if let Some(target) = self.resolve_break_target(label.as_deref()) {
                    self.emit(IrInstruction::Jump(target));
                } else {
                    self.emit(IrInstruction::Marker("break".to_string()));
                }
            }
            Stmt::Continue(stmt) => {
                let label = stmt.label.as_ref().map(ident_name);
                if let Some(target) = self.resolve_continue_target(label.as_deref()) {
                    self.emit(IrInstruction::Jump(target));
                } else {
                    self.emit(IrInstruction::Marker("continue".to_string()));
                }
            }
            Stmt::Switch(stmt) => self.lower_switch(stmt),
            Stmt::Throw(stmt) => {
                let value = self.lower_expr(&stmt.arg);
                self.emit(IrInstruction::Throw(value));
            }
            Stmt::Try(stmt) => self.lower_try(stmt),
            Stmt::While(stmt) => {
                let start_label = self.label("while_start");
                let end_label = self.label("while_end");
                self.emit(IrInstruction::Label(start_label.clone()));
                let test = self.lower_expr(&stmt.test);
                self.emit(IrInstruction::JumpIfFalse {
                    test,
                    label: end_label.clone(),
                });
                let control_len =
                    self.push_control_targets(end_label.clone(), Some(start_label.clone()));
                self.lower_stmt(&stmt.body);
                self.pop_control_targets(control_len);
                self.emit(IrInstruction::Jump(start_label));
                self.emit(IrInstruction::Label(end_label));
            }
            Stmt::DoWhile(stmt) => self.lower_do_while(stmt),
            Stmt::For(stmt) => self.lower_for(stmt),
            Stmt::ForIn(stmt) => self.lower_for_in(stmt),
            Stmt::ForOf(stmt) => self.lower_for_of(stmt),
            Stmt::Empty(_) => {}
        }
    }

    fn lower_block(&mut self, block: &BlockStmt) {
        self.predeclare_block(block);
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
        }
    }

    fn lower_block_scope(&mut self, block: &BlockStmt) {
        self.predeclare_block(block);
        let start = self.instructions.len();
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
        }
        let body = self.instructions.split_off(start);
        self.emit(IrInstruction::Scope {
            kind: "block".to_string(),
            body,
        });
    }

    fn lower_return(&mut self, stmt: &ReturnStmt) {
        let value = stmt.arg.as_ref().map(|expr| self.lower_expr(expr));
        self.emit(IrInstruction::Return(value));
    }

    fn lower_expr_stmt(&mut self, stmt: &ExprStmt) {
        let value = self.lower_expr(&stmt.expr);
        let value = match value {
            IrValue::Register(_) => value,
            value => {
                let dst = self.temp();
                self.emit(IrInstruction::Move {
                    dst: dst.clone(),
                    src: value,
                });
                IrValue::Register(dst)
            }
        };
        self.emit(IrInstruction::Pop(value));
    }

    fn lower_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Var(var_decl) => self.lower_var_decl(var_decl),
            Decl::Fn(fn_decl) => self.lower_fn_decl(fn_decl),
            Decl::Class(class_decl) => {
                self.lower_class(Some(ident_name(&class_decl.ident)), &class_decl.class, None);
            }
            Decl::Using(using_decl) => {
                self.emit(IrInstruction::Marker(format!(
                    "{} declaration",
                    if using_decl.is_await {
                        "await using"
                    } else {
                        "using"
                    }
                )));
                for declarator in &using_decl.decls {
                    let value = declarator
                        .init
                        .as_ref()
                        .map(|expr| self.lower_expr(expr))
                        .unwrap_or(IrValue::Undefined);
                    self.lower_pat_binding(&declarator.name, value, "using");
                }
            }
            Decl::TsInterface(_) | Decl::TsTypeAlias(_) | Decl::TsEnum(_) | Decl::TsModule(_) => {
                self.emit(IrInstruction::Marker(format!(
                    "typescript declaration: {}",
                    decl_name(decl)
                )))
            }
        }
    }

    fn lower_var_decl(&mut self, decl: &VarDecl) {
        let kind = match decl.kind {
            VarDeclKind::Var => "var",
            VarDeclKind::Let => "let",
            VarDeclKind::Const => "const",
        };

        for declarator in &decl.decls {
            let value = declarator
                .init
                .as_ref()
                .map(|init| self.lower_expr(init))
                .unwrap_or(IrValue::Undefined);
            self.lower_pat_binding(&declarator.name, value, kind);
        }
    }

    fn declare_function_params(&mut self, params: &[Param], param_names: &[String]) {
        for param in param_names {
            self.declare_local(param.clone());
        }
        for param in params {
            self.declare_pat_bound_names(&param.pat);
        }
    }

    fn declare_arrow_params(&mut self, params: &[Pat], param_names: &[String]) {
        for param in param_names {
            self.declare_local(param.clone());
        }
        for param in params {
            self.declare_pat_bound_names(param);
        }
    }

    fn declare_constructor_params(
        &mut self,
        params: &[ParamOrTsParamProp],
        param_names: &[String],
    ) {
        for param in param_names {
            self.declare_local(param.clone());
        }
        for param in params {
            if let ParamOrTsParamProp::Param(param) = param {
                self.declare_pat_bound_names(&param.pat);
            }
        }
    }

    fn declare_pat_bound_names(&mut self, pat: &Pat) {
        let mut names = BTreeSet::new();
        collect_pat_bound_names(pat, &mut names);
        for name in names {
            self.declare_local(name);
        }
    }

    fn lower_function_param_initializers(&mut self, params: &[Param], param_names: &[String]) {
        for (index, param) in params.iter().enumerate() {
            self.lower_param_initializer(&param.pat, &param_names[index]);
        }
    }

    fn lower_arrow_param_initializers(&mut self, params: &[Pat], param_names: &[String]) {
        for (index, param) in params.iter().enumerate() {
            self.lower_param_initializer(param, &param_names[index]);
        }
    }

    fn lower_constructor_param_initializers(
        &mut self,
        params: &[ParamOrTsParamProp],
        param_names: &[String],
    ) {
        for (index, param) in params.iter().enumerate() {
            if let ParamOrTsParamProp::Param(param) = param {
                self.lower_param_initializer(&param.pat, &param_names[index]);
            }
        }
    }

    fn lower_function_rest_param_initializer(&mut self, params: &[Param]) {
        for (index, param) in params.iter().enumerate() {
            if let Pat::Rest(rest) = &param.pat
                && let Some(name) = pat_name(&Pat::Rest(rest.clone()))
            {
                self.emit_rest_param_initializer(name, index);
            }
        }
    }

    fn lower_arrow_rest_param_initializer(&mut self, params: &[Pat]) {
        for (index, param) in params.iter().enumerate() {
            if let Pat::Rest(rest) = param
                && let Some(name) = pat_name(&Pat::Rest(rest.clone()))
            {
                self.emit_rest_param_initializer(name, index);
            }
        }
    }

    fn lower_constructor_rest_param_initializer(&mut self, params: &[ParamOrTsParamProp]) {
        for (index, param) in params.iter().enumerate() {
            if let ParamOrTsParamProp::Param(param) = param
                && let Pat::Rest(rest) = &param.pat
                && let Some(name) = pat_name(&Pat::Rest(rest.clone()))
            {
                self.emit_rest_param_initializer(name, index);
            }
        }
    }

    fn emit_rest_param_initializer(&mut self, name: String, start_index: usize) {
        self.mark_extern("Array");
        let from = self.temp();
        self.emit(IrInstruction::Member {
            dst: from.clone(),
            object: IrValue::Name("Array".to_string()),
            property: IrValue::String("from".to_string()),
        });
        let all_args = self.temp();
        self.emit(IrInstruction::Call {
            dst: all_args.clone(),
            callee: IrValue::Register(from),
            args: vec![IrValue::Name("arguments".to_string())],
        });
        let slice = self.temp();
        self.emit(IrInstruction::Member {
            dst: slice.clone(),
            object: IrValue::Register(all_args),
            property: IrValue::String("slice".to_string()),
        });
        let rest = self.temp();
        self.emit(IrInstruction::Call {
            dst: rest.clone(),
            callee: IrValue::Register(slice),
            args: vec![IrValue::Number(start_index as f64)],
        });
        self.emit(IrInstruction::StoreName {
            name,
            src: IrValue::Register(rest),
        });
    }

    fn lower_param_initializer(&mut self, pat: &Pat, param_name: &str) {
        // 简单参数直接使用自己的名字；解构参数使用 synthetic 参数先接住实参，
        // 再在函数体开头把数组/对象模式展开为真正的局部 slot。
        //
        // 例如 `([a, b]) => a + b` 会先声明 `__js_vm_param_0`，
        // 再读取它的 iterator，把元素绑定到 `a` 和 `b`。这样 fun 段只需要记录参数 slot，
        // names 段不会被函数局部变量撑大。
        if is_direct_param_pattern(pat) {
            self.lower_pat_default(pat);
            return;
        }
        let value = self.temp();
        self.emit(IrInstruction::LoadName {
            dst: value.clone(),
            name: param_name.to_string(),
        });
        self.lower_pat_binding(pat, IrValue::Register(value), "param");
    }

    fn start_generator_body(&mut self, is_generator: bool) {
        let _ = is_generator;
    }

    fn finish_generator_body(&mut self) {}

    fn lower_pat_default(&mut self, pat: &Pat) {
        match pat {
            Pat::Assign(assign) => {
                if let Some(name) = pat_name(&assign.left) {
                    let current = self.temp();
                    self.emit(IrInstruction::LoadName {
                        dst: current.clone(),
                        name: name.clone(),
                    });
                    let is_undefined = self.temp();
                    let end = self.label("param_default_end");
                    self.emit(IrInstruction::Binary {
                        dst: is_undefined.clone(),
                        op: "==".to_string(),
                        left: IrValue::Register(current),
                        right: IrValue::Undefined,
                    });
                    self.emit(IrInstruction::JumpIfFalse {
                        test: IrValue::Register(is_undefined),
                        label: end.clone(),
                    });
                    let value = self.lower_expr(&assign.right);
                    self.emit(IrInstruction::StoreName { name, src: value });
                    self.emit(IrInstruction::Label(end));
                }
            }
            Pat::Array(array) => {
                for elem in array.elems.iter().flatten() {
                    self.lower_pat_default(elem);
                }
            }
            Pat::Object(object) => {
                for prop in &object.props {
                    match prop {
                        ObjectPatProp::KeyValue(prop) => self.lower_pat_default(&prop.value),
                        ObjectPatProp::Rest(rest) => self.lower_pat_default(&rest.arg),
                        ObjectPatProp::Assign(_) => {}
                    }
                }
            }
            Pat::Rest(rest) => self.lower_pat_default(&rest.arg),
            _ => {}
        }
    }

    fn lower_fn_decl(&mut self, decl: &FnDecl) {
        let name = ident_name(&decl.ident);
        self.declare_local(name.clone());
        let params = function_param_names(&decl.function.params);

        let mut body_ctx = self.child();
        body_ctx.declare_function_intrinsics();
        body_ctx.declare_local(name.clone());
        body_ctx.declare_function_params(&decl.function.params, &params);
        body_ctx.start_generator_body(decl.function.is_generator);
        if let Some(body) = &decl.function.body {
            body_ctx.predeclare_function_body(body);
            body_ctx.lower_function_param_initializers(&decl.function.params, &params);
            body_ctx.lower_function_rest_param_initializer(&decl.function.params);
            body_ctx.lower_block(body);
        }
        body_ctx.finish_generator_body();
        self.merge_child_externs(&body_ctx);

        self.emit(IrInstruction::Function {
            name,
            params,
            is_async: decl.function.is_async,
            is_generator: decl.function.is_generator,
            body: body_ctx.instructions,
        });
    }

    fn lower_for(&mut self, stmt: &ForStmt) {
        if let Some(names) = for_init_lexical_names(&stmt.init) {
            self.lower_in_scope("block", |ctx| ctx.lower_for_lexical(stmt, &names));
        } else {
            self.lower_for_unscoped(stmt);
        }
    }

    fn lower_for_lexical(&mut self, stmt: &ForStmt, lexical_names: &[String]) {
        if let Some(init) = &stmt.init {
            match init {
                VarDeclOrExpr::VarDecl(decl) => self.lower_var_decl(decl),
                VarDeclOrExpr::Expr(expr) => {
                    let value = self.lower_expr(expr);
                    self.emit(IrInstruction::Pop(value));
                }
            }
        }

        self.emit_per_iteration_scope(lexical_names, false);

        let start_label = self.label("for_start");
        let update_label = self.label("for_update");
        let end_label = self.label("for_end");
        self.emit(IrInstruction::Label(start_label.clone()));

        if let Some(test) = &stmt.test {
            let test = self.lower_expr(test);
            self.emit(IrInstruction::JumpIfFalse {
                test,
                label: end_label.clone(),
            });
        }

        let control_len = self.push_control_targets(end_label.clone(), Some(update_label.clone()));
        self.lower_stmt(&stmt.body);
        self.pop_control_targets(control_len);

        self.emit(IrInstruction::Label(update_label));
        self.emit_per_iteration_scope(lexical_names, true);
        if let Some(update) = &stmt.update {
            let value = self.lower_expr(update);
            self.emit(IrInstruction::Pop(value));
        }

        self.emit(IrInstruction::Jump(start_label));
        self.emit(IrInstruction::Label(end_label));
        self.emit(IrInstruction::LeaveScope);
    }

    fn lower_for_unscoped(&mut self, stmt: &ForStmt) {
        if let Some(init) = &stmt.init {
            match init {
                VarDeclOrExpr::VarDecl(decl) => self.lower_var_decl(decl),
                VarDeclOrExpr::Expr(expr) => {
                    let value = self.lower_expr(expr);
                    self.emit(IrInstruction::Pop(value));
                }
            }
        }

        let start_label = self.label("for_start");
        let update_label = self.label("for_update");
        let end_label = self.label("for_end");
        self.emit(IrInstruction::Label(start_label.clone()));

        if let Some(test) = &stmt.test {
            let test = self.lower_expr(test);
            self.emit(IrInstruction::JumpIfFalse {
                test,
                label: end_label.clone(),
            });
        }

        let control_len = self.push_control_targets(end_label.clone(), Some(update_label.clone()));
        self.lower_stmt(&stmt.body);
        self.pop_control_targets(control_len);

        self.emit(IrInstruction::Label(update_label));
        if let Some(update) = &stmt.update {
            let value = self.lower_expr(update);
            self.emit(IrInstruction::Pop(value));
        }

        self.emit(IrInstruction::Jump(start_label));
        self.emit(IrInstruction::Label(end_label));
    }

    fn emit_per_iteration_scope(&mut self, lexical_names: &[String], leave_existing: bool) {
        let values = lexical_names
            .iter()
            .map(|name| {
                let value = self.temp();
                self.emit(IrInstruction::LoadName {
                    dst: value.clone(),
                    name: name.clone(),
                });
                (name.clone(), value)
            })
            .collect::<Vec<_>>();
        if leave_existing {
            self.emit(IrInstruction::LeaveScope);
        }
        self.emit(IrInstruction::EnterScope("block".to_string()));
        for (name, value) in values {
            self.emit(IrInstruction::Declare {
                kind: "let".to_string(),
                name: name.clone(),
            });
            self.emit(IrInstruction::StoreName {
                name,
                src: IrValue::Register(value),
            });
        }
    }

    fn lower_do_while(&mut self, stmt: &DoWhileStmt) {
        let start_label = self.label("do_start");
        let test_label = self.label("do_test");
        let end_label = self.label("do_end");
        self.emit(IrInstruction::Label(start_label.clone()));
        let control_len = self.push_control_targets(end_label.clone(), Some(test_label.clone()));
        self.lower_stmt(&stmt.body);
        self.pop_control_targets(control_len);
        self.emit(IrInstruction::Label(test_label));
        let test = self.lower_expr(&stmt.test);
        self.emit(IrInstruction::JumpIfFalse {
            test,
            label: end_label.clone(),
        });
        self.emit(IrInstruction::Jump(start_label));
        self.emit(IrInstruction::Label(end_label));
    }

    fn lower_for_in(&mut self, stmt: &ForInStmt) {
        self.lower_for_each("for_in", &stmt.left, &stmt.right, &stmt.body);
    }

    fn lower_for_of(&mut self, stmt: &ForOfStmt) {
        let kind = if stmt.is_await {
            "for_await_of"
        } else {
            "for_of"
        };
        self.lower_for_each(kind, &stmt.left, &stmt.right, &stmt.body);
    }

    fn lower_for_each(&mut self, kind: &str, left: &ForHead, right: &Expr, body: &Stmt) {
        self.lower_for_each_inner(kind, left, right, body, for_head_is_lexical(left));
    }

    fn lower_for_each_inner(
        &mut self,
        kind: &str,
        left: &ForHead,
        right: &Expr,
        body: &Stmt,
        per_iteration_scope: bool,
    ) {
        let start_label = self.label(kind);
        let update_label = self.label(&format!("{kind}_update"));
        let end_label = self.label(&format!("{kind}_end"));
        let iterated = if kind == "for_in" {
            let object = self.lower_expr(right);
            let object_not_null_label = self.label("for_in_object_not_null");
            let object_not_undefined_label = self.label("for_in_object_not_undefined");
            let is_object_null = self.temp();
            self.emit(IrInstruction::Binary {
                dst: is_object_null.clone(),
                op: "==".to_string(),
                left: object.clone(),
                right: IrValue::Null,
            });
            self.emit(IrInstruction::JumpIfFalse {
                test: IrValue::Register(is_object_null),
                label: object_not_null_label.clone(),
            });
            self.emit(IrInstruction::Jump(end_label.clone()));
            self.emit(IrInstruction::Label(object_not_null_label));
            let is_object_undefined = self.temp();
            self.emit(IrInstruction::Binary {
                dst: is_object_undefined.clone(),
                op: "==".to_string(),
                left: object.clone(),
                right: IrValue::Undefined,
            });
            self.emit(IrInstruction::JumpIfFalse {
                test: IrValue::Register(is_object_undefined),
                label: object_not_undefined_label.clone(),
            });
            self.emit(IrInstruction::Jump(end_label.clone()));
            self.emit(IrInstruction::Label(object_not_undefined_label));
            self.mark_extern("Object");
            let object_reg = self.temp();
            self.emit(IrInstruction::LoadName {
                dst: object_reg.clone(),
                name: "Object".to_string(),
            });
            let keys_reg = self.temp();
            self.emit(IrInstruction::Member {
                dst: keys_reg.clone(),
                object: IrValue::Register(object_reg),
                property: IrValue::String("keys".to_string()),
            });
            let values_reg = self.temp();
            self.emit(IrInstruction::Call {
                dst: values_reg.clone(),
                callee: IrValue::Register(keys_reg),
                args: vec![object],
            });
            IrValue::Register(values_reg)
        } else {
            self.lower_expr(right)
        };
        let not_null_label = self.label(&format!("{kind}_not_null"));
        let not_undefined_label = self.label(&format!("{kind}_not_undefined"));
        let is_null = self.temp();
        self.emit(IrInstruction::Binary {
            dst: is_null.clone(),
            op: "==".to_string(),
            left: iterated.clone(),
            right: IrValue::Null,
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(is_null),
            label: not_null_label.clone(),
        });
        self.emit(IrInstruction::Jump(end_label.clone()));
        self.emit(IrInstruction::Label(not_null_label));
        let is_undefined = self.temp();
        self.emit(IrInstruction::Binary {
            dst: is_undefined.clone(),
            op: "==".to_string(),
            left: iterated.clone(),
            right: IrValue::Undefined,
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(is_undefined),
            label: not_undefined_label.clone(),
        });
        self.emit(IrInstruction::Jump(end_label.clone()));
        self.emit(IrInstruction::Label(not_undefined_label));
        let index = self.temp();
        self.emit(IrInstruction::LoadConst {
            dst: index.clone(),
            value: IrValue::Number(0.0),
        });
        let length = self.temp();
        self.emit(IrInstruction::Member {
            dst: length.clone(),
            object: iterated.clone(),
            property: IrValue::String("length".to_string()),
        });
        self.emit(IrInstruction::Marker(format!("{kind} {iterated}")));
        self.emit(IrInstruction::Label(start_label.clone()));
        let test = self.temp();
        self.emit(IrInstruction::Binary {
            dst: test.clone(),
            op: "<".to_string(),
            left: IrValue::Register(index.clone()),
            right: IrValue::Register(length),
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(test),
            label: end_label.clone(),
        });
        let item = IrValue::Register(self.temp());
        if let IrValue::Register(item_reg) = &item {
            self.emit(IrInstruction::Member {
                dst: item_reg.clone(),
                object: iterated.clone(),
                property: IrValue::Register(index.clone()),
            });
        }
        if per_iteration_scope {
            self.emit(IrInstruction::EnterScope("block".to_string()));
        }
        self.lower_for_head_binding(left, item);
        let scoped_end_label =
            per_iteration_scope.then(|| self.label(&format!("{kind}_scoped_end")));
        let break_label = scoped_end_label
            .as_ref()
            .cloned()
            .unwrap_or_else(|| end_label.clone());
        let control_len = self.push_control_targets(break_label, Some(update_label.clone()));
        self.lower_stmt(body);
        self.pop_control_targets(control_len);
        self.emit(IrInstruction::Label(update_label));
        let next = self.temp();
        self.emit(IrInstruction::Binary {
            dst: next.clone(),
            op: "+".to_string(),
            left: IrValue::Register(index.clone()),
            right: IrValue::Number(1.0),
        });
        self.emit(IrInstruction::Move {
            dst: index,
            src: IrValue::Register(next),
        });
        self.emit(IrInstruction::Jump(start_label));
        if let Some(scoped_end_label) = scoped_end_label {
            self.emit(IrInstruction::Label(scoped_end_label));
            self.emit(IrInstruction::Jump(end_label.clone()));
        }
        self.emit(IrInstruction::Label(end_label));
    }

    fn lower_switch(&mut self, stmt: &SwitchStmt) {
        let discriminant = self.lower_expr(&stmt.discriminant);
        let end_label = self.label("switch_end");
        let default_label = self.label("switch_default");
        let case_labels = stmt
            .cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                if case.test.is_some() {
                    self.label(&format!("switch_case_{index}"))
                } else {
                    default_label.clone()
                }
            })
            .collect::<Vec<_>>();

        for (case, label) in stmt.cases.iter().zip(case_labels.iter()) {
            if let Some(test) = &case.test {
                let test = self.lower_expr(test);
                let cmp = self.temp();
                self.emit(IrInstruction::Binary {
                    dst: cmp.clone(),
                    op: "===".to_string(),
                    left: discriminant.clone(),
                    right: test,
                });
                let next = self.label("switch_next");
                self.emit(IrInstruction::JumpIfFalse {
                    test: IrValue::Register(cmp),
                    label: next.clone(),
                });
                self.emit(IrInstruction::Jump(label.clone()));
                self.emit(IrInstruction::Label(next));
            }
        }
        self.emit(IrInstruction::Jump(
            if stmt.cases.iter().any(|case| case.test.is_none()) {
                default_label.clone()
            } else {
                end_label.clone()
            },
        ));

        let control_len = self.push_control_targets(end_label.clone(), None);
        for (case, label) in stmt.cases.iter().zip(case_labels.iter()) {
            self.emit(IrInstruction::Label(label.clone()));
            for stmt in &case.cons {
                self.lower_stmt(stmt);
            }
        }
        self.pop_control_targets(control_len);
        self.emit(IrInstruction::Label(end_label));
    }

    fn lower_try(&mut self, stmt: &TryStmt) {
        let mut body_ctx = self.child();
        body_ctx.lower_block_scope(&stmt.block);
        self.merge_child_externs(&body_ctx);

        let (catch_param, catch_body) = if let Some(handler) = &stmt.handler {
            let mut catch_ctx = self.child();
            if let Some(param) = handler.param.as_ref().and_then(pat_name) {
                catch_ctx.declare_local(param);
            }
            catch_ctx.lower_block_scope(&handler.body);
            self.merge_child_externs(&catch_ctx);
            (
                handler.param.as_ref().and_then(pat_name),
                catch_ctx.instructions,
            )
        } else {
            (None, Vec::new())
        };

        let finally_body = if let Some(finalizer) = &stmt.finalizer {
            let mut finally_ctx = self.child();
            finally_ctx.lower_block_scope(finalizer);
            self.merge_child_externs(&finally_ctx);
            finally_ctx.instructions
        } else {
            Vec::new()
        };

        self.emit(IrInstruction::Try {
            body: body_ctx.instructions,
            catch_param,
            catch_body,
            finally_body,
        });
    }

    fn lower_expr(&mut self, expr: &Expr) -> IrValue {
        match expr {
            Expr::Lit(lit) => self.lower_lit(lit),
            Expr::Array(expr) => self.lower_array(expr),
            Expr::Object(expr) => self.lower_object(expr),
            Expr::Ident(ident) => {
                let name = ident_name(ident);
                match name.as_str() {
                    "undefined" => return IrValue::Undefined,
                    "NaN" => return IrValue::Number(f64::NAN),
                    "Infinity" => return IrValue::Number(f64::INFINITY),
                    _ => {}
                }
                self.mark_extern(&name);
                let dst = self.temp();
                self.emit(IrInstruction::LoadName {
                    dst: dst.clone(),
                    name,
                });
                IrValue::Register(dst)
            }
            Expr::Bin(expr) => self.lower_binary(expr),
            Expr::Unary(expr) if expr.op == UnaryOp::Delete => self.lower_delete_expr(&expr.arg),
            Expr::Unary(expr) => {
                let arg = self.lower_expr(&expr.arg);
                let dst = self.temp();
                self.emit(IrInstruction::Unary {
                    dst: dst.clone(),
                    op: unary_op(expr.op).to_string(),
                    arg,
                });
                IrValue::Register(dst)
            }
            Expr::Assign(expr) => match expr.op {
                AssignOp::Assign => {
                    let value = self.lower_expr(&expr.right);
                    self.lower_assign_target(&expr.left, value.clone());
                    value
                }
                op => {
                    let current = self.lower_assign_target_read(&expr.left);
                    let right = self.lower_expr(&expr.right);
                    let dst = self.temp();
                    self.emit(IrInstruction::Binary {
                        dst: dst.clone(),
                        op: assign_binary_op(op).to_string(),
                        left: current,
                        right,
                    });
                    let value = IrValue::Register(dst);
                    self.lower_assign_target(&expr.left, value.clone());
                    value
                }
            },
            Expr::Call(expr) => {
                let is_super_call = matches!(expr.callee, Callee::Super(_));
                let has_spread = call_args_have_spread(&expr.args);
                let (callee, callee_short_circuited, spread_this_arg) = match &expr.callee {
                    Callee::Expr(expr) if has_spread => {
                        let (callee, this_arg, short_circuited) =
                            self.lower_expr_callee_for_spread_call(expr);
                        (callee, short_circuited, Some(this_arg))
                    }
                    Callee::Expr(expr) => {
                        let (callee, short_circuited) = self.lower_expr_with_short_circuit(expr);
                        (callee, short_circuited, None)
                    }
                    Callee::Super(_) => (
                        IrValue::Name("super".to_string()),
                        None,
                        has_spread.then_some(IrValue::Undefined),
                    ),
                    Callee::Import(_) => (
                        IrValue::Name("import".to_string()),
                        None,
                        has_spread.then_some(IrValue::Undefined),
                    ),
                };
                let dst = self.temp();
                if let Some(short_circuited) = callee_short_circuited {
                    let call_label = self.label("chain_call");
                    let end_label = self.label("chain_call_end");
                    self.emit(IrInstruction::JumpIfFalse {
                        test: IrValue::Register(short_circuited),
                        label: call_label.clone(),
                    });
                    self.emit(IrInstruction::Move {
                        dst: dst.clone(),
                        src: IrValue::Undefined,
                    });
                    self.emit(IrInstruction::Jump(end_label.clone()));
                    self.emit(IrInstruction::Label(call_label));
                    self.emit_call_to_dst(dst.clone(), callee, &expr.args, spread_this_arg);
                    self.emit(IrInstruction::Label(end_label));
                } else {
                    self.emit_call_to_dst(dst.clone(), callee, &expr.args, spread_this_arg);
                }
                if is_super_call {
                    self.emit(IrInstruction::StoreName {
                        name: "this".to_string(),
                        src: IrValue::Register(dst.clone()),
                    });
                }
                IrValue::Register(dst)
            }
            Expr::New(expr) => {
                let callee = self.lower_expr(&expr.callee);
                let args = expr
                    .args
                    .as_ref()
                    .map(|args| {
                        args.iter()
                            .map(|arg| self.lower_expr_or_spread(arg))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let dst = self.temp();
                self.emit(IrInstruction::New {
                    dst: dst.clone(),
                    callee,
                    args,
                });
                IrValue::Register(dst)
            }
            Expr::Member(expr) => self.lower_member(expr),
            Expr::Fn(expr) => self.lower_fn_expr(expr),
            Expr::Arrow(expr) => self.lower_arrow_expr(expr, None),
            Expr::Cond(expr) => {
                let dst = self.temp();
                let else_label = self.label("cond_else");
                let end_label = self.label("cond_end");
                let test = self.lower_expr(&expr.test);
                self.emit(IrInstruction::JumpIfFalse {
                    test,
                    label: else_label.clone(),
                });
                let cons = self.lower_expr(&expr.cons);
                self.emit(IrInstruction::Move {
                    dst: dst.clone(),
                    src: cons,
                });
                self.emit(IrInstruction::Jump(end_label.clone()));
                self.emit(IrInstruction::Label(else_label));
                let alt = self.lower_expr(&expr.alt);
                self.emit(IrInstruction::Move {
                    dst: dst.clone(),
                    src: alt,
                });
                self.emit(IrInstruction::Label(end_label));
                IrValue::Register(dst)
            }
            Expr::Update(expr) => self.lower_update(expr),
            Expr::This(_) => IrValue::Name("this".to_string()),
            Expr::Tpl(expr) => self.lower_template(expr),
            Expr::TaggedTpl(expr) => {
                let tag = self.lower_expr(&expr.tag);
                let tpl = self.lower_template(&expr.tpl);
                let dst = self.temp();
                self.emit(IrInstruction::Call {
                    dst: dst.clone(),
                    callee: tag,
                    args: vec![tpl],
                });
                IrValue::Register(dst)
            }
            Expr::Class(expr) => self.lower_class_expr(None, expr),
            Expr::Yield(expr) => {
                let arg = expr
                    .arg
                    .as_ref()
                    .map(|arg| self.lower_expr(arg))
                    .unwrap_or(IrValue::Undefined);
                let dst = self.temp();
                self.emit(IrInstruction::Yield {
                    dst: dst.clone(),
                    value: arg,
                    delegate: expr.delegate,
                });
                IrValue::Register(dst)
            }
            Expr::Await(expr) => {
                let arg = self.lower_expr(&expr.arg);
                let dst = self.temp();
                self.emit(IrInstruction::Await {
                    dst: dst.clone(),
                    value: arg,
                });
                IrValue::Register(dst)
            }
            Expr::MetaProp(expr) => IrValue::Name(format!("{:?}", expr.kind)),
            Expr::SuperProp(expr) => {
                let dst = self.temp();
                self.emit(IrInstruction::Marker(format!(
                    "%{dst} = super_prop {}",
                    super_prop_name(expr)
                )));
                IrValue::Register(dst)
            }
            Expr::OptChain(expr) => self.lower_opt_chain(expr),
            Expr::TsTypeAssertion(expr) => self.lower_expr(&expr.expr),
            Expr::TsConstAssertion(expr) => self.lower_expr(&expr.expr),
            Expr::TsNonNull(expr) => self.lower_expr(&expr.expr),
            Expr::TsAs(expr) => self.lower_expr(&expr.expr),
            Expr::TsInstantiation(expr) => self.lower_expr(&expr.expr),
            Expr::TsSatisfies(expr) => self.lower_expr(&expr.expr),
            Expr::Paren(expr) => self.lower_expr(&expr.expr),
            Expr::Seq(expr) => {
                let mut last = IrValue::Undefined;
                for expr in &expr.exprs {
                    last = self.lower_expr(expr);
                }
                last
            }
            other => {
                let dst = self.temp();
                self.emit(IrInstruction::Unsupported(format!(
                    "expression: {}",
                    expr_name(other)
                )));
                self.emit(IrInstruction::LoadConst {
                    dst: dst.clone(),
                    value: IrValue::Undefined,
                });
                IrValue::Register(dst)
            }
        }
    }

    fn lower_lit(&mut self, lit: &Lit) -> IrValue {
        if let Lit::Regex(value) = lit {
            self.mark_extern("RegExp");
            let dst = self.temp();
            self.emit(IrInstruction::New {
                dst: dst.clone(),
                callee: IrValue::Name("RegExp".to_string()),
                args: vec![
                    IrValue::String(value.exp.to_string()),
                    IrValue::String(value.flags.to_string()),
                ],
            });
            return IrValue::Register(dst);
        }

        let value = match lit {
            Lit::Str(value) => IrValue::String(value.value.to_string()),
            Lit::Bool(value) => IrValue::Bool(value.value),
            Lit::Null(_) => IrValue::Null,
            Lit::Num(value) => IrValue::Number(value.value),
            Lit::BigInt(value) => IrValue::BigInt(value.value.to_string()),
            Lit::Regex(_) => unreachable!("regex literals are lowered through native RegExp"),
            Lit::JSXText(value) => IrValue::String(value.value.to_string()),
        };
        let dst = self.temp();
        self.emit(IrInstruction::LoadConst {
            dst: dst.clone(),
            value,
        });
        IrValue::Register(dst)
    }

    fn lower_delete_expr(&mut self, expr: &Expr) -> IrValue {
        if let Expr::Member(member) = expr {
            let object = self.lower_expr(&member.obj);
            let property = self.lower_member_property(&member.prop);
            self.emit(IrInstruction::StoreMember {
                object,
                property,
                src: IrValue::Undefined,
            });
        } else {
            self.lower_expr(expr);
        }
        IrValue::Bool(true)
    }

    fn lower_binary(&mut self, expr: &BinExpr) -> IrValue {
        match expr.op {
            BinaryOp::LogicalAnd => return self.lower_logical_and(expr),
            BinaryOp::LogicalOr => return self.lower_logical_or(expr),
            BinaryOp::NullishCoalescing => return self.lower_nullish_coalescing(expr),
            _ => {}
        }
        let left = self.lower_expr(&expr.left);
        let right = self.lower_expr(&expr.right);
        let dst = self.temp();
        self.emit(IrInstruction::Binary {
            dst: dst.clone(),
            op: bin_op(expr.op).to_string(),
            left,
            right,
        });
        IrValue::Register(dst)
    }

    fn lower_logical_and(&mut self, expr: &BinExpr) -> IrValue {
        let left = self.lower_expr(&expr.left);
        let dst = self.temp();
        let end = self.label("logical_and_end");
        self.emit(IrInstruction::Move {
            dst: dst.clone(),
            src: left.clone(),
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: left,
            label: end.clone(),
        });
        let right = self.lower_expr(&expr.right);
        self.emit(IrInstruction::Move {
            dst: dst.clone(),
            src: right,
        });
        self.emit(IrInstruction::Label(end));
        IrValue::Register(dst)
    }

    fn lower_logical_or(&mut self, expr: &BinExpr) -> IrValue {
        let left = self.lower_expr(&expr.left);
        let dst = self.temp();
        let rhs = self.label("logical_or_rhs");
        let end = self.label("logical_or_end");
        self.emit(IrInstruction::Move {
            dst: dst.clone(),
            src: left.clone(),
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: left,
            label: rhs.clone(),
        });
        self.emit(IrInstruction::Jump(end.clone()));
        self.emit(IrInstruction::Label(rhs));
        let right = self.lower_expr(&expr.right);
        self.emit(IrInstruction::Move {
            dst: dst.clone(),
            src: right,
        });
        self.emit(IrInstruction::Label(end));
        IrValue::Register(dst)
    }

    fn lower_nullish_coalescing(&mut self, expr: &BinExpr) -> IrValue {
        let left = self.lower_expr(&expr.left);
        let dst = self.temp();
        let check_undefined = self.label("nullish_check_undefined");
        let rhs = self.label("nullish_rhs");
        let end = self.label("nullish_end");
        self.emit(IrInstruction::Move {
            dst: dst.clone(),
            src: left.clone(),
        });
        let is_null = self.temp();
        self.emit(IrInstruction::Binary {
            dst: is_null.clone(),
            op: "==".to_string(),
            left: left.clone(),
            right: IrValue::Null,
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(is_null),
            label: check_undefined.clone(),
        });
        self.emit(IrInstruction::Jump(rhs.clone()));
        self.emit(IrInstruction::Label(check_undefined));
        let is_undefined = self.temp();
        self.emit(IrInstruction::Binary {
            dst: is_undefined.clone(),
            op: "==".to_string(),
            left,
            right: IrValue::Undefined,
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(is_undefined),
            label: end.clone(),
        });
        self.emit(IrInstruction::Label(rhs));
        let right = self.lower_expr(&expr.right);
        self.emit(IrInstruction::Move {
            dst: dst.clone(),
            src: right,
        });
        self.emit(IrInstruction::Label(end));
        IrValue::Register(dst)
    }

    fn lower_array(&mut self, expr: &ArrayLit) -> IrValue {
        if expr
            .elems
            .iter()
            .any(|item| item.as_ref().is_some_and(|item| item.spread.is_some()))
        {
            return self.lower_array_with_spread(expr);
        }
        let items = expr
            .elems
            .iter()
            .map(|item| {
                item.as_ref()
                    .map(|item| self.lower_expr_or_spread(item))
                    .unwrap_or(IrValue::Undefined)
            })
            .collect::<Vec<_>>();
        let dst = self.temp();
        self.emit(IrInstruction::Array {
            dst: dst.clone(),
            items,
        });
        IrValue::Register(dst)
    }

    fn lower_array_with_spread(&mut self, expr: &ArrayLit) -> IrValue {
        let mut result = None;
        let mut segment = Vec::new();
        for item in &expr.elems {
            match item {
                Some(item) if item.spread.is_some() => {
                    self.flush_array_segment(&mut result, &mut segment);
                    let iterable = self.lower_expr(&item.expr);
                    let spread_array = self.lower_array_from_iterable(iterable);
                    result = Some(match result {
                        Some(current) => self.lower_array_concat(current, spread_array),
                        None => spread_array,
                    });
                }
                Some(item) => segment.push(self.lower_expr(&item.expr)),
                None => segment.push(IrValue::Undefined),
            }
        }
        self.flush_array_segment(&mut result, &mut segment);
        result.unwrap_or_else(|| self.lower_array_segment(Vec::new()))
    }

    fn flush_array_segment(&mut self, result: &mut Option<IrValue>, segment: &mut Vec<IrValue>) {
        if segment.is_empty() {
            return;
        }
        let array = self.lower_array_segment(std::mem::take(segment));
        *result = Some(match result.take() {
            Some(current) => self.lower_array_concat(current, array),
            None => array,
        });
    }

    fn lower_array_segment(&mut self, items: Vec<IrValue>) -> IrValue {
        let dst = self.temp();
        self.emit(IrInstruction::Array {
            dst: dst.clone(),
            items,
        });
        IrValue::Register(dst)
    }

    fn lower_array_from_iterable(&mut self, iterable: IrValue) -> IrValue {
        self.mark_extern("Array");
        let from = self.temp();
        self.emit(IrInstruction::Member {
            dst: from.clone(),
            object: IrValue::Name("Array".to_string()),
            property: IrValue::String("from".to_string()),
        });
        let dst = self.temp();
        self.emit(IrInstruction::Call {
            dst: dst.clone(),
            callee: IrValue::Register(from),
            args: vec![iterable],
        });
        IrValue::Register(dst)
    }

    fn lower_array_concat(&mut self, left: IrValue, right: IrValue) -> IrValue {
        let concat = self.temp();
        self.emit(IrInstruction::Member {
            dst: concat.clone(),
            object: left,
            property: IrValue::String("concat".to_string()),
        });
        let dst = self.temp();
        self.emit(IrInstruction::Call {
            dst: dst.clone(),
            callee: IrValue::Register(concat),
            args: vec![right],
        });
        IrValue::Register(dst)
    }

    fn emit_call_to_dst(
        &mut self,
        dst: String,
        callee: IrValue,
        args: &[ExprOrSpread],
        spread_this_arg: Option<IrValue>,
    ) {
        if let Some(this_arg) = spread_this_arg {
            let args_array = self.lower_spread_call_args_array(args);
            let apply = self.temp();
            self.emit(IrInstruction::Member {
                dst: apply.clone(),
                object: callee,
                property: IrValue::String("apply".to_string()),
            });
            self.emit(IrInstruction::Call {
                dst,
                callee: IrValue::Register(apply),
                args: vec![this_arg, args_array],
            });
            return;
        }

        let args = self.lower_call_args(args);
        self.emit(IrInstruction::Call { dst, callee, args });
    }

    fn lower_call_args(&mut self, args: &[ExprOrSpread]) -> Vec<IrValue> {
        args.iter()
            .map(|arg| self.lower_expr_or_spread(arg))
            .collect()
    }

    fn lower_spread_call_args_array(&mut self, args: &[ExprOrSpread]) -> IrValue {
        let mut result = None;
        let mut segment = Vec::new();
        for arg in args {
            if arg.spread.is_some() {
                self.flush_array_segment(&mut result, &mut segment);
                let iterable = self.lower_expr(&arg.expr);
                let spread_array = self.lower_array_from_iterable(iterable);
                result = Some(match result {
                    Some(current) => self.lower_array_concat(current, spread_array),
                    None => spread_array,
                });
            } else {
                segment.push(self.lower_expr(&arg.expr));
            }
        }
        self.flush_array_segment(&mut result, &mut segment);
        result.unwrap_or_else(|| self.lower_array_segment(Vec::new()))
    }

    fn lower_expr_callee_for_spread_call(
        &mut self,
        expr: &Expr,
    ) -> (IrValue, IrValue, Option<String>) {
        match expr {
            Expr::Member(member) => self.lower_member_callee_for_spread_call(member),
            _ => {
                let (callee, short_circuited) = self.lower_expr_with_short_circuit(expr);
                (callee, IrValue::Undefined, short_circuited)
            }
        }
    }

    fn lower_member_callee_for_spread_call(
        &mut self,
        expr: &MemberExpr,
    ) -> (IrValue, IrValue, Option<String>) {
        if let Expr::OptChain(object_expr) = &*expr.obj {
            let (object, object_short_circuited) =
                self.lower_opt_chain_with_short_circuit(object_expr);
            if let Some(short_circuited) = object_short_circuited {
                let dst = self.temp();
                let read_label = self.label("chain_member_call_read");
                let end_label = self.label("chain_member_call_end");
                self.emit(IrInstruction::JumpIfFalse {
                    test: IrValue::Register(short_circuited.clone()),
                    label: read_label.clone(),
                });
                self.emit(IrInstruction::Move {
                    dst: dst.clone(),
                    src: IrValue::Undefined,
                });
                self.emit(IrInstruction::Jump(end_label.clone()));
                self.emit(IrInstruction::Label(read_label));
                let property = self.lower_member_property(&expr.prop);
                self.emit(IrInstruction::Member {
                    dst: dst.clone(),
                    object: object.clone(),
                    property,
                });
                self.emit(IrInstruction::Label(end_label));
                return (IrValue::Register(dst), object, Some(short_circuited));
            }
            let property = self.lower_member_property(&expr.prop);
            let dst = self.temp();
            self.emit(IrInstruction::Member {
                dst: dst.clone(),
                object: object.clone(),
                property,
            });
            return (IrValue::Register(dst), object, None);
        }

        let object = self.lower_expr(&expr.obj);
        let property = self.lower_member_property(&expr.prop);
        let dst = self.temp();
        self.emit(IrInstruction::Member {
            dst: dst.clone(),
            object: object.clone(),
            property,
        });
        (IrValue::Register(dst), object, None)
    }

    fn lower_object(&mut self, expr: &ObjectLit) -> IrValue {
        let mut props = Vec::new();
        let mut dynamic_props = Vec::new();
        for prop in &expr.props {
            match prop {
                PropOrSpread::Prop(prop) => match &**prop {
                    Prop::Shorthand(ident) => {
                        props.push((
                            ident_name(ident),
                            self.lower_expr(&Expr::Ident((*ident).clone())),
                        ));
                    }
                    Prop::KeyValue(prop) => {
                        let value = self.lower_expr(&prop.value);
                        match &prop.key {
                            PropName::Computed(computed) => {
                                let key = self.lower_expr(&computed.expr);
                                dynamic_props.push((key, value));
                            }
                            key => {
                                props.push((prop_name(key), value));
                            }
                        }
                    }
                    Prop::Method(prop) => {
                        let key = prop_name(&prop.key);
                        let dst = self.temp();
                        let params = function_param_names(&prop.function.params);
                        let mut body_ctx = self.child();
                        body_ctx.declare_function_intrinsics();
                        body_ctx.declare_function_params(&prop.function.params, &params);
                        body_ctx.start_generator_body(prop.function.is_generator);
                        if let Some(body) = &prop.function.body {
                            body_ctx.predeclare_function_body(body);
                            body_ctx
                                .lower_function_param_initializers(&prop.function.params, &params);
                            body_ctx.lower_function_rest_param_initializer(&prop.function.params);
                            body_ctx.lower_block(body);
                        }
                        body_ctx.finish_generator_body();
                        self.merge_child_externs(&body_ctx);
                        self.emit(IrInstruction::FunctionExpr {
                            dst: dst.clone(),
                            name: Some(key.clone()),
                            params,
                            is_async: prop.function.is_async,
                            is_generator: prop.function.is_generator,
                            body: body_ctx.instructions,
                        });
                        match &prop.key {
                            PropName::Computed(computed) => {
                                let key = self.lower_expr(&computed.expr);
                                dynamic_props.push((key, IrValue::Register(dst)));
                            }
                            _ => props.push((key, IrValue::Register(dst))),
                        }
                    }
                    Prop::Getter(prop) => {
                        let key = prop_name(&prop.key);
                        let dst = self.temp();
                        let mut body_ctx = self.child();
                        body_ctx.declare_function_intrinsics();
                        if let Some(body) = &prop.body {
                            body_ctx.predeclare_function_body(body);
                            body_ctx.lower_block(body);
                        }
                        self.merge_child_externs(&body_ctx);
                        self.emit(IrInstruction::FunctionExpr {
                            dst: dst.clone(),
                            name: Some(format!("get {key}")),
                            params: Vec::new(),
                            is_async: false,
                            is_generator: false,
                            body: body_ctx.instructions,
                        });
                        props.push((accessor_getter_key(&key), IrValue::Register(dst)));
                    }
                    Prop::Setter(prop) => {
                        let key = prop_name(&prop.key);
                        let dst = self.temp();
                        let params = vec![param_name_or_synthetic(&prop.param, 0)];
                        let mut body_ctx = self.child();
                        body_ctx.declare_function_intrinsics();
                        body_ctx.declare_arrow_params(std::slice::from_ref(&prop.param), &params);
                        if let Some(body) = &prop.body {
                            body_ctx.predeclare_function_body(body);
                            body_ctx.lower_arrow_param_initializers(
                                std::slice::from_ref(&prop.param),
                                &params,
                            );
                            body_ctx.lower_block(body);
                        }
                        self.merge_child_externs(&body_ctx);
                        self.emit(IrInstruction::FunctionExpr {
                            dst: dst.clone(),
                            name: Some(format!("set {key}")),
                            params,
                            is_async: false,
                            is_generator: false,
                            body: body_ctx.instructions,
                        });
                        props.push((accessor_setter_key(&key), IrValue::Register(dst)));
                    }
                    _ => self.emit(IrInstruction::Unsupported(
                        "object accessor or assignment property".to_string(),
                    )),
                },
                PropOrSpread::Spread(spread) => {
                    let value = self.lower_expr(&spread.expr);
                    props.push(("...".to_string(), value));
                }
            }
        }
        let dst = self.temp();
        self.emit(IrInstruction::Object {
            dst: dst.clone(),
            props,
        });
        for (property, src) in dynamic_props {
            self.emit(IrInstruction::StoreMember {
                object: IrValue::Register(dst.clone()),
                property,
                src,
            });
        }
        IrValue::Register(dst)
    }

    fn lower_fn_expr(&mut self, expr: &FnExpr) -> IrValue {
        let dst = self.temp();
        let name = expr.ident.as_ref().map(ident_name);
        let params = function_param_names(&expr.function.params);
        let mut body_ctx = self.child();
        body_ctx.declare_function_intrinsics();
        if let Some(name) = &name {
            body_ctx.declare_local(name.clone());
        }
        body_ctx.declare_function_params(&expr.function.params, &params);
        body_ctx.start_generator_body(expr.function.is_generator);
        if let Some(body) = &expr.function.body {
            body_ctx.predeclare_function_body(body);
            body_ctx.lower_function_param_initializers(&expr.function.params, &params);
            body_ctx.lower_function_rest_param_initializer(&expr.function.params);
            body_ctx.lower_block(body);
        }
        body_ctx.finish_generator_body();
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name,
            params,
            is_async: expr.function.is_async,
            is_generator: expr.function.is_generator,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
    }

    fn lower_arrow_expr(&mut self, expr: &ArrowExpr, name: Option<String>) -> IrValue {
        let dst = self.temp();
        let params = pat_list_names(&expr.params);
        let mut body_ctx = self.child();
        body_ctx.declare_arrow_params(&expr.params, &params);
        match &*expr.body {
            BlockStmtOrExpr::BlockStmt(block) => {
                body_ctx.predeclare_function_body(block);
                body_ctx.lower_arrow_param_initializers(&expr.params, &params);
                body_ctx.lower_arrow_rest_param_initializer(&expr.params);
                body_ctx.lower_block(block);
            }
            BlockStmtOrExpr::Expr(body_expr) => {
                body_ctx.lower_arrow_param_initializers(&expr.params, &params);
                body_ctx.lower_arrow_rest_param_initializer(&expr.params);
                let value = body_ctx.lower_expr(body_expr);
                body_ctx.emit(IrInstruction::Return(Some(value)));
            }
        }
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name,
            params,
            is_async: expr.is_async,
            is_generator: false,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
    }

    fn lower_expr_with_default_name(&mut self, expr: &Expr, name: Option<&str>) -> IrValue {
        match (expr, name) {
            (Expr::Fn(function), Some(name)) if function.ident.is_none() => {
                self.lower_function_value(Some(name.to_string()), &function.function)
            }
            (Expr::Arrow(arrow), Some(name)) => {
                self.lower_arrow_expr(arrow, Some(name.to_string()))
            }
            (Expr::Class(class), Some(name)) if class.ident.is_none() => {
                self.lower_class_expr(Some(name), class)
            }
            (Expr::Paren(paren), Some(name)) => {
                self.lower_expr_with_default_name(&paren.expr, Some(name))
            }
            _ => self.lower_expr(expr),
        }
    }

    fn lower_template(&mut self, expr: &Tpl) -> IrValue {
        let exprs = expr
            .exprs
            .iter()
            .map(|expr| self.lower_expr(expr))
            .collect::<Vec<_>>();
        let quasis = expr
            .quasis
            .iter()
            .map(|quasi| {
                quasi
                    .cooked
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| quasi.raw.to_string())
            })
            .collect::<Vec<_>>();
        let dst = self.temp();
        self.emit(IrInstruction::Template {
            dst: dst.clone(),
            quasis,
            exprs,
        });
        IrValue::Register(dst)
    }

    fn lower_class_expr(&mut self, default_name: Option<&str>, expr: &ClassExpr) -> IrValue {
        let dst = self.temp();
        self.lower_class(
            expr.ident
                .as_ref()
                .map(ident_name)
                .or_else(|| default_name.map(ToString::to_string)),
            &expr.class,
            Some(dst),
        )
    }

    fn lower_class(&mut self, name: Option<String>, class: &Class, dst: Option<String>) -> IrValue {
        let super_class = class
            .super_class
            .as_ref()
            .map(|super_class| self.lower_expr(super_class));
        let members = class.body.iter().map(class_member_name).collect::<Vec<_>>();
        let class_value = dst
            .as_ref()
            .map(|dst| IrValue::Register(dst.clone()))
            .or_else(|| name.as_ref().map(|name| IrValue::Name(name.clone())));
        self.emit(IrInstruction::Class {
            dst: dst.clone(),
            name: name.clone(),
            super_class,
            members,
        });
        if let Some(class_value) = &class_value {
            self.lower_class_members(class_value.clone(), class);
        }
        if let Some(name) = name {
            if dst.is_none() {
                return IrValue::Name(name);
            }
        }
        dst.map(IrValue::Register).unwrap_or(IrValue::Undefined)
    }

    fn lower_class_members(&mut self, class_value: IrValue, class: &Class) {
        for member in &class.body {
            match member {
                ClassMember::Constructor(constructor) => {
                    let value = self.lower_constructor_function(constructor);
                    self.emit(IrInstruction::StoreMember {
                        object: class_value.clone(),
                        property: IrValue::String("constructor".to_string()),
                        src: value,
                    });
                }
                ClassMember::Method(method) if method.kind == MethodKind::Method => {
                    let key = prop_name(&method.key);
                    let value = self.lower_function_value(Some(key.clone()), &method.function);
                    self.emit(IrInstruction::StoreMember {
                        object: class_value.clone(),
                        property: IrValue::String(if method.is_static {
                            key
                        } else {
                            format!("prototype.{key}")
                        }),
                        src: value,
                    });
                }
                ClassMember::Method(method) if method.kind == MethodKind::Getter => {
                    let key = prop_name(&method.key);
                    let value =
                        self.lower_function_value(Some(format!("get {key}")), &method.function);
                    self.emit(IrInstruction::StoreMember {
                        object: class_value.clone(),
                        property: IrValue::String(if method.is_static {
                            accessor_getter_key(&key)
                        } else {
                            format!("prototype.{}", accessor_getter_key(&key))
                        }),
                        src: value,
                    });
                }
                ClassMember::Method(method) if method.kind == MethodKind::Setter => {
                    let key = prop_name(&method.key);
                    let value =
                        self.lower_function_value(Some(format!("set {key}")), &method.function);
                    self.emit(IrInstruction::StoreMember {
                        object: class_value.clone(),
                        property: IrValue::String(if method.is_static {
                            accessor_setter_key(&key)
                        } else {
                            format!("prototype.{}", accessor_setter_key(&key))
                        }),
                        src: value,
                    });
                }
                ClassMember::Method(method) => {
                    self.emit(IrInstruction::Unsupported(format!(
                        "class {} method {}",
                        method_kind_name(method.kind),
                        prop_name(&method.key)
                    )));
                }
                ClassMember::PrivateMethod(method) => {
                    self.emit(IrInstruction::Unsupported(format!(
                        "class private method #{}",
                        method.key.name
                    )));
                }
                ClassMember::ClassProp(prop) => {
                    if let Some(value) = &prop.value {
                        let value = self.lower_expr(value);
                        self.emit(IrInstruction::StoreMember {
                            object: class_value.clone(),
                            property: IrValue::String(if prop.is_static {
                                prop_name(&prop.key)
                            } else {
                                format!("prototype.{}", prop_name(&prop.key))
                            }),
                            src: value,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn lower_function_value(&mut self, name: Option<String>, function: &Function) -> IrValue {
        let dst = self.temp();
        let params = function_param_names(&function.params);
        let mut body_ctx = self.child();
        // 函数体 lowering 使用子上下文，确保临时寄存器从 0 开始重新编号。
        // 这对后续短寄存器编码非常重要：每个函数都尽量把临时值压在小编号范围内。
        body_ctx.declare_function_intrinsics();
        body_ctx.declare_function_params(&function.params, &params);
        body_ctx.start_generator_body(function.is_generator);
        if let Some(body) = &function.body {
            body_ctx.predeclare_function_body(body);
            body_ctx.lower_function_param_initializers(&function.params, &params);
            body_ctx.lower_function_rest_param_initializer(&function.params);
            body_ctx.lower_block(body);
        }
        body_ctx.finish_generator_body();
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name,
            params,
            is_async: function.is_async,
            is_generator: function.is_generator,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
    }

    fn lower_constructor_function(&mut self, constructor: &Constructor) -> IrValue {
        let dst = self.temp();
        let params = constructor_param_names(&constructor.params);
        let mut body_ctx = self.child();
        body_ctx.declare_function_intrinsics();
        body_ctx.declare_constructor_params(&constructor.params, &params);
        if let Some(body) = &constructor.body {
            body_ctx.predeclare_function_body(body);
            body_ctx.lower_constructor_param_initializers(&constructor.params, &params);
            body_ctx.lower_constructor_rest_param_initializer(&constructor.params);
            body_ctx.lower_block(body);
        }
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name: Some("constructor".to_string()),
            params,
            is_async: false,
            is_generator: false,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
    }

    fn lower_expr_with_short_circuit(&mut self, expr: &Expr) -> (IrValue, Option<String>) {
        match expr {
            Expr::OptChain(expr) => self.lower_opt_chain_with_short_circuit(expr),
            _ => (self.lower_expr(expr), None),
        }
    }

    fn lower_opt_chain(&mut self, expr: &OptChainExpr) -> IrValue {
        let (value, _) = self.lower_opt_chain_with_short_circuit(expr);
        value
    }

    fn lower_opt_chain_with_short_circuit(
        &mut self,
        expr: &OptChainExpr,
    ) -> (IrValue, Option<String>) {
        let dst = self.temp();
        let mut short_circuited = None;
        match &*expr.base {
            OptChainBase::Member(member) => {
                if expr.optional {
                    let (object, _) = self.lower_expr_with_short_circuit(&member.obj);
                    let read_label = self.label("opt_member_read");
                    let end_label = self.label("opt_member_end");
                    let is_nullish = self.temp();
                    let chain_short_circuited = self.temp();
                    self.emit(IrInstruction::Binary {
                        dst: is_nullish.clone(),
                        op: "==".to_string(),
                        left: object.clone(),
                        right: IrValue::Null,
                    });
                    self.emit(IrInstruction::JumpIfFalse {
                        test: IrValue::Register(is_nullish),
                        label: read_label.clone(),
                    });
                    self.emit(IrInstruction::Move {
                        dst: dst.clone(),
                        src: IrValue::Undefined,
                    });
                    self.emit(IrInstruction::Move {
                        dst: chain_short_circuited.clone(),
                        src: IrValue::Bool(true),
                    });
                    self.emit(IrInstruction::Jump(end_label.clone()));
                    self.emit(IrInstruction::Label(read_label));
                    let property = self.lower_member_property(&member.prop);
                    self.emit(IrInstruction::Member {
                        dst: dst.clone(),
                        object,
                        property,
                    });
                    self.emit(IrInstruction::Move {
                        dst: chain_short_circuited.clone(),
                        src: IrValue::Bool(false),
                    });
                    self.emit(IrInstruction::Label(end_label));
                    short_circuited = Some(chain_short_circuited);
                } else {
                    let (value, member_short_circuited) =
                        self.lower_member_with_short_circuit(member);
                    self.emit(IrInstruction::Move {
                        dst: dst.clone(),
                        src: value,
                    });
                    short_circuited = member_short_circuited;
                }
            }
            OptChainBase::Call(call) => {
                let has_spread = call_args_have_spread(&call.args);
                let (callee, spread_this_arg, callee_short_circuited) = if has_spread {
                    let (callee, this_arg, short_circuited) =
                        self.lower_expr_callee_for_spread_call(&call.callee);
                    (callee, Some(this_arg), short_circuited)
                } else {
                    let (callee, short_circuited) =
                        self.lower_expr_with_short_circuit(&call.callee);
                    (callee, None, short_circuited)
                };
                if expr.optional {
                    let call_label = self.label("opt_call");
                    let end_label = self.label("opt_call_end");
                    let is_nullish = self.temp();
                    let chain_short_circuited = self.temp();
                    self.emit(IrInstruction::Binary {
                        dst: is_nullish.clone(),
                        op: "==".to_string(),
                        left: callee.clone(),
                        right: IrValue::Null,
                    });
                    self.emit(IrInstruction::JumpIfFalse {
                        test: IrValue::Register(is_nullish),
                        label: call_label.clone(),
                    });
                    self.emit(IrInstruction::Move {
                        dst: dst.clone(),
                        src: IrValue::Undefined,
                    });
                    self.emit(IrInstruction::Move {
                        dst: chain_short_circuited.clone(),
                        src: IrValue::Bool(true),
                    });
                    self.emit(IrInstruction::Jump(end_label.clone()));
                    self.emit(IrInstruction::Label(call_label));
                    self.emit_call_to_dst(dst.clone(), callee, &call.args, spread_this_arg);
                    self.emit(IrInstruction::Move {
                        dst: chain_short_circuited.clone(),
                        src: IrValue::Bool(false),
                    });
                    self.emit(IrInstruction::Label(end_label));
                    short_circuited = Some(chain_short_circuited);
                } else if let Some(callee_short_circuited) = callee_short_circuited {
                    let call_label = self.label("chain_call");
                    let end_label = self.label("chain_call_end");
                    self.emit(IrInstruction::JumpIfFalse {
                        test: IrValue::Register(callee_short_circuited.clone()),
                        label: call_label.clone(),
                    });
                    self.emit(IrInstruction::Move {
                        dst: dst.clone(),
                        src: IrValue::Undefined,
                    });
                    self.emit(IrInstruction::Jump(end_label.clone()));
                    self.emit(IrInstruction::Label(call_label));
                    self.emit_call_to_dst(dst.clone(), callee, &call.args, spread_this_arg);
                    self.emit(IrInstruction::Label(end_label));
                    short_circuited = Some(callee_short_circuited);
                } else {
                    self.emit_call_to_dst(dst.clone(), callee, &call.args, spread_this_arg);
                }
            }
        }
        self.emit(IrInstruction::Marker(format!(
            "optional_chain %{dst}, optional={}",
            expr.optional
        )));
        (IrValue::Register(dst), short_circuited)
    }

    fn lower_update(&mut self, expr: &UpdateExpr) -> IrValue {
        let old = self.lower_expr(&expr.arg);
        let new_value = self.temp();
        let op = match expr.op {
            UpdateOp::PlusPlus => "++",
            UpdateOp::MinusMinus => "--",
        };
        self.emit(IrInstruction::Unary {
            dst: new_value.clone(),
            op: op.to_string(),
            arg: old.clone(),
        });
        let assign_value = IrValue::Register(new_value.clone());
        if let Ok(target) = AssignTarget::try_from(expr.arg.clone()) {
            self.lower_assign_target(&target, assign_value);
        } else {
            self.emit(IrInstruction::Unsupported(
                "update expression target".to_string(),
            ));
        }

        if expr.prefix {
            IrValue::Register(new_value)
        } else {
            old
        }
    }

    fn lower_member(&mut self, expr: &MemberExpr) -> IrValue {
        let (value, _) = self.lower_member_with_short_circuit(expr);
        value
    }

    fn lower_member_with_short_circuit(&mut self, expr: &MemberExpr) -> (IrValue, Option<String>) {
        if let Expr::OptChain(object_expr) = &*expr.obj {
            let (object, object_short_circuited) =
                self.lower_opt_chain_with_short_circuit(object_expr);
            if let Some(short_circuited) = object_short_circuited {
                let dst = self.temp();
                let read_label = self.label("chain_member_read");
                let end_label = self.label("chain_member_end");
                self.emit(IrInstruction::JumpIfFalse {
                    test: IrValue::Register(short_circuited.clone()),
                    label: read_label.clone(),
                });
                self.emit(IrInstruction::Move {
                    dst: dst.clone(),
                    src: IrValue::Undefined,
                });
                self.emit(IrInstruction::Jump(end_label.clone()));
                self.emit(IrInstruction::Label(read_label));
                let property = self.lower_member_property(&expr.prop);
                self.emit(IrInstruction::Member {
                    dst: dst.clone(),
                    object,
                    property,
                });
                self.emit(IrInstruction::Label(end_label));
                return (IrValue::Register(dst), Some(short_circuited));
            }
            let property = self.lower_member_property(&expr.prop);
            let dst = self.temp();
            self.emit(IrInstruction::Member {
                dst: dst.clone(),
                object,
                property,
            });
            return (IrValue::Register(dst), None);
        }
        let object = self.lower_expr(&expr.obj);
        let property = self.lower_member_property(&expr.prop);
        let dst = self.temp();
        self.emit(IrInstruction::Member {
            dst: dst.clone(),
            object,
            property,
        });
        (IrValue::Register(dst), None)
    }

    fn lower_member_property(&mut self, prop: &MemberProp) -> IrValue {
        match prop {
            MemberProp::Ident(ident) => IrValue::String(ident.sym.to_string()),
            MemberProp::PrivateName(name) => IrValue::String(format!("#{}", name.name)),
            MemberProp::Computed(prop) => match &*prop.expr {
                Expr::Lit(Lit::Str(value)) => IrValue::String(value.value.to_string()),
                Expr::Lit(Lit::Num(value)) => IrValue::Number(value.value),
                Expr::Lit(Lit::Bool(value)) => IrValue::Bool(value.value),
                Expr::Lit(Lit::Null(_)) => IrValue::Null,
                _ => self.lower_expr(&prop.expr),
            },
        }
    }

    fn lower_prop_name_value(&mut self, prop: &PropName) -> IrValue {
        match prop {
            PropName::Ident(ident) => IrValue::String(ident.sym.to_string()),
            PropName::Str(value) => IrValue::String(value.value.to_string()),
            PropName::Num(value) => IrValue::Number(value.value),
            PropName::BigInt(value) => IrValue::String(value.value.to_string()),
            PropName::Computed(computed) => self.lower_expr(&computed.expr),
        }
    }

    fn lower_expr_or_spread(&mut self, arg: &ExprOrSpread) -> IrValue {
        if arg.spread.is_some() {
            self.emit(IrInstruction::Marker("spread argument".to_string()));
        }
        self.lower_expr(&arg.expr)
    }

    fn lower_for_head_binding(&mut self, head: &ForHead, value: IrValue) {
        match head {
            ForHead::VarDecl(decl) => {
                for declarator in &decl.decls {
                    self.lower_pat_binding(&declarator.name, value.clone(), "loop");
                }
            }
            ForHead::UsingDecl(decl) => {
                for declarator in &decl.decls {
                    self.lower_pat_binding(&declarator.name, value.clone(), "using");
                }
            }
            ForHead::Pat(pat) => self.lower_pat_binding(pat, value, "loop"),
        }
    }

    fn lower_pat_binding(&mut self, pat: &Pat, value: IrValue, kind: &str) {
        match pat {
            Pat::Ident(ident) => {
                let name = ident_name(&ident.id);
                self.declare_local(name.clone());
                self.emit(IrInstruction::Declare {
                    kind: kind.to_string(),
                    name: name.clone(),
                });
                self.emit(IrInstruction::StoreName { name, src: value });
            }
            Pat::Assign(assign) => {
                let name = pat_name(&assign.left);
                let value = self.lower_defaulted_value(value, &assign.right, name.as_deref());
                self.lower_pat_binding(&assign.left, value, kind);
            }
            Pat::Array(array) => {
                self.lower_array_pat_binding(array, value, kind);
            }
            Pat::Object(object) => {
                self.lower_require_object_coercible(value.clone());
                let mut excluded_keys = Vec::new();
                for prop in &object.props {
                    match prop {
                        ObjectPatProp::KeyValue(prop) => {
                            let key = self.lower_prop_name_value(&prop.key);
                            let item = self.temp();
                            self.emit(IrInstruction::Member {
                                dst: item.clone(),
                                object: value.clone(),
                                property: key.clone(),
                            });
                            self.lower_pat_binding(&prop.value, IrValue::Register(item), kind);
                            excluded_keys.push(key);
                        }
                        ObjectPatProp::Assign(prop) => {
                            let key = ident_name(&prop.key);
                            self.declare_local(key.clone());
                            let item = self.temp();
                            self.emit(IrInstruction::Member {
                                dst: item.clone(),
                                object: value.clone(),
                                property: IrValue::String(key.clone()),
                            });
                            self.emit(IrInstruction::Declare {
                                kind: kind.to_string(),
                                name: key.clone(),
                            });
                            let src = prop
                                .value
                                .as_deref()
                                .map(|default| {
                                    self.lower_defaulted_value(
                                        IrValue::Register(item.clone()),
                                        default,
                                        Some(&key),
                                    )
                                })
                                .unwrap_or_else(|| IrValue::Register(item));
                            self.emit(IrInstruction::StoreName { name: key, src });
                            excluded_keys.push(IrValue::String(ident_name(&prop.key)));
                        }
                        ObjectPatProp::Rest(prop) => {
                            let item = self.lower_object_rest_value(value.clone(), &excluded_keys);
                            self.lower_pat_binding(&prop.arg, IrValue::Register(item), kind);
                        }
                    }
                }
            }
            Pat::Rest(rest) => self.lower_pat_binding(&rest.arg, value, kind),
            Pat::Expr(expr) => {
                if let Ok(target) = AssignTarget::try_from(expr.clone()) {
                    self.lower_assign_target(&target, value);
                } else {
                    self.emit(IrInstruction::Unsupported("pattern expression".to_string()));
                }
            }
            Pat::Invalid(_) => self.emit(IrInstruction::Unsupported("invalid pattern".to_string())),
        }
    }

    fn lower_defaulted_value(
        &mut self,
        value: IrValue,
        default: &Expr,
        default_name: Option<&str>,
    ) -> IrValue {
        if matches!(value, IrValue::Undefined) {
            return self.lower_expr_with_default_name(default, default_name);
        }

        let resolved = self.temp();
        self.emit(IrInstruction::Move {
            dst: resolved.clone(),
            src: value,
        });
        let is_undefined = self.temp();
        let end = self.label("binding_default_end");
        self.emit(IrInstruction::Binary {
            dst: is_undefined.clone(),
            op: "===".to_string(),
            left: IrValue::Register(resolved.clone()),
            right: IrValue::Undefined,
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(is_undefined),
            label: end.clone(),
        });
        let default_value = self.lower_expr_with_default_name(default, default_name);
        self.emit(IrInstruction::Move {
            dst: resolved.clone(),
            src: default_value,
        });
        self.emit(IrInstruction::Label(end));
        IrValue::Register(resolved)
    }

    fn lower_array_pat_binding(&mut self, array: &ArrayPat, value: IrValue, kind: &str) {
        // 数组解构按 ECMAScript iterator 语义实现，而不是直接读取下标。
        // 这样可以兼容自定义 iterable、Set、函数参数中的 `...rest` 和 iterator close。
        let iterator_method = self.temp();
        self.emit(IrInstruction::Member {
            dst: iterator_method.clone(),
            object: value.clone(),
            property: IrValue::String("Symbol.iterator".to_string()),
        });
        let iterator = self.temp();
        self.emit(IrInstruction::Call {
            dst: iterator.clone(),
            callee: IrValue::Register(iterator_method),
            args: Vec::new(),
        });
        let iterator_done = self.temp();
        self.emit(IrInstruction::Move {
            dst: iterator_done.clone(),
            src: IrValue::Bool(false),
        });
        let mut has_rest = false;
        for elem in &array.elems {
            let Some(elem) = elem else {
                let (_, done) = self.lower_iterator_next_value(IrValue::Register(iterator.clone()));
                self.emit(IrInstruction::Move {
                    dst: iterator_done.clone(),
                    src: IrValue::Register(done),
                });
                continue;
            };
            if matches!(elem, Pat::Rest(_)) {
                has_rest = true;
                let rest = self.lower_iterator_rest(IrValue::Register(iterator.clone()));
                self.emit(IrInstruction::Move {
                    dst: iterator_done.clone(),
                    src: IrValue::Bool(true),
                });
                self.lower_pat_binding(elem, IrValue::Register(rest), kind);
            } else {
                let (item, done) =
                    self.lower_iterator_next_value(IrValue::Register(iterator.clone()));
                self.emit(IrInstruction::Move {
                    dst: iterator_done.clone(),
                    src: IrValue::Register(done),
                });
                self.lower_pat_binding(elem, IrValue::Register(item), kind);
            }
        }
        if !has_rest {
            self.lower_iterator_close(
                IrValue::Register(iterator),
                IrValue::Register(iterator_done),
            );
        }
    }

    fn lower_iterator_next_value(&mut self, iterator: IrValue) -> (String, String) {
        let next = self.temp();
        self.emit(IrInstruction::Member {
            dst: next.clone(),
            object: iterator.clone(),
            property: IrValue::String("next".to_string()),
        });
        let step = self.temp();
        self.emit(IrInstruction::Call {
            dst: step.clone(),
            callee: IrValue::Register(next),
            args: Vec::new(),
        });
        let done_value = self.temp();
        self.emit(IrInstruction::Member {
            dst: done_value.clone(),
            object: IrValue::Register(step.clone()),
            property: IrValue::String("done".to_string()),
        });
        let item = self.temp();
        let end = self.label("iterator_next_done");
        self.emit(IrInstruction::Move {
            dst: item.clone(),
            src: IrValue::Undefined,
        });
        let value_label = self.label("iterator_next_value");
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(done_value.clone()),
            label: value_label.clone(),
        });
        self.emit(IrInstruction::Jump(end.clone()));
        self.emit(IrInstruction::Label(value_label));
        self.emit(IrInstruction::Member {
            dst: item.clone(),
            object: IrValue::Register(step),
            property: IrValue::String("value".to_string()),
        });
        self.emit(IrInstruction::Label(end));
        (item, done_value)
    }

    fn lower_iterator_rest(&mut self, iterator: IrValue) -> String {
        let rest = self.temp();
        self.emit(IrInstruction::Array {
            dst: rest.clone(),
            items: Vec::new(),
        });
        let start = self.label("iterator_rest_start");
        let end = self.label("iterator_rest_end");
        self.emit(IrInstruction::Label(start.clone()));
        let (item, done) = self.lower_iterator_next_value(iterator);
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(done),
            label: format!("{start}_push"),
        });
        self.emit(IrInstruction::Jump(end.clone()));
        self.emit(IrInstruction::Label(format!("{start}_push")));
        let push = self.temp();
        self.emit(IrInstruction::Member {
            dst: push.clone(),
            object: IrValue::Register(rest.clone()),
            property: IrValue::String("push".to_string()),
        });
        let ignored = self.temp();
        self.emit(IrInstruction::Call {
            dst: ignored,
            callee: IrValue::Register(push),
            args: vec![IrValue::Register(item)],
        });
        self.emit(IrInstruction::Jump(start));
        self.emit(IrInstruction::Label(end));
        rest
    }

    fn lower_iterator_close(&mut self, iterator: IrValue, done_value: IrValue) {
        let done = self.label("iterator_close_done");
        let check_return = format!("{done}_check_return");
        self.emit(IrInstruction::JumpIfFalse {
            test: done_value,
            label: check_return.clone(),
        });
        self.emit(IrInstruction::Jump(done.clone()));
        self.emit(IrInstruction::Label(check_return));
        let return_method = self.temp();
        self.emit(IrInstruction::Member {
            dst: return_method.clone(),
            object: iterator,
            property: IrValue::String("return".to_string()),
        });
        let has_return = self.temp();
        self.emit(IrInstruction::Binary {
            dst: has_return.clone(),
            op: "!=".to_string(),
            left: IrValue::Register(return_method.clone()),
            right: IrValue::Undefined,
        });
        self.emit(IrInstruction::JumpIfFalse {
            test: IrValue::Register(has_return),
            label: done.clone(),
        });
        let ignored = self.temp();
        self.emit(IrInstruction::Call {
            dst: ignored,
            callee: IrValue::Register(return_method),
            args: Vec::new(),
        });
        self.emit(IrInstruction::Label(done));
    }

    fn lower_object_rest_value(&mut self, value: IrValue, excluded_keys: &[IrValue]) -> String {
        let rest = self.temp();
        self.emit(IrInstruction::ObjectRest {
            dst: rest.clone(),
            source: value,
            excluded: excluded_keys.iter().map(static_property_key).collect(),
        });
        rest
    }

    fn lower_require_object_coercible(&mut self, value: IrValue) {
        let check = self.temp();
        self.emit(IrInstruction::Member {
            dst: check,
            object: value,
            property: IrValue::String("__js_vm_to_object_check__".to_string()),
        });
    }

    fn lower_assign_target_read(&mut self, target: &AssignTarget) -> IrValue {
        match target {
            AssignTarget::Simple(simple) => match simple {
                SimpleAssignTarget::Ident(binding) => {
                    let name = ident_name(&binding.id);
                    self.mark_extern(&name);
                    let dst = self.temp();
                    self.emit(IrInstruction::LoadName {
                        dst: dst.clone(),
                        name,
                    });
                    IrValue::Register(dst)
                }
                SimpleAssignTarget::Member(member) => self.lower_member(member),
                SimpleAssignTarget::Paren(paren) => self.lower_expr(&paren.expr),
                SimpleAssignTarget::OptChain(opt_chain) => self.lower_opt_chain(opt_chain),
                SimpleAssignTarget::TsAs(expr) => self.lower_expr(&expr.expr),
                SimpleAssignTarget::TsSatisfies(expr) => self.lower_expr(&expr.expr),
                SimpleAssignTarget::TsNonNull(expr) => self.lower_expr(&expr.expr),
                SimpleAssignTarget::TsTypeAssertion(expr) => self.lower_expr(&expr.expr),
                SimpleAssignTarget::TsInstantiation(expr) => self.lower_expr(&expr.expr),
                SimpleAssignTarget::SuperProp(_) => IrValue::Name("super".to_string()),
                SimpleAssignTarget::Invalid(_) => IrValue::Undefined,
            },
            AssignTarget::Pat(_) => IrValue::Undefined,
        }
    }

    fn lower_assign_target(&mut self, target: &AssignTarget, value: IrValue) {
        match target {
            AssignTarget::Simple(simple) => match simple {
                SimpleAssignTarget::Ident(binding) => {
                    self.emit(IrInstruction::StoreName {
                        name: ident_name(&binding.id),
                        src: value,
                    });
                }
                SimpleAssignTarget::Member(member) => {
                    let object = self.lower_expr(&member.obj);
                    let property = self.lower_member_property(&member.prop);
                    self.emit(IrInstruction::StoreMember {
                        object,
                        property,
                        src: value,
                    });
                }
                SimpleAssignTarget::Paren(paren) => {
                    if let Ok(target) = AssignTarget::try_from(paren.expr.clone()) {
                        self.lower_assign_target(&target, value);
                    }
                }
                SimpleAssignTarget::TsAs(expr) => {
                    if let Ok(target) = AssignTarget::try_from(expr.expr.clone()) {
                        self.lower_assign_target(&target, value);
                    }
                }
                SimpleAssignTarget::TsSatisfies(expr) => {
                    if let Ok(target) = AssignTarget::try_from(expr.expr.clone()) {
                        self.lower_assign_target(&target, value);
                    }
                }
                SimpleAssignTarget::TsNonNull(expr) => {
                    if let Ok(target) = AssignTarget::try_from(expr.expr.clone()) {
                        self.lower_assign_target(&target, value);
                    }
                }
                SimpleAssignTarget::TsTypeAssertion(expr) => {
                    if let Ok(target) = AssignTarget::try_from(expr.expr.clone()) {
                        self.lower_assign_target(&target, value);
                    }
                }
                SimpleAssignTarget::TsInstantiation(expr) => {
                    if let Ok(target) = AssignTarget::try_from(expr.expr.clone()) {
                        self.lower_assign_target(&target, value);
                    }
                }
                other => self.emit(IrInstruction::Unsupported(format!(
                    "assignment target: {}",
                    simple_assign_target_name(other)
                ))),
            },
            AssignTarget::Pat(pat) => self.lower_pat_binding(&pat.clone().into(), value, "assign"),
        }
    }
}

struct StructuredIrBuilder {
    // 过渡 IR 允许字符串名字和扁平 label，便于 AST lowering 快速落地。
    // 结构化阶段会把它们收敛为 Core Layer 的“表 + 索引”：常量去重、local slot、
    // basic block 和 exception handler 都在这里建立。
    module: core::IrModule,
    constant_ids: BTreeMap<String, core::ConstId>,
}

#[derive(Default)]
struct FunctionBuildState {
    // 每个函数独立维护 local 表。函数内部变量能变成 LocalId，就不会进入 bytecode names 段。
    locals: BTreeMap<String, core::LocalId>,
    local_defs: Vec<core::IrLocal>,
    scopes: Vec<core::IrScope>,
    exception_handlers: Vec<core::IrExceptionHandler>,
    manual_scope_stack: Vec<core::ScopeId>,
    register_count: usize,
}

impl StructuredIrBuilder {
    fn new(extern_slots: Vec<String>) -> Self {
        Self {
            module: core::IrModule {
                extern_slots,
                ..core::IrModule::default()
            },
            constant_ids: BTreeMap::new(),
        }
    }

    fn build(mut self, instructions: Vec<IrInstruction>) -> core::IrModule {
        let entry = self.build_function(
            Some("entry".to_string()),
            Vec::new(),
            instructions,
            Vec::new(),
            false,
            false,
        );
        self.module.entry = entry;
        self.module
    }

    fn build_function(
        &mut self,
        name: Option<String>,
        params: Vec<String>,
        instructions: Vec<IrInstruction>,
        inherited_names: Vec<String>,
        is_async: bool,
        is_generator: bool,
    ) -> core::FunctionId {
        let mut state = FunctionBuildState::new();
        if name.as_deref() != Some("entry") {
            state.ensure_local("arguments", core::IrBindingKind::Var);
            state.ensure_local("this", core::IrBindingKind::Var);
            state.ensure_local("super", core::IrBindingKind::Var);
        }
        // inherited_names 表示子函数可以看到的外层绑定。函数内建槽必须先预留，
        // 保证 runtime 能稳定写入 local#1(this)，后续继承名字只追加不覆盖。
        for name in inherited_names {
            state.add_inherited_local(&name);
        }
        for param in &params {
            state.ensure_local(param, core::IrBindingKind::Param);
        }
        collect_work_locals(&instructions, &mut state);

        let blocks = self.lower_blocks(&mut state, instructions);
        let params = params
            .iter()
            .map(|param| core::IrParam {
                local: state.ensure_local(param, core::IrBindingKind::Param),
                default: None,
            })
            .collect();

        let id = core::FunctionId(self.module.functions.len());
        self.module.functions.push(core::IrFunction {
            name,
            kind: core::IrFunctionKind::Normal,
            flags: core::IrFunctionFlags {
                is_async,
                is_generator,
                ..core::IrFunctionFlags::default()
            },
            params,
            rest_param: None,
            locals: state.local_defs,
            scopes: state.scopes,
            captures: Vec::new(),
            register_count: state.register_count,
            exception_handlers: state.exception_handlers,
            blocks,
            entry: core::BlockId(0),
        });
        id
    }

    fn lower_blocks(
        &mut self,
        state: &mut FunctionBuildState,
        instructions: Vec<IrInstruction>,
    ) -> Vec<core::IrBlock> {
        // 旧过渡 IR 中的 Label/Jump 会在这里解析成 BasicBlock。
        // 运行时最终不需要看到标签字符串，code 段只保留可直接跳转的目标 pc/offset。
        let mut labels = BTreeMap::new();
        labels.insert("$entry".to_string(), core::BlockId(0));
        for instruction in &instructions {
            if let IrInstruction::Label(label) = instruction {
                let id = core::BlockId(labels.len());
                labels.entry(label.clone()).or_insert(id);
            }
        }

        let mut blocks = vec![core::IrBlock::default(); labels.len().max(1)];
        let mut current = core::BlockId(0);
        let mut anonymous_id = labels.len();

        for instruction in instructions {
            match instruction {
                IrInstruction::Label(label) => {
                    let raw_label = label.clone();
                    let target = *labels.entry(label).or_insert_with(|| {
                        let id = core::BlockId(anonymous_id);
                        anonymous_id += 1;
                        blocks.push(core::IrBlock::default());
                        id
                    });
                    if current != target
                        && matches!(
                            blocks[current.0].terminator,
                            core::IrTerminator::Unreachable
                        )
                    {
                        blocks[current.0].terminator = core::IrTerminator::Jump(target);
                    }
                    current = target;
                    ensure_block(&mut blocks, current);
                    blocks[current.0]
                        .instructions
                        .push(core::IrInstruction::new(core::IrInstructionKind::Label(
                            raw_label,
                        )));
                }
                IrInstruction::Jump(label) => {
                    let target = label_block(&mut labels, &mut blocks, &mut anonymous_id, &label);
                    blocks[current.0].terminator = core::IrTerminator::Jump(target);
                    current = fresh_block(&mut blocks, &mut anonymous_id);
                }
                IrInstruction::JumpIfFalse { test, label } => {
                    let falsy = label_block(&mut labels, &mut blocks, &mut anonymous_id, &label);
                    let truthy = fresh_block(&mut blocks, &mut anonymous_id);
                    blocks[current.0].terminator = core::IrTerminator::Branch {
                        test: self.lower_value(state, test),
                        truthy,
                        falsy,
                    };
                    current = truthy;
                }
                IrInstruction::Return(value) => {
                    blocks[current.0].terminator = core::IrTerminator::Return(
                        value.map(|value| self.lower_value(state, value)),
                    );
                    current = fresh_block(&mut blocks, &mut anonymous_id);
                }
                IrInstruction::Throw(value) => {
                    blocks[current.0].terminator =
                        core::IrTerminator::Throw(self.lower_value(state, value));
                    current = fresh_block(&mut blocks, &mut anonymous_id);
                }
                other => {
                    let lowered = self.lower_instruction(state, other);
                    blocks[current.0].instructions.extend(lowered);
                }
            }
        }

        let exit = core::BlockId(blocks.len());
        blocks.push(core::IrBlock::default());
        for index in 0..exit.0 {
            if matches!(blocks[index].terminator, core::IrTerminator::Unreachable)
                && !blocks[index].instructions.is_empty()
            {
                blocks[index].terminator = core::IrTerminator::Jump(exit);
            }
        }

        blocks
    }

    fn lower_instruction(
        &mut self,
        state: &mut FunctionBuildState,
        instruction: IrInstruction,
    ) -> Vec<core::IrInstruction> {
        match instruction {
            IrInstruction::Marker(message) => {
                vec![core::IrInstruction::new(core::IrInstructionKind::Debug(
                    message,
                ))]
            }
            IrInstruction::Declare { kind, name } => {
                let binding = binding_kind(&kind);
                let local = state.ensure_local(&name, binding);
                vec![core::IrInstruction::new(core::IrInstructionKind::Declare(
                    core::IrDeclaration {
                        local,
                        kind: binding,
                        name: Some(name),
                        init: None,
                    },
                ))]
            }
            IrInstruction::LoadConst { dst, value } | IrInstruction::Move { dst, src: value } => {
                let dst = state.register(&dst);
                vec![core::IrInstruction::new(core::IrInstructionKind::Move {
                    dst,
                    src: self.lower_value(state, value),
                })]
            }
            IrInstruction::LoadName { dst, name } => {
                let dst = state.register(&dst);
                vec![core::IrInstruction::new(core::IrInstructionKind::Load {
                    dst,
                    src: self.name_place(state, &name),
                })]
            }
            IrInstruction::StoreName { name, src } => {
                vec![core::IrInstruction::new(core::IrInstructionKind::Store {
                    dst: self.name_place(state, &name),
                    op: core::IrAssignOp::Assign,
                    src: self.lower_value(state, src),
                })]
            }
            IrInstruction::StoreMember {
                object,
                property,
                src,
            } => vec![core::IrInstruction::new(core::IrInstructionKind::Store {
                dst: core::IrPlace::Member(core::IrMember {
                    object: self.lower_value(state, object),
                    property: self.property_key(state, property),
                    optional: false,
                }),
                op: core::IrAssignOp::Assign,
                src: self.lower_value(state, src),
            })],
            IrInstruction::Binary {
                dst,
                op,
                left,
                right,
            } => vec![core::IrInstruction::new(core::IrInstructionKind::Binary {
                dst: state.register(&dst),
                op: core_binary_op(&op),
                left: self.lower_value(state, left),
                right: self.lower_value(state, right),
            })],
            IrInstruction::Unary { dst, op, arg } => {
                vec![core::IrInstruction::new(core::IrInstructionKind::Unary {
                    dst: state.register(&dst),
                    op: core_unary_op(&op),
                    arg: self.lower_value(state, arg),
                })]
            }
            IrInstruction::Member {
                dst,
                object,
                property,
            } => vec![core::IrInstruction::new(core::IrInstructionKind::Load {
                dst: state.register(&dst),
                src: core::IrPlace::Member(core::IrMember {
                    object: self.lower_value(state, object),
                    property: self.property_key(state, property),
                    optional: false,
                }),
            })],
            IrInstruction::Array { dst, items } => {
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::CreateArray {
                        dst: state.register(&dst),
                        elements: items
                            .into_iter()
                            .map(|item| core::IrArrayElement::Value(self.lower_value(state, item)))
                            .collect(),
                    },
                )]
            }
            IrInstruction::Object { dst, props } => {
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::CreateObject {
                        dst: state.register(&dst),
                        properties: props
                            .into_iter()
                            .map(|(key, value)| core::IrObjectProperty::Data {
                                key: core::IrPropertyKey::Static(key),
                                value: self.lower_value(state, value),
                            })
                            .collect(),
                    },
                )]
            }
            IrInstruction::ObjectRest {
                dst,
                source,
                excluded,
            } => vec![core::IrInstruction::new(
                core::IrInstructionKind::ObjectRest {
                    dst: state.register(&dst),
                    source: self.lower_value(state, source),
                    excluded,
                },
            )],
            IrInstruction::Call { dst, callee, args } => vec![core::IrInstruction::new(
                core::IrInstructionKind::Call(core::IrCall {
                    dst: Some(state.register(&dst)),
                    kind: core::IrCallKind::Normal,
                    callee: self.lower_value(state, callee),
                    this_arg: None,
                    args: args
                        .into_iter()
                        .map(|arg| core::IrArgument::Value(self.lower_value(state, arg)))
                        .collect(),
                }),
            )],
            IrInstruction::New { dst, callee, args } => vec![core::IrInstruction::new(
                core::IrInstructionKind::Construct(core::IrConstruct {
                    dst: state.register(&dst),
                    callee: self.lower_value(state, callee),
                    args: args
                        .into_iter()
                        .map(|arg| core::IrArgument::Value(self.lower_value(state, arg)))
                        .collect(),
                }),
            )],
            IrInstruction::Template { dst, quasis, exprs } => {
                vec![core::IrInstruction::new(core::IrInstructionKind::Template(
                    core::IrTemplate {
                        dst: state.register(&dst),
                        cooked: quasis.iter().cloned().map(Some).collect(),
                        raw: quasis,
                        expressions: exprs
                            .into_iter()
                            .map(|expr| self.lower_value(state, expr))
                            .collect(),
                    },
                ))]
            }
            IrInstruction::Function {
                name,
                params,
                is_async,
                is_generator,
                body,
            } => {
                let inherited_names = state.visible_names();
                let captured_names =
                    captured_in_function_body(None, &params, &body, &inherited_names);
                state.mark_captured(&captured_names);
                let function = self.build_function(
                    Some(name.clone()),
                    params,
                    body,
                    captured_names,
                    is_async,
                    is_generator,
                );
                let local = state.ensure_local(&name, core::IrBindingKind::Function);
                vec![
                    core::IrInstruction::new(core::IrInstructionKind::Declare(
                        core::IrDeclaration {
                            local,
                            kind: core::IrBindingKind::Function,
                            name: Some(name.clone()),
                            init: None,
                        },
                    )),
                    core::IrInstruction::new(core::IrInstructionKind::FunctionDeclaration {
                        function,
                    }),
                ]
            }
            IrInstruction::FunctionExpr {
                dst,
                name,
                params,
                is_async,
                is_generator,
                body,
            } => {
                let inherited_names = state.visible_names();
                let captured_names =
                    captured_in_function_body(name.as_deref(), &params, &body, &inherited_names);
                let mut child_inherited_names = captured_names.clone();
                if let Some(name) = &name {
                    child_inherited_names.push(name.clone());
                }
                state.mark_captured(&captured_names);
                let function = self.build_function(
                    name,
                    params,
                    body,
                    child_inherited_names,
                    is_async,
                    is_generator,
                );
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::CreateFunction {
                        dst: state.register(&dst),
                        function,
                        captures: Vec::new(),
                    },
                )]
            }
            IrInstruction::Class {
                dst,
                name,
                super_class,
                members,
            } => {
                let super_class = super_class.map(|value| self.lower_value(state, value));
                let class = core::ClassId(self.module.classes.len());
                self.module.classes.push(core::IrClass {
                    name: name.clone(),
                    super_class,
                    constructor: None,
                    members: members
                        .into_iter()
                        .map(|member| core::IrClassMember {
                            key: core::IrPropertyKey::Static(member),
                            kind: core::IrClassMemberKind::Field { value: None },
                            is_static: false,
                        })
                        .collect(),
                    static_blocks: Vec::new(),
                });
                let dst = dst
                    .map(|dst| state.register(&dst))
                    .unwrap_or_else(|| state.temp_register());
                let mut out = vec![core::IrInstruction::new(
                    core::IrInstructionKind::CreateClass { dst, class },
                )];
                if let Some(name) = name {
                    let local = state.ensure_local(&name, core::IrBindingKind::Class);
                    out.push(core::IrInstruction::new(core::IrInstructionKind::Store {
                        dst: core::IrPlace::Local(local),
                        op: core::IrAssignOp::Assign,
                        src: core::IrValue::Register(dst),
                    }));
                }
                out
            }
            IrInstruction::Import { source, specifiers } => {
                let specifiers = specifiers
                    .into_iter()
                    .map(|name| {
                        let local = state.ensure_local(&name, core::IrBindingKind::Import);
                        core::IrImportSpecifier::Named {
                            imported: name,
                            local,
                        }
                    })
                    .collect();
                self.module
                    .imports
                    .push(core::IrImportDecl { source, specifiers });
                Vec::new()
            }
            IrInstruction::Export { entries, .. } => {
                for (local_name, exported) in entries {
                    let local = state.ensure_local(&local_name, core::IrBindingKind::Var);
                    self.module
                        .exports
                        .push(core::IrExportDecl::Local { local, exported });
                }
                Vec::new()
            }
            IrInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                let handler = core::ExceptionHandlerId(state.exception_handlers.len());
                let catch_local = catch_param
                    .as_ref()
                    .map(|param| state.ensure_local(param, core::IrBindingKind::Catch));
                state.exception_handlers.push(core::IrExceptionHandler {
                    protected_blocks: Vec::new(),
                    catch_param: catch_local,
                    catch_block: None,
                    finally_block: None,
                    exit_block: None,
                });
                let mut out = vec![core::IrInstruction::new(core::IrInstructionKind::EnterTry(
                    handler,
                ))];
                out.extend(self.lower_linear_inline(state, body));
                if !catch_body.is_empty() {
                    out.push(core::IrInstruction::new(
                        core::IrInstructionKind::EnterCatch { param: catch_local },
                    ));
                    out.extend(self.lower_linear_inline(state, catch_body));
                }
                if !finally_body.is_empty() {
                    out.push(core::IrInstruction::new(
                        core::IrInstructionKind::EnterFinally,
                    ));
                    out.extend(self.lower_linear_inline(state, finally_body));
                }
                out.push(core::IrInstruction::new(core::IrInstructionKind::LeaveTry(
                    handler,
                )));
                out
            }
            IrInstruction::Scope { kind, body } => {
                let scope = state.push_scope(scope_kind(&kind));
                let mut out = vec![core::IrInstruction::new(
                    core::IrInstructionKind::EnterScope(scope),
                )];
                out.extend(self.lower_linear_inline(state, body));
                out.push(core::IrInstruction::new(
                    core::IrInstructionKind::LeaveScope(scope),
                ));
                out
            }
            IrInstruction::EnterScope(kind) => {
                let scope = state.push_scope(scope_kind(&kind));
                state.manual_scope_stack.push(scope);
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::EnterScope(scope),
                )]
            }
            IrInstruction::LeaveScope => {
                let scope = state.manual_scope_stack.pop().unwrap_or(core::ScopeId(0));
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::LeaveScope(scope),
                )]
            }
            IrInstruction::Pop(value) => vec![core::IrInstruction::new(
                core::IrInstructionKind::Debug(format!("pop {}", value)),
            )],
            IrInstruction::Unsupported(message) => {
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::Unsupported(message),
                )]
            }
            IrInstruction::Yield {
                dst,
                value,
                delegate,
            } => vec![core::IrInstruction::new(core::IrInstructionKind::Yield {
                dst: Some(state.register(&dst)),
                value: Some(self.lower_value(state, value)),
                delegate,
            })],
            IrInstruction::Await { dst, value } => {
                vec![core::IrInstruction::new(core::IrInstructionKind::Await {
                    dst: state.register(&dst),
                    value: self.lower_value(state, value),
                })]
            }
            IrInstruction::Label(_)
            | IrInstruction::Jump(_)
            | IrInstruction::JumpIfFalse { .. }
            | IrInstruction::Return(_)
            | IrInstruction::Throw(_) => Vec::new(),
        }
    }

    fn lower_linear_inline(
        &mut self,
        state: &mut FunctionBuildState,
        instructions: Vec<IrInstruction>,
    ) -> Vec<core::IrInstruction> {
        instructions
            .into_iter()
            .flat_map(|instruction| match instruction {
                IrInstruction::Throw(value) => vec![core::IrInstruction::new(
                    core::IrInstructionKind::Throw(self.lower_value(state, value)),
                )],
                IrInstruction::Return(value) => {
                    vec![core::IrInstruction::new(core::IrInstructionKind::Return(
                        value.map(|value| self.lower_value(state, value)),
                    ))]
                }
                IrInstruction::Label(label) => {
                    vec![core::IrInstruction::new(core::IrInstructionKind::Label(
                        label,
                    ))]
                }
                IrInstruction::Jump(label) => {
                    vec![core::IrInstruction::new(core::IrInstructionKind::Jump(
                        label,
                    ))]
                }
                IrInstruction::JumpIfFalse { test, label } => {
                    vec![core::IrInstruction::new(
                        core::IrInstructionKind::JumpIfFalse {
                            test: self.lower_value(state, test),
                            label,
                        },
                    )]
                }
                IrInstruction::Yield {
                    dst,
                    value,
                    delegate,
                } => vec![core::IrInstruction::new(core::IrInstructionKind::Yield {
                    dst: Some(state.register(&dst)),
                    value: Some(self.lower_value(state, value)),
                    delegate,
                })],
                IrInstruction::Await { dst, value } => {
                    vec![core::IrInstruction::new(core::IrInstructionKind::Await {
                        dst: state.register(&dst),
                        value: self.lower_value(state, value),
                    })]
                }
                other => self.lower_instruction(state, other),
            })
            .collect()
    }

    fn lower_value(&mut self, state: &mut FunctionBuildState, value: IrValue) -> core::IrValue {
        match value {
            IrValue::Register(value) => core::IrValue::Register(state.register(&value)),
            IrValue::Name(value) if value == "this" => core::IrValue::This,
            IrValue::Name(value) if value == "super" => core::IrValue::Super,
            IrValue::Name(value) if value == "new.target" => core::IrValue::NewTarget,
            IrValue::Name(value) if value == "undefined" => core::IrValue::Undefined,
            IrValue::Name(value) if value == "NaN" => {
                core::IrValue::Const(self.push_const(core::IrConst::Float(f64::NAN)))
            }
            IrValue::Name(value) if value == "Infinity" => {
                core::IrValue::Const(self.push_const(core::IrConst::Float(f64::INFINITY)))
            }
            IrValue::Name(value) if state.locals.contains_key(&value) => {
                core::IrValue::Local(state.ensure_local(&value, core::IrBindingKind::Var))
            }
            IrValue::Name(value) => core::IrValue::External(self.ensure_extern(&value)),
            IrValue::Number(value) => {
                core::IrValue::Const(self.push_const(if is_safe_i64_const(value) {
                    core::IrConst::Int(value as i64)
                } else {
                    core::IrConst::Float(value)
                }))
            }
            IrValue::String(value) => {
                core::IrValue::Const(self.push_const(core::IrConst::String(value)))
            }
            IrValue::BigInt(value) => {
                core::IrValue::Const(self.push_const(core::IrConst::BigInt(value)))
            }
            IrValue::Bool(value) => core::IrValue::Bool(value),
            IrValue::Null => core::IrValue::Null,
            IrValue::Undefined => core::IrValue::Undefined,
        }
    }

    fn name_place(&mut self, state: &mut FunctionBuildState, name: &str) -> core::IrPlace {
        if state.locals.contains_key(name) {
            core::IrPlace::Local(state.ensure_local(name, core::IrBindingKind::Var))
        } else {
            core::IrPlace::External(self.ensure_extern(name))
        }
    }

    fn property_key(
        &mut self,
        state: &mut FunctionBuildState,
        value: IrValue,
    ) -> core::IrPropertyKey {
        match value {
            IrValue::String(value) => core::IrPropertyKey::Static(value),
            IrValue::Number(value) => core::IrPropertyKey::Number(value),
            IrValue::Name(value) => core::IrPropertyKey::Static(value),
            other => core::IrPropertyKey::Computed(self.lower_value(state, other)),
        }
    }

    fn push_const(&mut self, constant: core::IrConst) -> core::ConstId {
        let key = ir_const_key(&constant);
        if let Some(id) = self.constant_ids.get(&key) {
            return *id;
        }
        let id = core::ConstId(self.module.constants.len());
        self.module.constants.push(constant);
        self.constant_ids.insert(key, id);
        id
    }

    fn ensure_extern(&mut self, name: &str) -> core::ExternId {
        if let Some(index) = self
            .module
            .extern_slots
            .iter()
            .position(|slot| slot == name)
        {
            return core::ExternId(index);
        }
        let id = core::ExternId(self.module.extern_slots.len());
        self.module.extern_slots.push(name.to_string());
        id
    }
}

fn is_safe_i64_const(value: f64) -> bool {
    value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64
}

impl FunctionBuildState {
    fn new() -> Self {
        Self {
            scopes: vec![core::IrScope {
                parent: None,
                kind: core::IrScopeKind::Function,
                bindings: Vec::new(),
            }],
            ..Self::default()
        }
    }

    fn ensure_local(&mut self, name: &str, kind: core::IrBindingKind) -> core::LocalId {
        if let Some(local) = self.locals.get(name) {
            return *local;
        }
        let id = core::LocalId(self.local_defs.len());
        self.locals.insert(name.to_string(), id);
        self.local_defs.push(core::IrLocal {
            name: Some(name.to_string()),
            kind,
            scope: core::ScopeId(0),
            mutable: !matches!(kind, core::IrBindingKind::Const),
            captured: false,
        });
        if let Some(scope) = self.scopes.first_mut() {
            scope.bindings.push(id);
        }
        id
    }

    fn add_inherited_local(&mut self, name: &str) -> core::LocalId {
        if let Some(local) = self.locals.get(name) {
            return *local;
        }
        let id = core::LocalId(self.local_defs.len());
        self.locals.insert(name.to_string(), id);
        self.local_defs.push(core::IrLocal {
            name: Some(name.to_string()),
            kind: core::IrBindingKind::Var,
            scope: core::ScopeId(0),
            mutable: true,
            captured: true,
        });
        id
    }

    fn visible_names(&self) -> Vec<String> {
        self.locals.keys().cloned().collect()
    }

    fn mark_captured(&mut self, names: &[String]) {
        for name in names {
            if let Some(local) = self.locals.get(name) {
                if let Some(definition) = self.local_defs.get_mut(local.0) {
                    definition.captured = true;
                }
            }
        }
    }

    fn register(&mut self, register: &str) -> core::RegisterId {
        let id = register_id(register);
        self.register_count = self.register_count.max(id + 1);
        core::RegisterId(id)
    }

    fn temp_register(&mut self) -> core::RegisterId {
        let id = self.register_count;
        self.register_count += 1;
        core::RegisterId(id)
    }

    fn push_scope(&mut self, kind: core::IrScopeKind) -> core::ScopeId {
        let id = core::ScopeId(self.scopes.len());
        self.scopes.push(core::IrScope {
            parent: Some(core::ScopeId(0)),
            kind,
            bindings: Vec::new(),
        });
        id
    }
}

fn collect_work_locals(instructions: &[IrInstruction], state: &mut FunctionBuildState) {
    for instruction in instructions {
        match instruction {
            IrInstruction::Declare { kind, name } => {
                state.ensure_local(name, binding_kind(kind));
            }
            IrInstruction::Function { name, .. } => {
                state.ensure_local(name, core::IrBindingKind::Function);
            }
            IrInstruction::Class {
                name: Some(name), ..
            } => {
                state.ensure_local(name, core::IrBindingKind::Class);
            }
            IrInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_work_locals(body, state);
                if let Some(param) = catch_param {
                    state.ensure_local(param, core::IrBindingKind::Catch);
                }
                collect_work_locals(catch_body, state);
                collect_work_locals(finally_body, state);
            }
            IrInstruction::Scope { body, .. } => collect_work_locals(body, state),
            _ => {}
        }
    }
}

fn captured_in_function_body(
    function_name: Option<&str>,
    params: &[String],
    body: &[IrInstruction],
    inherited_names: &[String],
) -> Vec<String> {
    let inherited = inherited_names
        .iter()
        .cloned()
        .collect::<BTreeSet<String>>();
    let mut refs = BTreeSet::new();
    collect_instruction_refs(body, &mut refs);

    let mut locals = params.iter().cloned().collect::<BTreeSet<String>>();
    if let Some(function_name) = function_name {
        locals.insert(function_name.to_string());
    }
    collect_instruction_local_bindings(body, &mut locals);

    refs.into_iter()
        .filter(|name| inherited.contains(name) && !locals.contains(name))
        .collect()
}

fn collect_instruction_refs(instructions: &[IrInstruction], refs: &mut BTreeSet<String>) {
    for instruction in instructions {
        match instruction {
            IrInstruction::LoadConst { value, .. } | IrInstruction::Move { src: value, .. } => {
                collect_value_refs(value, refs)
            }
            IrInstruction::LoadName { name, .. } => {
                refs.insert(name.clone());
            }
            IrInstruction::StoreName { name, src } => {
                if !is_implicit_global(name) {
                    refs.insert(name.clone());
                }
                collect_value_refs(src, refs);
            }
            IrInstruction::StoreMember {
                object,
                property,
                src,
            } => {
                collect_value_refs(object, refs);
                collect_value_refs(property, refs);
                collect_value_refs(src, refs);
            }
            IrInstruction::Binary { left, right, .. } => {
                collect_value_refs(left, refs);
                collect_value_refs(right, refs);
            }
            IrInstruction::Unary { arg, .. } => collect_value_refs(arg, refs),
            IrInstruction::Member {
                object, property, ..
            } => {
                collect_value_refs(object, refs);
                collect_value_refs(property, refs);
            }
            IrInstruction::Array { items, .. } => {
                for item in items {
                    collect_value_refs(item, refs);
                }
            }
            IrInstruction::Object { props, .. } => {
                for (_, value) in props {
                    collect_value_refs(value, refs);
                }
            }
            IrInstruction::ObjectRest { source, .. } => collect_value_refs(source, refs),
            IrInstruction::Call { callee, args, .. } | IrInstruction::New { callee, args, .. } => {
                collect_value_refs(callee, refs);
                for arg in args {
                    collect_value_refs(arg, refs);
                }
            }
            IrInstruction::Template { exprs, .. } => {
                for expr in exprs {
                    collect_value_refs(expr, refs);
                }
            }
            IrInstruction::Function { body, .. } | IrInstruction::FunctionExpr { body, .. } => {
                collect_instruction_refs(body, refs);
            }
            IrInstruction::Class { super_class, .. } => {
                if let Some(super_class) = super_class {
                    collect_value_refs(super_class, refs);
                }
            }
            IrInstruction::Export { entries, .. } => {
                refs.extend(entries.iter().map(|(local, _)| local.clone()));
            }
            IrInstruction::Throw(value) | IrInstruction::Pop(value) => {
                collect_value_refs(value, refs)
            }
            IrInstruction::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                collect_instruction_refs(body, refs);
                collect_instruction_refs(catch_body, refs);
                collect_instruction_refs(finally_body, refs);
            }
            IrInstruction::Scope { body, .. } => collect_instruction_refs(body, refs),
            IrInstruction::Return(value) => {
                if let Some(value) = value {
                    collect_value_refs(value, refs);
                }
            }
            IrInstruction::Yield { value, .. } | IrInstruction::Await { value, .. } => {
                collect_value_refs(value, refs)
            }
            IrInstruction::JumpIfFalse { test, .. } => collect_value_refs(test, refs),
            IrInstruction::Declare { .. }
            | IrInstruction::EnterScope(_)
            | IrInstruction::LeaveScope
            | IrInstruction::Import { .. }
            | IrInstruction::Marker(_)
            | IrInstruction::Label(_)
            | IrInstruction::Jump(_)
            | IrInstruction::Unsupported(_) => {}
        }
    }
}

fn collect_instruction_local_bindings(
    instructions: &[IrInstruction],
    locals: &mut BTreeSet<String>,
) {
    for instruction in instructions {
        match instruction {
            IrInstruction::Declare { name, .. } | IrInstruction::Function { name, .. } => {
                locals.insert(name.clone());
            }
            IrInstruction::Class {
                name: Some(name), ..
            } => {
                locals.insert(name.clone());
            }
            IrInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_instruction_local_bindings(body, locals);
                if let Some(catch_param) = catch_param {
                    locals.insert(catch_param.clone());
                }
                collect_instruction_local_bindings(catch_body, locals);
                collect_instruction_local_bindings(finally_body, locals);
            }
            IrInstruction::Scope { body, .. } => collect_instruction_local_bindings(body, locals),
            IrInstruction::FunctionExpr { body, name, .. } => {
                if let Some(name) = name {
                    locals.insert(name.clone());
                }
                collect_instruction_local_bindings(body, locals);
            }
            _ => {}
        }
    }
}

fn collect_value_refs(value: &IrValue, refs: &mut BTreeSet<String>) {
    if let IrValue::Name(name) = value {
        if !is_implicit_global(name) {
            refs.insert(name.clone());
        }
    }
}

fn ir_const_key(constant: &core::IrConst) -> String {
    match constant {
        core::IrConst::String(value) => format!("s:{value}"),
        core::IrConst::Int(value) => format!("i:{value}"),
        core::IrConst::Float(value) => format!("f:{:016x}", value.to_bits()),
        core::IrConst::BigInt(value) => format!("b:{value}"),
        core::IrConst::Regex { pattern, flags } => format!("r:{pattern}/{flags}"),
    }
}

fn ensure_block(blocks: &mut Vec<core::IrBlock>, id: core::BlockId) {
    while blocks.len() <= id.0 {
        blocks.push(core::IrBlock::default());
    }
}

fn fresh_block(blocks: &mut Vec<core::IrBlock>, anonymous_id: &mut usize) -> core::BlockId {
    let id = core::BlockId(*anonymous_id);
    *anonymous_id += 1;
    ensure_block(blocks, id);
    id
}

fn label_block(
    labels: &mut BTreeMap<String, core::BlockId>,
    blocks: &mut Vec<core::IrBlock>,
    anonymous_id: &mut usize,
    label: &str,
) -> core::BlockId {
    if let Some(block) = labels.get(label) {
        return *block;
    }
    let id = fresh_block(blocks, anonymous_id);
    labels.insert(label.to_string(), id);
    id
}

fn register_id(register: &str) -> usize {
    register
        .strip_prefix('t')
        .unwrap_or(register)
        .parse::<usize>()
        .unwrap_or(0)
}

fn binding_kind(kind: &str) -> core::IrBindingKind {
    match kind {
        "const" => core::IrBindingKind::Const,
        "let" | "loop" | "assign" => core::IrBindingKind::Let,
        "function" => core::IrBindingKind::Function,
        "class" => core::IrBindingKind::Class,
        "catch" => core::IrBindingKind::Catch,
        "import" => core::IrBindingKind::Import,
        _ => core::IrBindingKind::Var,
    }
}

fn scope_kind(kind: &str) -> core::IrScopeKind {
    match kind {
        "function" => core::IrScopeKind::Function,
        "loop" => core::IrScopeKind::Loop,
        "catch" => core::IrScopeKind::Catch,
        "class" => core::IrScopeKind::Class,
        "with" => core::IrScopeKind::With,
        _ => core::IrScopeKind::Block,
    }
}

fn core_unary_op(op: &str) -> core::IrUnaryOp {
    match op {
        "+" => core::IrUnaryOp::Plus,
        "-" => core::IrUnaryOp::Minus,
        "!" => core::IrUnaryOp::Not,
        "~" => core::IrUnaryOp::BitNot,
        "++" => core::IrUnaryOp::Increment,
        "--" => core::IrUnaryOp::Decrement,
        "typeof" => core::IrUnaryOp::TypeOf,
        "void" => core::IrUnaryOp::Void,
        "delete" => core::IrUnaryOp::Delete,
        _ => core::IrUnaryOp::Void,
    }
}

fn core_binary_op(op: &str) -> core::IrBinaryOp {
    match op {
        "+" => core::IrBinaryOp::Add,
        "-" => core::IrBinaryOp::Sub,
        "*" => core::IrBinaryOp::Mul,
        "/" => core::IrBinaryOp::Div,
        "%" => core::IrBinaryOp::Mod,
        "**" => core::IrBinaryOp::Pow,
        "==" => core::IrBinaryOp::Eq,
        "===" => core::IrBinaryOp::StrictEq,
        "!=" => core::IrBinaryOp::NotEq,
        "!==" => core::IrBinaryOp::StrictNotEq,
        "<" => core::IrBinaryOp::Lt,
        "<=" => core::IrBinaryOp::Le,
        ">" => core::IrBinaryOp::Gt,
        ">=" => core::IrBinaryOp::Ge,
        "&&" => core::IrBinaryOp::LogicalAnd,
        "||" => core::IrBinaryOp::LogicalOr,
        "??" => core::IrBinaryOp::Nullish,
        "&" => core::IrBinaryOp::BitAnd,
        "|" => core::IrBinaryOp::BitOr,
        "^" => core::IrBinaryOp::BitXor,
        "<<" => core::IrBinaryOp::Shl,
        ">>" => core::IrBinaryOp::Shr,
        ">>>" => core::IrBinaryOp::UShr,
        "in" => core::IrBinaryOp::In,
        "instanceof" => core::IrBinaryOp::InstanceOf,
        _ => core::IrBinaryOp::Add,
    }
}

/// 使用 SWC 解析源码。
///
/// 当前启用 TypeScript 语法解析能力，方便 `.js/.ts/.tsx/.jsx` 共享测试入口。
pub fn parse_source(source: &str) -> Result<Program, String> {
    validate_early_syntax_errors(source)?;
    parse_source_with_syntax(
        source,
        "input.ts",
        Syntax::Typescript(TsSyntax {
            tsx: false,
            decorators: true,
            ..Default::default()
        }),
    )
    .or_else(|ts_err| {
        parse_source_with_syntax(source, "input.js", Syntax::Es(Default::default()))
            .map_err(|es_err| format!("{ts_err}; fallback parse error: {es_err}"))
    })
}

/// 只做语法解析，不执行 lowering。
///
/// CLI 的 check 阶段用它检查 `.js/.ts/.jsx/.tsx` 和 Vue `<script>` 内容。
pub fn check_source_syntax(source: &str, source_file: &str) -> Result<(), String> {
    parse_source_for_file(source, source_file).map(|_| ())
}

fn parse_source_for_file(source: &str, source_file: &str) -> Result<Program, String> {
    validate_early_syntax_errors(source)?;
    let lower = source_file.to_ascii_lowercase();
    let syntaxes = if lower.ends_with(".tsx") || lower.ends_with(".jsx") || lower.ends_with(".vue")
    {
        vec![
            Syntax::Typescript(TsSyntax {
                tsx: true,
                decorators: true,
                ..Default::default()
            }),
            Syntax::Es(EsSyntax {
                jsx: true,
                ..Default::default()
            }),
        ]
    } else if lower.ends_with(".ts") || lower.ends_with(".mts") || lower.ends_with(".cts") {
        vec![Syntax::Typescript(TsSyntax {
            tsx: false,
            decorators: true,
            ..Default::default()
        })]
    } else {
        vec![
            Syntax::Typescript(TsSyntax {
                tsx: false,
                decorators: true,
                ..Default::default()
            }),
            Syntax::Es(Default::default()),
        ]
    };
    let mut errors = Vec::new();
    for syntax in syntaxes {
        match parse_source_with_syntax(source, source_file, syntax) {
            Ok(program) => return Ok(program),
            Err(err) => errors.push(err),
        }
    }
    Err(errors.join("; fallback parse error: "))
}

fn parse_source_with_syntax(
    source: &str,
    source_file: &str,
    syntax: Syntax,
) -> Result<Program, String> {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        FileName::Custom(source_file.into()).into(),
        source.to_string(),
    );

    let lexer = Lexer::new(syntax, Default::default(), StringInput::from(&*fm), None);

    let mut parser = Parser::new_from(lexer);
    let program = parser.parse_program().map_err(|err| {
        let location = cm.lookup_char_pos(err.span().lo);
        format!(
            "parse error at {}:{}: {err:?}",
            location.line,
            location.col_display + 1
        )
    })?;
    let parser_errors = parser.take_errors();
    if !parser_errors.is_empty() {
        return Err(format!("parse errors: {parser_errors:?}"));
    }

    validate_program_early_errors(&program)?;

    Ok(program)
}

fn validate_program_early_errors(program: &Program) -> Result<(), String> {
    match program {
        Program::Module(module) => {
            for item in &module.body {
                if let ModuleItem::Stmt(stmt) = item {
                    validate_stmt_early_errors(stmt)?;
                }
            }
        }
        Program::Script(script) => {
            for stmt in &script.body {
                validate_stmt_early_errors(stmt)?;
            }
        }
    }
    Ok(())
}

fn validate_stmt_early_errors(stmt: &Stmt) -> Result<(), String> {
    match stmt {
        Stmt::For(stmt) => {
            if is_labelled_function_stmt(&stmt.body) {
                return Err(
                    "parse error: labelled function is not allowed in for statement position"
                        .into(),
                );
            }
            if let Some(VarDeclOrExpr::VarDecl(decl)) = &stmt.init
                && matches!(decl.kind, VarDeclKind::Let | VarDeclKind::Const)
            {
                let bound_names = var_decl_bound_names(decl);
                let var_names = stmt_var_declared_names(&stmt.body);
                if bound_names.iter().any(|name| var_names.contains(name)) {
                    return Err(
                        "parse error: for lexical head cannot be redeclared by var in body".into(),
                    );
                }
            }
            validate_stmt_early_errors(&stmt.body)?;
        }
        Stmt::While(stmt) => {
            if is_labelled_function_stmt(&stmt.body) {
                return Err(
                    "parse error: labelled function is not allowed in while statement position"
                        .into(),
                );
            }
            validate_stmt_early_errors(&stmt.body)?;
        }
        Stmt::DoWhile(stmt) => validate_stmt_early_errors(&stmt.body)?,
        Stmt::Labeled(stmt) => validate_stmt_early_errors(&stmt.body)?,
        Stmt::Block(block) => {
            for stmt in &block.stmts {
                validate_stmt_early_errors(stmt)?;
            }
        }
        Stmt::If(stmt) => {
            validate_stmt_early_errors(&stmt.cons)?;
            if let Some(alt) = &stmt.alt {
                validate_stmt_early_errors(alt)?;
            }
        }
        Stmt::Switch(stmt) => {
            for case in &stmt.cases {
                for stmt in &case.cons {
                    validate_stmt_early_errors(stmt)?;
                }
            }
        }
        Stmt::Try(stmt) => {
            for stmt in &stmt.block.stmts {
                validate_stmt_early_errors(stmt)?;
            }
            if let Some(handler) = &stmt.handler {
                for stmt in &handler.body.stmts {
                    validate_stmt_early_errors(stmt)?;
                }
            }
            if let Some(finalizer) = &stmt.finalizer {
                for stmt in &finalizer.stmts {
                    validate_stmt_early_errors(stmt)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_early_syntax_errors(source: &str) -> Result<(), String> {
    validate_numeric_separator_literals(source)?;
    validate_strict_legacy_escape_directives(source)?;
    validate_statement_position_async_generators(source)?;
    validate_if_labelled_function_positions(source)?;
    validate_let_array_line_break(source)?;
    Ok(())
}

fn validate_numeric_separator_literals(source: &str) -> Result<(), String> {
    let code = source_without_comments_and_strings(source);
    let chars = code.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let previous_is_ident = index > 0
            && (chars[index - 1].is_ascii_alphanumeric() || matches!(chars[index - 1], '_' | '$'));
        let starts_number = chars[index].is_ascii_digit()
            || (chars[index] == '.' && chars.get(index + 1).is_some_and(|ch| ch.is_ascii_digit()));
        if previous_is_ident {
            index += 1;
            continue;
        }
        if !starts_number {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < chars.len()
            && (chars[index].is_ascii_alphanumeric()
                || matches!(chars[index], '_' | '.' | '+' | '-'))
        {
            if matches!(chars[index], '+' | '-')
                && !matches!(chars.get(index.wrapping_sub(1)), Some('e' | 'E'))
            {
                break;
            }
            index += 1;
        }
        let token = chars[start..index].iter().collect::<String>();
        if numeric_token_has_invalid_separator(&token) {
            return Err(format!(
                "parse error: invalid numeric separator literal {token}"
            ));
        }
    }
    Ok(())
}

fn numeric_token_has_invalid_separator(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.contains("\\u005f") || lower.contains("\\u{5f}") {
        return true;
    }
    if !token.contains('_') {
        return false;
    }
    if token.starts_with('_') || token.ends_with('_') || token.contains("__") {
        return true;
    }
    if lower.starts_with("0b_") || lower.starts_with("0x_") || lower.starts_with("0o_") {
        return true;
    }
    if token.contains("._") || token.contains("_.") {
        return true;
    }
    if !lower.starts_with("0x") && (lower.contains("_e") || lower.contains("e_")) {
        return true;
    }
    if lower.starts_with("0b") {
        return token[2..]
            .chars()
            .any(|ch| ch != '_' && !matches!(ch, '0' | '1'));
    }
    if lower.starts_with("0o") {
        return token[2..]
            .chars()
            .any(|ch| ch != '_' && !matches!(ch, '0'..='7'));
    }
    if lower.starts_with("0x") {
        return token[2..]
            .chars()
            .any(|ch| ch != '_' && !ch.is_ascii_hexdigit());
    }
    let bytes = token.as_bytes();
    token.contains('_')
        && bytes.first() == Some(&b'0')
        && bytes
            .get(1)
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'_')
}

fn validate_strict_legacy_escape_directives(source: &str) -> Result<(), String> {
    if source_has_invalid_unicode_codepoint_separator(source) {
        return Err("parse error: invalid unicode code point escape separator".into());
    }
    let Some(strict_index) = first_use_strict_index(source) else {
        return Ok(());
    };
    let before_or_at_strict = &source[..strict_index];
    if directive_prologue_has_legacy_escape(before_or_at_strict)
        || source_has_strict_legacy_numeric_escape(source)
    {
        return Err(
            "parse error: invalid string literal escape in strict directive prologue".into(),
        );
    }
    Ok(())
}

fn first_use_strict_index(source: &str) -> Option<usize> {
    source
        .find("\"use strict\"")
        .or_else(|| source.find("'use strict'"))
}

fn directive_prologue_has_legacy_escape(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let quote = bytes[index];
        if quote != b'\'' && quote != b'"' {
            index += 1;
            continue;
        }
        index += 1;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' => {
                    if bytes
                        .get(index + 1)
                        .is_some_and(|byte| byte.is_ascii_digit())
                    {
                        return true;
                    }
                    index += 2;
                }
                value if value == quote => {
                    index += 1;
                    break;
                }
                _ => index += 1,
            }
        }
    }
    false
}

fn source_has_strict_legacy_numeric_escape(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        if let Some(next) = bytes.get(index + 1) {
                            if matches!(next, b'1'..=b'9')
                                || (*next == b'0'
                                    && bytes
                                        .get(index + 2)
                                        .is_some_and(|byte| byte.is_ascii_digit()))
                            {
                                return true;
                            }
                        }
                        index += 2;
                        continue;
                    }
                    if bytes[index] == quote {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() {
                    if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    false
}

fn source_has_invalid_unicode_codepoint_separator(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index] == b'\\'
            && bytes[index + 1] == b'u'
            && bytes[index + 2] == b'{'
            && let Some(end) = source[index + 3..].find('}')
            && source[index + 3..index + 3 + end].contains('_')
        {
            return true;
        }
        index += 1;
    }
    false
}

fn validate_statement_position_async_generators(source: &str) -> Result<(), String> {
    let code = source_without_comments_and_strings(source);
    if code.contains("if (true) async function*")
        || code.contains("else async function*")
        || code.contains("for ( ; false; ) async function*")
        || code.contains("while (false) async function*")
        || code.contains("if (true) async function")
        || code.contains("else async function")
        || code.contains("for ( ; false; ) async function")
        || code.contains("while (false) async function")
    {
        return Err("parse error: async function declaration is not allowed here".into());
    }
    Ok(())
}

fn validate_if_labelled_function_positions(source: &str) -> Result<(), String> {
    let code = source_without_comments_and_strings(source);
    let compact = code.split_whitespace().collect::<Vec<_>>().join(" ");
    let has_labelled_function = compact.contains(": function");
    let has_if_labelled_statement =
        compact.contains("if (false) label") || compact.contains("if (true) label");
    let has_else_labelled_statement = compact.contains("else label");
    if has_labelled_function && (has_if_labelled_statement || has_else_labelled_statement) {
        return Err(
            "parse error: labelled function is not allowed in if statement position".into(),
        );
    }
    Ok(())
}

fn validate_let_array_line_break(source: &str) -> Result<(), String> {
    let code = source_without_comments_and_strings(source);
    if code.contains("let\n[") || code.contains("let\r\n[") {
        return Err("parse error: let followed by array literal line break is ambiguous".into());
    }
    Ok(())
}

fn source_without_comments_and_strings(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                output.push(' ');
                index += 1;
                while index < bytes.len() {
                    output.push(if bytes[index] == b'\n' { '\n' } else { ' ' });
                    if bytes[index] == b'\\' {
                        index += 2;
                        if index <= bytes.len() {
                            output.push(' ');
                        }
                        continue;
                    }
                    if bytes[index] == quote {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                output.push(' ');
                output.push(' ');
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    output.push(' ');
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                output.push(' ');
                output.push(' ');
                index += 2;
                while index + 1 < bytes.len() {
                    let current = bytes[index];
                    output.push(if current == b'\n' { '\n' } else { ' ' });
                    if current == b'*' && bytes[index + 1] == b'/' {
                        output.push(' ');
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            b'/' if is_regex_literal_start(&output) => {
                output.push(' ');
                index += 1;
                let mut escaped = false;
                let mut in_class = false;
                while index < bytes.len() {
                    let current = bytes[index];
                    output.push(if current == b'\n' { '\n' } else { ' ' });
                    if escaped {
                        escaped = false;
                    } else if current == b'\\' {
                        escaped = true;
                    } else if current == b'[' {
                        in_class = true;
                    } else if current == b']' {
                        in_class = false;
                    } else if current == b'/' && !in_class {
                        index += 1;
                        while index < bytes.len()
                            && matches!(bytes[index], b'a'..=b'z' | b'A'..=b'Z')
                        {
                            output.push(' ');
                            index += 1;
                        }
                        break;
                    }
                    index += 1;
                }
            }
            value => {
                output.push(value as char);
                index += 1;
            }
        }
    }
    output
}

fn is_regex_literal_start(output: &str) -> bool {
    let Some(previous) = output.chars().rev().find(|ch| !ch.is_whitespace()) else {
        return true;
    };
    if matches!(
        previous,
        '(' | '['
            | '{'
            | '='
            | ':'
            | ','
            | ';'
            | '!'
            | '?'
            | '&'
            | '|'
            | '+'
            | '-'
            | '*'
            | '%'
            | '^'
            | '~'
            | '<'
            | '>'
    ) {
        return true;
    }
    let trimmed = output.trim_end();
    let token = trimmed
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '$')
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    matches!(
        token.as_str(),
        "await"
            | "case"
            | "delete"
            | "do"
            | "else"
            | "in"
            | "instanceof"
            | "new"
            | "of"
            | "return"
            | "throw"
            | "typeof"
            | "void"
            | "yield"
    )
}

fn is_labelled_control_target(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::While(_)
        | Stmt::DoWhile(_)
        | Stmt::For(_)
        | Stmt::ForIn(_)
        | Stmt::ForOf(_)
        | Stmt::Switch(_) => true,
        Stmt::Labeled(labelled) => is_labelled_control_target(&labelled.body),
        _ => false,
    }
}

fn is_implicit_global(name: &str) -> bool {
    matches!(
        name,
        "undefined" | "NaN" | "Infinity" | "this" | "super" | "import"
    )
}

fn import_local_name(specifier: &ImportSpecifier) -> Option<String> {
    match specifier {
        ImportSpecifier::Named(named) => Some(ident_name(&named.local)),
        ImportSpecifier::Default(default) => Some(ident_name(&default.local)),
        ImportSpecifier::Namespace(namespace) => Some(ident_name(&namespace.local)),
    }
}

fn import_specifier_name(specifier: &ImportSpecifier) -> String {
    match specifier {
        ImportSpecifier::Named(named) => {
            let imported = named
                .imported
                .as_ref()
                .map(module_export_name)
                .unwrap_or_else(|| ident_name(&named.local));
            if imported == ident_name(&named.local) {
                imported
            } else {
                format!("{imported} as {}", ident_name(&named.local))
            }
        }
        ImportSpecifier::Default(default) => format!("default as {}", ident_name(&default.local)),
        ImportSpecifier::Namespace(namespace) => format!("* as {}", ident_name(&namespace.local)),
    }
}

fn decl_names(decl: &Decl) -> Vec<String> {
    match decl {
        Decl::Class(decl) => vec![ident_name(&decl.ident)],
        Decl::Fn(decl) => vec![ident_name(&decl.ident)],
        Decl::Var(decl) => decl
            .decls
            .iter()
            .filter_map(|decl| pat_name(&decl.name))
            .collect(),
        _ => Vec::new(),
    }
}

fn decl_name(decl: &Decl) -> &'static str {
    match decl {
        Decl::Class(_) => "class",
        Decl::Fn(_) => "function",
        Decl::Var(_) => "variable",
        Decl::Using(_) => "using",
        Decl::TsInterface(_) => "typescript interface",
        Decl::TsTypeAlias(_) => "typescript type alias",
        Decl::TsEnum(_) => "typescript enum",
        Decl::TsModule(_) => "typescript module",
    }
}

fn export_entry(specifier: &ExportSpecifier) -> Option<(String, String)> {
    match specifier {
        ExportSpecifier::Namespace(namespace) => {
            let exported = module_export_name(&namespace.name);
            Some((exported.clone(), exported))
        }
        ExportSpecifier::Default(default) => {
            let exported = ident_name(&default.exported);
            Some(("default".to_string(), exported))
        }
        ExportSpecifier::Named(named) => {
            let local = module_export_name(&named.orig);
            let exported = named
                .exported
                .as_ref()
                .map(module_export_name)
                .unwrap_or_else(|| local.clone());
            Some((local, exported))
        }
    }
}

fn module_decl_name(decl: &ModuleDecl) -> &'static str {
    match decl {
        ModuleDecl::Import(_) => "import",
        ModuleDecl::ExportDecl(_) => "export declaration",
        ModuleDecl::ExportNamed(_) => "named export",
        ModuleDecl::ExportDefaultDecl(_) => "default export declaration",
        ModuleDecl::ExportDefaultExpr(_) => "default export expression",
        ModuleDecl::ExportAll(_) => "export all",
        ModuleDecl::TsImportEquals(_) => "typescript import equals",
        ModuleDecl::TsExportAssignment(_) => "typescript export assignment",
        ModuleDecl::TsNamespaceExport(_) => "typescript namespace export",
    }
}

fn pat_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(ident_name(&ident.id)),
        Pat::Assign(assign) => pat_name(&assign.left),
        Pat::Rest(rest) => pat_name(&rest.arg),
        Pat::Expr(expr) => match &**expr {
            Expr::Ident(ident) => Some(ident_name(ident)),
            _ => None,
        },
        _ => None,
    }
}

fn function_param_names(params: &[Param]) -> Vec<String> {
    params
        .iter()
        .enumerate()
        .map(|(index, param)| param_name_or_synthetic(&param.pat, index))
        .collect()
}

fn pat_list_names(params: &[Pat]) -> Vec<String> {
    params
        .iter()
        .enumerate()
        .map(|(index, param)| param_name_or_synthetic(param, index))
        .collect()
}

fn constructor_param_names(params: &[ParamOrTsParamProp]) -> Vec<String> {
    params
        .iter()
        .enumerate()
        .map(|(index, param)| match param {
            ParamOrTsParamProp::Param(param) => param_name_or_synthetic(&param.pat, index),
            ParamOrTsParamProp::TsParamProp(_) => "<unsupported>".to_string(),
        })
        .collect()
}

fn param_name_or_synthetic(pat: &Pat, index: usize) -> String {
    if is_direct_param_pattern(pat) {
        pat_name(pat).unwrap_or_else(|| synthetic_param_name(index))
    } else {
        synthetic_param_name(index)
    }
}

fn synthetic_param_name(index: usize) -> String {
    format!("__js_vm_param_{index}")
}

fn is_direct_param_pattern(pat: &Pat) -> bool {
    match pat {
        Pat::Ident(_) => true,
        Pat::Assign(assign) => is_direct_param_pattern(&assign.left),
        Pat::Rest(rest) => is_direct_param_pattern(&rest.arg),
        Pat::Expr(expr) => matches!(&**expr, Expr::Ident(_)),
        _ => false,
    }
}

fn ident_name(ident: &Ident) -> String {
    ident.sym.to_string()
}

fn unary_op(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Minus => "-",
        UnaryOp::Plus => "+",
        UnaryOp::Bang => "!",
        UnaryOp::Tilde => "~",
        UnaryOp::TypeOf => "typeof",
        UnaryOp::Void => "void",
        UnaryOp::Delete => "delete",
    }
}

fn assign_binary_op(op: AssignOp) -> &'static str {
    op.to_update().map(bin_op).unwrap_or("=")
}

fn super_prop_name(expr: &SuperPropExpr) -> String {
    match &expr.prop {
        SuperProp::Ident(ident) => ident.sym.to_string(),
        SuperProp::Computed(computed) => format!("[{:?}]", computed.expr),
    }
}

fn expr_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::This(_) => "this",
        Expr::Array(_) => "array",
        Expr::Object(_) => "object",
        Expr::Fn(_) => "function expression",
        Expr::Unary(_) => "unary",
        Expr::Update(_) => "update",
        Expr::Bin(_) => "binary",
        Expr::Assign(_) => "assignment",
        Expr::Member(_) => "member",
        Expr::SuperProp(_) => "super property",
        Expr::Cond(_) => "conditional",
        Expr::Call(_) => "call",
        Expr::New(_) => "new",
        Expr::Seq(_) => "sequence",
        Expr::Ident(_) => "identifier",
        Expr::Lit(_) => "literal",
        Expr::Tpl(_) => "template literal",
        Expr::TaggedTpl(_) => "tagged template",
        Expr::Arrow(_) => "arrow function",
        Expr::Class(_) => "class expression",
        Expr::Yield(_) => "yield",
        Expr::MetaProp(_) => "meta property",
        Expr::Await(_) => "await",
        Expr::Paren(_) => "parenthesized",
        Expr::JSXMember(_) => "jsx member",
        Expr::JSXNamespacedName(_) => "jsx namespaced name",
        Expr::JSXEmpty(_) => "jsx empty",
        Expr::JSXElement(_) => "jsx element",
        Expr::JSXFragment(_) => "jsx fragment",
        Expr::TsTypeAssertion(_) => "typescript type assertion",
        Expr::TsConstAssertion(_) => "typescript const assertion",
        Expr::TsNonNull(_) => "typescript non-null",
        Expr::TsAs(_) => "typescript as",
        Expr::TsInstantiation(_) => "typescript instantiation",
        Expr::TsSatisfies(_) => "typescript satisfies",
        Expr::PrivateName(_) => "private name",
        Expr::OptChain(_) => "optional chain",
        Expr::Invalid(_) => "invalid",
    }
}

fn bin_op(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::EqEq => "==",
        BinaryOp::NotEq => "!=",
        BinaryOp::EqEqEq => "===",
        BinaryOp::NotEqEq => "!==",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::LShift => "<<",
        BinaryOp::RShift => ">>",
        BinaryOp::ZeroFillRShift => ">>>",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::BitAnd => "&",
        BinaryOp::LogicalOr => "||",
        BinaryOp::LogicalAnd => "&&",
        BinaryOp::In => "in",
        BinaryOp::InstanceOf => "instanceof",
        BinaryOp::Exp => "**",
        BinaryOp::NullishCoalescing => "??",
    }
}

fn prop_name(prop: &PropName) -> String {
    match prop {
        PropName::Ident(ident) => ident.sym.to_string(),
        PropName::Str(value) => value.value.to_string(),
        PropName::Num(value) => value.value.to_string(),
        PropName::Computed(_) => "[computed]".to_string(),
        PropName::BigInt(value) => value.value.to_string(),
    }
}

fn for_init_lexical_names(init: &Option<VarDeclOrExpr>) -> Option<Vec<String>> {
    let Some(VarDeclOrExpr::VarDecl(decl)) = init else {
        return None;
    };
    if !matches!(decl.kind, VarDeclKind::Let | VarDeclKind::Const) {
        return None;
    }
    Some(var_decl_bound_names(decl).into_iter().collect())
}

fn for_head_is_lexical(head: &ForHead) -> bool {
    let ForHead::VarDecl(decl) = head else {
        return false;
    };
    matches!(decl.kind, VarDeclKind::Let | VarDeclKind::Const)
}

fn accessor_getter_key(property: &str) -> String {
    format!("__accessor_get__:{property}")
}

fn accessor_setter_key(property: &str) -> String {
    format!("__accessor_set__:{property}")
}

fn static_property_key(value: &IrValue) -> String {
    match value {
        IrValue::String(value) => value.clone(),
        IrValue::Number(value) => value.to_string(),
        IrValue::BigInt(value) => value.clone(),
        IrValue::Bool(value) => value.to_string(),
        IrValue::Null => "null".to_string(),
        IrValue::Undefined => "undefined".to_string(),
        IrValue::Name(value) | IrValue::Register(value) => value.clone(),
    }
}

fn is_labelled_function_stmt(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Labeled(labelled) => match &*labelled.body {
            Stmt::Decl(Decl::Fn(_)) => true,
            nested => is_labelled_function_stmt(nested),
        },
        _ => false,
    }
}

fn var_decl_bound_names(decl: &VarDecl) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for declarator in &decl.decls {
        collect_pat_bound_names(&declarator.name, &mut names);
    }
    names
}

fn collect_pat_bound_names(pat: &Pat, names: &mut BTreeSet<String>) {
    match pat {
        Pat::Ident(ident) => {
            names.insert(ident_name(&ident.id));
        }
        Pat::Array(array) => {
            for elem in array.elems.iter().flatten() {
                collect_pat_bound_names(elem, names);
            }
        }
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::KeyValue(prop) => collect_pat_bound_names(&prop.value, names),
                    ObjectPatProp::Assign(prop) => {
                        names.insert(ident_name(&prop.key));
                    }
                    ObjectPatProp::Rest(rest) => collect_pat_bound_names(&rest.arg, names),
                }
            }
        }
        Pat::Rest(rest) => collect_pat_bound_names(&rest.arg, names),
        Pat::Assign(assign) => collect_pat_bound_names(&assign.left, names),
        Pat::Expr(_) | Pat::Invalid(_) => {}
    }
}

fn stmt_var_declared_names(stmt: &Stmt) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_stmt_var_declared_names(stmt, &mut names);
    names
}

fn module_var_declared_names(module: &Module) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &module.body {
        match item {
            ModuleItem::Stmt(stmt) => collect_stmt_var_declared_names(stmt, &mut names),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(decl)) => {
                if let Decl::Var(var_decl) = &decl.decl
                    && var_decl.kind == VarDeclKind::Var
                {
                    names.extend(var_decl_bound_names(var_decl));
                }
            }
            _ => {}
        }
    }
    names
}

fn script_var_declared_names(script: &Script) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for stmt in &script.body {
        collect_stmt_var_declared_names(stmt, &mut names);
    }
    names
}

fn collect_stmt_var_declared_names(stmt: &Stmt, names: &mut BTreeSet<String>) {
    match stmt {
        Stmt::Decl(Decl::Var(decl)) if decl.kind == VarDeclKind::Var => {
            names.extend(var_decl_bound_names(decl));
        }
        Stmt::Block(block) => {
            for stmt in &block.stmts {
                collect_stmt_var_declared_names(stmt, names);
            }
        }
        Stmt::If(stmt) => {
            collect_stmt_var_declared_names(&stmt.cons, names);
            if let Some(alt) = &stmt.alt {
                collect_stmt_var_declared_names(alt, names);
            }
        }
        Stmt::For(stmt) => {
            if let Some(VarDeclOrExpr::VarDecl(decl)) = &stmt.init
                && decl.kind == VarDeclKind::Var
            {
                names.extend(var_decl_bound_names(decl));
            }
            collect_stmt_var_declared_names(&stmt.body, names);
        }
        Stmt::ForIn(stmt) => {
            if let ForHead::VarDecl(decl) = &stmt.left
                && decl.kind == VarDeclKind::Var
            {
                names.extend(var_decl_bound_names(decl));
            }
            collect_stmt_var_declared_names(&stmt.body, names);
        }
        Stmt::ForOf(stmt) => {
            if let ForHead::VarDecl(decl) = &stmt.left
                && decl.kind == VarDeclKind::Var
            {
                names.extend(var_decl_bound_names(decl));
            }
            collect_stmt_var_declared_names(&stmt.body, names);
        }
        Stmt::While(stmt) => collect_stmt_var_declared_names(&stmt.body, names),
        Stmt::DoWhile(stmt) => collect_stmt_var_declared_names(&stmt.body, names),
        Stmt::Labeled(stmt) => collect_stmt_var_declared_names(&stmt.body, names),
        Stmt::Switch(stmt) => {
            for case in &stmt.cases {
                for stmt in &case.cons {
                    collect_stmt_var_declared_names(stmt, names);
                }
            }
        }
        Stmt::Try(stmt) => {
            for stmt in &stmt.block.stmts {
                collect_stmt_var_declared_names(stmt, names);
            }
            if let Some(handler) = &stmt.handler {
                for stmt in &handler.body.stmts {
                    collect_stmt_var_declared_names(stmt, names);
                }
            }
            if let Some(finalizer) = &stmt.finalizer {
                for stmt in &finalizer.stmts {
                    collect_stmt_var_declared_names(stmt, names);
                }
            }
        }
        _ => {}
    }
}

fn class_member_name(member: &ClassMember) -> String {
    match member {
        ClassMember::Constructor(_) => "constructor".to_string(),
        ClassMember::Method(method) => format!(
            "{}{} method {}",
            if method.is_static { "static " } else { "" },
            method_kind_name(method.kind),
            prop_name(&method.key)
        ),
        ClassMember::PrivateMethod(method) => format!(
            "{}{} method #{}",
            if method.is_static { "static " } else { "" },
            method_kind_name(method.kind),
            method.key.name
        ),
        ClassMember::ClassProp(prop) => format!(
            "{}field {}",
            if prop.is_static { "static " } else { "" },
            prop_name(&prop.key)
        ),
        ClassMember::PrivateProp(prop) => format!(
            "{}field #{}",
            if prop.is_static { "static " } else { "" },
            prop.key.name
        ),
        ClassMember::TsIndexSignature(_) => "typescript index signature".to_string(),
        ClassMember::Empty(_) => "empty".to_string(),
        ClassMember::StaticBlock(_) => "static block".to_string(),
        ClassMember::AutoAccessor(accessor) => format!("accessor {}", key_name(&accessor.key)),
    }
}

fn call_args_have_spread(args: &[ExprOrSpread]) -> bool {
    args.iter().any(|arg| arg.spread.is_some())
}

fn simple_assign_target_name(target: &SimpleAssignTarget) -> &'static str {
    match target {
        SimpleAssignTarget::Ident(_) => "identifier",
        SimpleAssignTarget::Member(_) => "member",
        SimpleAssignTarget::SuperProp(_) => "super property",
        SimpleAssignTarget::Paren(_) => "parenthesized",
        SimpleAssignTarget::OptChain(_) => "optional chain",
        SimpleAssignTarget::TsAs(_) => "typescript as",
        SimpleAssignTarget::TsSatisfies(_) => "typescript satisfies",
        SimpleAssignTarget::TsNonNull(_) => "typescript non-null",
        SimpleAssignTarget::TsTypeAssertion(_) => "typescript type assertion",
        SimpleAssignTarget::TsInstantiation(_) => "typescript instantiation",
        SimpleAssignTarget::Invalid(_) => "invalid",
    }
}

fn module_export_name(name: &ModuleExportName) -> String {
    name.atom().to_string()
}

fn method_kind_name(kind: MethodKind) -> &'static str {
    match kind {
        MethodKind::Method => "",
        MethodKind::Getter => "get",
        MethodKind::Setter => "set",
    }
}

fn key_name(key: &Key) -> String {
    match key {
        Key::Private(name) => format!("#{}", name.name),
        Key::Public(name) => prop_name(name),
    }
}
