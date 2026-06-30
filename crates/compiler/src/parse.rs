use js_token_core as core;
use std::collections::{BTreeMap, BTreeSet};
use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

#[derive(Debug, Clone, PartialEq)]
enum IrValue {
    Register(String),
    Name(String),
    Number(f64),
    String(String),
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
        body: Vec<IrInstruction>,
    },
    FunctionExpr {
        dst: String,
        name: Option<String>,
        params: Vec<String>,
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
        names: Vec<String>,
    },
    Throw(IrValue),
    Try {
        body: Vec<IrInstruction>,
        catch_param: Option<String>,
        catch_body: Vec<IrInstruction>,
        finally_body: Vec<IrInstruction>,
    },
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
    Unsupported(String),
}

pub struct LoweringContext {
    instructions: Vec<IrInstruction>,
    temp_id: usize,
    label_id: usize,
    child_id: usize,
    label_prefix: String,
    break_stack: Vec<String>,
    continue_stack: Vec<String>,
    extern_slots: BTreeMap<String, usize>,
    locals: BTreeSet<String>,
    yield_array: Option<String>,
}

impl LoweringContext {
    pub fn with_externals(externs: &[String]) -> Self {
        Self {
            instructions: Vec::new(),
            temp_id: 0,
            label_id: 0,
            child_id: 0,
            label_prefix: "c0".to_string(),
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            extern_slots: externs
                .iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), index))
                .collect(),
            locals: BTreeSet::new(),
            yield_array: None,
        }
    }

    pub fn into_module(self) -> core::IrModule {
        let mut extern_slots = vec![String::new(); self.extern_slots.len()];
        for (name, slot) in self.extern_slots {
            extern_slots[slot] = name;
        }
        StructuredIrBuilder::new(extern_slots).build(self.instructions)
    }

    fn child(&mut self) -> Self {
        let child_id = self.child_id;
        self.child_id += 1;
        Self {
            instructions: Vec::new(),
            temp_id: 0,
            label_id: 0,
            child_id: 0,
            label_prefix: format!("{}_{}", self.label_prefix, child_id),
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            extern_slots: BTreeMap::new(),
            locals: self.locals.clone(),
            yield_array: None,
        }
    }

    fn merge_child_externs(&mut self, child: &LoweringContext) {
        for name in child.extern_slots.keys() {
            self.mark_extern(name);
        }
    }

    fn declare_local(&mut self, name: impl Into<String>) {
        self.locals.insert(name.into());
    }

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

    fn emit(&mut self, instruction: IrInstruction) {
        self.instructions.push(instruction);
    }

    pub fn lower_module(&mut self, module: &Module) {
        self.predeclare_module(module);
        for item in &module.body {
            self.lower_module_item(item);
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

    fn predeclare_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Var(var_decl) => {
                for declarator in &var_decl.decls {
                    if let Some(name) = pat_name(&declarator.name) {
                        self.declare_local(name);
                    }
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
                    names: decl_names(&decl.decl),
                });
            }
            ModuleDecl::ExportNamed(decl) => {
                self.emit(IrInstruction::Export {
                    kind: decl
                        .src
                        .as_ref()
                        .map(|src| format!("named from {:?}", src.value))
                        .unwrap_or_else(|| "named".to_string()),
                    names: decl.specifiers.iter().map(export_specifier_name).collect(),
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
                self.emit(IrInstruction::StoreName {
                    name: "default".to_string(),
                    src: value,
                });
                self.emit(IrInstruction::Export {
                    kind: "default declaration".to_string(),
                    names: vec!["default".to_string()],
                });
            }
            ModuleDecl::ExportDefaultExpr(decl) => {
                let value = self.lower_expr(&decl.expr);
                self.emit(IrInstruction::StoreName {
                    name: "default".to_string(),
                    src: value,
                });
                self.emit(IrInstruction::Export {
                    kind: "default expression".to_string(),
                    names: vec!["default".to_string()],
                });
            }
            ModuleDecl::ExportAll(decl) => {
                self.emit(IrInstruction::Export {
                    kind: format!("all from {:?}", decl.src.value),
                    names: Vec::new(),
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
                self.emit(IrInstruction::Label(ident_name(&stmt.label)));
                self.lower_stmt(&stmt.body);
            }
            Stmt::Break(stmt) => {
                let label = stmt
                    .label
                    .as_ref()
                    .map(ident_name)
                    .or_else(|| self.break_stack.last().cloned());
                if let Some(label) = label {
                    self.emit(IrInstruction::Jump(label));
                } else {
                    self.emit(IrInstruction::Marker("break".to_string()));
                }
            }
            Stmt::Continue(stmt) => {
                let label = stmt
                    .label
                    .as_ref()
                    .map(ident_name)
                    .or_else(|| self.continue_stack.last().cloned());
                if let Some(label) = label {
                    self.emit(IrInstruction::Jump(label));
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
                self.break_stack.push(end_label.clone());
                self.continue_stack.push(start_label.clone());
                self.lower_stmt(&stmt.body);
                self.continue_stack.pop();
                self.break_stack.pop();
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

    fn lower_param_defaults(&mut self, params: &[Param]) {
        for param in params {
            self.lower_pat_default(&param.pat);
        }
    }

    fn lower_arrow_param_defaults(&mut self, params: &[Pat]) {
        for param in params {
            self.lower_pat_default(param);
        }
    }

    fn lower_constructor_param_defaults(&mut self, params: &[ParamOrTsParamProp]) {
        for param in params {
            if let ParamOrTsParamProp::Param(param) = param {
                self.lower_pat_default(&param.pat);
            }
        }
    }

    fn start_generator_body(&mut self, is_generator: bool) {
        if !is_generator {
            return;
        }
        let name = "__yield".to_string();
        self.declare_local(name.clone());
        self.yield_array = Some(name.clone());
        self.emit(IrInstruction::Declare {
            kind: "let".to_string(),
            name: name.clone(),
        });
        let array = self.temp();
        self.emit(IrInstruction::Array {
            dst: array.clone(),
            items: Vec::new(),
        });
        self.emit(IrInstruction::StoreName {
            name,
            src: IrValue::Register(array),
        });
    }

    fn finish_generator_body(&mut self) {
        if let Some(name) = &self.yield_array {
            self.emit(IrInstruction::Return(Some(IrValue::Name(name.clone()))));
        }
    }

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
        body_ctx.declare_local(name.clone());
        for param in &params {
            body_ctx.declare_local(param.clone());
        }
        body_ctx.start_generator_body(decl.function.is_generator);
        body_ctx.lower_param_defaults(&decl.function.params);
        if let Some(body) = &decl.function.body {
            body_ctx.lower_block(body);
        }
        body_ctx.finish_generator_body();
        self.merge_child_externs(&body_ctx);

        self.emit(IrInstruction::Function {
            name,
            params,
            body: body_ctx.instructions,
        });
    }

    fn lower_for(&mut self, stmt: &ForStmt) {
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

        self.break_stack.push(end_label.clone());
        self.continue_stack.push(update_label.clone());
        self.lower_stmt(&stmt.body);
        self.continue_stack.pop();
        self.break_stack.pop();

        self.emit(IrInstruction::Label(update_label));
        if let Some(update) = &stmt.update {
            let value = self.lower_expr(update);
            self.emit(IrInstruction::Pop(value));
        }

        self.emit(IrInstruction::Jump(start_label));
        self.emit(IrInstruction::Label(end_label));
    }

    fn lower_do_while(&mut self, stmt: &DoWhileStmt) {
        let start_label = self.label("do_start");
        let test_label = self.label("do_test");
        let end_label = self.label("do_end");
        self.emit(IrInstruction::Label(start_label.clone()));
        self.break_stack.push(end_label.clone());
        self.continue_stack.push(test_label.clone());
        self.lower_stmt(&stmt.body);
        self.continue_stack.pop();
        self.break_stack.pop();
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
        let iterated = if kind == "for_in" {
            let object = self.lower_expr(right);
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
        let start_label = self.label(kind);
        let update_label = self.label(&format!("{kind}_update"));
        let end_label = self.label(&format!("{kind}_end"));
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
        self.lower_for_head_binding(left, item);
        self.break_stack.push(end_label.clone());
        self.continue_stack.push(update_label.clone());
        self.lower_stmt(body);
        self.continue_stack.pop();
        self.break_stack.pop();
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

        self.break_stack.push(end_label.clone());
        for (case, label) in stmt.cases.iter().zip(case_labels.iter()) {
            self.emit(IrInstruction::Label(label.clone()));
            for stmt in &case.cons {
                self.lower_stmt(stmt);
            }
        }
        self.break_stack.pop();
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
                let callee = match &expr.callee {
                    Callee::Expr(expr) => self.lower_expr(expr),
                    Callee::Super(_) => IrValue::Name("super".to_string()),
                    Callee::Import(_) => IrValue::Name("import".to_string()),
                };
                let args = expr
                    .args
                    .iter()
                    .map(|arg| self.lower_expr_or_spread(arg))
                    .collect::<Vec<_>>();
                let dst = self.temp();
                self.emit(IrInstruction::Call {
                    dst: dst.clone(),
                    callee,
                    args,
                });
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
            Expr::Arrow(expr) => {
                let dst = self.temp();
                let params = pat_list_names(&expr.params);
                let mut body_ctx = self.child();
                for param in &params {
                    body_ctx.declare_local(param.clone());
                }
                body_ctx.lower_arrow_param_defaults(&expr.params);
                match &*expr.body {
                    BlockStmtOrExpr::BlockStmt(block) => body_ctx.lower_block(block),
                    BlockStmtOrExpr::Expr(expr) => {
                        let value = body_ctx.lower_expr(expr);
                        body_ctx.emit(IrInstruction::Return(Some(value)));
                    }
                }
                self.merge_child_externs(&body_ctx);
                self.emit(IrInstruction::FunctionExpr {
                    dst: dst.clone(),
                    name: None,
                    params,
                    body: body_ctx.instructions,
                });
                IrValue::Register(dst)
            }
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
                if let Some(array_name) = self.yield_array.clone() {
                    let push = self.temp();
                    self.emit(IrInstruction::Member {
                        dst: push.clone(),
                        object: IrValue::Name(array_name),
                        property: IrValue::String("push".to_string()),
                    });
                    self.emit(IrInstruction::Call {
                        dst: dst.clone(),
                        callee: IrValue::Register(push),
                        args: vec![arg.clone()],
                    });
                    self.emit(IrInstruction::Marker(format!(
                        "%{dst} = collect {} {arg}",
                        if expr.delegate { "yield*" } else { "yield" }
                    )));
                } else {
                    self.emit(IrInstruction::Marker(format!(
                        "%{dst} = {} {arg}",
                        if expr.delegate { "yield*" } else { "yield" }
                    )));
                }
                IrValue::Register(dst)
            }
            Expr::Await(expr) => {
                let arg = self.lower_expr(&expr.arg);
                let dst = self.temp();
                self.emit(IrInstruction::Marker(format!("%{dst} = await {arg}")));
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
        let value = match lit {
            Lit::Str(value) => IrValue::String(value.value.to_string()),
            Lit::Bool(value) => IrValue::Bool(value.value),
            Lit::Null(_) => IrValue::Null,
            Lit::Num(value) => IrValue::Number(value.value),
            Lit::BigInt(value) => IrValue::String(format!("{}n", value.value)),
            Lit::Regex(value) => IrValue::String(format!("/{}/{}", value.exp, value.flags)),
            Lit::JSXText(value) => IrValue::String(value.value.to_string()),
        };
        let dst = self.temp();
        self.emit(IrInstruction::LoadConst {
            dst: dst.clone(),
            value,
        });
        IrValue::Register(dst)
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

    fn lower_object(&mut self, expr: &ObjectLit) -> IrValue {
        let mut props = Vec::new();
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
                        let key = prop_name(&prop.key);
                        let value = self.lower_expr(&prop.value);
                        props.push((key, value));
                    }
                    Prop::Method(prop) => {
                        let key = prop_name(&prop.key);
                        let dst = self.temp();
                        let params = function_param_names(&prop.function.params);
                        let mut body_ctx = self.child();
                        for param in &params {
                            body_ctx.declare_local(param.clone());
                        }
                        body_ctx.start_generator_body(prop.function.is_generator);
                        body_ctx.lower_param_defaults(&prop.function.params);
                        if let Some(body) = &prop.function.body {
                            body_ctx.lower_block(body);
                        }
                        body_ctx.finish_generator_body();
                        self.merge_child_externs(&body_ctx);
                        self.emit(IrInstruction::FunctionExpr {
                            dst: dst.clone(),
                            name: Some(key.clone()),
                            params,
                            body: body_ctx.instructions,
                        });
                        props.push((key, IrValue::Register(dst)));
                    }
                    _ => self.emit(IrInstruction::Unsupported(
                        "object accessor or assignment property".to_string(),
                    )),
                },
                PropOrSpread::Spread(spread) => {
                    self.emit(IrInstruction::Unsupported("object spread".to_string()));
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
        IrValue::Register(dst)
    }

    fn lower_fn_expr(&mut self, expr: &FnExpr) -> IrValue {
        let dst = self.temp();
        let name = expr.ident.as_ref().map(ident_name);
        let params = function_param_names(&expr.function.params);
        let mut body_ctx = self.child();
        if let Some(name) = &name {
            body_ctx.declare_local(name.clone());
        }
        for param in &params {
            body_ctx.declare_local(param.clone());
        }
        body_ctx.start_generator_body(expr.function.is_generator);
        body_ctx.lower_param_defaults(&expr.function.params);
        if let Some(body) = &expr.function.body {
            body_ctx.lower_block(body);
        }
        body_ctx.finish_generator_body();
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name,
            params,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
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
        for param in &params {
            body_ctx.declare_local(param.clone());
        }
        body_ctx.start_generator_body(function.is_generator);
        body_ctx.lower_param_defaults(&function.params);
        if let Some(body) = &function.body {
            body_ctx.lower_block(body);
        }
        body_ctx.finish_generator_body();
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name,
            params,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
    }

    fn lower_constructor_function(&mut self, constructor: &Constructor) -> IrValue {
        let dst = self.temp();
        let params = constructor_param_names(&constructor.params);
        let mut body_ctx = self.child();
        for param in &params {
            body_ctx.declare_local(param.clone());
        }
        body_ctx.lower_constructor_param_defaults(&constructor.params);
        if let Some(body) = &constructor.body {
            body_ctx.lower_block(body);
        }
        self.merge_child_externs(&body_ctx);
        self.emit(IrInstruction::FunctionExpr {
            dst: dst.clone(),
            name: Some("constructor".to_string()),
            params,
            body: body_ctx.instructions,
        });
        IrValue::Register(dst)
    }

    fn lower_opt_chain(&mut self, expr: &OptChainExpr) -> IrValue {
        let dst = self.temp();
        match &*expr.base {
            OptChainBase::Member(member) => {
                let value = self.lower_member(member);
                self.emit(IrInstruction::Move {
                    dst: dst.clone(),
                    src: value,
                });
            }
            OptChainBase::Call(call) => {
                let callee = self.lower_expr(&call.callee);
                let args = call
                    .args
                    .iter()
                    .map(|arg| self.lower_expr_or_spread(arg))
                    .collect::<Vec<_>>();
                self.emit(IrInstruction::Call {
                    dst: dst.clone(),
                    callee,
                    args,
                });
            }
        }
        self.emit(IrInstruction::Marker(format!(
            "optional_chain %{dst}, optional={}",
            expr.optional
        )));
        IrValue::Register(dst)
    }

    fn lower_update(&mut self, expr: &UpdateExpr) -> IrValue {
        let old = self.lower_expr(&expr.arg);
        let one = self.temp();
        self.emit(IrInstruction::LoadConst {
            dst: one.clone(),
            value: IrValue::Number(1.0),
        });
        let new_value = self.temp();
        let op = match expr.op {
            UpdateOp::PlusPlus => "+",
            UpdateOp::MinusMinus => "-",
        };
        self.emit(IrInstruction::Binary {
            dst: new_value.clone(),
            op: op.to_string(),
            left: old.clone(),
            right: IrValue::Register(one),
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
        let object = self.lower_expr(&expr.obj);
        let property = self.lower_member_property(&expr.prop);
        let dst = self.temp();
        self.emit(IrInstruction::Member {
            dst: dst.clone(),
            object,
            property,
        });
        IrValue::Register(dst)
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
                let value = if matches!(value, IrValue::Undefined) {
                    self.lower_expr(&assign.right)
                } else {
                    value
                };
                self.lower_pat_binding(&assign.left, value, kind);
            }
            Pat::Array(array) => {
                for (index, elem) in array.elems.iter().enumerate() {
                    if let Some(elem) = elem {
                        let item = self.temp();
                        self.emit(IrInstruction::Member {
                            dst: item.clone(),
                            object: value.clone(),
                            property: IrValue::Number(index as f64),
                        });
                        self.lower_pat_binding(elem, IrValue::Register(item), kind);
                    }
                }
            }
            Pat::Object(object) => {
                for prop in &object.props {
                    match prop {
                        ObjectPatProp::KeyValue(prop) => {
                            let key = prop_name(&prop.key);
                            let item = self.temp();
                            self.emit(IrInstruction::Member {
                                dst: item.clone(),
                                object: value.clone(),
                                property: IrValue::String(key),
                            });
                            self.lower_pat_binding(&prop.value, IrValue::Register(item), kind);
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
                            self.emit(IrInstruction::StoreName {
                                name: key,
                                src: IrValue::Register(item),
                            });
                        }
                        ObjectPatProp::Rest(prop) => {
                            let item = self.temp();
                            self.emit(IrInstruction::Marker(format!(
                                "%{item} = destructure_rest {value}"
                            )));
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
    module: core::IrModule,
}

#[derive(Default)]
struct FunctionBuildState {
    locals: BTreeMap<String, core::LocalId>,
    local_defs: Vec<core::IrLocal>,
    scopes: Vec<core::IrScope>,
    exception_handlers: Vec<core::IrExceptionHandler>,
    register_count: usize,
}

impl StructuredIrBuilder {
    fn new(extern_slots: Vec<String>) -> Self {
        Self {
            module: core::IrModule {
                extern_slots,
                ..core::IrModule::default()
            },
        }
    }

    fn build(mut self, instructions: Vec<IrInstruction>) -> core::IrModule {
        let entry = self.build_function(
            Some("entry".to_string()),
            Vec::new(),
            instructions,
            Vec::new(),
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
    ) -> core::FunctionId {
        let mut state = FunctionBuildState::new();
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
            flags: core::IrFunctionFlags::default(),
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
                        && !blocks[current.0].instructions.is_empty()
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
            IrInstruction::Function { name, params, body } => {
                let inherited_names = state.visible_names();
                let function =
                    self.build_function(Some(name.clone()), params, body, inherited_names);
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
                body,
            } => {
                let mut inherited_names = state.visible_names();
                if let Some(name) = &name {
                    inherited_names.push(name.clone());
                }
                let function = self.build_function(name, params, body, inherited_names);
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
            IrInstruction::Export { kind, names } => {
                for name in names {
                    let local = state.ensure_local(&name, core::IrBindingKind::Var);
                    self.module.exports.push(core::IrExportDecl::Local {
                        local,
                        exported: if kind == "default" {
                            "default".to_string()
                        } else {
                            name
                        },
                    });
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
            IrInstruction::Pop(value) => vec![core::IrInstruction::new(
                core::IrInstructionKind::Debug(format!("pop {}", value)),
            )],
            IrInstruction::Unsupported(message) => {
                vec![core::IrInstruction::new(
                    core::IrInstructionKind::Unsupported(message),
                )]
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
                core::IrValue::Const(self.push_const(if value.fract() == 0.0 {
                    core::IrConst::Int(value as i64)
                } else {
                    core::IrConst::Float(value)
                }))
            }
            IrValue::String(value) => {
                core::IrValue::Const(self.push_const(core::IrConst::String(value)))
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
        let id = core::ConstId(self.module.constants.len());
        self.module.constants.push(constant);
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

pub fn parse_source(source: &str) -> Result<Program, String> {
    parse_source_with_syntax(
        source,
        Syntax::Typescript(TsSyntax {
            tsx: false,
            decorators: true,
            ..Default::default()
        }),
    )
    .or_else(|ts_err| {
        parse_source_with_syntax(source, Syntax::Es(Default::default()))
            .map_err(|es_err| format!("{ts_err}; fallback parse error: {es_err}"))
    })
}

fn parse_source_with_syntax(source: &str, syntax: Syntax) -> Result<Program, String> {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        FileName::Custom("input.ts".into()).into(),
        source.to_string(),
    );

    let lexer = Lexer::new(syntax, Default::default(), StringInput::from(&*fm), None);

    let mut parser = Parser::new_from(lexer);
    let program = parser
        .parse_program()
        .map_err(|err| format!("parse error: {err:?}"))?;
    let parser_errors = parser.take_errors();
    if !parser_errors.is_empty() {
        return Err(format!("parse errors: {parser_errors:?}"));
    }

    Ok(program)
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

fn export_specifier_name(specifier: &ExportSpecifier) -> String {
    match specifier {
        ExportSpecifier::Namespace(namespace) => {
            format!("* as {}", module_export_name(&namespace.name))
        }
        ExportSpecifier::Default(default) => format!("default {}", ident_name(&default.exported)),
        ExportSpecifier::Named(named) => {
            let orig = module_export_name(&named.orig);
            let exported = named.exported.as_ref().map(module_export_name);
            exported
                .map(|exported| format!("{orig} as {exported}"))
                .unwrap_or(orig)
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
        .map(|param| pat_name(&param.pat).unwrap_or_else(|| "<unsupported>".to_string()))
        .collect()
}

fn pat_list_names(params: &[Pat]) -> Vec<String> {
    params
        .iter()
        .map(|param| pat_name(param).unwrap_or_else(|| "<unsupported>".to_string()))
        .collect()
}

fn constructor_param_names(params: &[ParamOrTsParamProp]) -> Vec<String> {
    params
        .iter()
        .map(|param| match param {
            ParamOrTsParamProp::Param(param) => {
                pat_name(&param.pat).unwrap_or_else(|| "<unsupported>".to_string())
            }
            ParamOrTsParamProp::TsParamProp(_) => "<unsupported>".to_string(),
        })
        .collect()
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
