//! 执行器内部 Value 模型。
//!
//! 运行时正在逐步向裸 `JsValue` 靠拢，但 VM 仍需要若干内部值来表示：
//! bytecode 函数、class、module namespace、extern 引用、generator frame 和闭包环境。
//! 这些内部值在跨到宿主环境时会由 `HostBridge` 转成真实 `JsValue`。

use crate::env::LexicalEnv;
use crate::host::{
    HostValue, JsHostBridge, js_overlay_set, js_reflect_property_key, js_value_display,
    value_to_js_value,
};
use js_sys::{Array as JsArray, Object as JsObject, Reflect};
use js_token_core::{BytecodeModule, BytecodeOperand};
use std::{cell::RefCell, collections::BTreeMap, fmt, rc::Rc};
use wasm_bindgen::JsValue;

/// 外部值引用。
///
/// `slot` 指向执行入口传入的 externals 数组，`root/path` 描述从该槽继续访问的属性链。
/// 例如 `console.log` 会表示为 `slot = console 的槽位`、`path = ["log"]`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRefValue {
    /// externals 数组下标。
    pub slot: u32,
    /// 根名字，主要用于错误信息和全局根判断。
    pub root: String,
    /// 属性访问路径。
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

/// 执行器内部值。
///
/// JS 对象、数组、字符串等应尽量落到 `JsValue`，保证浏览器/Node 原生方法语义一致。
/// VM 专有结构仍保留独立 variant，用于描述 bytecode 函数和运行帧等宿主无法直接表达的值。
#[derive(Debug, Default, Clone)]
pub enum Value {
    /// number。
    Number(f64),
    /// BigInt 文本值。
    BigInt(String),
    /// string。
    String(String),
    /// Symbol 描述。
    Symbol(String),
    /// boolean。
    Bool(bool),
    /// 原生 JS 值。
    JsValue(JsValue),
    /// 已绑定 `this` 的 JS 函数。
    BoundJsFunction(JsValue, JsValue),
    /// 兼容保留的 VM 数组结构，新路径优先使用 `JsValue(Array)`。
    Array(Rc<RefCell<Vec<Value>>>),
    /// 兼容保留的 VM 对象结构，新路径优先使用 `JsValue(Object)`。
    Object(Rc<RefCell<BTreeMap<String, Value>>>),
    /// VM bytecode 函数。
    Function(FunctionValue),
    /// 已绑定 `this` 的 VM 函数。
    BoundFunction(FunctionValue, Box<Value>),
    /// VM 内建/native 函数。
    NativeFunction(NativeFunctionValue),
    /// 已绑定 `this` 的 VM native 函数。
    BoundNativeFunction(NativeFunctionValue, Box<Value>),
    /// 外部槽引用。
    ExternalRef(ExternalRefValue),
    /// VM class 值。
    Class(ClassValue),
    /// VM module namespace 值。
    Module(ModuleValue),
    /// generator 暂停/恢复状态。
    GeneratorState(Rc<RefCell<GeneratorState>>),
    /// `null`。
    Null,
    /// `undefined`。
    #[default]
    Undefined,
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Number(left), Value::Number(right)) => left == right,
            (Value::BigInt(left), Value::BigInt(right)) => left == right,
            (Value::String(left), Value::String(right)) => left == right,
            (Value::Symbol(left), Value::Symbol(right)) => left == right,
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::JsValue(left), Value::JsValue(right)) => js_sys::Object::is(left, right),
            (
                Value::BoundJsFunction(left_fn, left_this),
                Value::BoundJsFunction(right_fn, right_this),
            ) => js_sys::Object::is(left_fn, right_fn) && js_sys::Object::is(left_this, right_this),
            (Value::Array(left), Value::Array(right)) => Rc::ptr_eq(left, right),
            (Value::Object(left), Value::Object(right)) => Rc::ptr_eq(left, right),
            (Value::Function(left), Value::Function(right)) => left == right,
            (
                Value::BoundFunction(left_fn, left_this),
                Value::BoundFunction(right_fn, right_this),
            ) => left_fn == right_fn && left_this == right_this,
            (Value::NativeFunction(left), Value::NativeFunction(right)) => left == right,
            (
                Value::BoundNativeFunction(left_fn, left_this),
                Value::BoundNativeFunction(right_fn, right_this),
            ) => left_fn == right_fn && left_this == right_this,
            (Value::ExternalRef(left), Value::ExternalRef(right)) => left == right,
            (Value::Class(left), Value::Class(right)) => {
                left.name == right.name && left.constructor == right.constructor
            }
            (Value::Module(left), Value::Module(right)) => left.source == right.source,
            (Value::GeneratorState(left), Value::GeneratorState(right)) => Rc::ptr_eq(left, right),
            (Value::Null, Value::Null) | (Value::Undefined, Value::Undefined) => true,
            _ => false,
        }
    }
}

