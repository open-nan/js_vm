use crate::env::LexicalEnv;
use crate::host::HostValue;
use js_token_core::BytecodeOperand;
use std::{cell::RefCell, collections::BTreeMap, fmt, rc::Rc};
use wasm_bindgen::JsValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRefValue {
    pub slot: u32,
    pub root: String,
    pub path: Vec<String>,
}

impl ExternalRefValue {
    pub(crate) fn new(slot: u32, root: impl Into<String>) -> Self {
        Self {
            slot,
            root: root.into(),
            path: Vec::new(),
        }
    }

    pub(crate) fn member(&self, property: &str) -> Self {
        let mut next = self.clone();
        next.path.push(property.to_string());
        next
    }

    pub(crate) fn display_path(&self) -> String {
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
    BigInt(String),
    String(String),
    Symbol(String),
    Bool(bool),
    JsValue(JsValue),
    BoundJsFunction(JsValue, JsValue),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<BTreeMap<String, Value>>>),
    Function(FunctionValue),
    BoundFunction(FunctionValue, Box<Value>),
    NativeFunction(NativeFunctionValue),
    BoundNativeFunction(NativeFunctionValue, Box<Value>),
    ExternalRef(ExternalRefValue),
    Class(ClassValue),
    Module(ModuleValue),
    GeneratorState(Rc<RefCell<GeneratorState>>),
    Null,
    #[default]
    Undefined,
}

#[derive(Debug, Clone)]
pub struct FunctionValue {
    pub name: Option<String>,
    pub params: Vec<BytecodeOperand>,
    pub has_return: bool,
    pub is_generator: bool,
    pub body_start: usize,
    pub body_end: usize,
    pub env: LexicalEnv,
    pub(crate) props: Rc<RefCell<BTreeMap<String, Value>>>,
    pub(crate) end_pc: usize,
}

impl PartialEq for FunctionValue {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.params == other.params
            && self.has_return == other.has_return
            && self.is_generator == other.is_generator
            && self.body_start == other.body_start
            && self.body_end == other.body_end
            && self.end_pc == other.end_pc
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeFunctionValue {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct GeneratorState {
    pub initialized: bool,
    pub done: bool,
    pub pc: usize,
    pub resume_dst: Option<u32>,
    pub registers: Vec<Value>,
    pub lexical_env: LexicalEnv,
    pub last_value: Value,
}

impl PartialEq for GeneratorState {
    fn eq(&self, other: &Self) -> bool {
        self.initialized == other.initialized
            && self.done == other.done
            && self.pc == other.pc
            && self.resume_dst == other.resume_dst
            && self.registers == other.registers
            && self.last_value == other.last_value
    }
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

pub(crate) fn array_value(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
}

pub(crate) fn object_value(props: BTreeMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(props)))
}

impl Value {
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Value::Number(value) => *value != 0.0 && !value.is_nan(),
            Value::BigInt(value) => value != "0",
            Value::String(value) => !value.is_empty(),
            Value::Symbol(_) => true,
            Value::Bool(value) => *value,
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => value.is_truthy(),
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
            | Value::NativeFunction(_)
            | Value::BoundNativeFunction(_, _)
            | Value::ExternalRef(_)
            | Value::Class(_)
            | Value::Module(_)
            | Value::GeneratorState(_) => true,
            Value::Null | Value::Undefined => false,
        }
    }

    pub(crate) fn to_number(&self) -> f64 {
        match self {
            Value::Number(value) => *value,
            Value::BigInt(value) => value.parse().unwrap_or(f64::NAN),
            Value::String(value) => value.parse().unwrap_or(f64::NAN),
            Value::Symbol(_) => f64::NAN,
            Value::Bool(value) => f64::from(*value as u8),
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => value.to_number(),
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
            | Value::GeneratorState(_)
            | Value::Undefined => f64::NAN,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Number(value) => write!(f, "{value}"),
            Value::BigInt(value) => write!(f, "{value}"),
            Value::String(value) => write!(f, "{value}"),
            Value::Symbol(value) => write!(f, "Symbol({value})"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                write!(f, "{}", value.display())
            }
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
            Value::GeneratorState(_) => write!(f, "[generator state]"),
            Value::Null => write!(f, "null"),
            Value::Undefined => write!(f, "undefined"),
        }
    }
}
