use js_token_core::{
    BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fmt,
    rc::Rc,
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use js_sys::{Array as JsArray, Function as JsFunction, Reflect};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = __jsVmHostLog)]
    fn wasm_host_log(level: &str, value: &str);
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn js_execute_bytes_with_seed(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<String, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge = HostBridge::from_js_values(externals.into_vec());
    Executor::run_with_host_bridge(&module, host_bridge)
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
}

#[derive(Debug)]
pub struct Executor {
    registers: Vec<Value>,
    lexical_env: LexicalEnv,
    labels: BTreeMap<String, usize>,
    label_scope_depths: BTreeMap<String, usize>,
    instruction_scope_depths: Vec<usize>,
    last_value: Value,
    exports: BTreeMap<String, Value>,
    host_bridge: HostBridge,
    external_names: Vec<String>,
    call_depth: usize,
    execution_budget: Rc<Cell<usize>>,
}

const MAX_CALL_DEPTH: usize = 8;
const MAX_EXECUTION_STEPS: usize = 250_000;

impl Default for Executor {
    fn default() -> Self {
        Self {
            registers: Vec::new(),
            lexical_env: LexicalEnv::default(),
            labels: BTreeMap::new(),
            label_scope_depths: BTreeMap::new(),
            instruction_scope_depths: Vec::new(),
            last_value: Value::Undefined,
            exports: BTreeMap::new(),
            host_bridge: HostBridge::default(),
            external_names: Vec::new(),
            call_depth: 0,
            execution_budget: Rc::new(Cell::new(MAX_EXECUTION_STEPS)),
        }
    }
}

