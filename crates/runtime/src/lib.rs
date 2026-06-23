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
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = __jsVmHostLog)]
    fn wasm_host_log(level: &str, value: &str);
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn js_execute_bytes_with_seed(bytes: &[u8], seed: &str) -> Result<String, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    Executor::run(&module)
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
        executor.inject_externals(module.extern_slots.iter().cloned());
        executor.inject_externals(externals.iter().cloned());
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
        let mut executor = Self::with_host_bridge(host_bridge);
        executor.inject_externals(module.extern_slots.iter().cloned());
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

    fn inject_externals(&mut self, externals: impl IntoIterator<Item = String>) {
        for name in externals {
            self.lexical_env
                .define_global_if_absent(name.clone(), Value::ExternalRef(name));
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
                    let name = self.read_name(module, operand(instruction, 1)?)?;
                    let value = self.get_name(&name);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreName => {
                    let name = self.read_name(module, operand(instruction, 0)?)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    self.lexical_env.set_or_define_current(name, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreMember => {
                    let object_operand = operand(instruction, 0)?;
                    let mut object = self.read_value(module, object_operand)?;
                    let property_value = self.read_value(module, operand(instruction, 1)?)?;
                    let property = property_key(&property_value);
                    let value = self.read_value(module, operand(instruction, 2)?)?;
                    set_member(&mut object, &property, value.clone())?;
                    self.write_operand_target(module, object_operand, object)?;
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
                    let value = get_member(&object, &property)?;
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
                    let flow =
                        match self.execute_range(module, parts.body_start, parts.body_end) {
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
            Value::ExternalRef(path) => self.host_bridge.call(&path, args),
            Value::Class(class) => Ok(Value::Object(class.static_props)),
            _ => Err(ExecuteError::TypeError(format!("{callee} is not callable"))),
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
            Value::ExternalRef(path) => construct_external(&path, args),
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
                    let start = args
                        .first()
                        .map(Value::to_number)
                        .unwrap_or(0.0)
                        .trunc() as isize;
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
                let name = external_string(module, *index)?;
                Ok(self.get_name(&name))
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
            BytecodeOperand::External(index) => external_string(module, *index),
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
                self.lexical_env
                    .set_or_define_current(external_string(module, *index)?, value);
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
    ExternalRef(String),
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
            Value::ExternalRef(name) => write!(f, "[external {name}]"),
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

fn get_member(object: &Value, property: &str) -> Result<Value, ExecuteError> {
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
        Value::ExternalRef(path) => {
            let normalized = HostBridge::normalize_path(path);
            if normalized == "Symbol" {
                return Ok(Value::Symbol(format!("Symbol.{property}")));
            }
            Ok(HostBridge::get(path, property))
        }
        Value::Array(items) if property == "length" => Ok(Value::Number(items.borrow().len() as f64)),
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
        }
    }

    pub fn with_default_capabilities() -> Self {
        let mut bridge = Self::empty();
        bridge.register_function("console.log", host_console_log);
        bridge.register_function("console.info", host_console_info);
        bridge.register_function("console.warn", host_console_warn);
        bridge.register_function("console.error", host_console_error);
        bridge.register_function("console.debug", host_console_debug);
        bridge.register_function("fetch", host_fetch);
        bridge.register_function("gc", host_noop);
        bridge.register_function("gcparam", host_noop);
        bridge.register_function("uneval", host_uneval);
        bridge.register_function("print", host_print);
        bridge.register_function("alert", host_print);
        bridge.register_function("fail", host_noop);
        bridge.register_function("failWithMessage", host_noop);
        bridge.register_function("triggerAssertFalse", host_noop);
        bridge.register_function("quit", host_noop);
        bridge.register_function("Symbol", host_symbol);
        bridge.register_function("Symbol.for", host_symbol_for);
        bridge.register_function("Object.freeze", host_object_freeze);
        bridge.register_function("Object.seal", host_object_freeze);
        bridge.register_function("Object.preventExtensions", host_object_freeze);
        bridge.register_function("Object.keys", host_object_keys);
        bridge.register_function("Object.values", host_object_values);
        bridge.register_function("Object.entries", host_object_entries);
        bridge.register_function("Object.hasOwn", host_object_has_own);
        bridge.register_function("Object.create", host_object_create);
        bridge.register_function("Object.assign", host_object_assign);
        bridge.register_function("Object.defineProperty", host_object_define_property);
        bridge.register_function("Object.isFrozen", host_object_false_predicate);
        bridge.register_function("Object.isSealed", host_object_false_predicate);
        bridge.register_function("Object.isExtensible", host_object_true_predicate);
        bridge.register_function(
            "Object.getOwnPropertyDescriptor",
            host_object_get_own_property_descriptor,
        );
        bridge.register_function("Object.getOwnPropertyNames", host_object_get_own_property_names);
        bridge.register_function("Object.getPrototypeOf", host_object_get_prototype_of);
        bridge.register_function("Object", host_object);
        bridge.register_function("Array", host_array);
        bridge.register_function("Array.isArray", host_array_is_array);
        bridge.register_function("String", host_string);
        bridge.register_function("Number", host_number);
        bridge.register_function("Boolean", host_boolean);
        bridge.register_function("Math.random", host_math_random);
        bridge
    }

    pub fn register_function(&mut self, path: impl Into<String>, function: HostFunction) {
        let path = Self::normalize_path(&path.into());
        self.functions.insert(path, function);
    }

    pub fn has_function(&self, path: &str) -> bool {
        self.functions.contains_key(&Self::normalize_path(path))
    }

    fn get(path: &str, property: &str) -> Value {
        Value::ExternalRef(format!("{path}.{property}"))
    }

    fn call(&self, path: &str, args: Vec<Value>) -> Result<Value, ExecuteError> {
        let normalized = Self::normalize_path(path);
        let Some(function) = self.functions.get(&normalized) else {
            return Err(ExecuteError::Runtime(format!(
                "external function {path} is not registered"
            )));
        };
        function(&args)
    }

    fn normalize_path(path: &str) -> String {
        path.strip_prefix("window.")
            .or_else(|| path.strip_prefix("globalThis."))
            .unwrap_or(path)
            .to_string()
    }
}

fn host_console_log(args: &[Value]) -> Result<Value, ExecuteError> {
    host_console("log", &host_message(args));
    Ok(Value::Undefined)
}

fn host_console_info(args: &[Value]) -> Result<Value, ExecuteError> {
    host_console("info", &host_message(args));
    Ok(Value::Undefined)
}

fn host_console_warn(args: &[Value]) -> Result<Value, ExecuteError> {
    host_console("warn", &host_message(args));
    Ok(Value::Undefined)
}

fn host_console_error(args: &[Value]) -> Result<Value, ExecuteError> {
    host_console("error", &host_message(args));
    Ok(Value::Undefined)
}

fn host_console_debug(args: &[Value]) -> Result<Value, ExecuteError> {
    host_console("debug", &host_message(args));
    Ok(Value::Undefined)
}

fn host_fetch(args: &[Value]) -> Result<Value, ExecuteError> {
    let target = args
        .first()
        .map(ToString::to_string)
        .unwrap_or_else(|| "undefined".to_string());
    host_console("log", &format!("NETWORK fetch {target}"));
    Ok(Value::Undefined)
}

fn host_noop(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Undefined)
}

fn host_print(args: &[Value]) -> Result<Value, ExecuteError> {
    host_console("log", &host_message(args));
    Ok(Value::Undefined)
}

fn host_uneval(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::String(
        args.first()
            .map(ToString::to_string)
            .unwrap_or_else(|| "undefined".to_string()),
    ))
}

fn host_symbol(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Symbol(symbol_description(args)))
}

fn host_symbol_for(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Symbol(symbol_description(args)))
}

