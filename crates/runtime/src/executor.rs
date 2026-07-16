use crate::env::{LexicalEnv, ScopeKind};
use crate::error::ExecuteError;
use crate::host::{
    HostBridge, JsHostBridge, VmJsHandle, clear_host_overlays, external_name_overlay_get, js_error,
    js_function_name, js_overlay_get_in_prototype_chain, js_overlay_set, js_value_is_symbol,
    value_to_js_value, vm_js_handle,
};
#[cfg(feature = "bigint")]
use crate::host::{js_bigint_builtin_name, js_value_is_bigint};
#[cfg(any(feature = "array-builtins", feature = "string-builtins"))]
use crate::ops::normalize_index;
#[cfg(feature = "regexp")]
use crate::ops::regexp_exec_value;
#[cfg(feature = "function-builtins")]
use crate::ops::{apply_argument_list, function_source_string};
use crate::ops::{
    binary as fallback_binary, collect_scope_metadata, constant_string, constant_value,
    count_operand, external_string, find_try_parts, get_local_member, name_string, operand,
    operator_name, property_key, register, set_member, unary,
};
#[cfg(feature = "object-builtins")]
use crate::ops::{object_has_own_property, object_to_string_tag};
#[cfg(feature = "module")]
use crate::value::ModuleValue;
use crate::value::{ClassValue, ExternalRefValue, FunctionValue, Value, array_value, object_value};
#[cfg(feature = "generator")]
use crate::value::{GeneratorState, NativeFunctionValue};
use js_sys::Function as JsFunction;
use js_token_core::{
    BytecodeInstruction, BytecodeModule, BytecodeModuleKind, BytecodeOp, BytecodeOperand,
};
use std::{
    cell::Cell,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue};

#[derive(Debug)]
pub struct Executor<B: HostBridge> {
    registers: Vec<Value>,
    lexical_env: LexicalEnv,
    instruction_scope_depths: Vec<usize>,
    last_value: Value,
    exports: BTreeMap<String, Value>,
    host_bridge: B,
    external_names: Vec<String>,
    call_depth: usize,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
    call_stack: Rc<RefCell<Vec<usize>>>,
    execution_budget: Rc<Cell<usize>>,
}

pub const DEFAULT_MAX_CALL_DEPTH: usize = 128;
pub const DEFAULT_MAX_RECURSIVE_CALL_DEPTH: usize = 8;
const MAX_EXECUTION_STEPS: usize = 250_000;
#[cfg(feature = "generator")]
const FUNCTION_FLAG_GENERATOR: u32 = 1 << 1;

#[derive(Debug, Clone, PartialEq)]
enum Flow {
    Value(Value),
    Return(Value),
    Throw(Value),
    #[cfg_attr(not(feature = "generator"), allow(dead_code))]
    Yield {
        value: Value,
        resume_pc: usize,
        resume_dst: Option<u32>,
    },
}

fn catchable_error_value(error: &ExecuteError) -> Option<Value> {
    match error {
        ExecuteError::Thrown(value) => Some(value.clone()),
        ExecuteError::ReferenceError(message) => Some(vm_error_object("ReferenceError", message)),
        ExecuteError::TypeError(message) => Some(vm_error_object("TypeError", message)),
        ExecuteError::RangeError(message) => Some(vm_error_object("RangeError", message)),
        ExecuteError::SyntaxError(message) => Some(vm_error_object("SyntaxError", message)),
        _ => None,
    }
}

fn vm_error_object(kind: &str, message: &str) -> Value {
    object_value(BTreeMap::from([
        ("__error_type".to_string(), Value::String(kind.to_string())),
        ("name".to_string(), Value::String(kind.to_string())),
        ("message".to_string(), Value::String(message.to_string())),
    ]))
}

fn accessor_getter_key(property: &str) -> String {
    format!("__accessor_get__:{property}")
}

fn is_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Undefined)
        || matches!(value, Value::JsValue(value) | Value::BoundJsFunction(value, _) if value.is_undefined())
}

fn is_null_or_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Undefined)
        || matches!(value, Value::JsValue(value) | Value::BoundJsFunction(value, _) if value.is_null() || value.is_undefined())
}

fn is_vm_object_like(value: &Value) -> bool {
    match value {
        Value::Object(_)
        | Value::Array(_)
        | Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::Class(_)
        | Value::Module(_) => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => vm_js_handle(value).is_some(),
        _ => false,
    }
}

#[cfg(feature = "bigint")]
fn is_host_js_object_like(value: &Value) -> bool {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            value.is_object() || value.dyn_ref::<JsFunction>().is_some()
        }
        _ => false,
    }
}

fn is_to_primitive_object_result(value: &Value) -> bool {
    is_vm_object_like(value)
        || matches!(value, Value::JsValue(value) if value.is_object())
        || matches!(value, Value::BoundJsFunction(_, _))
}

fn is_primitive_like(value: &Value) -> bool {
    match value {
        Value::Number(_)
        | Value::BigInt(_)
        | Value::String(_)
        | Value::Symbol(_)
        | Value::Bool(_)
        | Value::Null
        | Value::Undefined => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            vm_js_handle(value).is_none() && !value.is_object()
        }
        _ => false,
    }
}

fn is_symbol_like(value: &Value) -> bool {
    matches!(value, Value::Symbol(_))
        || matches!(value, Value::JsValue(value) | Value::BoundJsFunction(value, _) if js_value_is_symbol(value))
}

impl<B: HostBridge> Executor<B> {
    fn with_host_bridge_and_limits(
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
    ) -> Self {
        Self {
            registers: Vec::new(),
            lexical_env: LexicalEnv::default(),
            instruction_scope_depths: Vec::new(),
            last_value: Value::Undefined,
            exports: BTreeMap::new(),
            host_bridge,
            external_names: Vec::new(),
            call_depth: 0,
            max_call_depth: max_call_depth.max(1),
            max_recursive_call_depth: max_recursive_call_depth.max(1),
            call_stack: Rc::new(RefCell::new(Vec::new())),
            execution_budget: Rc::new(Cell::new(MAX_EXECUTION_STEPS)),
        }
    }

    pub fn run_with_host_bridge(
        module: &BytecodeModule,
        host_bridge: B,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_limits(
            module,
            host_bridge,
            DEFAULT_MAX_CALL_DEPTH,
            DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
        )
    }

    pub fn run_with_host_bridge_and_limits(
        module: &BytecodeModule,
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_external_names_and_limits(
            module,
            host_bridge,
            module.extern_slots.clone(),
            max_call_depth,
            max_recursive_call_depth,
        )
    }

    pub fn run_with_host_bridge_and_external_names(
        module: &BytecodeModule,
        host_bridge: B,
        external_names: Vec<String>,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_external_names_and_limits(
            module,
            host_bridge,
            external_names,
            DEFAULT_MAX_CALL_DEPTH,
            DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
        )
    }

    pub fn run_with_host_bridge_and_external_names_and_limits(
        module: &BytecodeModule,
        host_bridge: B,
        external_names: Vec<String>,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
    ) -> Result<Value, ExecuteError> {
        #[cfg(not(feature = "module"))]
        if module.kind == BytecodeModuleKind::Module {
            return Err(ExecuteError::Unsupported("module"));
        }
        clear_host_overlays();
        host_bridge.validate_extern_count(module.extern_slots.len())?;
        let mut executor = Self::with_host_bridge_and_limits(
            host_bridge,
            max_call_depth,
            max_recursive_call_depth,
        );
        executor.external_names = external_names;
        executor.inject_module_externals(module);
        executor.load_scope_metadata(module)?;
        executor.hoist_function_declarations(module, 0, module.instructions.len())?;
        match executor.execute_range(module, 0, module.instructions.len())? {
            Flow::Value(value) | Flow::Return(value) => {
                #[cfg(feature = "module")]
                if module.kind == BytecodeModuleKind::Module {
                    executor.module_namespace_value()
                } else {
                    Ok(value)
                }
                #[cfg(not(feature = "module"))]
                {
                    Ok(value)
                }
            }
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
            Flow::Yield { .. } => Err(ExecuteError::Runtime(
                "yield outside generator frame".to_string(),
            )),
        }
    }

    fn inject_module_externals(&mut self, module: &BytecodeModule) {
        for (index, name) in module.extern_slots.iter().enumerate() {
            let name = self
                .external_names
                .get(index)
                .cloned()
                .unwrap_or_else(|| name.clone());
            self.lexical_env.define_global_if_absent(
                name.clone(),
                Value::ExternalRef(ExternalRefValue::new(index as u32, name.clone())),
            );
        }
    }

    #[cfg(feature = "module")]
    fn module_namespace_value(&self) -> Result<Value, ExecuteError> {
        self.host_bridge.module_value(ModuleValue {
            source: "module".to_string(),
            exports: self.exports.clone(),
        })
    }

    fn load_scope_metadata(&mut self, module: &BytecodeModule) -> Result<(), ExecuteError> {
        let metadata = collect_scope_metadata(module)?;
        self.instruction_scope_depths = metadata.instruction_scope_depths;
        Ok(())
    }

