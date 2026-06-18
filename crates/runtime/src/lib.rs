use js_token_core::{
    BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand,
};
use std::{collections::BTreeMap, fmt};

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

#[derive(Debug, Default)]
pub struct Executor {
    registers: Vec<Value>,
    globals: BTreeMap<String, Value>,
    labels: BTreeMap<String, usize>,
    last_value: Value,
    exports: BTreeMap<String, Value>,
    call_depth: usize,
}

const MAX_CALL_DEPTH: usize = 1024;

impl Executor {
    pub fn run(module: &BytecodeModule) -> Result<Value, ExecuteError> {
        let mut executor = Self::default();
        executor.inject_externals(module.extern_slots.iter().cloned());
        executor.labels = collect_labels(module)?;
        match executor.execute_range(module, 0, module.instructions.len())? {
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
        }
    }

    pub fn run_with_external_names(
        module: &BytecodeModule,
        externals: &[String],
    ) -> Result<Value, ExecuteError> {
        let mut executor = Self::default();
        executor.inject_externals(module.extern_slots.iter().cloned());
        executor.inject_externals(externals.iter().cloned());
        executor.labels = collect_labels(module)?;
        match executor.execute_range(module, 0, module.instructions.len())? {
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
        }
    }

    fn inject_externals(&mut self, externals: impl IntoIterator<Item = String>) {
        for name in externals {
            self.globals
                .entry(name.clone())
                .or_insert(Value::ExternalRef(name));
        }
    }