fn host_object_freeze(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(args.first().cloned().unwrap_or(Value::Undefined))
}

fn host_object_keys(args: &[Value]) -> Result<Value, ExecuteError> {
    let object = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(array_value(
        enumerable_property_names(&object)
            .into_iter()
            .map(Value::String)
            .collect(),
    ))
}

fn host_object_values(args: &[Value]) -> Result<Value, ExecuteError> {
    let object = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(array_value(
        enumerable_property_names(&object)
            .into_iter()
            .filter_map(|property| own_property_value(&object, &property))
            .collect(),
    ))
}

fn host_object_entries(args: &[Value]) -> Result<Value, ExecuteError> {
    let object = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(array_value(
        enumerable_property_names(&object)
            .into_iter()
            .filter_map(|property| {
                own_property_value(&object, &property)
                    .map(|value| array_value(vec![Value::String(property), value]))
            })
            .collect(),
    ))
}

fn host_object_has_own(args: &[Value]) -> Result<Value, ExecuteError> {
    let object = args.first().cloned().unwrap_or(Value::Undefined);
    let property = args.get(1).map(ToString::to_string).unwrap_or_default();
    Ok(Value::Bool(own_property_value(&object, &property).is_some()))
}

fn host_object_create(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Object(BTreeMap::new()))
}