impl Executor {
    pub fn run(module: &BytecodeModule) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge(module, HostBridge::default())
    }

    pub fn run_with_external_names(
        module: &BytecodeModule,
        externals: &[String],
    ) -> Result<Value, ExecuteError> {
        let mut executor = Self::with_host_bridge(HostBridge::default());
        executor.external_names = externals.to_vec();
        executor.inject_module_externals(module);
        executor.inject_external_names(externals.iter().cloned());
        executor.load_scope_metadata(module)?;
        match executor.execute_range(module, 0, module.instructions.len())? {
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
        }
    }

    pub fn run_with_host_bridge(
        module: &BytecodeModule,
        host_bridge: HostBridge,
    ) -> Result<Value, ExecuteError> {
        host_bridge.validate_extern_count(module.extern_slots.len())?;
        let mut executor = Self::with_host_bridge(host_bridge);
        executor.external_names = module.extern_slots.clone();
        executor.inject_module_externals(module);
        executor.load_scope_metadata(module)?;
        match executor.execute_range(module, 0, module.instructions.len())? {
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
        }
    }

    fn with_host_bridge(host_bridge: HostBridge) -> Self {
        Self {
            host_bridge,
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

    fn inject_external_names(&mut self, externals: impl IntoIterator<Item = String>) {
        let offset = self.host_bridge.external_count();
        for (index, name) in externals.into_iter().enumerate() {
            self.lexical_env.define_global_if_absent(
                name.clone(),
                Value::ExternalRef(ExternalRefValue::new((offset + index) as u32, name)),
            );
        }
    }

    fn load_scope_metadata(&mut self, module: &BytecodeModule) -> Result<(), ExecuteError> {
        let metadata = collect_scope_metadata(module)?;
        self.labels = metadata.labels;
        self.label_scope_depths = metadata.label_scope_depths;
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
                    let name = self.read_name(module, operand(instruction, 1)?)?;
                    self.lexical_env.define_current(name, Value::Undefined);
                }
                BytecodeOp::LoadConst => {
                    let dst = register(instruction, 0)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::LoadName => {
                    let dst = register(instruction, 0)?;
                    let source = operand(instruction, 1)?;
                    let value = match source {
                        BytecodeOperand::External(_) => self.read_value(module, source)?,
                        _ => {
                            let name = self.read_name(module, source)?;
                            self.get_name(&name)
                        }
                    };
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreName => {
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
                    let value = Value::Object(props);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Call => {
                    let dst = register(instruction, 0)?;
                    let callee = self.read_value(module, operand(instruction, 1)?)?;
                    let count = count_operand(instruction, 2)? as usize;
                    let args = self.read_args(module, instruction, 3, count)?;
                    let value = self.call(module, callee, args)?;
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
                    let end_pc = function.end_pc;
                    self.lexical_env
                        .set_or_define_current(name, Value::Function(function));
                    pc = end_pc + 1;
                    continue;
                }
                BytecodeOp::FunctionExprStart => {
                    let dst = register(instruction, 0)?;
                    let function = self.function_from_start(module, pc, true)?;
                    let end_pc = function.end_pc;
                    self.write_register(dst, Value::Function(function));
                    pc = end_pc + 1;
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
                    }
                    self.last_value = value;
                }
                BytecodeOp::Import => {
                    let source = self.read_constant_string(module, operand(instruction, 0)?)?;
                    self.last_value = Value::Module(ModuleValue {
                        source,
                        exports: BTreeMap::new(),
                    });
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
                            if let Some(param) = parts.catch_param {
                                self.lexical_env.define_current(param, value);
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
                    let label = self.read_label(module, operand(instruction, 0)?)?;
                    pc = self.jump_target(&label, pc)?;
                    continue;
                }
                BytecodeOp::JumpIfFalse => {
                    let test = self.read_value(module, operand(instruction, 0)?)?;
                    if !test.is_truthy() {
                        let label = self.read_label(module, operand(instruction, 1)?)?;
                        pc = self.jump_target(&label, pc)?;
                        continue;
                    }
                }
                BytecodeOp::Unsupported => {
                    return Err(ExecuteError::Unsupported(instruction.op.mnemonic()));
                }
                BytecodeOp::LoadConstConst | BytecodeOp::PopReg | BytecodeOp::CallOne => {
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

    fn jump_target(&mut self, label: &str, pc: usize) -> Result<usize, ExecuteError> {
        let target = *self
            .labels
            .get(label)
            .ok_or_else(|| ExecuteError::UnknownLabel(label.to_string()))?;
        let current_scope_depth = self
            .instruction_scope_depths
            .get(pc)
            .copied()
            .unwrap_or_default();
        let target_scope_depth = self
            .label_scope_depths
            .get(label)
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
            Value::ExternalRef(reference) => self.host_bridge.call(&reference, args),
            Value::Class(class) => Ok(Value::Object(class.static_props)),
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
            Value::Class(class) => {
                let mut this_value = Value::Object(class.instance_props.clone());
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
            Value::Function(function) => {
                let this_value = Value::Object(BTreeMap::new());
                let result = self.call_function(module, &function, this_value.clone(), args)?;
                if matches!(result, Value::Undefined) {
                    Ok(this_value)
                } else {
                    Ok(result)
                }
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
            Value::ExternalRef(reference) => self.host_bridge.construct(&reference, args),
            _ => Err(ExecuteError::TypeError(format!(
                "{callee} is not constructable"
            ))),
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
            _ => Err(ExecuteError::Runtime(format!(
                "native method {name} is not registered"
            ))),
        }
    }

    fn call_function_frame(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<(Value, LexicalEnv), ExecuteError> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(ExecuteError::RangeError(
                "maximum call stack exceeded".to_string(),
            ));
        }
        let mut lexical_env = function.env.clone();
        lexical_env.push_frame(ScopeKind::Function);
        lexical_env.define_current("this".to_string(), this_value);
        for (index, param) in function.params.iter().enumerate() {
            lexical_env.define_current(
                param.clone(),
                args.get(index).cloned().unwrap_or(Value::Undefined),
            );
        }

        let metadata = collect_scope_metadata(module)?;
        let mut frame = Executor {
            registers: Vec::new(),
            lexical_env,
            labels: metadata.labels,
            label_scope_depths: metadata.label_scope_depths,
            instruction_scope_depths: metadata.instruction_scope_depths,
            last_value: Value::Undefined,
            exports: self.exports.clone(),
            host_bridge: self.host_bridge.clone(),
            external_names: self.external_names.clone(),
            call_depth: self.call_depth + 1,
            execution_budget: self.execution_budget.clone(),
        };
        let flow = frame.execute_range(module, function.body_start, function.body_end)?;
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
        let params = function_meta
            .params
            .iter()
            .map(|index| name_string(module, *index))
            .collect::<Result<Vec<_>, _>>()?;
        let end_op = if expr {
            BytecodeOp::FunctionExprEnd
        } else {
            BytecodeOp::FunctionEnd
        };
        let end_pc = find_matching_end(module, pc + 1, end_op)?;
        Ok(FunctionValue {
            name,
            params,
            body_start: pc + 1,
            body_end: end_pc,
            end_pc,
            env: self.lexical_env.clone(),
            has_return: function_meta.has_return,
        })
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
        Ok(Value::Class(ClassValue {
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
                Ok(self.get_name(&name))
            }
            BytecodeOperand::External(index) => {
                self.external_ref(module, *index).map(Value::ExternalRef)
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

    fn read_label(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        match operand {
            BytecodeOperand::Label(index) => Ok(index.to_string()),
            BytecodeOperand::Constant(index) => constant_string(module, *index),
            _ => Err(ExecuteError::InvalidOperand("label")),
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

#[derive(Debug, Clone)]
pub struct LexicalEnv {
    frames: Vec<ScopeFrame>,
}

impl Default for LexicalEnv {
    fn default() -> Self {
        let global = ScopeFrame::new(ScopeKind::Global);
        let mut this_props = BTreeMap::new();
        this_props.insert("WScript".to_string(), Value::Object(BTreeMap::new()));
        global
            .record
            .borrow_mut()
            .bindings
            .insert("this".to_string(), Value::Object(this_props));
        Self {
            frames: vec![global],
        }
    }
}

impl LexicalEnv {
    fn depth(&self) -> usize {
        self.frames.len()
    }

    fn push_frame(&mut self, kind: ScopeKind) {
        self.frames.push(ScopeFrame::new(kind));
    }

    fn pop_frame(&mut self) {
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.kind != ScopeKind::Global)
        {
            self.frames.pop();
        }
    }

    fn truncate_to_depth(&mut self, depth: usize) {
        let depth = depth.max(1);
        while self.frames.len() > depth {
            self.pop_frame();
        }
    }

    fn get(&self, name: &str) -> Option<Value> {
        self.frames
            .iter()
            .rev()
            .find_map(|frame| frame.record.borrow().bindings.get(name).cloned())
    }

    fn define_current(&self, name: String, value: Value) {
        if let Some(frame) = self.frames.last() {
            frame.record.borrow_mut().bindings.insert(name, value);
        }
    }

    fn define_global_if_absent(&self, name: String, value: Value) {
        if let Some(frame) = self.frames.first() {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name)
                .or_insert(value);
        }
    }

    fn set_or_define_current(&self, name: String, value: Value) {
        if self.set_existing(&name, value.clone()) {
            return;
        }
        self.define_current(name, value);
    }

    fn set_existing(&self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter().rev() {
            let has_binding = frame.record.borrow().bindings.contains_key(name);
            if has_binding {
                frame
                    .record
                    .borrow_mut()
                    .bindings
                    .insert(name.to_string(), value);
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone)]
pub struct ScopeFrame {
    kind: ScopeKind,
    record: Rc<RefCell<EnvironmentRecord>>,
}

impl ScopeFrame {
    fn new(kind: ScopeKind) -> Self {
        Self {
            kind,
            record: Rc::new(RefCell::new(EnvironmentRecord::default())),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct EnvironmentRecord {
    bindings: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Global,
    Function,
    Block,
    Catch,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRefValue {
    pub slot: u32,
    pub root: String,
    pub path: Vec<String>,
}

impl ExternalRefValue {
    fn new(slot: u32, root: impl Into<String>) -> Self {
        Self {
            slot,
            root: root.into(),
            path: Vec::new(),
        }
    }

    fn member(&self, property: &str) -> Self {
        let mut next = self.clone();
        next.path.push(property.to_string());
        next
    }

    fn display_path(&self) -> String {
        if self.path.is_empty() {
            self.root.clone()
        } else {
            format!("{}.{}", self.root, self.path.join("."))
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    String(String),
    Symbol(String),
    Bool(bool),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(BTreeMap<String, Value>),
    Function(FunctionValue),
    BoundFunction(FunctionValue, Box<Value>),
    NativeFunction(NativeFunctionValue),
    BoundNativeFunction(NativeFunctionValue, Box<Value>),
    ExternalRef(ExternalRefValue),
    Class(ClassValue),
    Module(ModuleValue),
    Null,
    #[default]
    Undefined,
}

#[derive(Debug, Clone)]
pub struct FunctionValue {
    pub name: Option<String>,
    pub params: Vec<String>,
    pub has_return: bool,
    pub body_start: usize,
    pub body_end: usize,
    pub env: LexicalEnv,
    end_pc: usize,
}

impl PartialEq for FunctionValue {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.params == other.params
            && self.has_return == other.has_return
            && self.body_start == other.body_start
            && self.body_end == other.body_end
            && self.end_pc == other.end_pc
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeFunctionValue {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassValue {
    pub name: Option<String>,
    pub constructor: Option<FunctionValue>,
    pub static_props: BTreeMap<String, Value>,
    pub instance_props: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleValue {
    pub source: String,
    pub exports: BTreeMap<String, Value>,
}

fn array_value(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
}

impl Value {
    fn is_truthy(&self) -> bool {
        match self {
            Value::Number(value) => *value != 0.0 && !value.is_nan(),
            Value::String(value) => !value.is_empty(),
            Value::Symbol(_) => true,
            Value::Bool(value) => *value,
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
            | Value::NativeFunction(_)
            | Value::BoundNativeFunction(_, _)
            | Value::ExternalRef(_)
            | Value::Class(_)
            | Value::Module(_) => true,
            Value::Null | Value::Undefined => false,
        }
    }

    fn to_number(&self) -> f64 {
        match self {
            Value::Number(value) => *value,
            Value::String(value) => value.parse().unwrap_or(f64::NAN),
            Value::Symbol(_) => f64::NAN,
            Value::Bool(value) => f64::from(*value as u8),
            Value::Null => 0.0,
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
            | Value::NativeFunction(_)
            | Value::BoundNativeFunction(_, _)
            | Value::ExternalRef(_)
            | Value::Class(_)
            | Value::Module(_)
            | Value::Undefined => f64::NAN,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Number(value) => write!(f, "{value}"),
            Value::String(value) => write!(f, "{value}"),
            Value::Symbol(value) => write!(f, "Symbol({value})"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::Array(items) => {
                let items = items
                    .borrow()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                write!(f, "[{items}]")
            }
            Value::Object(_) => write!(f, "[object Object]"),
            Value::Function(function) => write!(
                f,
                "function {}",
                function.name.as_deref().unwrap_or("<anonymous>")
            ),
            Value::BoundFunction(function, _) => write!(
                f,
                "function {}",
                function.name.as_deref().unwrap_or("<anonymous>")
            ),
            Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => {
                write!(f, "function {}", function.name)
            }
            Value::ExternalRef(reference) => write!(f, "[external {}]", reference.display_path()),
            Value::Class(class) => write!(
                f,
                "class {}",
                class.name.as_deref().unwrap_or("<anonymous>")
            ),
            Value::Module(module) => write!(f, "module {}", module.source),
            Value::Null => write!(f, "null"),
            Value::Undefined => write!(f, "undefined"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecuteError {
    MissingOperand { op: &'static str, index: usize },
    InvalidOperand(&'static str),
    BadConstant(u32),
    UnknownLabel(String),
    Unsupported(&'static str),
    Thrown(Value),
    TypeError(String),
    RangeError(String),
    Runtime(String),
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecuteError::MissingOperand { op, index } => {
                write!(f, "{op} missing operand {index}")
            }
            ExecuteError::InvalidOperand(kind) => write!(f, "invalid {kind} operand"),
            ExecuteError::BadConstant(index) => write!(f, "bad constant index {index}"),
            ExecuteError::UnknownLabel(label) => write!(f, "unknown label {label}"),
            ExecuteError::Unsupported(op) => write!(f, "unsupported opcode {op}"),
            ExecuteError::Thrown(value) => write!(f, "uncaught exception {value}"),
            ExecuteError::TypeError(message) => write!(f, "TypeError: {message}"),
            ExecuteError::RangeError(message) => write!(f, "RangeError: {message}"),
            ExecuteError::Runtime(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ExecuteError {}

struct TryParts {
    body_start: usize,
    body_end: usize,
    catch_start: usize,
    catch_end: usize,
    catch_param: Option<String>,
    finally_start: usize,
    finally_end: usize,
    end: usize,
}

fn find_try_parts(module: &BytecodeModule, try_start: usize) -> Result<TryParts, ExecuteError> {
    let mut depth = 0usize;
    let mut catch = None;
    let mut finally = None;
    let mut end = None;
    for index in try_start + 1..module.instructions.len() {
        match module.instructions[index].op {
            BytecodeOp::TryStart => depth += 1,
            BytecodeOp::TryEnd if depth == 0 => {
                end = Some(index);
                break;
            }
            BytecodeOp::TryEnd => depth -= 1,
            BytecodeOp::CatchStart if depth == 0 => catch = Some(index),
            BytecodeOp::FinallyStart if depth == 0 => finally = Some(index),
            _ => {}
        }
    }
    let end = end.ok_or(ExecuteError::Runtime("missing TRY_END".to_string()))?;
    let body_end = catch.or(finally).unwrap_or(end);
    let catch_start = catch.map(|index| index + 1).unwrap_or(end);
    let catch_end = finally.unwrap_or(end);
    let catch_param = catch
        .map(|index| match operand(&module.instructions[index], 0)? {
            BytecodeOperand::None => Ok(None),
            BytecodeOperand::Name(value) => name_string(module, *value).map(Some),
            BytecodeOperand::External(value) => external_string(module, *value).map(Some),
            BytecodeOperand::Constant(value) => constant_string(module, *value).map(Some),
            _ => Err(ExecuteError::InvalidOperand("catch param")),
        })
        .transpose()?
        .flatten();
    let finally_start = finally.map(|index| index + 1).unwrap_or(end);
    let finally_end = end;
    Ok(TryParts {
        body_start: try_start + 1,
        body_end,
        catch_start,
        catch_end,
        catch_param,
        finally_start,
        finally_end,
        end,
    })
}

fn find_matching_end(
    module: &BytecodeModule,
    start: usize,
    end_op: BytecodeOp,
) -> Result<usize, ExecuteError> {
    let mut depth = 0usize;
    for index in start..module.instructions.len() {
        match module.instructions[index].op {
            BytecodeOp::FunctionStart | BytecodeOp::FunctionExprStart => depth += 1,
            op if op == end_op && depth == 0 => return Ok(index),
            BytecodeOp::FunctionEnd | BytecodeOp::FunctionExprEnd => {
                depth = depth.saturating_sub(1)
            }
            _ => {}
        }
    }
    Err(ExecuteError::Runtime(format!(
        "missing matching {}",
        end_op.mnemonic()
    )))
}

struct ScopeMetadata {
    labels: BTreeMap<String, usize>,
    label_scope_depths: BTreeMap<String, usize>,
    instruction_scope_depths: Vec<usize>,
}

fn collect_scope_metadata(module: &BytecodeModule) -> Result<ScopeMetadata, ExecuteError> {
    let mut labels = BTreeMap::new();
    let mut label_scope_depths = BTreeMap::new();
    let mut instruction_scope_depths = Vec::with_capacity(module.instructions.len());
    let mut scope_depth = 0usize;
    for (index, instruction) in module.instructions.iter().enumerate() {
        instruction_scope_depths.push(scope_depth);
        if instruction.op == BytecodeOp::Label {
            let label = match operand(instruction, 0)? {
                BytecodeOperand::Label(index) => index.to_string(),
                BytecodeOperand::Constant(index) => constant_string(module, *index)?,
                _ => return Err(ExecuteError::InvalidOperand("label")),
            };
            labels.insert(label.clone(), index + 1);
            label_scope_depths.insert(label, scope_depth);
        }
        match instruction.op {
            BytecodeOp::EnterScope => scope_depth += 1,
            BytecodeOp::LeaveScope => scope_depth = scope_depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(ScopeMetadata {
        labels,
        label_scope_depths,
        instruction_scope_depths,
    })
}

fn operand(
    instruction: &BytecodeInstruction,
    index: usize,
) -> Result<&BytecodeOperand, ExecuteError> {
    instruction
        .operands
        .get(index)
        .ok_or_else(|| ExecuteError::MissingOperand {
            op: instruction.op.mnemonic(),
            index,
        })
}

fn register(instruction: &BytecodeInstruction, index: usize) -> Result<u32, ExecuteError> {
    match operand(instruction, index)? {
        BytecodeOperand::Register(index) => Ok(*index),
        _ => Err(ExecuteError::InvalidOperand("register")),
    }
}

fn count_operand(instruction: &BytecodeInstruction, index: usize) -> Result<u32, ExecuteError> {
    match operand(instruction, index)? {
        BytecodeOperand::Count(value) => Ok(*value),
        _ => Err(ExecuteError::InvalidOperand("count")),
    }
}

fn constant_value(module: &BytecodeModule, index: u32) -> Result<Value, ExecuteError> {
    match module.constants.get(index as usize) {
        Some(BytecodeConstant::Number(value)) => Ok(Value::Number(*value)),
        Some(BytecodeConstant::String(value)) => Ok(Value::String(value.clone())),
        Some(BytecodeConstant::Bool(value)) => Ok(Value::Bool(*value)),
        Some(BytecodeConstant::Null) => Ok(Value::Null),
        Some(BytecodeConstant::Undefined) => Ok(Value::Undefined),
        None => Err(ExecuteError::BadConstant(index)),
    }
}

fn constant_string(module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
    match module.constants.get(index as usize) {
        Some(BytecodeConstant::String(value)) => Ok(value.clone()),
        Some(value) => Ok(value.to_string()),
        None => Err(ExecuteError::BadConstant(index)),
    }
}

fn name_string(module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
    module
        .names
        .get(index as usize)
        .cloned()
        .ok_or(ExecuteError::BadConstant(index))
}

fn external_string(module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
    module
        .extern_slots
        .get(index as usize)
        .cloned()
        .ok_or(ExecuteError::BadConstant(index))
}

fn operator_name(operator: u32) -> Option<&'static str> {
    OPERATOR_NAMES.get(operator as usize).copied()
}

const OPERATOR_NAMES: &[&str] = &[
    "+",
    "-",
    "*",
    "/",
    "%",
    "**",
    "<",
    "<=",
    ">",
    ">=",
    "==",
    "===",
    "!=",
    "!==",
    "&&",
    "||",
    "??",
    "&",
    "|",
    "^",
    "<<",
    ">>",
    ">>>",
    "!",
    "~",
    "typeof",
    "void",
    "delete",
    "in",
    "instanceof",
];

fn get_local_member(object: &Value, property: &str) -> Result<Value, ExecuteError> {
    match object {
        Value::Object(props) => match props.get(property).cloned() {
            Some(Value::Function(function)) => {
                Ok(Value::BoundFunction(function, Box::new(object.clone())))
            }
            Some(value) => Ok(value),
            None => Ok(Value::Undefined),
        },
        Value::Class(class) => match class.static_props.get(property).cloned() {
            Some(Value::Function(function)) => {
                Ok(Value::BoundFunction(function, Box::new(object.clone())))
            }
            Some(value) => Ok(value),
            None => Ok(Value::Undefined),
        },
        Value::ExternalRef(reference) => Ok(Value::ExternalRef(reference.member(property))),
        Value::Array(items) if property == "length" => {
            Ok(Value::Number(items.borrow().len() as f64))
        }
        Value::Array(_) if is_array_native_method(property) => Ok(Value::BoundNativeFunction(
            NativeFunctionValue {
                name: format!("Array.{property}"),
            },
            Box::new(object.clone()),
        )),
        Value::Array(items) => Ok(property
            .parse::<usize>()
            .ok()
            .and_then(|index| items.borrow().get(index).cloned())
            .unwrap_or(Value::Undefined)),
        Value::String(value) if property == "length" => {
            Ok(Value::Number(value.chars().count() as f64))
        }
        Value::String(_) if is_string_native_method(property) => Ok(Value::BoundNativeFunction(
            NativeFunctionValue {
                name: format!("String.{property}"),
            },
            Box::new(object.clone()),
        )),
        Value::String(value) => Ok(property
            .parse::<usize>()
            .ok()
            .and_then(|index| value.chars().nth(index))
            .map(|value| Value::String(value.to_string()))
            .unwrap_or(Value::Undefined)),
        Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
            "cannot read property {property:?} of {object}"
        ))),
        _ => Ok(Value::Undefined),
    }
}

fn is_array_native_method(property: &str) -> bool {
    matches!(
        property,
        "push"
            | "fill"
            | "join"
            | "forEach"
            | "map"
            | "filter"
            | "includes"
            | "indexOf"
            | "slice"
            | "concat"
    )
}

fn is_string_native_method(property: &str) -> bool {
    matches!(property, "charAt" | "includes" | "indexOf")
}

fn normalize_index(index: isize, len: isize) -> isize {
    if index < 0 {
        (len + index).clamp(0, len)
    } else {
        index.clamp(0, len)
    }
}

fn property_key(value: &Value) -> String {
    match value {
        Value::Number(value) if value.is_finite() && value.fract() == 0.0 => {
            format!("{}", *value as i64)
        }
        Value::String(value) | Value::Symbol(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".to_string(),
        Value::Undefined => "undefined".to_string(),
        value => value.to_string(),
    }
}

fn set_member(object: &mut Value, property: &str, value: Value) -> Result<(), ExecuteError> {
    match object {
        Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
            "cannot set property {property:?} of {object}"
        ))),
        Value::Object(props) => {
            props.insert(property.to_string(), value);
            Ok(())
        }
        Value::Class(class) => {
            if property == "constructor" {
                match value {
                    Value::Function(function) => {
                        class.constructor = Some(function);
                        Ok(())
                    }
                    Value::Undefined => {
                        class.constructor = None;
                        Ok(())
                    }
                    value => Err(ExecuteError::Runtime(format!(
                        "class constructor must be a function, got {value}"
                    ))),
                }
            } else if let Some(property) = property.strip_prefix("prototype.") {
                class.instance_props.insert(property.to_string(), value);
                Ok(())
            } else {
                class.static_props.insert(property.to_string(), value);
                Ok(())
            }
        }
        Value::Array(items) => {
            if property == "length" {
                if let Value::Number(length) = value {
                    items.borrow_mut().truncate(length.max(0.0) as usize);
                }
                return Ok(());
            }
            let Ok(index) = property.parse::<usize>() else {
                return Ok(());
            };
            let mut items = items.borrow_mut();
            if items.len() <= index {
                items.resize(index + 1, Value::Undefined);
            }
            items[index] = value;
            Ok(())
        }
        Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::ExternalRef(_) => Ok(()),
        Value::Number(_) | Value::String(_) | Value::Bool(_) | Value::Symbol(_) => Ok(()),
        _ => Err(ExecuteError::Runtime(format!(
            "cannot set {property} on {object}"
        ))),
    }
}

pub type HostFunction = fn(&[Value]) -> Result<Value, ExecuteError>;

#[derive(Debug, Clone)]
pub struct HostBridge {
    functions: BTreeMap<String, HostFunction>,
    #[cfg(target_arch = "wasm32")]
    values: Rc<RefCell<Vec<JsValue>>>,
}

impl Default for HostBridge {
    fn default() -> Self {
        Self::with_default_capabilities()
    }
}

impl HostBridge {
    pub fn empty() -> Self {
        Self {
            functions: BTreeMap::new(),
            #[cfg(target_arch = "wasm32")]
            values: Rc::new(RefCell::new(Vec::new())),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn from_js_values(values: Vec<JsValue>) -> Self {
        Self {
            functions: BTreeMap::new(),
            values: Rc::new(RefCell::new(values)),
        }
    }

    pub fn with_default_capabilities() -> Self {
        let mut bridge = Self::empty();
        bridge.register_function("console.log", host_noop);
        bridge.register_function("console.info", host_noop);
        bridge.register_function("console.warn", host_noop);
        bridge.register_function("console.error", host_noop);
        bridge.register_function("console.debug", host_noop);
        bridge.register_function("console.assert", host_noop);
        bridge.register_function("console.clear", host_noop);
        bridge.register_function("console.count", host_noop);
        bridge.register_function("console.countReset", host_noop);
        bridge.register_function("console.dir", host_noop);
        bridge.register_function("console.dirxml", host_noop);
        bridge.register_function("console.group", host_noop);
        bridge.register_function("console.groupCollapsed", host_noop);
        bridge.register_function("console.groupEnd", host_noop);
        bridge.register_function("console.profile", host_noop);
        bridge.register_function("console.profileEnd", host_noop);
        bridge.register_function("console.table", host_noop);
        bridge.register_function("console.time", host_noop);
        bridge.register_function("console.timeEnd", host_noop);
        bridge.register_function("console.timeLog", host_noop);
        bridge.register_function("console.trace", host_noop);
        bridge.register_function("fetch", host_noop);
        bridge.register_function("gc", host_noop);
        bridge.register_function("gcparam", host_noop);
        bridge.register_function("uneval", host_string);
        bridge.register_function("__vmPrint", host_noop);
        bridge.register_function("print", host_noop);
        bridge.register_function("alert", host_noop);
        bridge.register_function("fail", host_noop);
        bridge.register_function("failWithMessage", host_noop);
        bridge.register_function("triggerAssertFalse", host_noop);
        bridge.register_function("quit", host_noop);
        bridge.register_function("Symbol", host_symbol);
        bridge.register_function("Symbol.for", host_symbol_for);
        bridge.register_function("Object", host_object);
        bridge.register_function("Object.freeze", host_first_arg);
        bridge.register_function("Object.seal", host_first_arg);
        bridge.register_function("Object.preventExtensions", host_first_arg);
        bridge.register_function("Object.keys", host_object_keys);
        bridge.register_function("Object.values", host_object_values);
        bridge.register_function("Object.entries", host_object_entries);
        bridge.register_function("Object.hasOwn", host_object_has_own);
        bridge.register_function("Object.create", host_object_create);
        bridge.register_function("Object.assign", host_object_assign);
        bridge.register_function("Object.defineProperty", host_object_define_property);
        bridge.register_function("Object.isFrozen", host_false);
        bridge.register_function("Object.isSealed", host_false);
        bridge.register_function("Object.isExtensible", host_true);
        bridge.register_function(
            "Object.getOwnPropertyDescriptor",
            host_object_get_descriptor,
        );
        bridge.register_function("Object.getOwnPropertyNames", host_object_keys);
        bridge.register_function("Object.getPrototypeOf", host_noop);
        bridge.register_function("Array", host_array);
        bridge.register_function("Array.isArray", host_array_is_array);
        bridge.register_function("String", host_string);
        bridge.register_function("Number", host_number);
        bridge.register_function("Boolean", host_boolean);
        bridge.register_function("Math.random", host_math_random);
        bridge
    }

    pub fn register_function(&mut self, path: impl Into<String>, function: HostFunction) {
        self.functions
            .insert(Self::normalize_path(&path.into()), function);
    }

    pub fn has_function(&self, path: &str) -> bool {
        self.functions.contains_key(&Self::normalize_path(path))
    }

    fn external_count(&self) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            return self.values.borrow().len();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            return 0;
        }
    }

    fn validate_extern_count(&self, expected: usize) -> Result<(), ExecuteError> {
        #[cfg(target_arch = "wasm32")]
        {
            let actual = self.values.borrow().len();
            if actual != expected {
                return Err(ExecuteError::Runtime(format!(
                    "external slot count mismatch: expected {expected}, got {actual}"
                )));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = expected;
        Ok(())
    }

    fn validate_slot(&self, slot: u32) -> Result<(), ExecuteError> {
        #[cfg(target_arch = "wasm32")]
        {
            if self.values.borrow().get(slot as usize).is_none() {
                return Err(ExecuteError::Runtime(format!(
                    "external slot {slot} is not available"
                )));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = slot;
        Ok(())
    }

    fn get(&self, reference: &ExternalRefValue, property: &str) -> Result<Value, ExecuteError> {
        #[cfg(target_arch = "wasm32")]
        {
            let next = reference.member(property);
            let value = self.resolve_js_value(&next)?;
            if is_js_value_convertible(&value) {
                js_value_to_value(value, self)
            } else {
                Ok(Value::ExternalRef(next))
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(Value::ExternalRef(reference.member(property)))
        }
    }

    fn set(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        value: &Value,
    ) -> Result<(), ExecuteError> {
        #[cfg(target_arch = "wasm32")]
        {
            let target = self.resolve_js_value(reference)?;
            Reflect::set(
                &target,
                &JsValue::from_str(property),
                &value_to_js_value(value, self)?,
            )
            .map_err(js_error)?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (reference, property, value);
        Ok(())
    }

    fn set_slot(&self, slot: u32, value: &Value) -> Result<(), ExecuteError> {
        #[cfg(target_arch = "wasm32")]
        {
            let next_value = value_to_js_value(value, self)?;
            let mut values = self.values.borrow_mut();
            let Some(target) = values.get_mut(slot as usize) else {
                return Err(ExecuteError::Runtime(format!(
                    "external slot {slot} is not available"
                )));
            };
            *target = next_value;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (slot, value);
        Ok(())
    }

    fn call(&self, reference: &ExternalRefValue, args: Vec<Value>) -> Result<Value, ExecuteError> {
        #[cfg(target_arch = "wasm32")]
        {
            let (callee, this_value) = self.resolve_js_callable(reference)?;
            let Some(function) = callee.dyn_ref::<JsFunction>() else {
                return Err(ExecuteError::TypeError(format!(
                    "{} is not callable",
                    reference.display_path()
                )));
            };
            let js_args = values_to_js_array(&args, self)?;
            let result = function.apply(&this_value, &js_args).map_err(js_error)?;
            return self.store_or_convert_js_value(result, reference.display_path());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = reference.display_path();
            let Some(function) = self.functions.get(&Self::normalize_path(&path)) else {
                return Err(ExecuteError::Runtime(format!(
                    "external function {path} is not registered"
                )));
            };
            function(&args)
        }
    }

    fn construct(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            return construct_external(&reference.display_path(), args);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let constructor = self.resolve_js_value(reference)?;
            if is_js_proxy_constructor(&constructor) {
                return Ok(args
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| Value::Object(BTreeMap::new())));
            }
            let Some(constructor) = constructor.dyn_ref::<JsFunction>() else {
                return Err(ExecuteError::TypeError(format!(
                    "{} is not constructable",
                    reference.display_path()
                )));
            };
            let js_args = values_to_js_array(&args, self)?;
            let result = Reflect::construct(constructor, &js_args).map_err(js_error)?;
            return self.store_or_convert_js_value(result, reference.display_path());
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn resolve_js_value(&self, reference: &ExternalRefValue) -> Result<JsValue, ExecuteError> {
        self.validate_slot(reference.slot)?;
        let mut value = self.values.borrow()[reference.slot as usize].clone();
        for property in &reference.path {
            value = Reflect::get(&value, &JsValue::from_str(property)).map_err(js_error)?;
        }
        Ok(value)
    }

    #[cfg(target_arch = "wasm32")]
    fn resolve_js_callable(
        &self,
        reference: &ExternalRefValue,
    ) -> Result<(JsValue, JsValue), ExecuteError> {
        self.validate_slot(reference.slot)?;
        let mut this_value = JsValue::UNDEFINED;
        let mut value = self.values.borrow()[reference.slot as usize].clone();
        if reference.path.is_empty() {
            return Ok((value, this_value));
        }
        for property in &reference.path[..reference.path.len() - 1] {
            value = Reflect::get(&value, &JsValue::from_str(property)).map_err(js_error)?;
        }
        this_value = value.clone();
        let property = reference.path.last().expect("checked non-empty path");
        let callee = Reflect::get(&this_value, &JsValue::from_str(property)).map_err(js_error)?;
        Ok((callee, this_value))
    }

    #[cfg(target_arch = "wasm32")]
    fn store_or_convert_js_value(
        &self,
        value: JsValue,
        root_hint: String,
    ) -> Result<Value, ExecuteError> {
        if is_js_value_convertible(&value) {
            return js_value_to_value(value, self);
        }
        let mut values = self.values.borrow_mut();
        let slot = values.len() as u32;
        values.push(value);
        Ok(Value::ExternalRef(ExternalRefValue::new(
            slot,
            format!("{root_hint}#result"),
        )))
    }

    fn normalize_path(path: &str) -> String {
        path.strip_prefix("window.")
            .or_else(|| path.strip_prefix("globalThis."))
            .unwrap_or(path)
            .to_string()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn construct_external(path: &str, args: Vec<Value>) -> Result<Value, ExecuteError> {
    match HostBridge::normalize_path(path).as_str() {
        "Proxy" => Ok(args
            .into_iter()
            .next()
            .unwrap_or_else(|| Value::Object(BTreeMap::new()))),
        "Array" => host_array(&args),
        "Object" => host_object(&args),
        "String" => host_string(&args),
        "Number" => host_number(&args),
        "Boolean" => host_boolean(&args),
        _ => Err(ExecuteError::Runtime(format!(
            "external constructor {path} is not registered"
        ))),
    }
}

fn host_noop(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Undefined)
}

fn host_first_arg(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(args.first().cloned().unwrap_or(Value::Undefined))
}

fn host_true(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(true))
}

fn host_false(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(false))
}

fn host_symbol(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Symbol(
        args.first().map(ToString::to_string).unwrap_or_default(),
    ))
}

fn host_symbol_for(args: &[Value]) -> Result<Value, ExecuteError> {
    host_symbol(args)
}

fn host_object(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(match args.first() {
        Some(Value::Null | Value::Undefined) | None => Value::Object(BTreeMap::new()),
        Some(value) => value.clone(),
    })
}

fn host_array(args: &[Value]) -> Result<Value, ExecuteError> {
    if args.len() == 1 {
        if let Value::Number(length) = args[0] {
            let length = length.max(0.0).trunc() as usize;
            return Ok(array_value(vec![Value::Undefined; length]));
        }
    }
    Ok(array_value(args.to_vec()))
}

fn host_array_is_array(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(matches!(args.first(), Some(Value::Array(_)))))
}

fn host_string(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::String(
        args.first().map(ToString::to_string).unwrap_or_default(),
    ))
}

fn host_number(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Number(
        args.first().map(Value::to_number).unwrap_or(0.0),
    ))
}

fn host_boolean(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(
        args.first().map(Value::is_truthy).unwrap_or(false),
    ))
}

fn host_math_random(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Number(0.5))
}

fn host_object_keys(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(array_value(
        object_keys(args.first())
            .into_iter()
            .map(Value::String)
            .collect(),
    ))
}

fn host_object_values(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(array_value(match args.first() {
        Some(Value::Object(props)) => props.values().cloned().collect(),
        Some(Value::Array(items)) => items.borrow().clone(),
        _ => Vec::new(),
    }))
}

fn host_object_entries(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(array_value(match args.first() {
        Some(Value::Object(props)) => props
            .iter()
            .map(|(key, value)| array_value(vec![Value::String(key.clone()), value.clone()]))
            .collect(),
        Some(Value::Array(items)) => items
            .borrow()
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, value)| array_value(vec![Value::String(index.to_string()), value]))
            .collect(),
        _ => Vec::new(),
    }))
}

fn host_object_has_own(args: &[Value]) -> Result<Value, ExecuteError> {
    let key = args.get(1).map(ToString::to_string).unwrap_or_default();
    Ok(Value::Bool(match args.first() {
        Some(Value::Object(props)) => props.contains_key(&key),
        Some(Value::Array(items)) => key
            .parse::<usize>()
            .is_ok_and(|index| index < items.borrow().len()),
        _ => false,
    }))
}

fn host_object_create(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Object(BTreeMap::new()))
}

fn host_object_assign(args: &[Value]) -> Result<Value, ExecuteError> {
    let mut target = match args.first().cloned() {
        Some(Value::Object(props)) => props,
        _ => BTreeMap::new(),
    };
    for source in args.iter().skip(1) {
        if let Value::Object(props) = source {
            target.extend(props.clone());
        }
    }
    Ok(Value::Object(target))
}

fn host_object_define_property(args: &[Value]) -> Result<Value, ExecuteError> {
    let target = args
        .first()
        .cloned()
        .unwrap_or(Value::Object(BTreeMap::new()));
    let key = args.get(1).map(ToString::to_string).unwrap_or_default();
    let descriptor = args.get(2).cloned().unwrap_or(Value::Undefined);
    let value = match descriptor {
        Value::Object(props) => props.get("value").cloned().unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    };
    Ok(match target {
        Value::Object(mut props) => {
            props.insert(key, value);
            Value::Object(props)
        }
        value => value,
    })
}

fn host_object_get_descriptor(args: &[Value]) -> Result<Value, ExecuteError> {
    let key = args.get(1).map(ToString::to_string).unwrap_or_default();
    let value = match args.first() {
        Some(Value::Object(props)) => props.get(&key).cloned(),
        Some(Value::Array(items)) => key
            .parse::<usize>()
            .ok()
            .and_then(|index| items.borrow().get(index).cloned()),
        _ => None,
    };
    Ok(value
        .map(|value| {
            let mut props = BTreeMap::new();
            props.insert("value".to_string(), value);
            Value::Object(props)
        })
        .unwrap_or(Value::Undefined))
}

fn object_keys(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Object(props)) => props.keys().cloned().collect(),
        Some(Value::Array(items)) => (0..items.borrow().len())
            .map(|index| index.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(target_arch = "wasm32")]
fn values_to_js_array(values: &[Value], bridge: &HostBridge) -> Result<JsArray, ExecuteError> {
    let array = JsArray::new();
    for value in values {
        array.push(&value_to_js_value(value, bridge)?);
    }
    Ok(array)
}

#[cfg(target_arch = "wasm32")]
fn value_to_js_value(value: &Value, bridge: &HostBridge) -> Result<JsValue, ExecuteError> {
    Ok(match value {
        Value::Number(value) => JsValue::from_f64(*value),
        Value::String(value) | Value::Symbol(value) => JsValue::from_str(value),
        Value::Bool(value) => JsValue::from_bool(*value),
        Value::Null => JsValue::NULL,
        Value::Undefined => JsValue::UNDEFINED,
        Value::Array(items) => {
            let array = JsArray::new();
            for item in items.borrow().iter() {
                array.push(&value_to_js_value(item, bridge)?);
            }
            array.into()
        }
        Value::Object(props) => {
            let object = js_sys::Object::new();
            for (key, value) in props {
                Reflect::set(
                    &object,
                    &JsValue::from_str(key),
                    &value_to_js_value(value, bridge)?,
                )
                .map_err(js_error)?;
            }
            object.into()
        }
        Value::ExternalRef(reference) => bridge.resolve_js_value(reference)?,
        Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::Class(_)
        | Value::Module(_) => JsValue::UNDEFINED,
    })
}

#[cfg(target_arch = "wasm32")]
fn js_value_to_value(value: JsValue, bridge: &HostBridge) -> Result<Value, ExecuteError> {
    if value.is_undefined() {
        Ok(Value::Undefined)
    } else if value.is_null() {
        Ok(Value::Null)
    } else if let Some(value) = value.as_bool() {
        Ok(Value::Bool(value))
    } else if let Some(value) = value.as_f64() {
        Ok(Value::Number(value))
    } else if let Some(value) = value.as_string() {
        Ok(Value::String(value))
    } else if JsArray::is_array(&value) {
        let array = JsArray::from(&value);
        let mut items = Vec::with_capacity(array.length() as usize);
        for item in array.iter() {
            if is_js_value_convertible(&item) {
                items.push(js_value_to_value(item, bridge)?);
            } else {
                items.push(bridge.store_or_convert_js_value(item, "host-array-item".to_string())?);
            }
        }
        Ok(array_value(items))
    } else {
        Ok(Value::Undefined)
    }
}

#[cfg(target_arch = "wasm32")]
fn is_js_value_convertible(value: &JsValue) -> bool {
    value.is_undefined()
        || value.is_null()
        || value.as_bool().is_some()
        || value.as_f64().is_some()
        || value.as_string().is_some()
        || JsArray::is_array(value)
}

#[cfg(target_arch = "wasm32")]
fn is_js_proxy_constructor(value: &JsValue) -> bool {
    Reflect::get(&js_sys::global(), &JsValue::from_str("Proxy"))
        .ok()
        .is_some_and(|proxy| js_sys::Object::is(value, &proxy))
}

#[cfg(target_arch = "wasm32")]
fn js_error(value: JsValue) -> ExecuteError {
    let message = value
        .as_string()
        .or_else(|| {
            Reflect::get(&value, &JsValue::from_str("stack"))
                .ok()
                .and_then(|value| value.as_string())
        })
        .or_else(|| {
            Reflect::get(&value, &JsValue::from_str("message"))
                .ok()
                .and_then(|value| value.as_string())
        })
        .unwrap_or_else(|| "host bridge JavaScript error".to_string());
    ExecuteError::Runtime(message)
}

fn binary(op: &str, left: Value, right: Value) -> Result<Value, ExecuteError> {
    match op {
        "+" => match (left, right) {
            (Value::String(left), right) => Ok(Value::String(format!("{left}{right}"))),
            (left, Value::String(right)) => Ok(Value::String(format!("{left}{right}"))),
            (left, right) => Ok(Value::Number(left.to_number() + right.to_number())),
        },
        "-" => Ok(Value::Number(left.to_number() - right.to_number())),
        "*" => Ok(Value::Number(left.to_number() * right.to_number())),
        "/" => Ok(Value::Number(left.to_number() / right.to_number())),
        "%" => Ok(Value::Number(left.to_number() % right.to_number())),
        "==" => Ok(Value::Bool(loose_eq(&left, &right))),
        "!=" => Ok(Value::Bool(!loose_eq(&left, &right))),
        "===" => Ok(Value::Bool(left == right)),
        "!==" => Ok(Value::Bool(left != right)),
        "<" => Ok(Value::Bool(left.to_number() < right.to_number())),
        "<=" => Ok(Value::Bool(left.to_number() <= right.to_number())),
        ">" => Ok(Value::Bool(left.to_number() > right.to_number())),
        ">=" => Ok(Value::Bool(left.to_number() >= right.to_number())),
        "&&" => Ok(if left.is_truthy() { right } else { left }),
        "||" => Ok(if left.is_truthy() { left } else { right }),
        "??" => Ok(match left {
            Value::Null | Value::Undefined => right,
            value => value,
        }),
        _ => Err(ExecuteError::Unsupported("BINARY")),
    }
}

fn loose_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,
        (Value::Number(left), Value::String(right)) => {
            right.parse::<f64>().is_ok_and(|right| *left == right)
        }
        (Value::String(left), Value::Number(right)) => {
            left.parse::<f64>().is_ok_and(|left| left == *right)
        }
        (Value::Bool(left), right) => Value::Number(f64::from(*left as u8)) == *right,
        (left, Value::Bool(right)) => *left == Value::Number(f64::from(*right as u8)),
        _ => left == right,
    }
}

fn unary(op: &str, arg: Value) -> Result<Value, ExecuteError> {
    match op {
        "-" => Ok(Value::Number(-arg.to_number())),
        "+" => Ok(Value::Number(arg.to_number())),
        "!" => Ok(Value::Bool(!arg.is_truthy())),
        "~" => Ok(Value::Number((!(arg.to_number() as i32)) as f64)),
        "delete" => Ok(Value::Bool(true)),
        "void" => Ok(Value::Undefined),
        "typeof" => Ok(Value::String(
            match arg {
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Symbol(_) => "symbol",
                Value::Bool(_) => "boolean",
                Value::Function(_)
                | Value::BoundFunction(_, _)
                | Value::NativeFunction(_)
                | Value::BoundNativeFunction(_, _) => "function",
                Value::Null => "object",
                Value::Array(_)
                | Value::Object(_)
                | Value::Class(_)
                | Value::Module(_)
                | Value::ExternalRef(_) => "object",
                Value::Undefined => "undefined",
            }
            .to_string(),
        )),
        _ => Err(ExecuteError::Unsupported("UNARY")),
    }
}

#[cfg(test)]
mod tests {
    use super::{Executor, HostBridge};
    use js_token_core::{
        BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand,
    };

    fn call_host_module(root: &str, member: &str, args: Vec<BytecodeOperand>) -> BytecodeModule {
        let mut operands = vec![
            BytecodeOperand::Register(2),
            BytecodeOperand::Register(1),
            BytecodeOperand::Count(args.len() as u32),
        ];
        operands.extend(args);

        BytecodeModule {
            extern_slots: vec![root.to_string()],
            names: Vec::new(),
            functions: Vec::new(),
            constants: vec![
                BytecodeConstant::String(member.to_string()),
                BytecodeConstant::Number(2.0),
            ],
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::LoadName,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::External(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Member,
                    operands: vec![
                        BytecodeOperand::Register(1),
                        BytecodeOperand::Register(0),
                        BytecodeOperand::Constant(0),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Call,
                    operands,
                },
                BytecodeInstruction {
                    op: BytecodeOp::Pop,
                    operands: vec![BytecodeOperand::Register(2)],
                },
            ],
        }
    }

    #[test]
    fn host_bridge_rejects_unregistered_capability() {
        let module = call_host_module("host", "missing", Vec::new());
        let err = Executor::run_with_host_bridge(&module, HostBridge::empty()).unwrap_err();

        assert_eq!(
            err.to_string(),
            "external function host.missing is not registered"
        );
    }
}