/// VM 函数对象。
#[derive(Debug, Clone)]
pub struct FunctionValue {
    /// 函数名。
    pub name: Option<String>,
    /// 参数 local slot 列表。
    pub params: Vec<BytecodeOperand>,
    /// 是否有显式返回值。
    pub has_return: bool,
    /// 是否为 async 函数。
    pub is_async: bool,
    /// 是否为 generator 函数。
    pub is_generator: bool,
    /// 函数体起始 pc。
    pub body_start: usize,
    /// 函数体结束 pc。
    pub body_end: usize,
    /// 创建函数时捕获的词法环境。
    pub env: LexicalEnv,
    pub(crate) props: Rc<RefCell<BTreeMap<String, Value>>>,
    pub(crate) end_pc: usize,
    pub(crate) host_bridge: Option<JsHostBridge>,
    pub(crate) module: Option<Rc<BytecodeModule>>,
}

impl PartialEq for FunctionValue {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.params == other.params
            && self.has_return == other.has_return
            && self.is_async == other.is_async
            && self.is_generator == other.is_generator
            && self.body_start == other.body_start
            && self.body_end == other.body_end
            && self.end_pc == other.end_pc
    }
}

/// VM native 函数标识。
///
/// 当前 native 函数通过名字分发，用于最小内建能力和 polyfill 辅助。
#[derive(Debug, Clone, PartialEq)]
pub struct NativeFunctionValue {
    /// native 函数名。
    pub name: String,
}