fn host_object_assign(args: &[Value]) -> Result<Value, ExecuteError> {
    let mut target = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::Object(BTreeMap::new()));
    for source in args.iter().skip(1) {
        for property in enumerable_property_names(source) {
            if let Some(value) = own_property_value(source, &property) {
                set_member(&mut target, &property, value)?;
            }
        }
    }
    Ok(target)
}

fn host_object_define_property(args: &[Value]) -> Result<Value, ExecuteError> {
    let mut target = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::Object(BTreeMap::new()));
    let property = args.get(1).map(ToString::to_string).unwrap_or_default();
    let descriptor = args.get(2).cloned().unwrap_or(Value::Undefined);
    let value = match descriptor {
        Value::Object(props) => props.get("value").cloned().unwrap_or(Value::Undefined),
        value => value,
    };
    if !property.is_empty() {
        set_member(&mut target, &property, value)?;
    }
    Ok(target)
}

fn host_object_true_predicate(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(true))
}

fn host_object_false_predicate(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(false))
}

fn host_object_get_own_property_descriptor(args: &[Value]) -> Result<Value, ExecuteError> {
    let object = args.first().cloned().unwrap_or(Value::Undefined);
    let property = args.get(1).map(ToString::to_string).unwrap_or_default();
    Ok(match own_property_value(&object, &property) {
        Some(value) => Value::Object(BTreeMap::from([
            ("value".to_string(), value),
            ("writable".to_string(), Value::Bool(true)),
            ("enumerable".to_string(), Value::Bool(true)),
            ("configurable".to_string(), Value::Bool(true)),
        ])),
        None => Value::Undefined,
    })
}

fn host_object_get_own_property_names(args: &[Value]) -> Result<Value, ExecuteError> {
    let object = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(array_value(
        own_property_names(&object)
            .into_iter()
            .map(Value::String)
            .collect(),
    ))
}

fn host_object_get_prototype_of(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Null)
}

fn host_object(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(match args.first() {
        Some(Value::Null | Value::Undefined) | None => Value::Object(BTreeMap::new()),
        Some(value) => value.clone(),
    })
}

fn host_array(args: &[Value]) -> Result<Value, ExecuteError> {
    const MAX_HOST_ARRAY_LENGTH: usize = 65_536;

    if args.len() == 1 {
        if let Value::Number(length) = args[0] {
            if length.is_finite() && length >= 0.0 && length.fract() == 0.0 {
                let length = length as usize;
                if length > MAX_HOST_ARRAY_LENGTH {
                    return Err(ExecuteError::RangeError(format!(
                        "invalid array length {length}"
                    )));
                }
                return Ok(array_value(vec![Value::Undefined; length]));
            }
        }
    }

    Ok(array_value(args.to_vec()))
}

fn host_array_is_array(args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Bool(matches!(args.first(), Some(Value::Array(_)))))
}