    fn execute_range(
        &mut self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<Flow, ExecuteError> {
        let mut pc = start;
        while pc < end {
            let instruction = &module.instructions[pc];
            match instruction.op {
                BytecodeOp::Marker | BytecodeOp::Label | BytecodeOp::Declare => {}
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
                    self.globals.insert(name, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreMember => {
                    let object_operand = operand(instruction, 0)?;
                    let mut object = self.read_value(module, object_operand)?;
                    let property = self.read_constant_string(module, operand(instruction, 1)?)?;
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
                    let property = self.read_constant_string(module, operand(instruction, 2)?)?;
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
                    let value = Value::Array(items);
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
                    self.globals.insert(name, Value::Function(function));
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
                    return Ok(Flow::Value(Value::Undefined));
                }
                BytecodeOp::Class => {
                    let value = self.class_from_instruction(module, instruction)?;
                    if let BytecodeOperand::Register(dst) = operand(instruction, 0)? {
                        self.write_register(*dst, value.clone());
                    }
                    if let BytecodeOperand::Name(index) = operand(instruction, 1)? {
                        self.globals
                            .insert(name_string(module, *index)?, value.clone());
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
                    let flow = self.execute_range(module, parts.body_start, parts.body_end)?;
                    let flow = match flow {
                        Flow::Throw(value) if parts.catch_start < parts.catch_end => {
                            if let Some(param) = parts.catch_param {
                                self.globals.insert(param, value);
                            }
                            self.execute_range(module, parts.catch_start, parts.catch_end)?
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
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        Flow::Throw(value) => return Ok(Flow::Throw(value)),
                    }
                    continue;
                }
                BytecodeOp::CatchStart | BytecodeOp::FinallyStart | BytecodeOp::TryEnd => {
                    return Ok(Flow::Value(self.last_value.clone()));
                }
                BytecodeOp::Throw => {
                    let value = self.read_value(module, operand(instruction, 0)?)?;
                    return Ok(Flow::Throw(value));
                }
                BytecodeOp::Return => {
                    let value = self.read_value(module, operand(instruction, 0)?)?;
                    return Ok(Flow::Return(value));
                }
                BytecodeOp::Pop => {
                    self.last_value = self.read_value(module, operand(instruction, 0)?)?;
                }
                BytecodeOp::Jump => {
                    let label = self.read_label(module, operand(instruction, 0)?)?;
                    pc = *self
                        .labels
                        .get(&label)
                        .ok_or_else(|| ExecuteError::UnknownLabel(label))?;
                    continue;
                }
                BytecodeOp::JumpIfFalse => {
                    let test = self.read_value(module, operand(instruction, 0)?)?;
                    if !test.is_truthy() {
                        let label = self.read_label(module, operand(instruction, 1)?)?;
                        pc = *self
                            .labels
                            .get(&label)
                            .ok_or_else(|| ExecuteError::UnknownLabel(label))?;
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
        Ok(Flow::Value(self.last_value.clone()))
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
                self.call_function(module, &function, Value::Undefined, args)
            }
            Value::BoundFunction(function, this_value) => {
                self.call_function(module, &function, *this_value, args)
            }
            Value::ExternalRef(path) => HostBridge::call(&path, args),
            Value::Class(class) => Ok(Value::Object(class.static_props)),
            _ => Err(ExecuteError::Runtime(format!("{callee} is not callable"))),
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
                    let (result, globals) =
                        self.call_function_frame(module, constructor, this_value.clone(), args)?;
                    if !matches!(result, Value::Undefined) {
                        return Ok(result);
                    }
                    this_value = globals.get("this").cloned().unwrap_or(this_value);
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
            _ => Err(ExecuteError::Runtime(format!(
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

    fn call_function_frame(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<(Value, BTreeMap<String, Value>), ExecuteError> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(ExecuteError::Runtime(
                "maximum call stack exceeded".to_string(),
            ));
        }
        let mut frame = Executor {
            registers: Vec::new(),
            globals: function.env.clone(),
            labels: collect_labels(module)?,
            last_value: Value::Undefined,
            exports: self.exports.clone(),
            call_depth: self.call_depth + 1,
        };
        for (name, value) in &self.globals {
            frame
                .globals
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        frame.globals.insert("this".to_string(), this_value);
        for (index, param) in function.params.iter().enumerate() {
            frame.globals.insert(
                param.clone(),
                args.get(index).cloned().unwrap_or(Value::Undefined),
            );
        }
        let flow = frame.execute_range(module, function.body_start, function.body_end)?;
        let globals = frame.globals;
        match flow {
            Flow::Value(_) => Ok((Value::Undefined, globals)),
            Flow::Return(value) => Ok((value, globals)),
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
            env: self.globals.clone(),
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
            | BytecodeOperand::Count(_) => Err(ExecuteError::InvalidOperand("value")),
        }
    }

    fn get_name(&self, name: &str) -> Value {
        self.globals.get(name).cloned().unwrap_or(Value::Undefined)
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
                self.globals.insert(name_string(module, *index)?, value);
                Ok(())
            }
            BytecodeOperand::External(index) => {
                self.globals.insert(external_string(module, *index)?, value);
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

#[derive(Debug, Clone, PartialEq)]
enum Flow {
    Value(Value),
    Return(Value),
    Throw(Value),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    String(String),
    Bool(bool),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    Function(FunctionValue),
    BoundFunction(FunctionValue, Box<Value>),
    ExternalRef(String),
    Class(ClassValue),
    Module(ModuleValue),
    Null,
    #[default]
    Undefined,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionValue {
    pub name: Option<String>,
    pub params: Vec<String>,
    pub has_return: bool,
    pub body_start: usize,
    pub body_end: usize,
    pub env: BTreeMap<String, Value>,
    end_pc: usize,
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

impl Value {
    fn is_truthy(&self) -> bool {
        match self {
            Value::Number(value) => *value != 0.0 && !value.is_nan(),
            Value::String(value) => !value.is_empty(),
            Value::Bool(value) => *value,
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
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
            Value::Bool(value) => f64::from(*value as u8),
            Value::Null => 0.0,
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
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
            Value::Bool(value) => write!(f, "{value}"),
            Value::Array(items) => {
                let items = items
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

fn collect_labels(module: &BytecodeModule) -> Result<BTreeMap<String, usize>, ExecuteError> {
    let mut labels = BTreeMap::new();
    for (index, instruction) in module.instructions.iter().enumerate() {
        if instruction.op == BytecodeOp::Label {
            let label = match operand(instruction, 0)? {
                BytecodeOperand::Label(index) => index.to_string(),
                BytecodeOperand::Constant(index) => constant_string(module, *index)?,
                _ => return Err(ExecuteError::InvalidOperand("label")),
            };
            labels.insert(label, index + 1);
        }
    }
    Ok(labels)
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
        Value::ExternalRef(path) => Ok(HostBridge::get(path, property)),
        Value::Array(items) if property == "length" => Ok(Value::Number(items.len() as f64)),
        Value::Array(items) => Ok(property
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index).cloned())
            .unwrap_or(Value::Undefined)),
        Value::String(value) if property == "length" => {
            Ok(Value::Number(value.chars().count() as f64))
        }
        Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
            "cannot read property {property:?} of {object}"
        ))),
        _ => Ok(Value::Undefined),
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
            let index = property
                .parse::<usize>()
                .map_err(|_| ExecuteError::InvalidOperand("array index"))?;
            if items.len() <= index {
                items.resize(index + 1, Value::Undefined);
            }
            items[index] = value;
            Ok(())
        }
        _ => Err(ExecuteError::Runtime(format!(
            "cannot set {property} on {object}"
        ))),
    }
}

struct HostBridge;

impl HostBridge {
    fn get(path: &str, property: &str) -> Value {
        Value::ExternalRef(format!("{path}.{property}"))
    }

    fn call(path: &str, args: Vec<Value>) -> Result<Value, ExecuteError> {
        let normalized = Self::normalize_path(path);
        match normalized.as_str() {
            "console.log" => {
                host_console("log", &Self::message(&args));
                Ok(Value::Undefined)
            }
            "console.info" => {
                host_console("info", &Self::message(&args));
                Ok(Value::Undefined)
            }
            "console.warn" => {
                host_console("warn", &Self::message(&args));
                Ok(Value::Undefined)
            }
            "console.error" => {
                host_console("error", &Self::message(&args));
                Ok(Value::Undefined)
            }
            "console.debug" => {
                host_console("debug", &Self::message(&args));
                Ok(Value::Undefined)
            }
            "fetch" | "window.fetch" | "globalThis.fetch" => {
                let target = args
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "undefined".to_string());
                host_console("log", &format!("NETWORK fetch {target}"));
                Ok(Value::Undefined)
            }
            _ => Err(ExecuteError::Runtime(format!(
                "external function {path} is not registered"
            ))),
        }
    }

    fn normalize_path(path: &str) -> String {
        path.strip_prefix("window.")
            .or_else(|| path.strip_prefix("globalThis."))
            .unwrap_or(path)
            .to_string()
    }

    fn message(args: &[Value]) -> String {
        args.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    }
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
        "==" | "===" => Ok(Value::Bool(left == right)),
        "!=" | "!==" => Ok(Value::Bool(left != right)),
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

fn unary(op: &str, arg: Value) -> Result<Value, ExecuteError> {
    match op {
        "-" => Ok(Value::Number(-arg.to_number())),
        "+" => Ok(Value::Number(arg.to_number())),
        "!" => Ok(Value::Bool(!arg.is_truthy())),
        "void" => Ok(Value::Undefined),
        "typeof" => Ok(Value::String(
            match arg {
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Bool(_) => "boolean",
                Value::Function(_) | Value::BoundFunction(_, _) => "function",
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