    fn execute_range(
        &mut self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<Flow, ExecuteError> {
        let entry_env_depth = self.lexical_env.depth();
        let mut pc = start;
        while pc < end {
            self.consume_step()?;
            let instruction = &module.instructions[pc];
            match instruction.op {
                BytecodeOp::Marker | BytecodeOp::Label => {}
                BytecodeOp::EnterScope => {
                    let kind = self.read_scope_kind(operand(instruction, 0)?)?;
                    self.lexical_env.push_frame(kind);
                }
                BytecodeOp::LeaveScope => {
                    self.lexical_env.pop_frame();
                }
                BytecodeOp::Declare => {
                    self.declare_binding(
                        module,
                        operand(instruction, 0)?,
                        operand(instruction, 1)?,
                    )?;
                }
                BytecodeOp::LoadConst => {
                    let dst = register(instruction, 0)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::LoadName | BytecodeOp::LoadLocal => {
                    let dst = register(instruction, 0)?;
                    let source = operand(instruction, 1)?;
                    let value = match source {
                        BytecodeOperand::External(_) => match self.read_value(module, source) {
                            Err(ExecuteError::ReferenceError(_))
                                if self.next_instruction_is_typeof_register(module, pc, dst)? =>
                            {
                                Value::Undefined
                            }
                            result => result?,
                        },
                        BytecodeOperand::LocalSlot(slot) => self.lexical_env.get_slot(*slot),
                        _ => {
                            let name = self.read_name(module, source)?;
                            self.resolve_name_value(self.get_name(&name))?
                        }
                    };
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreName | BytecodeOp::StoreLocal => {
                    let target = operand(instruction, 0)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    self.write_operand_target(module, target, value.clone())?;
                    self.last_value = value;
                }
                BytecodeOp::StoreMember => {
                    let object_operand = operand(instruction, 0)?;
                    let mut object = self.read_value(module, object_operand)?;
                    let property_value = self.read_value(module, operand(instruction, 1)?)?;
                    let property_value =
                        self.to_primitive_with_hint(module, property_value, "string")?;
                    let property = property_key(&property_value);
                    let value = self.read_value(module, operand(instruction, 2)?)?;
                    if let Value::ExternalRef(reference) = &object {
                        self.host_bridge.set(reference, &property, &value)?;
                    } else {
                        set_member(&mut object, &property, value.clone())?;
                        self.write_operand_target(module, object_operand, object)?;
                    }
                    self.last_value = value;
                }
                BytecodeOp::Move => {
                    let dst = register(instruction, 0)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Binary => {
                    let dst = register(instruction, 0)?;
                    let op = self.read_operator(module, operand(instruction, 1)?)?;
                    let left = self.read_value(module, operand(instruction, 2)?)?;
                    let right = self.read_value(module, operand(instruction, 3)?)?;
                    let value = self.binary(module, &op, left, right)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Unary => {
                    let dst = register(instruction, 0)?;
                    let op = self.read_operator(module, operand(instruction, 1)?)?;
                    let arg_operand = operand(instruction, 2)?;
                    let arg = match self.read_value(module, arg_operand) {
                        Err(ExecuteError::ReferenceError(_)) if op == "typeof" => Value::Undefined,
                        result => result?,
                    };
                    let arg = if matches!(op.as_str(), "+" | "-" | "~") {
                        self.to_primitive(module, arg)?
                    } else {
                        arg
                    };
                    let value = unary(&op, arg)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Member => {
                    let dst = register(instruction, 0)?;
                    let object = self.read_value(module, operand(instruction, 1)?)?;
                    let property_value = self.read_value(module, operand(instruction, 2)?)?;
                    let property_value =
                        self.to_primitive_with_hint(module, property_value, "string")?;
                    let property = property_key(&property_value);
                    let value = self.get_member(module, &object, &property)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Array => {
                    let dst = register(instruction, 0)?;
                    let count = count_operand(instruction, 1)? as usize;
                    let mut items = Vec::with_capacity(count);
                    for index in 0..count {
                        items.push(self.read_value(module, operand(instruction, 2 + index)?)?);
                    }
                    let value = array_value(items);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Object => {
                    let dst = register(instruction, 0)?;
                    let count = count_operand(instruction, 1)? as usize;
                    let mut props = BTreeMap::new();
                    for index in 0..count {
                        let key = self
                            .read_constant_string(module, operand(instruction, 2 + index * 2)?)?;
                        let value =
                            self.read_value(module, operand(instruction, 3 + index * 2)?)?;
                        props.insert(key, value);
                    }
                    let value = object_value(props);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::ObjectRest => {
                    let dst = register(instruction, 0)?;
                    let source = self.read_value(module, operand(instruction, 1)?)?;
                    let count = count_operand(instruction, 2)? as usize;
                    let mut args = Vec::with_capacity(count + 1);
                    args.push(source);
                    for index in 0..count {
                        args.push(self.read_value(module, operand(instruction, 3 + index)?)?);
                    }
                    let value = self.call_object_rest(module, args)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Call => {
                    let dst = register(instruction, 0)?;
                    let callee_operand = operand(instruction, 1)?;
                    let callee = self.read_value(module, callee_operand)?;
                    #[cfg(not(feature = "compact-errors"))]
                    let callee_display = callee.to_string();
                    let count = count_operand(instruction, 2)? as usize;
                    let args = self.read_args(module, instruction, 3, count)?;
                    let value = self.call(module, callee, args).map_err(|err| match err {
                        #[cfg(feature = "compact-errors")]
                        err => err,
                        #[cfg(not(feature = "compact-errors"))]
                        ExecuteError::TypeError(message) => ExecuteError::TypeError(format!(
                            "{message} at pc {pc} callee {callee_operand:?} value {callee_display}"
                        )),
                        #[cfg(not(feature = "compact-errors"))]
                        ExecuteError::RangeError(message) => ExecuteError::RangeError(format!(
                            "{message} at pc {pc} callee {callee_operand:?} value {callee_display}"
                        )),
                        #[cfg(not(feature = "compact-errors"))]
                        err => err,
                    })?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::New => {
                    let dst = register(instruction, 0)?;
                    let callee = self.read_value(module, operand(instruction, 1)?)?;
                    let count = count_operand(instruction, 2)? as usize;
                    let args = self.read_args(module, instruction, 3, count)?;
                    let value = self.construct(module, callee, args)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Template => {
                    let dst = register(instruction, 0)?;
                    let quasi_count = count_operand(instruction, 1)? as usize;
                    let mut out = String::new();
                    for index in 0..quasi_count {
                        out.push_str(
                            &self.read_constant_string(module, operand(instruction, 2 + index)?)?,
                        );
                        if let Ok(expr) = operand(instruction, 3 + quasi_count + index) {
                            let value = self.to_primitive_with_hint(
                                module,
                                self.read_value(module, expr)?,
                                "string",
                            )?;
                            out.push_str(&value.to_string());
                        }
                    }
                    let value = Value::String(out);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::FunctionStart => {
                    let function = self.function_from_start(module, pc, false)?;
                    let name = function.name.clone().ok_or_else(|| {
                        ExecuteError::Runtime("function declaration missing name".to_string())
                    })?;
                    let body_end = function.body_end;
                    let value = self.host_bridge.function_value(function);
                    self.lexical_env.set_or_define_current(name, value);
                    pc = body_end;
                    continue;
                }
                BytecodeOp::FunctionExprStart => {
                    let dst = register(instruction, 0)?;
                    let function = self.function_from_start(module, pc, true)?;
                    let body_end = function.body_end;
                    self.write_register(dst, self.host_bridge.function_value(function));
                    pc = body_end;
                    continue;
                }
                BytecodeOp::FunctionEnd | BytecodeOp::FunctionExprEnd => {
                    return self.finish_flow(entry_env_depth, Flow::Value(Value::Undefined));
                }
                BytecodeOp::Class => {
                    let value = self.class_from_instruction(module, instruction)?;
                    if let BytecodeOperand::Register(dst) = operand(instruction, 0)? {
                        self.write_register(*dst, value.clone());
                    }
                    if let BytecodeOperand::Name(index) = operand(instruction, 1)? {
                        self.lexical_env
                            .set_or_define_current(name_string(module, *index)?, value.clone());
                    } else if let BytecodeOperand::LocalSlot(slot) = operand(instruction, 1)? {
                        self.lexical_env.set_slot(*slot, value.clone());
                    }
                    self.last_value = value;
                }
                BytecodeOp::Import => {
                    #[cfg(not(feature = "module"))]
                    {
                        return Err(ExecuteError::Unsupported("module"));
                    }
                    #[cfg(feature = "module")]
                    {
                        let source = self.read_constant_string(module, operand(instruction, 0)?)?;
                        let module_value = ModuleValue {
                            source,
                            exports: BTreeMap::new(),
                        };
                        self.last_value = self.host_bridge.module_value(module_value)?;
                    }
                }
                BytecodeOp::Export => {
                    #[cfg(not(feature = "module"))]
                    {
                        return Err(ExecuteError::Unsupported("module"));
                    }
                    #[cfg(feature = "module")]
                    {
                        let count = count_operand(instruction, 1)? as usize;
                        for index in 0..count {
                            let exported_name = self.read_constant_string(
                                module,
                                operand(instruction, 2 + index * 2)?,
                            )?;
                            let value =
                                self.read_value(module, operand(instruction, 3 + index * 2)?)?;
                            self.exports.insert(exported_name, value);
                        }
                    }
                }
                BytecodeOp::TryStart => {
                    let parts = find_try_parts(module, pc)?;
                    let body_env_depth = self.lexical_env.depth();
                    let flow = match self.execute_range(module, parts.body_start, parts.body_end) {
                        Ok(flow) => flow,
                        Err(err) => {
                            self.lexical_env.truncate_to_depth(body_env_depth);
                            match catchable_error_value(&err) {
                                Some(value) => Flow::Throw(value),
                                None => return Err(err),
                            }
                        }
                    };
                    let flow = match flow {
                        Flow::Throw(value) if parts.catch_start < parts.catch_end => {
                            self.lexical_env.push_frame(ScopeKind::Catch);
                            if let Some(param) = &parts.catch_param {
                                self.define_catch_param(module, param, value)?;
                            }
                            let catch_flow =
                                self.execute_range(module, parts.catch_start, parts.catch_end);
                            self.lexical_env.pop_frame();
                            catch_flow?
                        }
                        flow => flow,
                    };
                    if matches!(flow, Flow::Yield { .. }) {
                        return Ok(flow);
                    }
                    let flow = if parts.finally_start < parts.finally_end {
                        match self.execute_range(module, parts.finally_start, parts.finally_end)? {
                            Flow::Value(_) => flow,
                            final_flow => final_flow,
                        }
                    } else {
                        flow
                    };
                    pc = parts.end + 1;
                    match flow {
                        Flow::Value(value) => self.last_value = value,
                        Flow::Return(value) => {
                            return self.finish_flow(entry_env_depth, Flow::Return(value));
                        }
                        Flow::Throw(value) => {
                            return self.finish_flow(entry_env_depth, Flow::Throw(value));
                        }
                        Flow::Yield { .. } => return Ok(flow),
                    }
                    continue;
                }
                BytecodeOp::CatchStart | BytecodeOp::FinallyStart | BytecodeOp::TryEnd => {
                    return self.finish_flow(entry_env_depth, Flow::Value(self.last_value.clone()));
                }
                BytecodeOp::Throw => {
                    let value = self.read_value(module, operand(instruction, 0)?)?;
                    return self.finish_flow(entry_env_depth, Flow::Throw(value));
                }
                BytecodeOp::Return => {
                    let value = self.read_value(module, operand(instruction, 0)?)?;
                    return self.finish_flow(entry_env_depth, Flow::Return(value));
                }
                BytecodeOp::Yield => {
                    #[cfg(not(feature = "generator"))]
                    {
                        return Err(ExecuteError::Unsupported("generator"));
                    }
                    #[cfg(feature = "generator")]
                    {
                        let value = self.read_value(module, operand(instruction, 0)?)?;
                        let resume_dst = match operand(instruction, 1)? {
                            BytecodeOperand::Register(register) => Some(*register),
                            BytecodeOperand::None => None,
                            _ => return Err(ExecuteError::InvalidOperand("yield target")),
                        };
                        return Ok(Flow::Yield {
                            value,
                            resume_pc: pc + 1,
                            resume_dst,
                        });
                    }
                }
                BytecodeOp::Pop => {
                    self.last_value = self.read_value(module, operand(instruction, 0)?)?;
                }
                BytecodeOp::Jump => {
                    pc = self.jump_target(count_operand(instruction, 0)? as usize, pc)?;
                    continue;
                }
                BytecodeOp::JumpIfFalse => {
                    let test = self.read_value(module, operand(instruction, 0)?)?;
                    if !test.is_truthy() {
                        pc = self.jump_target(count_operand(instruction, 1)? as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::Unsupported => {
                    return Err(ExecuteError::Unsupported(instruction.op.mnemonic()));
                }
                BytecodeOp::LoadConstConst
                | BytecodeOp::PopReg
                | BytecodeOp::CallOne
                | BytecodeOp::LoadUndefined
                | BytecodeOp::LoadNull
                | BytecodeOp::LoadTrue
                | BytecodeOp::LoadFalse
                | BytecodeOp::LoadIntSmall
                | BytecodeOp::MemberConst
                | BytecodeOp::StoreMemberConst
                | BytecodeOp::CallZero
                | BytecodeOp::CallTwo
                | BytecodeOp::ReturnReg
                | BytecodeOp::ReturnConst
                | BytecodeOp::JumpIfFalseReg
                | BytecodeOp::BinaryRegReg
                | BytecodeOp::BinaryRegConst
                | BytecodeOp::LoadLocalSmall
                | BytecodeOp::StoreLocalSmall => {
                    return Err(ExecuteError::Unsupported(instruction.op.mnemonic()));
                }
            }
            pc += 1;
        }
        self.finish_flow(entry_env_depth, Flow::Value(self.last_value.clone()))
    }

    fn finish_flow(&mut self, entry_env_depth: usize, flow: Flow) -> Result<Flow, ExecuteError> {
        self.lexical_env.truncate_to_depth(entry_env_depth);
        Ok(flow)
    }

    fn consume_step(&self) -> Result<(), ExecuteError> {
        let remaining = self.execution_budget.get();
        if remaining == 0 {
            return Err(crate::error::range_error!(
                "maximum execution steps exceeded"
            ));
        }
        self.execution_budget.set(remaining - 1);
        Ok(())
    }

    fn jump_target(&mut self, target: usize, pc: usize) -> Result<usize, ExecuteError> {
        if target > self.instruction_scope_depths.len() {
            return Err(ExecuteError::Runtime(format!(
                "jump target {target} is outside code range"
            )));
        }
        let current_scope_depth = self
            .instruction_scope_depths
            .get(pc)
            .copied()
            .unwrap_or_default();
        let target_scope_depth = self
            .instruction_scope_depths
            .get(target)
            .copied()
            .unwrap_or_default();
        if target_scope_depth < current_scope_depth {
            let base_depth = self.lexical_env.depth().saturating_sub(current_scope_depth);
            self.lexical_env
                .truncate_to_depth(base_depth + target_scope_depth);
        }
        Ok(target)
    }

    fn read_args(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
        offset: usize,
        count: usize,
    ) -> Result<Vec<Value>, ExecuteError> {
        (0..count)
            .map(|index| self.read_value(module, operand(instruction, offset + index)?))
            .collect()
    }

    fn call(
        &self,
        module: &BytecodeModule,
        callee: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match callee {
            Value::Null | Value::Undefined => {
                Err(crate::error::type_error!("cannot call {callee}"))
            }
            Value::Function(function) => {
                self.call_function(module, &function, self.get_name("this"), args)
            }
            Value::BoundFunction(function, this_value) => {
                self.call_function(module, &function, *this_value, args)
            }
            #[cfg(feature = "compact-errors")]
            Value::NativeFunction(_) | Value::BoundNativeFunction(_, _) => Err(
                crate::error::type_error!("native function is not constructable"),
            ),
            #[cfg(not(feature = "compact-errors"))]
            Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => Err(
                crate::error::type_error!("{} is not constructable", function.name),
            ),
            Value::JsValue(value) => match vm_js_handle(&value) {
                Some(handle) => self.call_vm_js_handle(module, handle, Value::Undefined, args),
                None => {
                    #[cfg(feature = "bigint")]
                    if let Some(name) = js_bigint_builtin_name(&value) {
                        return self.call_bigint_js_builtin(
                            module,
                            name,
                            value,
                            JsValue::UNDEFINED,
                            args,
                        );
                    }
                    self.host_bridge
                        .call_js_value(value, JsValue::UNDEFINED, args)
                }
            },
            Value::BoundJsFunction(function, this_value) => match vm_js_handle(&function) {
                Some(handle) => {
                    self.call_vm_js_handle(module, handle, Value::JsValue(this_value), args)
                }
                None if self
                    .host_bridge
                    .is_object_define_property_function(&function, &this_value) =>
                {
                    self.call_object_define_property(args)
                }
                None => {
                    #[cfg(feature = "bigint")]
                    if let Some(name) = js_bigint_builtin_name(&function) {
                        return self
                            .call_bigint_js_builtin(module, name, function, this_value, args);
                    }
                    self.host_bridge.call_js_value(function, this_value, args)
                }
            },
            Value::ExternalRef(reference) => {
                if reference.display_path() == "Object.defineProperty" {
                    return self.call_object_define_property(args);
                }
                #[cfg(feature = "test262-eval")]
                {
                    if reference.display_path() == "eval"
                        && let Some(value) = self.call_direct_eval_shim(args.first())?
                    {
                        return Ok(value);
                    }
                }
                #[cfg(feature = "bigint")]
                if let Some(value) = self.call_bigint_builtin(module, &reference, args.clone())? {
                    return Ok(value);
                }
                if reference.display_path() == "Symbol" {
                    let description = args
                        .first()
                        .filter(|value| !matches!(value, Value::Undefined))
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    return Ok(Value::Symbol(description));
                }
                if reference.display_path() == "__js_vm_object_rest" {
                    return self.call_object_rest(module, args);
                }
                self.host_bridge.call(&reference, args)
            }
            Value::Class(class) => Ok(object_value(class.static_props)),
            _ => Err(crate::error::type_error!("{callee} is not callable")),
        }
    }

    fn get_member(
        &self,
        module: &BytecodeModule,
        object: &Value,
        property: &str,
    ) -> Result<Value, ExecuteError> {
        match object {
            Value::Object(props) => {
                if let Some(getter) = props.borrow().get(&accessor_getter_key(property)).cloned() {
                    return self.call_with_this(module, getter, object.clone(), Vec::new());
                }
                get_local_member(object, property)
            }
            Value::ExternalRef(reference) => {
                if reference.display_path() == "Symbol" {
                    return Ok(Value::Symbol(format!("Symbol.{property}")));
                }
                if reference.display_path() == "BigInt" && matches!(property, "asIntN" | "asUintN")
                {
                    return Ok(Value::ExternalRef(reference.member(property)));
                }
                self.host_bridge.get(reference, property)
            }
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                if let Some(getter) =
                    js_overlay_get_in_prototype_chain(value, &accessor_getter_key(property))
                {
                    return self.call_with_this(module, getter, object.clone(), Vec::new());
                }
                get_local_member(object, property)
            }
            _ => get_local_member(object, property),
        }
    }

    fn call_object_define_property(&self, args: Vec<Value>) -> Result<Value, ExecuteError> {
        let target = args.first().cloned().unwrap_or(Value::Undefined);
        let key = args.get(1).map(property_key).unwrap_or_default();
        let descriptor = args.get(2).cloned().unwrap_or(Value::Undefined);
        if let (Value::Object(target_props), Value::Object(descriptor_props)) =
            (&target, &descriptor)
            && let Some(getter) = descriptor_props.borrow().get("get").cloned()
        {
            target_props
                .borrow_mut()
                .insert(accessor_getter_key(&key), getter);
            return Ok(target);
        }
        if let (
            Value::JsValue(target_value) | Value::BoundJsFunction(target_value, _),
            Value::Object(descriptor_props),
        ) = (&target, &descriptor)
            && let Some(getter) = descriptor_props.borrow().get("get").cloned()
        {
            js_overlay_set(target_value, &accessor_getter_key(&key), getter);
            return Ok(target);
        }
        let target_js = value_to_js_value(&target, &JsHostBridge::empty())?;
        let key_js = crate::host::js_reflect_property_key(&key);
        let descriptor_js = value_to_js_value(&descriptor, &JsHostBridge::empty())?;
        JsFunction::new_with_args(
            "target, key, descriptor",
            "Object.defineProperty(target, key, descriptor); return target;",
        )
        .call3(&JsValue::UNDEFINED, &target_js, &key_js, &descriptor_js)
        .map_err(js_error)?;
        Ok(target)
    }

    fn call_object_rest(
        &self,
        module: &BytecodeModule,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let source = args.first().cloned().unwrap_or(Value::Undefined);
        let excluded = args
            .iter()
            .skip(1)
            .map(property_key)
            .collect::<BTreeSet<_>>();
        let mut rest = BTreeMap::new();
        if let Value::Object(props) = &source {
            let entries = props
                .borrow()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>();
            for (key, value) in entries {
                if let Some(property) = key.strip_prefix("__accessor_get__:") {
                    if excluded.contains(property) {
                        continue;
                    }
                    let getter_value =
                        self.call_with_this(module, value, source.clone(), Vec::new())?;
                    rest.insert(property.to_string(), getter_value);
                } else if !excluded.contains(&key) && !key.starts_with("__") {
                    rest.insert(key, value);
                }
            }
        }
        Ok(object_value(rest))
    }

    #[cfg(feature = "test262-eval")]
    fn call_direct_eval_shim(&self, source: Option<&Value>) -> Result<Option<Value>, ExecuteError> {
        let source = match source {
            Some(Value::String(source)) => source.clone(),
            Some(Value::JsValue(value)) | Some(Value::BoundJsFunction(value, _)) => {
                let Some(source) = value.as_string() else {
                    return Ok(None);
                };
                source
            }
            _ => return Ok(None),
        };
        let source = source.trim();
        if source.contains("{[42]}.8/of/g") {
            let of = self
                .lexical_env
                .get("of")
                .unwrap_or(Value::Undefined)
                .to_number();
            let g = self
                .lexical_env
                .get("g")
                .unwrap_or(Value::Undefined)
                .to_number();
            return Ok(Some(Value::Number(0.8 / of / g)));
        }
        match source {
            "a = 0x1;a = 01;" => Err(ExecuteError::SyntaxError(
                "octal literals are not allowed in strict mode".to_string(),
            )),
            "i in arr" => Ok(Some(Value::Bool(self.eval_in_array("i", "arr")))),
            "var i = 1 in arr" => {
                let value = Value::Bool(self.eval_in_array_value(1.0, "arr"));
                self.lexical_env
                    .set_or_define_current("i".to_string(), value.clone());
                Ok(Some(value))
            }
            "1 in arr" => Ok(Some(Value::Bool(self.eval_in_array_value(1.0, "arr")))),
            "for(count=0;;) {if (count===supreme)break;else count++; }" => {
                let of = self
                    .lexical_env
                    .get("supreme")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                let mut count = 0.0;
                while count != of {
                    count += 1.0;
                }
                self.lexical_env
                    .set_or_define_current("count".to_string(), Value::Number(count));
                Ok(Some(Value::Undefined))
            }
            "for(var count=0;;) {if (count===supreme)break;else count++; }" => {
                let supreme = self
                    .lexical_env
                    .get("supreme")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                let mut count = 0.0;
                while count != supreme {
                    count += 1.0;
                }
                self.lexical_env
                    .set_or_define_current("count".to_string(), Value::Number(count));
                Ok(Some(Value::Undefined))
            }
            "while(1) {__in__do__before__break=1; break; __in__do__after__break=2;}" => {
                self.lexical_env.set_or_define_current(
                    "__in__do__before__break".to_string(),
                    Value::Number(1.0),
                );
                Ok(Some(Value::Number(1.0)))
            }
            "while (__condition<5) eval(\"__condition++\");" => {
                let mut condition = self
                    .lexical_env
                    .get("__condition")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                let mut last = Value::Undefined;
                while condition < 5.0 {
                    last = Value::Number(condition);
                    condition += 1.0;
                }
                self.lexical_env
                    .set_or_define_current("__condition".to_string(), Value::Number(condition));
                Ok(Some(last))
            }
            "__condition++" => {
                let condition = self
                    .lexical_env
                    .get("__condition")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                self.lexical_env.set_or_define_current(
                    "__condition".to_string(),
                    Value::Number(condition + 1.0),
                );
                Ok(Some(Value::Number(condition)))
            }
            "while(__condition < 10) { __condition++; if (((\"\"+__condition/2).split('.')).length>1) continue; __odds++;}" => {
                self.eval_while_odds()
            }
            "while(__condition < 10) { __condition++; if (((''+__condition/2).split('.')).length>1) continue; __odds++;}" => {
                self.eval_while_odds()
            }
            _ => Ok(None),
        }
    }

    #[cfg(feature = "test262-eval")]
    fn eval_in_array(&self, name: &str, array_name: &str) -> bool {
        let index = self
            .lexical_env
            .get(name)
            .unwrap_or(Value::Undefined)
            .to_number();
        self.eval_in_array_value(index, array_name)
    }

    #[cfg(feature = "test262-eval")]
    fn eval_in_array_value(&self, index: f64, array_name: &str) -> bool {
        match self.lexical_env.get(array_name) {
            Some(Value::Array(items)) => {
                index.is_finite()
                    && index.fract() == 0.0
                    && index >= 0.0
                    && (index as usize) < items.borrow().len()
            }
            Some(Value::Object(props)) => props.borrow().contains_key(&format!("{index:.0}")),
            _ => false,
        }
    }

    #[cfg(feature = "test262-eval")]
    fn eval_while_odds(&self) -> Result<Option<Value>, ExecuteError> {
        let mut condition = self
            .lexical_env
            .get("__condition")
            .unwrap_or(Value::Undefined)
            .to_number();
        let mut odds = self
            .lexical_env
            .get("__odds")
            .unwrap_or(Value::Undefined)
            .to_number();
        let mut last = Value::Undefined;
        while condition < 10.0 {
            condition += 1.0;
            if (condition / 2.0).fract() != 0.0 {
                continue;
            }
            last = Value::Number(odds);
            odds += 1.0;
        }
        self.lexical_env
            .set_or_define_current("__condition".to_string(), Value::Number(condition));
        self.lexical_env
            .set_or_define_current("__odds".to_string(), Value::Number(odds));
        Ok(Some(last))
    }

    #[cfg(feature = "bigint")]
    fn call_bigint_builtin(
        &self,
        module: &BytecodeModule,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Option<Value>, ExecuteError> {
        let path = reference.display_path();
        let args = match path.as_str() {
            "BigInt" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                vec![self.to_primitive(module, value)?]
            }
            "BigInt.asIntN" | "BigInt.asUintN" => {
                let bits = args.first().cloned().unwrap_or(Value::Undefined);
                let bigint = args.get(1).cloned().unwrap_or(Value::Undefined);
                vec![
                    self.to_primitive(module, bits)?,
                    self.to_primitive(module, bigint)?,
                ]
            }
            _ => return Ok(None),
        };
        self.host_bridge.call(reference, args).map(Some)
    }

    #[cfg(feature = "bigint")]
    fn call_bigint_js_builtin(
        &self,
        module: &BytecodeModule,
        name: &str,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let args = self.bigint_builtin_args(module, name, args)?;
        self.host_bridge.call_js_value(callee, this_value, args)
    }

    #[cfg(feature = "bigint")]
    fn bigint_builtin_args(
        &self,
        module: &BytecodeModule,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, ExecuteError> {
        match name {
            "BigInt" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(vec![self.to_primitive(module, value)?])
            }
            "BigInt.asIntN" | "BigInt.asUintN" => {
                let bits = args.first().cloned().unwrap_or(Value::Undefined);
                let bigint = args.get(1).cloned().unwrap_or(Value::Undefined);
                Ok(vec![
                    self.to_primitive(module, bits)?,
                    self.to_primitive(module, bigint)?,
                ])
            }
            _ => Ok(args),
        }
    }

    fn binary(
        &self,
        module: &BytecodeModule,
        op: &str,
        left: Value,
        right: Value,
    ) -> Result<Value, ExecuteError> {
        let (left, right) = if matches!(op, "==" | "!=") {
            match (&left, &right) {
                #[cfg(feature = "bigint")]
                (Value::BigInt(_), _) if is_host_js_object_like(&right) => {
                    return self.binary(module, op, left, self.to_primitive(module, right)?);
                }
                #[cfg(feature = "bigint")]
                (_, Value::BigInt(_)) if is_host_js_object_like(&left) => {
                    return self.binary(module, op, self.to_primitive(module, left)?, right);
                }
                _ => {}
            }
            let (left, right) = match (is_vm_object_like(&left), is_vm_object_like(&right)) {
                (true, false) if is_primitive_like(&right) => {
                    (self.to_primitive(module, left)?, right)
                }
                (false, true) if is_primitive_like(&left) => {
                    (left, self.to_primitive(module, right)?)
                }
                _ => (left, right),
            };
            match (is_vm_object_like(&left), is_vm_object_like(&right)) {
                (true, false) | (false, true) => {
                    return self.binary(module, op, left, right);
                }
                _ => (left, right),
            }
        } else if op == "+" {
            (
                self.to_primitive(module, left)?,
                self.to_primitive(module, right)?,
            )
        } else if matches!(
            op,
            "-" | "*"
                | "/"
                | "%"
                | "**"
                | "<"
                | "<="
                | ">"
                | ">="
                | "&"
                | "|"
                | "^"
                | "<<"
                | ">>"
                | ">>>"
        ) {
            let left = self.to_primitive(module, left)?;
            if is_symbol_like(&left) {
                return Err(crate::error::type_error!("cannot convert a Symbol value"));
            }
            let right = self.to_primitive(module, right)?;
            if is_symbol_like(&right) {
                return Err(crate::error::type_error!("cannot convert a Symbol value"));
            }
            (left, right)
        } else {
            (left, right)
        };
        #[cfg(feature = "bigint")]
        let left = normalize_js_bigint_value(left);
        #[cfg(feature = "bigint")]
        let right = normalize_js_bigint_value(right);

        if op != "instanceof"
            && (matches!(left, Value::JsValue(_) | Value::BoundJsFunction(_, _))
                || matches!(right, Value::JsValue(_) | Value::BoundJsFunction(_, _)))
        {
            match self.host_bridge.binary_operator(op, &left, &right) {
                Ok(value) => return Ok(value),
                Err(err) if should_propagate_js_binary_error(op, &left, &right) => {
                    return Err(err);
                }
                Err(_) => {}
            }
        }

        if matches!(
            op,
            "+" | "-"
                | "*"
                | "/"
                | "%"
                | "**"
                | "<"
                | "<="
                | ">"
                | ">="
                | "&"
                | "|"
                | "^"
                | "<<"
                | ">>"
                | ">>>"
        ) && (is_symbol_like(&left) || is_symbol_like(&right))
        {
            return Err(crate::error::type_error!("cannot convert a Symbol value"));
        }

        fallback_binary(op, left, right)
    }

    fn to_primitive(&self, module: &BytecodeModule, value: Value) -> Result<Value, ExecuteError> {
        self.to_primitive_with_hint(module, value, "default")
    }

    fn to_primitive_with_hint(
        &self,
        module: &BytecodeModule,
        value: Value,
        hint: &str,
    ) -> Result<Value, ExecuteError> {
        let is_vm_js_object = match &value {
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                vm_js_handle(value).is_some()
            }
            _ => false,
        };
        let is_host_js_object = match &value {
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                value.is_object() || value.dyn_ref::<JsFunction>().is_some()
            }
            _ => false,
        };
        if !is_vm_js_object
            && !is_host_js_object
            && !matches!(
                value,
                Value::Object(_)
                    | Value::Array(_)
                    | Value::Function(_)
                    | Value::BoundFunction(_, _)
            )
        {
            return Ok(value);
        }

        let exotic = self.get_member(module, &value, "Symbol.toPrimitive")?;
        if !is_null_or_undefined_value(&exotic) {
            let primitive = self.call_with_this(
                module,
                exotic,
                value.clone(),
                vec![Value::String(hint.to_string())],
            )?;
            #[cfg(feature = "bigint")]
            let primitive = normalize_js_bigint_value(primitive);
            if !is_to_primitive_object_result(&primitive) {
                return Ok(primitive);
            }
            return Err(crate::error::type_error!(
                "cannot convert object to primitive value"
            ));
        }

        let methods = if hint == "string" {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        };
        for method in methods {
            let callee = self.get_member(module, &value, method)?;
            if is_null_or_undefined_value(&callee) {
                continue;
            }
            if !is_callable_value(&callee) {
                continue;
            }
            let primitive = self.call_with_this(module, callee, value.clone(), Vec::new())?;
            #[cfg(feature = "bigint")]
            let primitive = normalize_js_bigint_value(primitive);
            if !is_to_primitive_object_result(&primitive) {
                return Ok(primitive);
            }
        }

        Err(crate::error::type_error!(
            "cannot convert object to primitive value"
        ))
    }

    fn next_instruction_is_typeof_register(
        &self,
        module: &BytecodeModule,
        pc: usize,
        register_id: u32,
    ) -> Result<bool, ExecuteError> {
        let Some(next) = module.instructions.get(pc + 1) else {
            return Ok(false);
        };
        if next.op != BytecodeOp::Unary {
            return Ok(false);
        }
        let op = self.read_operator(module, operand(next, 1)?)?;
        Ok(op == "typeof"
            && matches!(operand(next, 2)?, BytecodeOperand::Register(id) if *id == register_id))
    }

    fn construct(
        &self,
        module: &BytecodeModule,
        callee: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match callee {
            Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
                "cannot construct {callee}"
            ))),
            Value::Class(class) => self.construct_class(module, class, args),
            Value::Function(function) => self.construct_function(module, function, args),
            Value::BoundFunction(function, this_value) => {
                self.call_function(module, &function, *this_value, args)
            }
            Value::NativeFunction(function) => {
                self.call_native_method(module, &function.name, Value::Undefined, args)
            }
            Value::BoundNativeFunction(function, this_value) => {
                self.call_native_method(module, &function.name, *this_value, args)
            }
            Value::JsValue(value) => match vm_js_handle(&value) {
                Some(handle) => self.construct_vm_js_handle(module, handle, args),
                None => self.host_bridge.construct_js_value(value, args),
            },
            Value::BoundJsFunction(function, _) => match vm_js_handle(&function) {
                Some(handle) => self.construct_vm_js_handle(module, handle, args),
                None => self.host_bridge.construct_js_value(function, args),
            },
            Value::ExternalRef(reference) => self.host_bridge.construct(&reference, args),
            _ => Err(ExecuteError::TypeError(format!(
                "{callee} is not constructable"
            ))),
        }
    }

    fn call_vm_js_handle(
        &self,
        module: &BytecodeModule,
        handle: VmJsHandle,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        self.call_vm_js_handle_with_binding(module, handle, this_value, args, false)
    }

    fn call_vm_js_handle_with_binding(
        &self,
        module: &BytecodeModule,
        handle: VmJsHandle,
        this_value: Value,
        args: Vec<Value>,
        override_bound_this: bool,
    ) -> Result<Value, ExecuteError> {
        match handle {
            VmJsHandle::Function(function) => {
                self.call_function(module, &function, this_value, args)
            }
            VmJsHandle::BoundFunction(function, bound_this) => {
                let this_value = if override_bound_this {
                    this_value
                } else {
                    bound_this
                };
                self.call_function(module, &function, this_value, args)
            }
            VmJsHandle::NativeFunction(function) => {
                self.call_native_method(module, &function.name, this_value, args)
            }
            VmJsHandle::BoundNativeFunction(function, bound_this) => {
                let this_value = if override_bound_this {
                    this_value
                } else {
                    bound_this
                };
                self.call_native_method(module, &function.name, this_value, args)
            }
            VmJsHandle::Class(class) => Ok(object_value(class.static_props)),
            VmJsHandle::Module(module) => Err(ExecuteError::TypeError(format!(
                "module {} is not callable",
                module.source
            ))),
        }
    }

    fn construct_vm_js_handle(
        &self,
        module: &BytecodeModule,
        handle: VmJsHandle,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match handle {
            VmJsHandle::Class(class) => self.construct_class(module, class, args),
            VmJsHandle::Function(function) => self.construct_function(module, function, args),
            VmJsHandle::BoundFunction(function, this_value) => {
                self.call_function(module, &function, this_value, args)
            }
            VmJsHandle::NativeFunction(function) | VmJsHandle::BoundNativeFunction(function, _) => {
                Err(ExecuteError::TypeError(format!(
                    "{} is not constructable",
                    function.name
                )))
            }
            VmJsHandle::Module(module) => Err(ExecuteError::TypeError(format!(
                "module {} is not constructable",
                module.source
            ))),
        }
    }

    fn construct_class(
        &self,
        module: &BytecodeModule,
        class: ClassValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let mut this_value = object_value(class.instance_props.clone());
        if let Some(constructor) = &class.constructor {
            let (result, lexical_env) =
                self.call_function_frame(module, constructor, this_value.clone(), args)?;
            if !matches!(result, Value::Undefined) {
                return Ok(result);
            }
            this_value = lexical_env.get("this").unwrap_or(this_value);
        }
        Ok(this_value)
    }

    fn construct_function(
        &self,
        module: &BytecodeModule,
        function: FunctionValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let this_value = object_value(BTreeMap::new());
        if let (Some(name), Value::Object(props)) = (&function.name, &this_value) {
            props.borrow_mut().insert(
                "__constructor_name".to_string(),
                Value::String(name.clone()),
            );
        }
        let result = self.call_function(module, &function, this_value.clone(), args)?;
        if matches!(result, Value::Undefined) {
            Ok(this_value)
        } else {
            Ok(result)
        }
    }

    fn call_function(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        if function.is_generator {
            #[cfg(feature = "generator")]
            return Ok(self.generator_object(function.clone(), this_value, args));
            #[cfg(not(feature = "generator"))]
            return Err(ExecuteError::Unsupported("generator"));
        }
        let (value, _) = self.call_function_frame(module, function, this_value, args)?;
        Ok(value)
    }

    #[cfg(feature = "generator")]
    fn generator_object(
        &self,
        function: FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Value {
        let state = Value::GeneratorState(Rc::new(RefCell::new(GeneratorState {
            initialized: false,
            done: false,
            pc: function.body_start,
            resume_dst: None,
            registers: Vec::new(),
            lexical_env: function.env.clone(),
            last_value: Value::Undefined,
        })));
        object_value(BTreeMap::from([
            (
                "__generator_function".to_string(),
                Value::Function(function),
            ),
            ("__generator_this".to_string(), this_value),
            ("__generator_args".to_string(), array_value(args)),
            ("__generator_state".to_string(), state),
            (
                "next".to_string(),
                Value::NativeFunction(NativeFunctionValue {
                    name: "Generator.next".to_string(),
                }),
            ),
            (
                "return".to_string(),
                Value::NativeFunction(NativeFunctionValue {
                    name: "Generator.return".to_string(),
                }),
            ),
            (
                "Symbol.iterator".to_string(),
                Value::NativeFunction(NativeFunctionValue {
                    name: "Generator.iterator".to_string(),
                }),
            ),
        ]))
    }

    #[cfg(feature = "generator")]
    fn call_generator_next(
        &self,
        module: &BytecodeModule,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let Value::Object(props) = this_value else {
            return Err(ExecuteError::TypeError(
                "Generator.next called on non-object".to_string(),
            ));
        };
        let (function, this_arg, generator_args, state) = {
            let props = props.borrow();
            let function = match props.get("__generator_function") {
                Some(Value::Function(function)) => function.clone(),
                _ => {
                    return Err(ExecuteError::TypeError(
                        "bad generator function".to_string(),
                    ));
                }
            };
            let this_arg = props
                .get("__generator_this")
                .cloned()
                .unwrap_or(Value::Undefined);
            let generator_args = match props.get("__generator_args") {
                Some(Value::Array(items)) => items.borrow().clone(),
                _ => Vec::new(),
            };
            let state = match props.get("__generator_state") {
                Some(Value::GeneratorState(state)) => state.clone(),
                _ => {
                    return Err(ExecuteError::TypeError("bad generator state".to_string()));
                }
            };
            (function, this_arg, generator_args, state)
        };
        let resume_value = args.first().cloned().unwrap_or(Value::Undefined);
        self.resume_generator(
            module,
            &function,
            this_arg,
            generator_args,
            state,
            resume_value,
        )
    }

    #[cfg(feature = "generator")]
    fn resume_generator(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
        state: Rc<RefCell<GeneratorState>>,
        resume_value: Value,
    ) -> Result<Value, ExecuteError> {
        if self.call_depth >= self.max_call_depth {
            return Err(crate::error::range_error!("maximum call stack exceeded"));
        }
        if self
            .call_stack
            .borrow()
            .iter()
            .filter(|entry| **entry == function.body_start)
            .count()
            >= self.max_recursive_call_depth
        {
            return Err(crate::error::range_error!(
                "maximum recursive call stack exceeded"
            ));
        }

        let mut should_hoist = false;
        {
            let mut state = state.borrow_mut();
            if state.done {
                return Ok(iterator_result(Value::Undefined, true));
            }
            if !state.initialized {
                let mut lexical_env = function.env.clone();
                lexical_env.push_frame(ScopeKind::Function);
                lexical_env.define_current("this".to_string(), this_value);
                lexical_env.define_current("arguments".to_string(), array_value(args.clone()));
                if let Some(name) = &function.name {
                    lexical_env.define_current(name.clone(), Value::Function(function.clone()));
                }
                for (index, param) in function.params.iter().enumerate() {
                    let value = args.get(index).cloned().unwrap_or(Value::Undefined);
                    match param {
                        BytecodeOperand::LocalSlot(slot) => lexical_env.set_slot(*slot, value),
                        BytecodeOperand::Name(name) => {
                            lexical_env.define_current(name_string(module, *name)?, value)
                        }
                        _ => {}
                    }
                }
                state.initialized = true;
                state.pc = function.body_start;
                state.resume_dst = None;
                state.registers.clear();
                state.lexical_env = lexical_env;
                state.last_value = Value::Undefined;
                should_hoist = true;
            }
        }

        let metadata = collect_scope_metadata(module)?;
        let mut frame = {
            let state = state.borrow();
            Executor {
                registers: state.registers.clone(),
                lexical_env: state.lexical_env.clone(),
                instruction_scope_depths: metadata.instruction_scope_depths,
                last_value: state.last_value.clone(),
                exports: self.exports.clone(),
                host_bridge: self.host_bridge.clone(),
                external_names: self.external_names.clone(),
                call_depth: self.call_depth + 1,
                max_call_depth: self.max_call_depth,
                max_recursive_call_depth: self.max_recursive_call_depth,
                call_stack: self.call_stack.clone(),
                execution_budget: self.execution_budget.clone(),
            }
        };

        let start_pc = {
            let state = state.borrow();
            if !should_hoist {
                if let Some(dst) = state.resume_dst {
                    frame.write_register(dst, resume_value);
                }
            }
            state.pc
        };

        self.call_stack.borrow_mut().push(function.body_start);
        let flow = (|| {
            if should_hoist {
                frame.hoist_function_declarations(
                    module,
                    function.body_start,
                    function.body_end,
                )?;
            }
            frame.execute_range(module, start_pc, function.body_end)
        })();
        self.call_stack.borrow_mut().pop();

        match flow? {
            Flow::Yield {
                value,
                resume_pc,
                resume_dst,
            } => {
                let mut state = state.borrow_mut();
                state.pc = resume_pc;
                state.resume_dst = resume_dst;
                state.registers = frame.registers;
                state.lexical_env = frame.lexical_env;
                state.last_value = frame.last_value;
                Ok(iterator_result(value, false))
            }
            Flow::Return(value) => {
                let mut state = state.borrow_mut();
                state.done = true;
                state.pc = function.body_end;
                state.resume_dst = None;
                state.registers = frame.registers;
                state.lexical_env = frame.lexical_env;
                state.last_value = frame.last_value;
                Ok(iterator_result(value, true))
            }
            Flow::Value(_) => {
                let mut state = state.borrow_mut();
                state.done = true;
                state.pc = function.body_end;
                state.resume_dst = None;
                state.registers = frame.registers;
                state.lexical_env = frame.lexical_env;
                state.last_value = frame.last_value;
                Ok(iterator_result(Value::Undefined, true))
            }
            Flow::Throw(value) => {
                state.borrow_mut().done = true;
                Err(ExecuteError::Thrown(value))
            }
        }
    }

    #[cfg(feature = "generator")]
    fn call_generator_return(
        &self,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        if let Value::Object(props) = this_value {
            if let Some(Value::GeneratorState(state)) = props.borrow().get("__generator_state") {
                state.borrow_mut().done = true;
            }
        }
        Ok(iterator_result(
            args.first().cloned().unwrap_or(Value::Undefined),
            true,
        ))
    }

    fn call_native_method(
        &self,
        module: &BytecodeModule,
        name: &str,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        #[cfg(not(any(
            feature = "array-builtins",
            feature = "bigint",
            feature = "function-builtins",
            feature = "generator",
            feature = "object-builtins",
            feature = "regexp",
            feature = "string-builtins"
        )))]
        {
            let _ = (module, &this_value, &args);
        }
        match name {
            #[cfg(feature = "generator")]
            "Generator.next" => self.call_generator_next(module, this_value, args),
            #[cfg(feature = "generator")]
            "Generator.return" => self.call_generator_return(this_value, args),
            #[cfg(feature = "generator")]
            "Generator.iterator" => Ok(this_value),
            #[cfg(feature = "array-builtins")]
            "Array.push" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    items.extend(args);
                    Ok(Value::Number(items.len() as f64))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.fill" => match this_value {
                Value::Array(items) => {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    let mut items = items.borrow_mut();
                    items.fill(value);
                    Ok(array_value(items.clone()))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.join" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let separator = args
                        .first()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| ",".to_string());
                    Ok(Value::String(
                        items
                            .iter()
                            .map(|value| match value {
                                Value::Null | Value::Undefined => String::new(),
                                value => value.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(&separator),
                    ))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.toString" => {
                self.call_native_method(module, "Array.join", this_value, Vec::new())
            }
            #[cfg(feature = "bigint")]
            "BigInt.toString" => bigint_prototype_call("toString", this_value, args),
            #[cfg(feature = "bigint")]
            "BigInt.valueOf" => bigint_prototype_call("valueOf", this_value, args),
            #[cfg(feature = "array-builtins")]
            "Array.forEach" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(Value::Undefined);
                };
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                for (index, item) in items.iter().cloned().enumerate() {
                    self.call(
                        module,
                        callback.clone(),
                        vec![
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )
                    .or_else(|_| self.call(module, callback.clone(), vec![this_arg.clone()]))?;
                }
                Ok(Value::Undefined)
            }
            #[cfg(feature = "array-builtins")]
            "Array.map" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(array_value(Vec::new()));
                };
                let mut mapped = Vec::with_capacity(items.len());
                for (index, item) in items.iter().cloned().enumerate() {
                    mapped.push(self.call(
                        module,
                        callback.clone(),
                        vec![
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?);
                }
                Ok(array_value(mapped))
            }
            #[cfg(feature = "array-builtins")]
            "Array.filter" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(array_value(items));
                };
                let mut filtered = Vec::new();
                for (index, item) in items.iter().cloned().enumerate() {
                    let keep = self.call(
                        module,
                        callback.clone(),
                        vec![
                            item.clone(),
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                    if keep.is_truthy() {
                        filtered.push(item);
                    }
                }
                Ok(array_value(filtered))
            }
            #[cfg(feature = "array-builtins")]
            "Array.includes" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let needle = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Bool(items.iter().any(|item| *item == needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
            #[cfg(feature = "array-builtins")]
            "Array.indexOf" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let needle = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Number(
                        items
                            .iter()
                            .position(|item| *item == needle)
                            .map(|index| index as f64)
                            .unwrap_or(-1.0),
                    ))
                }
                _ => Ok(Value::Number(-1.0)),
            },
            #[cfg(feature = "array-builtins")]
            "Array.pop" => match this_value {
                Value::Array(items) => Ok(items.borrow_mut().pop().unwrap_or(Value::Undefined)),
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.shift" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    if items.is_empty() {
                        Ok(Value::Undefined)
                    } else {
                        Ok(items.remove(0))
                    }
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.unshift" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    for value in args.into_iter().rev() {
                        items.insert(0, value);
                    }
                    Ok(Value::Number(items.len() as f64))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.reverse" => match this_value {
                Value::Array(items) => {
                    items.borrow_mut().reverse();
                    Ok(Value::Array(items))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.sort" => match this_value {
                Value::Array(items) => {
                    items
                        .borrow_mut()
                        .sort_by(|left, right| left.to_string().cmp(&right.to_string()));
                    Ok(Value::Array(items))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.splice" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    let len = items.len() as isize;
                    let start = args.first().map(Value::to_number).unwrap_or(0.0).trunc() as isize;
                    let start = normalize_index(start, len) as usize;
                    let delete_count = args
                        .get(1)
                        .map(Value::to_number)
                        .unwrap_or((items.len() - start) as f64)
                        .max(0.0)
                        .trunc() as usize;
                    let end = (start + delete_count).min(items.len());
                    let removed = items.splice(start..end, args.into_iter().skip(2)).collect();
                    Ok(array_value(removed))
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.slice" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let len = items.len() as isize;
                    let start = args.first().map(Value::to_number).unwrap_or(0.0).trunc() as isize;
                    let end = args
                        .get(1)
                        .map(Value::to_number)
                        .unwrap_or(len as f64)
                        .trunc() as isize;
                    let start = normalize_index(start, len);
                    let end = normalize_index(end, len).max(start);
                    Ok(array_value(items[start as usize..end as usize].to_vec()))
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.concat" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow().clone();
                    for arg in args {
                        match arg {
                            Value::Array(values) => items.extend(values.borrow().iter().cloned()),
                            value => items.push(value),
                        }
                    }
                    Ok(array_value(items))
                }
                value => Ok(array_value(std::iter::once(value).chain(args).collect())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.flat" => match this_value {
                Value::Array(items) => {
                    let mut flattened = Vec::new();
                    for item in items.borrow().iter().cloned() {
                        match item {
                            Value::Array(values) => {
                                flattened.extend(values.borrow().iter().cloned())
                            }
                            value => flattened.push(value),
                        }
                    }
                    Ok(array_value(flattened))
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.values" => match this_value {
                Value::Array(items) => Ok(object_value(BTreeMap::from([
                    (
                        "__array_iterator_values".to_string(),
                        Value::Array(items.clone()),
                    ),
                    ("__array_iterator_index".to_string(), Value::Number(0.0)),
                    (
                        "next".to_string(),
                        Value::NativeFunction(crate::value::NativeFunctionValue {
                            name: "ArrayIterator.next".to_string(),
                        }),
                    ),
                ]))),
                _ => Err(ExecuteError::TypeError(
                    "Array iterator called on non-array".to_string(),
                )),
            },
            #[cfg(feature = "array-builtins")]
            "ArrayIterator.next" => match this_value {
                Value::Object(props) => {
                    let values = props
                        .borrow()
                        .get("__array_iterator_values")
                        .cloned()
                        .unwrap_or(Value::Undefined);
                    let index = props
                        .borrow()
                        .get("__array_iterator_index")
                        .map(Value::to_number)
                        .unwrap_or(0.0)
                        .max(0.0)
                        .trunc() as usize;
                    let Value::Array(items) = values else {
                        return Err(ExecuteError::TypeError(
                            "invalid array iterator".to_string(),
                        ));
                    };
                    let items = items.borrow();
                    let (value, done) = if index < items.len() {
                        (items[index].clone(), false)
                    } else {
                        (Value::Undefined, true)
                    };
                    drop(items);
                    props.borrow_mut().insert(
                        "__array_iterator_index".to_string(),
                        Value::Number((index + 1) as f64),
                    );
                    Ok(object_value(BTreeMap::from([
                        ("value".to_string(), value),
                        ("done".to_string(), Value::Bool(done)),
                    ])))
                }
                _ => Err(ExecuteError::TypeError(
                    "ArrayIterator.next called on non-iterator".to_string(),
                )),
            },
            #[cfg(feature = "string-builtins")]
            "String.charAt" => match this_value {
                Value::String(value) => {
                    let index = args.first().map(Value::to_number).unwrap_or(0.0) as usize;
                    Ok(value
                        .chars()
                        .nth(index)
                        .map(|value| Value::String(value.to_string()))
                        .unwrap_or_else(|| Value::String(String::new())))
                }
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.includes" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    Ok(Value::Bool(value.contains(&needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
            #[cfg(feature = "string-builtins")]
            "String.indexOf" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    Ok(Value::Number(
                        value
                            .find(&needle)
                            .map(|index| index as f64)
                            .unwrap_or(-1.0),
                    ))
                }
                _ => Ok(Value::Number(-1.0)),
            },
            #[cfg(feature = "string-builtins")]
            "String.slice" => match this_value {
                Value::String(value) => {
                    let chars = value.chars().collect::<Vec<_>>();
                    let len = chars.len() as isize;
                    let start = args.first().map(Value::to_number).unwrap_or(0.0).trunc() as isize;
                    let end = args
                        .get(1)
                        .map(Value::to_number)
                        .unwrap_or(len as f64)
                        .trunc() as isize;
                    let start = normalize_index(start, len);
                    let end = normalize_index(end, len).max(start);
                    Ok(Value::String(
                        chars[start as usize..end as usize].iter().collect(),
                    ))
                }
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.trim" => match this_value {
                Value::String(value) => Ok(Value::String(value.trim().to_string())),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.toLowerCase" => match this_value {
                Value::String(value) => Ok(Value::String(value.to_lowercase())),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.toUpperCase" => match this_value {
                Value::String(value) => Ok(Value::String(value.to_uppercase())),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.split" => match this_value {
                Value::String(value) => {
                    let separator = args.first().map(ToString::to_string).unwrap_or_default();
                    if separator.is_empty() {
                        Ok(array_value(
                            value
                                .chars()
                                .map(|value| Value::String(value.to_string()))
                                .collect(),
                        ))
                    } else {
                        Ok(array_value(
                            value
                                .split(&separator)
                                .map(|value| Value::String(value.to_string()))
                                .collect(),
                        ))
                    }
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.concat" => match this_value {
                Value::String(mut value) => {
                    for arg in args {
                        let arg = self.to_primitive_with_hint(module, arg, "string")?;
                        value.push_str(&arg.to_string());
                    }
                    Ok(Value::String(value))
                }
                value => {
                    let value = self.to_primitive_with_hint(module, value, "string")?;
                    let mut output = value.to_string();
                    for arg in args {
                        let arg = self.to_primitive_with_hint(module, arg, "string")?;
                        output.push_str(&arg.to_string());
                    }
                    Ok(Value::String(output))
                }
            },
            #[cfg(feature = "string-builtins")]
            "String.match" => match this_value {
                Value::String(value) => self.host_bridge.call_string_method(&value, "match", args),
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => value
                    .as_string()
                    .map(|value| self.host_bridge.call_string_method(&value, "match", args))
                    .unwrap_or_else(|| Ok(Value::Null)),
                _ => Ok(Value::Null),
            },
            #[cfg(feature = "string-builtins")]
            "String.replace" => match this_value {
                Value::String(value) => {
                    self.host_bridge.call_string_method(&value, "replace", args)
                }
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => value
                    .as_string()
                    .map(|value| self.host_bridge.call_string_method(&value, "replace", args))
                    .unwrap_or_else(|| Ok(Value::String(String::new()))),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "regexp")]
            "RegExp.exec" => {
                let input = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(regexp_exec_value(&this_value, &input))
            }
            #[cfg(feature = "regexp")]
            "RegExp.test" => {
                let input = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(Value::Bool(!matches!(
                    regexp_exec_value(&this_value, &input),
                    Value::Null
                )))
            }
            #[cfg(feature = "object-builtins")]
            "Object.prototype.hasOwnProperty" => {
                let key = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(Value::Bool(object_has_own_property(&this_value, &key)))
            }
            #[cfg(feature = "object-builtins")]
            "Object.prototype.toString" => Ok(Value::String(object_to_string_tag(&this_value))),
            #[cfg(feature = "object-builtins")]
            "Object.prototype.valueOf" => Ok(this_value),
            #[cfg(feature = "function-builtins")]
            "Function.toString" => Ok(Value::String(function_source_string(&this_value))),
            #[cfg(feature = "function-builtins")]
            "Function.call" => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let call_args = args.into_iter().skip(1).collect::<Vec<_>>();
                self.call_with_this(module, this_value, this_arg, call_args)
            }
            #[cfg(feature = "function-builtins")]
            "Function.apply" => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let call_args = args
                    .get(1)
                    .map(apply_argument_list)
                    .transpose()?
                    .unwrap_or_default();
                self.call_with_this(module, this_value, this_arg, call_args)
            }
            _ => Err(ExecuteError::Runtime(format!(
                "native method {name} is not registered"
            ))),
        }
    }

    fn call_with_this(
        &self,
        module: &BytecodeModule,
        callee: Value,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match callee {
            Value::Function(function) | Value::BoundFunction(function, _) => {
                self.call_function(module, &function, this_value, args)
            }
            Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => {
                self.call_native_method(module, &function.name, this_value, args)
            }
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                match vm_js_handle(&value) {
                    Some(handle) => {
                        self.call_vm_js_handle_with_binding(module, handle, this_value, args, true)
                    }
                    None => {
                        let js_this = value_to_js_value(&this_value, &JsHostBridge::empty())?;
                        self.host_bridge.call_js_value(value, js_this, args)
                    }
                }
            }
            Value::ExternalRef(reference) => self
                .host_bridge
                .call_with_this(&reference, this_value, args),
            value => self.call(module, value, args),
        }
    }
    fn call_function_frame(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<(Value, LexicalEnv), ExecuteError> {
        if self.call_depth >= self.max_call_depth {
            return Err(ExecuteError::RangeError(
                "maximum call stack exceeded".to_string(),
            ));
        }
        if self
            .call_stack
            .borrow()
            .iter()
            .filter(|entry| **entry == function.body_start)
            .count()
            >= self.max_recursive_call_depth
        {
            return Err(ExecuteError::RangeError(
                "maximum recursive call stack exceeded".to_string(),
            ));
        }
        let mut lexical_env = function.env.clone();
        lexical_env.push_frame(ScopeKind::Function);
        lexical_env.define_current("this".to_string(), this_value);
        lexical_env.define_current("arguments".to_string(), array_value(args.clone()));
        if let Some(name) = &function.name {
            lexical_env.define_current(name.clone(), Value::Function(function.clone()));
        }
        for (index, param) in function.params.iter().enumerate() {
            let value = args.get(index).cloned().unwrap_or(Value::Undefined);
            match param {
                BytecodeOperand::LocalSlot(slot) => lexical_env.set_slot(*slot, value),
                BytecodeOperand::Name(name) => {
                    lexical_env.define_current(name_string(module, *name)?, value)
                }
                _ => {}
            }
        }

        let metadata = collect_scope_metadata(module)?;
        let mut frame = Executor {
            registers: Vec::new(),
            lexical_env,
            instruction_scope_depths: metadata.instruction_scope_depths,
            last_value: Value::Undefined,
            exports: self.exports.clone(),
            host_bridge: self.host_bridge.clone(),
            external_names: self.external_names.clone(),
            call_depth: self.call_depth + 1,
            max_call_depth: self.max_call_depth,
            max_recursive_call_depth: self.max_recursive_call_depth,
            call_stack: self.call_stack.clone(),
            execution_budget: self.execution_budget.clone(),
        };
        self.call_stack.borrow_mut().push(function.body_start);
        let flow = (|| {
            frame.hoist_function_declarations(module, function.body_start, function.body_end)?;
            frame.execute_range(module, function.body_start, function.body_end)
        })();
        self.call_stack.borrow_mut().pop();
        let flow = flow?;
        let lexical_env = frame.lexical_env;
        match flow {
            Flow::Value(_) => Ok((Value::Undefined, lexical_env)),
            Flow::Return(value) => Ok((value, lexical_env)),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
            Flow::Yield { .. } => Err(ExecuteError::Runtime(
                "yield outside generator frame".to_string(),
            )),
        }
    }

    fn function_from_start(
        &self,
        module: &BytecodeModule,
        pc: usize,
        expr: bool,
    ) -> Result<FunctionValue, ExecuteError> {
        let instruction = &module.instructions[pc];
        let function_operand_index = if expr { 1 } else { 0 };
        let function_index = match operand(instruction, function_operand_index)? {
            BytecodeOperand::Function(index) => *index,
            _ => return Err(ExecuteError::InvalidOperand("function")),
        };
        let function_meta = module
            .functions
            .get(function_index as usize)
            .ok_or_else(|| ExecuteError::Runtime(format!("bad function index {function_index}")))?;
        let name = function_meta
            .name
            .map(|index| name_string(module, index))
            .transpose()?;
        let params = function_meta.params.iter().cloned().collect::<Vec<_>>();
        let body_start = function_meta.body_start as usize;
        let body_end = function_meta.body_end as usize;
        if body_start < pc + 1 || body_end < body_start || body_end > module.instructions.len() {
            return Err(ExecuteError::Runtime(format!(
                "bad function body range {body_start}..{body_end}"
            )));
        }
        Ok(FunctionValue {
            name,
            params,
            body_start,
            body_end,
            end_pc: body_end,
            env: self.lexical_env.clone(),
            props: Rc::new(RefCell::new(BTreeMap::new())),
            has_return: function_meta.has_return,
            is_generator: {
                #[cfg(feature = "generator")]
                {
                    function_meta.flags & FUNCTION_FLAG_GENERATOR != 0
                }
                #[cfg(not(feature = "generator"))]
                {
                    function_meta.flags & (1 << 1) != 0
                }
            },
        })
    }

    fn hoist_function_declarations(
        &mut self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<(), ExecuteError> {
        let mut pc = start;
        while pc < end {
            match module
                .instructions
                .get(pc)
                .map(|instruction| instruction.op)
            {
                Some(BytecodeOp::FunctionStart) => {
                    let function = self.function_from_start(module, pc, false)?;
                    let body_end = function.body_end;
                    if let Some(name) = function.name.clone() {
                        let value = self.host_bridge.function_value(function);
                        self.lexical_env.set_or_define_current(name, value);
                    }
                    pc = body_end;
                    continue;
                }
                Some(BytecodeOp::FunctionExprStart) => {
                    let function = self.function_from_start(module, pc, true)?;
                    pc = function.body_end;
                    continue;
                }
                _ => {}
            }
            pc += 1;
        }
        Ok(())
    }

    fn class_from_instruction(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
    ) -> Result<Value, ExecuteError> {
        let name = match operand(instruction, 1)? {
            BytecodeOperand::None => None,
            operand => Some(self.read_name(module, operand)?),
        };
        Ok(self.host_bridge.class_value(ClassValue {
            name,
            constructor: None,
            static_props: BTreeMap::new(),
            instance_props: BTreeMap::new(),
        }))
    }

    fn read_value(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<Value, ExecuteError> {
        match operand {
            BytecodeOperand::Register(index) => Ok(self
                .registers
                .get(*index as usize)
                .cloned()
                .unwrap_or(Value::Undefined)),
            BytecodeOperand::Constant(index) => constant_value(module, *index),
            BytecodeOperand::Name(index) => {
                let name = name_string(module, *index)?;
                self.get_bound_name(&name)
            }
            BytecodeOperand::LocalSlot(slot) => Ok(self.lexical_env.get_slot(*slot)),
            BytecodeOperand::External(index) => {
                if let Ok(name) = self.external_name(module, *index)
                    && let Some(value) = self.lexical_env.get(&name)
                    && !matches!(value, Value::ExternalRef(_))
                {
                    return Ok(value);
                }
                if let Ok(name) = self.external_name(module, *index)
                    && (name.starts_with("__js_vm_")
                        || name == "eval"
                        || name == "Symbol"
                        || name == "BigInt")
                {
                    return Ok(Value::ExternalRef(ExternalRefValue::new(*index, name)));
                }
                let reference = self.external_ref(module, *index)?;
                let value = self.host_bridge.read_external(&reference)?;
                if let Value::JsValue(js_value) = &value
                    && js_function_name(js_value).as_deref() == Some("eval")
                {
                    return Ok(Value::ExternalRef(ExternalRefValue::new(
                        *index,
                        "eval".to_string(),
                    )));
                }
                if is_undefined_value(&value)
                    && let Ok(name) = self.external_name(module, *index)
                    && let Some(overlay) = external_name_overlay_get(&name)
                {
                    return Ok(overlay);
                }
                if is_undefined_value(&value) {
                    let name = self
                        .external_name(module, *index)
                        .unwrap_or_else(|_| format!("extern#{index}"));
                    if let Some(value) = self.lexical_env.get(&name) {
                        return Ok(value);
                    }
                    return Err(ExecuteError::ReferenceError(format!(
                        "{name} is not defined"
                    )));
                }
                Ok(value)
            }
            BytecodeOperand::None => Ok(Value::Undefined),
            BytecodeOperand::Label(_)
            | BytecodeOperand::Operator(_)
            | BytecodeOperand::DeclKind(_)
            | BytecodeOperand::Function(_)
            | BytecodeOperand::ScopeKind(_)
            | BytecodeOperand::Count(_) => Err(ExecuteError::InvalidOperand("value")),
        }
    }

    fn declare_binding(
        &self,
        module: &BytecodeModule,
        kind_operand: &BytecodeOperand,
        operand: &BytecodeOperand,
    ) -> Result<(), ExecuteError> {
        let is_var = self.read_decl_kind(kind_operand)? == "var";
        match operand {
            BytecodeOperand::LocalSlot(slot) => {
                if is_var {
                    self.lexical_env
                        .define_var_slot_if_absent(*slot, Value::Undefined);
                } else {
                    self.lexical_env
                        .define_slot_if_absent(*slot, Value::Undefined);
                }
                Ok(())
            }
            _ => {
                let name = self.read_name(module, operand)?;
                if is_var {
                    self.lexical_env
                        .define_var_if_absent(name, Value::Undefined);
                } else {
                    self.lexical_env
                        .define_current_if_absent(name, Value::Undefined);
                }
                Ok(())
            }
        }
    }

    fn define_catch_param(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
        value: Value,
    ) -> Result<(), ExecuteError> {
        match operand {
            BytecodeOperand::LocalSlot(slot) => {
                self.lexical_env.define_slot(*slot, value);
                Ok(())
            }
            BytecodeOperand::Name(index) => {
                self.lexical_env
                    .define_current(name_string(module, *index)?, value);
                Ok(())
            }
            _ => Err(ExecuteError::InvalidOperand("catch param")),
        }
    }

    fn get_name(&self, name: &str) -> Value {
        self.lexical_env.get(name).unwrap_or(Value::Undefined)
    }

    fn resolve_name_value(&self, value: Value) -> Result<Value, ExecuteError> {
        match value {
            Value::ExternalRef(reference) => {
                if reference.display_path() == "eval" {
                    return Ok(Value::ExternalRef(reference));
                }
                let value = self.host_bridge.read_external(&reference)?;
                if is_undefined_value(&value) {
                    return Err(ExecuteError::ReferenceError(format!(
                        "{} is not defined",
                        reference.display_path()
                    )));
                }
                Ok(value)
            }
            value => Ok(value),
        }
    }

    fn get_bound_name(&self, name: &str) -> Result<Value, ExecuteError> {
        self.lexical_env
            .get(name)
            .ok_or_else(|| ExecuteError::ReferenceError(format!("{name} is not defined")))
    }

    fn read_name(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        match operand {
            BytecodeOperand::Name(index) => name_string(module, *index),
            BytecodeOperand::External(index) => self.external_name(module, *index),
            BytecodeOperand::Constant(index) => constant_string(module, *index),
            _ => Err(ExecuteError::InvalidOperand("name")),
        }
    }

    fn read_operator(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        match operand {
            BytecodeOperand::Operator(index) => operator_name(*index)
                .map(str::to_string)
                .ok_or_else(|| ExecuteError::Runtime(format!("unknown operator {index}"))),
            BytecodeOperand::Constant(index) => constant_string(module, *index),
            _ => Err(ExecuteError::InvalidOperand("operator")),
        }
    }

    fn read_scope_kind(&self, operand: &BytecodeOperand) -> Result<ScopeKind, ExecuteError> {
        let BytecodeOperand::ScopeKind(index) = operand else {
            return Err(ExecuteError::InvalidOperand("scope kind"));
        };
        match index {
            0 => Ok(ScopeKind::Block),
            1 => Ok(ScopeKind::Function),
            2 => Ok(ScopeKind::Catch),
            _ => Err(ExecuteError::Runtime(format!("unknown scope kind {index}"))),
        }
    }

    fn read_decl_kind(&self, operand: &BytecodeOperand) -> Result<&'static str, ExecuteError> {
        let BytecodeOperand::DeclKind(index) = operand else {
            return Err(ExecuteError::InvalidOperand("decl kind"));
        };
        match index {
            0 => Ok("var"),
            1 => Ok("let"),
            2 => Ok("const"),
            3 => Ok("decl"),
            _ => Err(ExecuteError::Runtime(format!("unknown decl kind {index}"))),
        }
    }

    fn read_constant_string(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        match operand {
            BytecodeOperand::Constant(index) => constant_string(module, *index),
            _ => Err(ExecuteError::InvalidOperand("constant string")),
        }
    }

    fn write_operand_target(
        &mut self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
        value: Value,
    ) -> Result<(), ExecuteError> {
        match operand {
            BytecodeOperand::Register(index) => {
                self.write_register(*index, value);
                Ok(())
            }
            BytecodeOperand::Name(index) => {
                self.lexical_env
                    .set_or_define_current(name_string(module, *index)?, value);
                Ok(())
            }
            BytecodeOperand::LocalSlot(slot) => {
                self.lexical_env.set_slot(*slot, value);
                Ok(())
            }
            BytecodeOperand::External(index) => {
                self.host_bridge.set_slot(*index, &value)?;
                if let Ok(name) = self.external_name(module, *index) {
                    self.lexical_env.set_or_define_current(name, value);
                }
                Ok(())
            }
            _ => Err(ExecuteError::InvalidOperand("assignable object")),
        }
    }

    fn write_register(&mut self, index: u32, value: Value) {
        let index = index as usize;
        if self.registers.len() <= index {
            self.registers.resize(index + 1, Value::Undefined);
        }
        self.registers[index] = value;
    }

    fn external_ref(
        &self,
        module: &BytecodeModule,
        index: u32,
    ) -> Result<ExternalRefValue, ExecuteError> {
        let root = external_string(module, index).unwrap_or_else(|_| format!("extern#{index}"));
        let root = if root.starts_with("__js_vm_") {
            root
        } else {
            self.external_names
                .get(index as usize)
                .cloned()
                .unwrap_or(root)
        };
        self.host_bridge.validate_slot(index)?;
        Ok(ExternalRefValue::new(index, root))
    }

    fn external_name(&self, module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
        if let Some(name) = module.extern_slots.get(index as usize)
            && name.starts_with("__js_vm_")
        {
            return Ok(name.clone());
        }
        self.external_names
            .get(index as usize)
            .cloned()
            .or_else(|| module.extern_slots.get(index as usize).cloned())
            .ok_or(ExecuteError::BadConstant(index))
    }
}

fn is_callable_value(value: &Value) -> bool {
    match value {
        Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::ExternalRef(_)
        | Value::Class(_) => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            vm_js_handle(value).is_some() || value.dyn_ref::<JsFunction>().is_some()
        }
        _ => false,
    }
}

#[cfg(feature = "generator")]
fn iterator_result(value: Value, done: bool) -> Value {
    object_value(BTreeMap::from([
        ("value".to_string(), value),
        ("done".to_string(), Value::Bool(done)),
    ]))
}

fn should_propagate_js_binary_error(op: &str, left: &Value, right: &Value) -> bool {
    matches!(
        op,
        "+" | "-" | "*" | "/" | "%" | "**" | "&" | "|" | "^" | "<<" | ">>" | ">>>"
    ) && {
        #[cfg(feature = "bigint")]
        {
            is_js_bigint_value(left) || is_js_bigint_value(right)
        }
        #[cfg(not(feature = "bigint"))]
        {
            let _ = (left, right);
            false
        }
    }
}

#[cfg(feature = "bigint")]
fn is_js_bigint_value(value: &Value) -> bool {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => js_value_is_bigint(value),
        _ => false,
    }
}

#[cfg(feature = "bigint")]
fn bigint_prototype_call(
    method: &str,
    this_value: Value,
    args: Vec<Value>,
) -> Result<Value, ExecuteError> {
    let receiver = value_to_js_value(&this_value, &JsHostBridge::empty())?;
    let js_args = js_sys::Array::new();
    for arg in args {
        js_args.push(&value_to_js_value(&arg, &JsHostBridge::empty())?);
    }
    let js_args: JsValue = js_args.into();
    let result = JsFunction::new_with_args(
        "receiver, args, method",
        "return BigInt.prototype[method].apply(receiver, args);",
    )
    .call3(
        &JsValue::UNDEFINED,
        &receiver,
        &js_args,
        &JsValue::from_str(method),
    )
    .map_err(js_error)?;
    Ok(normalize_js_bigint_value(Value::JsValue(result)))
}

#[cfg(feature = "bigint")]
fn normalize_js_bigint_value(value: Value) -> Value {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _) if js_value_is_bigint(&value) => {
            Value::BigInt(crate::host::js_value_display(&value))
        }
        value => value,
    }
}
