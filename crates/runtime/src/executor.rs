use crate::env::{LexicalEnv, ScopeKind};
use crate::error::ExecuteError;
use crate::host::{
    HostBridge, VmJsHandle, external_name_overlay_get, get_js_property, js_error,
    value_to_js_value, vm_class_to_js_value, vm_function_to_js_value, vm_js_handle,
    vm_module_to_js_value,
};
use crate::ops::{
    apply_argument_list, binary, collect_scope_metadata, constant_string, constant_value,
    count_operand, external_string, find_try_parts, function_source_string, get_local_member,
    name_string, normalize_index, object_has_own_property, object_to_string_tag, operand,
    operator_name, property_key, regex_string_parts, regexp_exec_value, register, set_member,
    unary,
};
use crate::value::{
    ClassValue, ExternalRefValue, FunctionValue, ModuleValue, Value, array_value, object_value,
};
use js_sys::{Array as JsArray, Function as JsFunction};
use js_token_core::{BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand};
use std::{cell::Cell, cell::RefCell, collections::BTreeMap, rc::Rc};
use wasm_bindgen::{JsCast, JsValue};

#[derive(Debug)]
pub struct Executor {
    registers: Vec<Value>,
    lexical_env: LexicalEnv,
    instruction_scope_depths: Vec<usize>,
    last_value: Value,
    exports: BTreeMap<String, Value>,
    host_bridge: HostBridge,
    external_names: Vec<String>,
    call_depth: usize,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
    call_stack: Rc<RefCell<Vec<usize>>>,
    execution_budget: Rc<Cell<usize>>,
}

pub(crate) const DEFAULT_MAX_CALL_DEPTH: usize = 128;
pub(crate) const DEFAULT_MAX_RECURSIVE_CALL_DEPTH: usize = 8;
const MAX_EXECUTION_STEPS: usize = 250_000;

#[derive(Debug, Clone, PartialEq)]
enum Flow {
    Value(Value),
    Return(Value),
    Throw(Value),
}

fn catchable_error_value(error: &ExecuteError) -> Option<Value> {
    match error {
        ExecuteError::Thrown(value) => Some(value.clone()),
        ExecuteError::TypeError(message) => Some(Value::String(format!("TypeError: {message}"))),
        ExecuteError::RangeError(message) => Some(Value::String(format!("RangeError: {message}"))),
        _ => None,
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            registers: Vec::new(),
            lexical_env: LexicalEnv::default(),
            instruction_scope_depths: Vec::new(),
            last_value: Value::Undefined,
            exports: BTreeMap::new(),
            host_bridge: HostBridge::default(),
            external_names: Vec::new(),
            call_depth: 0,
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            max_recursive_call_depth: DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
            call_stack: Rc::new(RefCell::new(Vec::new())),
            execution_budget: Rc::new(Cell::new(MAX_EXECUTION_STEPS)),
        }
    }
}