/// generator 执行帧。
///
/// 暂停时保存 pc、寄存器、词法环境和上一次 yield 值，resume 时恢复。
#[derive(Debug, Clone)]
pub struct GeneratorState {
    /// 是否已经初始化执行。
    pub initialized: bool,
    /// 是否已经完成。
    pub done: bool,
    /// 下次恢复执行的 pc。
    pub pc: usize,
    /// resume 参数需要写回的寄存器。
    pub resume_dst: Option<u32>,
    /// 暂停时的寄存器快照。
    pub registers: Vec<Value>,
    /// 暂停时的词法环境。
    pub lexical_env: LexicalEnv,
    /// 最后一次产出的值。
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

/// VM class 值。
#[derive(Debug, Clone, PartialEq)]
pub struct ClassValue {
    /// 类名。
    pub name: Option<String>,
    /// 父类值。
    pub super_class: Option<Box<Value>>,
    /// 构造器函数。
    pub constructor: Option<FunctionValue>,
    /// static 属性。
    pub static_props: BTreeMap<String, Value>,
    /// 实例默认属性和方法。
    pub instance_props: BTreeMap<String, Value>,
}

/// VM module namespace 值。
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleValue {
    /// 模块来源路径。
    pub source: String,
    /// 导出表。
    pub exports: BTreeMap<String, Value>,
}

/// 创建 JS Array 值。
///
/// 当前优先返回 `Value::JsValue(Array)`，使 `flatMap`、迭代器、原型方法等行为由宿主 JS 引擎保证。
pub(crate) fn array_value(items: Vec<Value>) -> Value {
    let array = JsArray::new();
    let bridge = JsHostBridge::empty();
    for item in &items {
        array.push(&value_to_js_value(item, &bridge).unwrap_or(JsValue::UNDEFINED));
    }
    Value::JsValue(array.into())
}

/// 创建 JS Object 值。
///
/// 普通属性通过 `Reflect.set` 写入；以内部 accessor key 标记的 getter/setter 会转成
/// `Object.defineProperty` 描述符，尽量贴近 JS 对象字面量语义。
pub(crate) fn object_value(props: BTreeMap<String, Value>) -> Value {
    let object = JsObject::new();
    let object_value: JsValue = object.clone().into();
    let bridge = JsHostBridge::empty();
    let mut accessors: BTreeMap<String, (Option<Value>, Option<Value>)> = BTreeMap::new();
    for (key, value) in &props {
        if let Some(property) = key.strip_prefix("__accessor_get__:") {
            accessors.entry(property.to_string()).or_default().0 = Some(value.clone());
            continue;
        }
        if let Some(property) = key.strip_prefix("__accessor_set__:") {
            accessors.entry(property.to_string()).or_default().1 = Some(value.clone());
            continue;
        }
        let _ = Reflect::set(
            &object,
            &js_reflect_property_key(key),
            &value_to_js_value(value, &bridge).unwrap_or(JsValue::UNDEFINED),
        );
    }
    for (key, (getter, setter)) in accessors {
        let descriptor = JsObject::new();
        if let Some(getter) = getter {
            let _ = Reflect::set(
                &descriptor,
                &JsValue::from_str("get"),
                &value_to_js_value(&getter, &bridge).unwrap_or(JsValue::UNDEFINED),
            );
            js_overlay_set(&object_value, &format!("__accessor_get__:{key}"), getter);
        }
        if let Some(setter) = setter {
            let _ = Reflect::set(
                &descriptor,
                &JsValue::from_str("set"),
                &value_to_js_value(&setter, &bridge).unwrap_or(JsValue::UNDEFINED),
            );
            js_overlay_set(&object_value, &format!("__accessor_set__:{key}"), setter);
        }
        let _ = Reflect::set(
            &descriptor,
            &JsValue::from_str("enumerable"),
            &JsValue::TRUE,
        );
        let _ = Reflect::set(
            &descriptor,
            &JsValue::from_str("configurable"),
            &JsValue::TRUE,
        );
        let _ = Reflect::define_property(&object, &js_reflect_property_key(&key), &descriptor);
    }
    Value::JsValue(object_value)
}

impl Value {
    /// 判断 JS truthy/falsy。
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

    /// 按 JS number coercion 规则尽量转换成 `f64`。
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
        fmt_value(self, f, 0, &mut Vec::new())
    }
}

fn fmt_value(
    value: &Value,
    f: &mut fmt::Formatter<'_>,
    depth: usize,
    seen_arrays: &mut Vec<usize>,
) -> fmt::Result {
    match value {
        Value::Number(value) => write!(f, "{value}"),
        Value::BigInt(value) => write!(f, "{value}"),
        Value::String(value) => write!(f, "{value}"),
        Value::Symbol(value) => write!(f, "Symbol({value})"),
        Value::Bool(value) => write!(f, "{value}"),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            write!(f, "{}", js_value_display(value))
        }
        Value::Array(items) => {
            let ptr = Rc::as_ptr(items) as usize;
            if depth > 32 || seen_arrays.contains(&ptr) {
                return Ok(());
            }
            seen_arrays.push(ptr);
            write!(f, "[")?;
            for (index, item) in items.borrow().iter().enumerate() {
                if index > 0 {
                    write!(f, ",")?;
                }
                fmt_value(item, f, depth + 1, seen_arrays)?;
            }
            seen_arrays.pop();
            write!(f, "]")
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