fn host_math_random(_args: &[Value]) -> Result<Value, ExecuteError> {
    Ok(Value::Number(0.5))
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

fn construct_external(path: &str, args: Vec<Value>) -> Result<Value, ExecuteError> {
    match HostBridge::normalize_path(path).as_str() {
        "Proxy" => Ok(args.into_iter().next().unwrap_or(Value::Object(BTreeMap::new()))),
        "Symbol" => Ok(Value::Symbol(symbol_description(&args))),
        "String" => Ok(Value::String(
            args.first().map(ToString::to_string).unwrap_or_default(),
        )),
        "Number" => Ok(Value::Number(
            args.first().map(Value::to_number).unwrap_or(0.0),
        )),
        "Boolean" => Ok(Value::Bool(
            args.first().map(Value::is_truthy).unwrap_or(false),
        )),
        "Object" => Ok(args.into_iter().next().unwrap_or(Value::Object(BTreeMap::new()))),
        "Array" => Ok(array_value(args)),
        _ => Err(ExecuteError::Runtime(format!(
            "[external {path}] is not constructable"
        ))),
    }
}

fn symbol_description(args: &[Value]) -> String {
    args.first()
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn own_property_value(object: &Value, property: &str) -> Option<Value> {
    match object {
        Value::Object(props) => props.get(property).cloned(),
        Value::Class(class) => class.static_props.get(property).cloned(),
        Value::Array(items) if property == "length" => Some(Value::Number(items.borrow().len() as f64)),
        Value::Array(items) => property
            .parse::<usize>()
            .ok()
            .and_then(|index| items.borrow().get(index).cloned()),
        Value::String(value) if property == "length" => Some(Value::Number(value.len() as f64)),
        Value::String(value) => property
            .parse::<usize>()
            .ok()
            .and_then(|index| value.chars().nth(index))
            .map(|value| Value::String(value.to_string())),
        _ => None,
    }
}

fn enumerable_property_names(object: &Value) -> Vec<String> {
    own_property_names(object)
        .into_iter()
        .filter(|property| property != "length")
        .collect()
}

fn own_property_names(object: &Value) -> Vec<String> {
    match object {
        Value::Object(props) => props.keys().cloned().collect(),
        Value::Class(class) => class.static_props.keys().cloned().collect(),
        Value::Array(items) => (0..items.borrow().len())
            .map(|index| index.to_string())
            .chain(std::iter::once("length".to_string()))
            .collect(),
        Value::String(value) => (0..value.chars().count())
            .map(|index| index.to_string())
            .chain(std::iter::once("length".to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

fn host_message(args: &[Value]) -> String {
    args.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_arch = "wasm32")]
fn host_console(level: &str, message: &str) {
    wasm_host_log(level, message);
}

#[cfg(not(target_arch = "wasm32"))]
fn host_console(level: &str, message: &str) {
    match level {
        "warn" | "error" => eprintln!("{message}"),
        _ => println!("{message}"),
    }
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
    use super::{ExecuteError, Executor, HostBridge, Value};
    use js_token_core::{
        BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand,
    };

    fn custom_answer(args: &[Value]) -> Result<Value, ExecuteError> {
        let offset = args.first().map(Value::to_number).unwrap_or(0.0);
        Ok(Value::Number(40.0 + offset))
    }

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
    fn host_bridge_registers_custom_capability() {
        let module = call_host_module("host", "answer", vec![BytecodeOperand::Constant(1)]);
        let mut bridge = HostBridge::empty();
        bridge.register_function("host.answer", custom_answer);

        let value = Executor::run_with_host_bridge(&module, bridge).unwrap();

        assert_eq!(value, Value::Number(42.0));
    }

    #[test]
    fn host_bridge_defaults_normalize_window_aliases() {
        let bridge = HostBridge::default();

        assert!(bridge.has_function("console.log"));
        assert!(bridge.has_function("window.console.log"));
        assert!(bridge.has_function("globalThis.fetch"));
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