impl Executor {
    pub fn run(module: &BytecodeModule) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge(module, HostBridge::default())
    }

    pub fn run_with_host_bridge(
        module: &BytecodeModule,
        host_bridge: HostBridge,
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
        host_bridge: HostBridge,
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
        host_bridge: HostBridge,
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
        host_bridge: HostBridge,
        external_names: Vec<String>,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
    ) -> Result<Value, ExecuteError> {
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
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
        }
    }

    fn with_host_bridge_and_limits(
        host_bridge: HostBridge,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
    ) -> Self {
        Self {
            host_bridge,
            max_call_depth: max_call_depth.max(1),
            max_recursive_call_depth: max_recursive_call_depth.max(1),
            ..Self::default()
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
                    self.declare_binding(module, operand(instruction, 1)?)?;
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
                        BytecodeOperand::External(_) => self.read_value(module, source)?,
                        BytecodeOperand::LocalSlot(slot) => self.lexical_env.get_slot(*slot),
                        _ => {
                            let name = self.read_name(module, source)?;
                            self.get_name(&name)
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
                    let value = binary(&op, left, right)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Unary => {
                    let dst = register(instruction, 0)?;
                    let op = self.read_operator(module, operand(instruction, 1)?)?;
                    let arg = self.read_value(module, operand(instruction, 2)?)?;
                    let value = unary(&op, arg)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Member => {
                    let dst = register(instruction, 0)?;
                    let object = self.read_value(module, operand(instruction, 1)?)?;
                    let property_value = self.read_value(module, operand(instruction, 2)?)?;
                    let property = property_key(&property_value);
                    let value = self.get_member(&object, &property)?;
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
                BytecodeOp::Call => {
                    let dst = register(instruction, 0)?;
                    let callee_operand = operand(instruction, 1)?;
                    let callee = self.read_value(module, callee_operand)?;
                    let callee_display = callee.to_string();
                    let count = count_operand(instruction, 2)? as usize;
                    let args = self.read_args(module, instruction, 3, count)?;
                    let value = self.call(module, callee, args).map_err(|err| match err {
                        ExecuteError::TypeError(message) => ExecuteError::TypeError(format!(
                            "{message} at pc {pc} callee {callee_operand:?} value {callee_display}"
                        )),
                        ExecuteError::RangeError(message) => ExecuteError::RangeError(format!(
                            "{message} at pc {pc} callee {callee_operand:?} value {callee_display}"
                        )),
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
                            out.push_str(&self.read_value(module, expr)?.to_string());
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
                    let value = Value::JsValue(vm_function_to_js_value(function));
                    self.lexical_env.set_or_define_current(name, value);
                    pc = body_end;
                    continue;
                }
                BytecodeOp::FunctionExprStart => {
                    let dst = register(instruction, 0)?;
                    let function = self.function_from_start(module, pc, true)?;
                    let body_end = function.body_end;
                    self.write_register(dst, Value::JsValue(vm_function_to_js_value(function)));
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
                    let source = self.read_constant_string(module, operand(instruction, 0)?)?;
                    let module_value = ModuleValue {
                        source,
                        exports: BTreeMap::new(),
                    };
                    self.last_value = Value::JsValue(vm_module_to_js_value(module_value)?);
                }
                BytecodeOp::Export => {
                    let count = count_operand(instruction, 1)? as usize;
                    for index in 0..count {
                        let name = self.read_name(module, operand(instruction, 2 + index)?)?;
                        self.exports.insert(name.clone(), self.get_name(&name));
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
            return Err(ExecuteError::RangeError(
                "maximum execution steps exceeded".to_string(),
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
                Err(ExecuteError::TypeError(format!("cannot call {callee}")))
            }
            Value::Function(function) => {
                self.call_function(module, &function, self.get_name("this"), args)
            }
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
                Some(handle) => self.call_vm_js_handle(module, handle, Value::Undefined, args),
                None => self
                    .host_bridge
                    .call_js_value(value, JsValue::UNDEFINED, args),
            },
            Value::BoundJsFunction(function, this_value) => match vm_js_handle(&function) {
                Some(handle) => {
                    self.call_vm_js_handle(module, handle, Value::JsValue(this_value), args)
                }
                None => self.host_bridge.call_js_value(function, this_value, args),
            },
            Value::ExternalRef(reference) => self.host_bridge.call(&reference, args),
            Value::Class(class) => Ok(object_value(class.static_props)),
            _ => Err(ExecuteError::TypeError(format!("{callee} is not callable"))),
        }
    }

    fn get_member(&self, object: &Value, property: &str) -> Result<Value, ExecuteError> {
        match object {
            Value::ExternalRef(reference) => {
                if reference.display_path() == "Symbol" {
                    return Ok(Value::Symbol(format!("Symbol.{property}")));
                }
                self.host_bridge.get(reference, property)
            }
            Value::JsValue(_) | Value::BoundJsFunction(_, _) => get_local_member(object, property),
            _ => get_local_member(object, property),
        }
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
            VmJsHandle::NativeFunction(function) => {
                self.call_native_method(module, &function.name, Value::Undefined, args)
            }
            VmJsHandle::BoundNativeFunction(function, this_value) => {
                self.call_native_method(module, &function.name, this_value, args)
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
        let (value, _) = self.call_function_frame(module, function, this_value, args)?;
        Ok(value)
    }

    fn call_native_method(
        &self,
        module: &BytecodeModule,
        name: &str,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match name {
            "Array.push" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    items.extend(args);
                    Ok(Value::Number(items.len() as f64))
                }
                _ => Ok(Value::Undefined),
            },
            "Array.fill" => match this_value {
                Value::Array(items) => {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    let mut items = items.borrow_mut();
                    items.fill(value);
                    Ok(array_value(items.clone()))
                }
                _ => Ok(Value::Undefined),
            },
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
            "Array.includes" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let needle = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Bool(items.iter().any(|item| *item == needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
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
            "Array.pop" => match this_value {
                Value::Array(items) => Ok(items.borrow_mut().pop().unwrap_or(Value::Undefined)),
                _ => Ok(Value::Undefined),
            },
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
            "Array.reverse" => match this_value {
                Value::Array(items) => {
                    items.borrow_mut().reverse();
                    Ok(Value::Array(items))
                }
                _ => Ok(Value::Undefined),
            },
            "Array.sort" => match this_value {
                Value::Array(items) => {
                    items
                        .borrow_mut()
                        .sort_by(|left, right| left.to_string().cmp(&right.to_string()));
                    Ok(Value::Array(items))
                }
                _ => Ok(Value::Undefined),
            },
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
            "String.includes" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    Ok(Value::Bool(value.contains(&needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
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
            "String.trim" => match this_value {
                Value::String(value) => Ok(Value::String(value.trim().to_string())),
                _ => Ok(Value::String(String::new())),
            },
            "String.toLowerCase" => match this_value {
                Value::String(value) => Ok(Value::String(value.to_lowercase())),
                _ => Ok(Value::String(String::new())),
            },
            "String.toUpperCase" => match this_value {
                Value::String(value) => Ok(Value::String(value.to_uppercase())),
                _ => Ok(Value::String(String::new())),
            },
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
            "String.concat" => match this_value {
                Value::String(mut value) => {
                    for arg in args {
                        value.push_str(&arg.to_string());
                    }
                    Ok(Value::String(value))
                }
                value => {
                    let mut output = value.to_string();
                    for arg in args {
                        output.push_str(&arg.to_string());
                    }
                    Ok(Value::String(output))
                }
            },
            "String.match" => match this_value {
                Value::String(value) => self.call_js_string_method(&value, "match", args),
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => value
                    .as_string()
                    .map(|value| self.call_js_string_method(&value, "match", args))
                    .unwrap_or_else(|| Ok(Value::Null)),
                _ => Ok(Value::Null),
            },
            "String.replace" => match this_value {
                Value::String(value) => self.call_js_string_method(&value, "replace", args),
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => value
                    .as_string()
                    .map(|value| self.call_js_string_method(&value, "replace", args))
                    .unwrap_or_else(|| Ok(Value::String(String::new()))),
                _ => Ok(Value::String(String::new())),
            },
            "RegExp.exec" => {
                let input = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(regexp_exec_value(&this_value, &input))
            }
            "RegExp.test" => {
                let input = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(Value::Bool(!matches!(
                    regexp_exec_value(&this_value, &input),
                    Value::Null
                )))
            }
            "Object.prototype.hasOwnProperty" => {
                let key = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(Value::Bool(object_has_own_property(&this_value, &key)))
            }
            "Object.prototype.toString" => Ok(Value::String(object_to_string_tag(&this_value))),
            "Object.prototype.valueOf" => Ok(this_value),
            "Function.toString" => Ok(Value::String(function_source_string(&this_value))),
            "Function.call" => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let call_args = args.into_iter().skip(1).collect::<Vec<_>>();
                self.call_with_this(module, this_value, this_arg, call_args)
            }
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
                    None => self.call(module, Value::JsValue(value), args),
                }
            }
            Value::ExternalRef(reference) => self
                .host_bridge
                .call_with_this(&reference, this_value, args),
            value => self.call(module, value, args),
        }
    }
    fn call_js_string_method(
        &self,
        value: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let this_value = JsValue::from_str(value);
        let callee = get_js_property(&this_value, method)?;
        let js_args = JsArray::new();
        for (index, arg) in args.iter().enumerate() {
            if index == 0 {
                let pattern = match arg {
                    Value::String(pattern) => Some(pattern.clone()),
                    Value::JsValue(value) => value.as_string(),
                    Value::BoundJsFunction(value, _) => value.as_string(),
                    _ => None,
                };
                if let Some(pattern) = pattern {
                    if let Some((body, global)) = regex_string_parts(&pattern) {
                        js_args
                            .push(&js_sys::RegExp::new(body, if global { "g" } else { "" }).into());
                        continue;
                    }
                }
            }
            js_args.push(&value_to_js_value(arg, &self.host_bridge)?);
        }
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(ExecuteError::TypeError(format!(
                "String.{method} is not callable"
            )));
        };
        function
            .apply(&this_value, &js_args)
            .map(Value::JsValue)
            .map_err(js_error)
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
                        let value = Value::JsValue(vm_function_to_js_value(function));
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
        Ok(Value::JsValue(vm_class_to_js_value(ClassValue {
            name,
            constructor: None,
            static_props: BTreeMap::new(),
            instance_props: BTreeMap::new(),
        })))
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
                Ok(self.get_name(&name))
            }
            BytecodeOperand::LocalSlot(slot) => Ok(self.lexical_env.get_slot(*slot)),
            BytecodeOperand::External(index) => {
                let reference = self.external_ref(module, *index)?;
                let value = self.host_bridge.read_external(&reference)?;
                if matches!(value, Value::Undefined)
                    && let Ok(name) = self.external_name(module, *index)
                    && let Some(overlay) = external_name_overlay_get(&name)
                {
                    return Ok(overlay);
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
        operand: &BytecodeOperand,
    ) -> Result<(), ExecuteError> {
        match operand {
            BytecodeOperand::LocalSlot(slot) => {
                self.lexical_env
                    .define_slot_if_absent(*slot, Value::Undefined);
                Ok(())
            }
            _ => {
                let name = self.read_name(module, operand)?;
                self.lexical_env
                    .define_current_if_absent(name, Value::Undefined);
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
        let root = self
            .external_names
            .get(index as usize)
            .cloned()
            .unwrap_or(root);
        self.host_bridge.validate_slot(index)?;
        Ok(ExternalRefValue::new(index, root))
    }

    fn external_name(&self, module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
        self.external_names
            .get(index as usize)
            .cloned()
            .or_else(|| module.extern_slots.get(index as usize).cloned())
            .ok_or(ExecuteError::BadConstant(index))
    }
}
