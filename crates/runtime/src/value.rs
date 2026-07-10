use crate::env::LexicalEnv;
use crate::host::{
    HostBridge, can_represent_value_as_js, js_value_display, js_value_is_truthy,
    js_value_to_number, value_to_js_value,
};
use js_sys::{Array as JsArray, Object as JsObject, Reflect};
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
    Null,
    #[default]
    Undefined,
}

#[derive(Debug, Clone)]
pub struct FunctionValue {
    pub name: Option<String>,
    pub params: Vec<BytecodeOperand>,
    pub has_return: bool,
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

pub(crate) fn array_value(items: Vec<Value>) -> Value {
    if items.iter().all(can_represent_value_as_js) {
        let array = JsArray::new();
        for item in &items {
            let Ok(value) = value_to_js_value(item, &HostBridge::empty()) else {
                return Value::Array(Rc::new(RefCell::new(items)));
            };
            array.push(&value);
        }
        return Value::JsValue(array.into());
    }
    Value::Array(Rc::new(RefCell::new(items)))
}

pub(crate) fn object_value(props: BTreeMap<String, Value>) -> Value {
    if props.values().all(can_represent_value_as_js) {
        let object = JsObject::new();
        for (key, value) in &props {
            let Ok(value) = value_to_js_value(value, &HostBridge::empty()) else {
                return Value::Object(Rc::new(RefCell::new(props)));
            };
            if Reflect::set(&object, &JsValue::from_str(key), &value).is_err() {
                return Value::Object(Rc::new(RefCell::new(props)));
            }
        }
        return Value::JsValue(object.into());
    }
    Value::Object(Rc::new(RefCell::new(props)))
}

impl Value {
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Value::Number(value) => *value != 0.0 && !value.is_nan(),
            Value::String(value) => !value.is_empty(),
            Value::Symbol(_) => true,
            Value::Bool(value) => *value,
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => js_value_is_truthy(value),
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

    pub(crate) fn to_number(&self) -> f64 {
        match self {
            Value::Number(value) => *value,
            Value::String(value) => value.parse().unwrap_or(f64::NAN),
            Value::Symbol(_) => f64::NAN,
            Value::Bool(value) => f64::from(*value as u8),
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => js_value_to_number(value),
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
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                write!(f, "{}", js_value_display(value))
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
            Value::Null => write!(f, "null"),
            Value::Undefined => write!(f, "undefined"),
        }
    }
}
