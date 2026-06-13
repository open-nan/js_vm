use std::collections::{BTreeMap, BTreeSet};

use js_token_core::{BytecodeModule, EncodingConfig, IrInstruction, IrModule, IrValue};
use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use wasm_bindgen::prelude::*;

use crate::executor::Executor;

struct LoweringContext {
    instructions: Vec<IrInstruction>,
    temp_id: usize,
    label_id: usize,
    break_stack: Vec<String>,
    continue_stack: Vec<String>,
    extern_slots: BTreeMap<String, usize>,
    locals: BTreeSet<String>,
}

impl LoweringContext {
    fn with_externals(externs: &[String]) -> Self {
        Self {
            instructions: Vec::new(),
            temp_id: 0,
            label_id: 0,
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            extern_slots: externs
                .iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), index))
                .collect(),
            locals: BTreeSet::new(),
        }
    }

    fn into_module(self) -> IrModule {
        let mut extern_slots = vec![String::new(); self.extern_slots.len()];
        for (name, slot) in self.extern_slots {
            extern_slots[slot] = name;
        }
        IrModule {
            extern_slots,
            instructions: self.instructions,
        }
    }

    fn child(&self) -> Self {
        Self {
            instructions: Vec::new(),
            temp_id: 0,
            label_id: 0,
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            extern_slots: BTreeMap::new(),
            locals: self.locals.clone(),
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
        let label = format!("{prefix}_{}", self.label_id);
        self.label_id += 1;
        label
    }

    fn emit(&mut self, instruction: IrInstruction) {
        self.instructions.push(instruction);
    }

    fn lower_module(&mut self, module: &Module) {
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

    fn predeclare_stmt(&mut self, stmt: &Stmt) {
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

    fn lower_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Block(block) => self.lower_block(block),
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

    fn lower_fn_decl(&mut self, decl: &FnDecl) {
        let name = ident_name(&decl.ident);
        self.declare_local(name.clone());
        let params = decl
            .function
            .params
            .iter()
            .map(|param| pat_name(&param.pat).unwrap_or_else(|| "<unsupported>".to_string()))
            .collect::<Vec<_>>();

        let mut body_ctx = self.child();
        body_ctx.declare_local(name.clone());
        for param in &params {
            body_ctx.declare_local(param.clone());
        }
        if let Some(body) = &decl.function.body {
            body_ctx.lower_block(body);
        }
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
        let iterated = self.lower_expr(right);
        let start_label = self.label(kind);
        let end_label = self.label(&format!("{kind}_end"));
        self.emit(IrInstruction::Marker(format!("{kind} {iterated}")));
        self.emit(IrInstruction::Label(start_label.clone()));
        let item = IrValue::Register(self.temp());
        self.emit(IrInstruction::Marker(format!(
            "iterator_next {iterated}, {item}"
        )));
        self.lower_for_head_binding(left, item);
        self.break_stack.push(end_label.clone());
        self.continue_stack.push(start_label.clone());
        self.lower_stmt(body);
        self.continue_stack.pop();
        self.break_stack.pop();
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
        body_ctx.lower_block(&stmt.block);
        self.merge_child_externs(&body_ctx);

        let (catch_param, catch_body) = if let Some(handler) = &stmt.handler {
            let mut catch_ctx = self.child();
            if let Some(param) = handler.param.as_ref().and_then(pat_name) {
                catch_ctx.declare_local(param);
            }
            catch_ctx.lower_block(&handler.body);
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
            finally_ctx.lower_block(finalizer);
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
                let params = expr
                    .params
                    .iter()
                    .map(|param| pat_name(param).unwrap_or_else(|| "<unsupported>".to_string()))
                    .collect::<Vec<_>>();
                let mut body_ctx = self.child();
                for param in &params {
                    body_ctx.declare_local(param.clone());
                }
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
                self.emit(IrInstruction::Marker(format!(
                    "%{dst} = {} {arg}",
                    if expr.delegate { "yield*" } else { "yield" }
                )));
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
                        let params = prop
                            .function
                            .params
                            .iter()
                            .map(|param| {
                                pat_name(&param.pat).unwrap_or_else(|| "<unsupported>".to_string())
                            })
                            .collect::<Vec<_>>();
                        let mut body_ctx = self.child();
                        for param in &params {
                            body_ctx.declare_local(param.clone());
                        }
                        if let Some(body) = &prop.function.body {
                            body_ctx.lower_block(body);
                        }
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
        let params = expr
            .function
            .params
            .iter()
            .map(|param| pat_name(&param.pat).unwrap_or_else(|| "<unsupported>".to_string()))
            .collect::<Vec<_>>();
        let mut body_ctx = self.child();
        if let Some(name) = &name {
            body_ctx.declare_local(name.clone());
        }
        for param in &params {
            body_ctx.declare_local(param.clone());
        }
        if let Some(body) = &expr.function.body {
            body_ctx.lower_block(body);
        }
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
        self.emit(IrInstruction::Class {
            dst: dst.clone(),
            name: name.clone(),
            super_class,
            members,
        });
        if let Some(name) = name {
            if dst.is_none() {
                return IrValue::Name(name);
            }
        }
        dst.map(IrValue::Register).unwrap_or(IrValue::Undefined)
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

    fn lower_member_property(&mut self, prop: &MemberProp) -> String {
        match prop {
            MemberProp::Ident(ident) => ident.sym.to_string(),
            MemberProp::PrivateName(name) => format!("#{}", name.name),
            MemberProp::Computed(prop) => match &*prop.expr {
                Expr::Lit(Lit::Str(value)) => value.value.to_string(),
                Expr::Lit(Lit::Num(value)) => value.value.to_string(),
                Expr::Lit(Lit::Bool(value)) => value.value.to_string(),
                _ => self.lower_expr(&prop.expr).to_string(),
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
                        self.emit(IrInstruction::Marker(format!(
                            "%{item} = destructure_array {value}, {index}"
                        )));
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
                            self.emit(IrInstruction::Marker(format!(
                                "%{item} = destructure_object {value}, {key}"
                            )));
                            self.lower_pat_binding(&prop.value, IrValue::Register(item), kind);
                        }
                        ObjectPatProp::Assign(prop) => {
                            let key = ident_name(&prop.key);
                            self.declare_local(key.clone());
                            let item = self.temp();
                            self.emit(IrInstruction::Marker(format!(
                                "%{item} = destructure_object {value}, {key}"
                            )));
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

pub fn compile_to_ir(source: &str) -> Result<IrModule, String> {
    compile_to_ir_with_externals(source, &[])
}

pub fn compile_to_ir_with_externals(
    source: &str,
    externals: &[String],
) -> Result<IrModule, String> {
    let program = parse_source(source)?;

    let mut ctx = LoweringContext::with_externals(externals);
    match program {
        Program::Module(module) => ctx.lower_module(&module),
        Program::Script(script) => {
            for stmt in &script.body {
                ctx.predeclare_stmt(stmt);
            }
            for stmt in &script.body {
                ctx.lower_stmt(stmt);
            }
        }
    }

    Ok(ctx.into_module())
}

pub fn compile_to_ir_text(source: &str) -> Result<String, String> {
    compile_to_ir(source).map(|module| module.to_text())
}

pub fn compile_to_bytecode(source: &str) -> Result<BytecodeModule, String> {
    compile_to_ir(source).map(|module| module.to_bytecode())
}

pub fn compile_to_bytecode_with_externals(
    source: &str,
    externals: &[String],
) -> Result<BytecodeModule, String> {
    compile_to_ir_with_externals(source, externals).map(|module| module.to_bytecode())
}

pub fn compile_to_bytecode_text(source: &str) -> Result<String, String> {
    compile_to_bytecode(source).map(|module| module.to_text())
}

pub fn compile_to_bytecode_bytes(source: &str) -> Result<Vec<u8>, String> {
    compile_to_bytecode(source).map(|module| module.to_bytes())
}

pub fn compile_to_bytecode_bytes_with_encoding(
    source: &str,
    encoding: &EncodingConfig,
) -> Result<Vec<u8>, String> {
    compile_to_bytecode(source)?
        .to_bytes_with_encoding(encoding)
        .map_err(|err| err.to_string())
}

pub fn compile_to_bytecode_bytes_with_encoding_yaml(
    source: &str,
    yaml: &str,
) -> Result<Vec<u8>, String> {
    let encoding = EncodingConfig::from_yaml(yaml).map_err(|err| err.to_string())?;
    compile_to_bytecode_bytes_with_encoding(source, &encoding)
}

pub fn execute_source(source: &str) -> Result<String, String> {
    let bytecode = compile_to_bytecode(source)?;
    Executor::run(&bytecode)
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
}

pub fn execute_source_with_externals(source: &str, externals: &[String]) -> Result<String, String> {
    let bytecode = compile_to_bytecode_with_externals(source, externals)?;
    Executor::run_with_external_names(&bytecode, externals)
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
}

pub fn execute_bytecode_bytes(bytes: &[u8]) -> Result<String, String> {
    let bytecode = BytecodeModule::from_bytes(bytes).map_err(|err| err.to_string())?;
    Executor::run(&bytecode)
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
}

pub fn execute_bytecode_bytes_with_encoding_yaml(
    bytes: &[u8],
    yaml: &str,
) -> Result<String, String> {
    let encoding = EncodingConfig::from_yaml(yaml).map_err(|err| err.to_string())?;
    let bytecode = BytecodeModule::from_bytes_with_encoding(bytes, &encoding)
        .map_err(|err| err.to_string())?;
    Executor::run(&bytecode)
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct Compiler {
    ir: IrModule,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl Compiler {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(source: &str) -> Result<Compiler, JsValue> {
        let ir = compile_to_ir(source).map_err(|err| JsValue::from_str(&err))?;
        Ok(Compiler { ir })
    }

    pub fn with_externals(source: &str, externals: Box<[JsValue]>) -> Result<Compiler, JsValue> {
        let externals = js_values_to_strings(&externals);
        let ir = compile_to_ir_with_externals(source, &externals)
            .map_err(|err| JsValue::from_str(&err))?;
        Ok(Compiler { ir })
    }

    pub fn extern_slots(&self) -> Vec<String> {
        self.ir.extern_slots.clone()
    }

    pub fn to_text(&self) -> String {
        self.ir.to_text()
    }

    pub fn to_bytecode(&self) -> String {
        self.ir.to_bytecode().to_text()
    }

    pub fn to_bytecode_text(&self) -> String {
        self.to_bytecode()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.ir.to_bytecode().to_bytes()
    }

    pub fn to_bytecode_bytes(&self) -> Vec<u8> {
        self.to_bytes()
    }

    pub fn to_bytes_with_encoding(&self, yaml: &str) -> Result<Vec<u8>, String> {
        let encoding = EncodingConfig::from_yaml(yaml).map_err(|err| err.to_string())?;
        self.ir
            .to_bytecode()
            .to_bytes_with_encoding(&encoding)
            .map_err(|err| err.to_string())
    }

    pub fn execute(&self) -> Result<String, String> {
        Executor::run(&self.ir.to_bytecode())
            .map(|value| value.to_string())
            .map_err(|err| err.to_string())
    }

    pub fn execute_with_externals(&self, externals: Box<[JsValue]>) -> Result<String, String> {
        let externals = js_values_to_strings(&externals);
        Executor::run_with_external_names(&self.ir.to_bytecode(), &externals)
            .map(|value| value.to_string())
            .map_err(|err| err.to_string())
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn js_execute(source: &str) -> Result<String, String> {
    execute_source(source)
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn js_execute_with_externals(
    source: &str,
    externals: Box<[JsValue]>,
) -> Result<String, String> {
    let externals = js_values_to_strings(&externals);
    execute_source_with_externals(source, &externals)
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn js_execute_bytes(bytes: &[u8]) -> Result<String, String> {
    execute_bytecode_bytes(bytes)
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn js_execute_bytes_with_encoding(bytes: &[u8], yaml: &str) -> Result<String, String> {
    execute_bytecode_bytes_with_encoding_yaml(bytes, yaml)
}

fn js_values_to_strings(values: &[JsValue]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}

fn is_implicit_global(name: &str) -> bool {
    matches!(
        name,
        "undefined" | "NaN" | "Infinity" | "this" | "super" | "import"
    )
}

fn parse_source(source: &str) -> Result<Program, String> {
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

#[cfg(test)]
mod active_tests {
    use js_token_core::BytecodeModule;

    use super::{
        Compiler, compile_to_bytecode_bytes_with_encoding_yaml, compile_to_ir, execute_source,
    };

    #[test]
    fn wasm_compiler_facade_exposes_all_outputs() {
        let compiler = Compiler::new("const a = 1;").unwrap();
        assert!(compiler.to_bytes().starts_with(b"JSTKBC01"));
    }

    #[test]
    fn compiles_with_yaml_encoding() {
        let bytes = compile_to_bytecode_bytes_with_encoding_yaml(
            "const a = 1;",
            r#"
            magic: "YAMLBC01"
            opcodes:
              LOAD_CONST: 77
            "#,
        )
        .unwrap();

        assert!(bytes.starts_with(b"YAMLBC01"));
        assert!(bytes.contains(&77));
    }

    #[test]
    fn wasm_facade_executes_source() {
        assert_eq!(execute_source("const a = 1 + 2; a;").unwrap(), "3");
        let compiler = Compiler::new("const obj = {a: 2}; obj.a;").unwrap();
        assert_eq!(compiler.execute().unwrap(), "2");
    }

    #[test]
    fn builds_external_slot_table() {
        let ir = compile_to_ir("console.log(window.title);").unwrap();
        assert_eq!(
            ir.extern_slots,
            vec!["console".to_string(), "window".to_string()]
        );
        assert!(ir.to_bytecode().to_bytes().starts_with(b"JSTKBC01"));
    }

    #[test]
    fn executes_with_external_names() {
        let result = execute_source("console.log(1); 2;").unwrap();
        assert_eq!(result, "2");
    }

    #[test]
    fn syntax_corpus_compiles_to_roundtrippable_bytecode() {
        for source in syntax_corpus() {
            assert_roundtrips(source);
        }
    }

    #[test]
    fn generated_valid_js_syntax_fuzz_roundtrips() {
        let mut fuzzer = JsFuzzer::new(0x9e37_79b9_7f4a_7c15);
        for index in 0..256 {
            let source = fuzzer.program(index);
            assert_roundtrips(&source);
        }
    }

    #[test]
    fn generated_executable_js_fuzz_does_not_crash_executor() {
        let mut fuzzer = JsFuzzer::new(0x5eed_cafe_f00d_beef);
        for index in 0..128 {
            let source = fuzzer.executable_program(index);
            execute_source(&source).unwrap_or_else(|err| {
                panic!("executor rejected generated JS at case {index}\nsource:\n{source}\nerror: {err}")
            });
        }
    }

    fn assert_roundtrips(source: &str) {
        let module = super::compile_to_bytecode(source)
            .unwrap_or_else(|err| panic!("compiler rejected JS\nsource:\n{source}\nerror: {err}"));
        let bytes = module.to_bytes();
        let decoded = BytecodeModule::from_bytes(&bytes).unwrap_or_else(|err| {
            panic!("bytecode roundtrip failed\nsource:\n{source}\nerror: {err}")
        });
        assert_eq!(
            decoded, module,
            "bytecode changed after roundtrip\nsource:\n{source}"
        );
    }

    fn syntax_corpus() -> &'static [&'static str] {
        &[
            "let a = 1; const b = 2; var c = a + b; c;",
            "let a = 1; a += 2; a++; --a;",
            "const arr = [1, , 3, ...xs]; arr[0];",
            "const obj = {a: 1, b, m(x) { return x + this.a; }, ...rest}; obj.m(2);",
            "const {a, b: c, ...rest} = source; const [x, , y = 3, ...tail] = list;",
            "if (flag) { call(1); } else { call(2); }",
            "while (i < 10) { i++; if (i === 4) continue; if (i === 8) break; }",
            "do { i = i + 1; } while (i < 3);",
            "for (let i = 0; i < 4; i++) { total += i; }",
            "for (const key in object) { keys.push(key); }",
            "for (const item of items) { sum += item; }",
            "switch (kind) { case 'a': out = 1; break; case 'b': out = 2; default: out = 3; }",
            "try { throw value; } catch (err) { console.log(err); } finally { cleanup(); }",
            "function add(a, b) { return a + b; } add(1, 2);",
            "const fn = function named(x) { return x ? named(x - 1) : 0; }; fn(3);",
            "const inc = (x) => x + 1; const block = (x) => { return x * 2; };",
            "class Box { constructor(value) { this.value = value; } get() { return this.value; } } new Box(1);",
            "class Child extends Parent { static make() { return new Child(); } field = 1; #secret = 2; }",
            "const msg = `hello ${name}!`; tag`x${value}y`; ",
            "const result = a ? b : c ? d : e;",
            "const maybe = root?.child?.call?.(arg);",
            "async function load() { const value = await fetch(url); return value; }",
            "function* ids() { yield 1; yield* more; }",
            "import def, {a as localA, b} from 'pkg'; export {localA as a}; export default b;",
            "export class Exported { method() { return 1; } } export * from 'other';",
            "interface Shape { x: number } type Name = string; enum Kind { A, B }",
            "const x = value as number; const y = value satisfies unknown; const z = value!;",
            "with (scope) { value = name; } debugger;",
            "label: for (let i = 0; i < 2; i++) { break label; }",
        ]
    }

    struct JsFuzzer {
        state: u64,
        next_var: usize,
    }

    impl JsFuzzer {
        fn new(seed: u64) -> Self {
            Self {
                state: seed,
                next_var: 0,
            }
        }

        fn program(&mut self, index: usize) -> String {
            self.next_var = 0;
            let mut locals = vec!["a".to_string(), "b".to_string(), "c".to_string()];
            let mut source = format!(
                "let a = {}; let b = {}; let c = {};",
                index % 7,
                (index + 1) % 11,
                (index + 2) % 13
            );
            for _ in 0..self.range(3, 7) {
                source.push_str(&self.stmt(3, &mut locals, false));
            }
            source
        }

        fn executable_program(&mut self, index: usize) -> String {
            self.next_var = 0;
            let mut locals = vec!["a".to_string(), "b".to_string(), "c".to_string()];
            let mut source = format!(
                "let a = {}; let b = {}; let c = {};",
                index % 5,
                (index + 2) % 7,
                (index + 4) % 9
            );
            for _ in 0..self.range(2, 5) {
                source.push_str(&self.safe_stmt(3, &mut locals));
            }
            source.push_str("a + b + c;");
            source
        }

        fn stmt(&mut self, depth: usize, locals: &mut Vec<String>, in_loop: bool) -> String {
            match self.pick(12) {
                0 | 1 => {
                    let name = self.new_local();
                    let expr = self.expr(depth, locals);
                    locals.push(name.clone());
                    format!("let {name} = {expr};")
                }
                2 => format!("{} = {};", self.local(locals), self.expr(depth, locals)),
                3 => format!("({});", self.expr(depth, locals)),
                4 if depth > 0 => format!(
                    "if ({}) {{{}}} else {{{}}}",
                    self.expr(1, locals),
                    self.stmt(depth - 1, locals, in_loop),
                    self.stmt(depth - 1, locals, in_loop)
                ),
                5 if depth > 0 => {
                    let name = self.new_local();
                    locals.push(name.clone());
                    format!(
                        "for (let {name} = 0; {name} < {}; {name}++) {{{}}}",
                        self.range(1, 4),
                        self.stmt(depth - 1, locals, true)
                    )
                }
                6 if depth > 0 => format!(
                    "while ({} < {}) {{{} break;}}",
                    self.local(locals),
                    self.range(1, 9),
                    self.stmt(depth - 1, locals, true)
                ),
                7 if in_loop => "continue;".to_string(),
                8 if in_loop => "break;".to_string(),
                9 if depth > 0 => format!(
                    "try {{{}}} catch (err) {{{}}} finally {{{}}}",
                    self.stmt(depth - 1, locals, in_loop),
                    self.stmt(depth - 1, locals, in_loop),
                    self.stmt(depth - 1, locals, in_loop)
                ),
                10 => format!("console.log({});", self.expr(1, locals)),
                _ => format!("({});", self.expr(depth, locals)),
            }
        }

        fn safe_stmt(&mut self, depth: usize, locals: &mut Vec<String>) -> String {
            match self.pick(6) {
                0 | 1 => {
                    let name = self.new_local();
                    let expr = self.safe_expr(depth, locals);
                    locals.push(name.clone());
                    format!("let {name} = {expr};")
                }
                2 => format!(
                    "{} = {};",
                    self.local(locals),
                    self.safe_expr(depth, locals)
                ),
                3 if depth > 0 => format!(
                    "if ({}) {{{}}} else {{{}}}",
                    self.safe_expr(1, locals),
                    self.safe_stmt(depth - 1, locals),
                    self.safe_stmt(depth - 1, locals)
                ),
                4 if depth > 0 => {
                    let value = self.safe_expr(depth - 1, locals);
                    format!("{{ let inner = {value}; inner; }}")
                }
                _ => format!("({});", self.safe_expr(depth, locals)),
            }
        }

        fn expr(&mut self, depth: usize, locals: &[String]) -> String {
            if depth == 0 {
                return self.atom(locals);
            }

            match self.pick(14) {
                0 => self.atom(locals),
                1 => format!(
                    "({} + {})",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
                2 => format!(
                    "({} * {})",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
                3 => format!(
                    "({} === {})",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
                4 => format!(
                    "({} ? {} : {})",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
                5 => format!(
                    "[{}, {}, ...xs]",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
                6 => format!(
                    "{{p: {}, q: {}, m(x) {{ return x + 1; }}}}",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
                7 => format!("({}).p", self.expr(depth - 1, locals)),
                8 => format!("({})?.p", self.expr(depth - 1, locals)),
                9 => format!("((x) => x + 1)({})", self.expr(depth - 1, locals)),
                10 => format!(
                    "(function(x) {{ return x; }})({})",
                    self.expr(depth - 1, locals)
                ),
                11 => format!("new Ctor({})", self.expr(depth - 1, locals)),
                12 => format!("`v=${{{}}}`", self.expr(depth - 1, locals)),
                _ => format!(
                    "({} ?? {})",
                    self.expr(depth - 1, locals),
                    self.expr(depth - 1, locals)
                ),
            }
        }

        fn safe_expr(&mut self, depth: usize, locals: &[String]) -> String {
            if depth == 0 {
                return self.safe_atom(locals);
            }

            match self.pick(8) {
                0 => self.safe_atom(locals),
                1 => format!(
                    "({} + {})",
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals)
                ),
                2 => format!(
                    "({} - {})",
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals)
                ),
                3 => format!(
                    "({} * {})",
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals)
                ),
                4 => format!(
                    "({} < {})",
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals)
                ),
                5 => format!(
                    "({} ? {} : {})",
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals)
                ),
                6 => format!("((x) => x + 1)({})", self.safe_expr(depth - 1, locals)),
                _ => format!(
                    "[{}, {}].length",
                    self.safe_expr(depth - 1, locals),
                    self.safe_expr(depth - 1, locals)
                ),
            }
        }

        fn atom(&mut self, locals: &[String]) -> String {
            match self.pick(8) {
                0 => self.range(0, 17).to_string(),
                1 => format!("{:?}", format!("s{}", self.range(0, 9))),
                2 => ["true", "false", "null", "undefined"][self.pick(4)].to_string(),
                3 | 4 => self.local(locals),
                5 => {
                    ["externalValue", "window", "document", "globalThis"][self.pick(4)].to_string()
                }
                6 => format!("!{}", self.local(locals)),
                _ => format!("({})", self.local(locals)),
            }
        }

        fn safe_atom(&mut self, locals: &[String]) -> String {
            match self.pick(5) {
                0 => self.range(0, 17).to_string(),
                1 => ["true", "false"][self.pick(2)].to_string(),
                _ => self.local(locals),
            }
        }

        fn new_local(&mut self) -> String {
            let name = format!("v{}", self.next_var);
            self.next_var += 1;
            name
        }

        fn local(&mut self, locals: &[String]) -> String {
            locals[self.pick(locals.len())].clone()
        }

        fn range(&mut self, start: usize, end: usize) -> usize {
            start + self.pick(end - start)
        }

        fn pick(&mut self, count: usize) -> usize {
            (self.next() as usize) % count
        }

        fn next(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.state
        }
    }
}

fn pat_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(ident_name(&ident.id)),
        Pat::Expr(expr) => match &**expr {
            Expr::Ident(ident) => Some(ident_name(ident)),
            _ => None,
        },
        _ => None,
    }
}

fn ident_name(ident: &Ident) -> String {
    ident.sym.to_string()
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

fn module_export_name(name: &ModuleExportName) -> String {
    name.atom().to_string()
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

fn import_local_name(specifier: &ImportSpecifier) -> Option<String> {
    match specifier {
        ImportSpecifier::Named(named) => Some(ident_name(&named.local)),
        ImportSpecifier::Default(default) => Some(ident_name(&default.local)),
        ImportSpecifier::Namespace(namespace) => Some(ident_name(&namespace.local)),
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

fn key_name(key: &Key) -> String {
    match key {
        Key::Private(name) => format!("#{}", name.name),
        Key::Public(name) => prop_name(name),
    }
}

fn method_kind_name(kind: MethodKind) -> &'static str {
    match kind {
        MethodKind::Method => "",
        MethodKind::Getter => "get",
        MethodKind::Setter => "set",
    }
}

fn super_prop_name(expr: &SuperPropExpr) -> String {
    match &expr.prop {
        SuperProp::Ident(ident) => ident.sym.to_string(),
        SuperProp::Computed(computed) => format!("[{:?}]", computed.expr),
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

fn assign_binary_op(op: AssignOp) -> &'static str {
    op.to_update().map(bin_op).unwrap_or("=")
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
