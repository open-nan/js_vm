//! 宿主桥接层。
//!
//! 执行器不应该硬编码 `window`、`console`、`fetch` 等环境对象，而是通过 `HostBridge`
//! 访问外部世界。浏览器和 Node 的 wasm 包可以分别提供自己的 bridge 实现或外部数组，
//! runtime core 只依赖这里的 trait。

use crate::error::ExecuteError;
use crate::ops::get_local_member;
use crate::value::{
    ClassValue, ExternalRefValue, FunctionValue, ModuleValue, NativeFunctionValue, Value,
};
use js_sys::{
    Array as JsArray, Error as JsError, Float32Array, Function as JsFunction, Int32Array, Reflect,
    Uint8Array, Uint16Array,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
#[cfg(feature = "runtime-profile")]
use std::{panic::PanicHookInfo, sync::Once};
use wasm_bindgen::{JsCast, prelude::*};

#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = globalThis, js_name = __jsVmHostLog)]
    pub(crate) fn wasm_host_log(level: &str, value: &str);

    #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = Reflect, js_name = get, catch)]
    fn reflect_get_with_receiver(
        target: &JsValue,
        property_key: &JsValue,
        receiver: &JsValue,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = Object)]
    fn js_object(value: &JsValue) -> JsValue;
}

thread_local! {
    // Overlay 是 VM 与真实 JS 对象之间的“补丁层”。
    //
    // 有些 VM 内部值无法稳定写成普通 JsValue，例如 VM 函数、generator state、
    // Symbol key 或非浏览器原生对象。直接 Reflect.set 会丢信息，因此这里用弱约定的 id
    // 在 Rust 侧保存额外属性，Reflect.get 前先查 overlay，再回落到宿主对象。
    static HOST_VALUE_OVERLAY: RefCell<BTreeMap<String, Value>> = RefCell::new(BTreeMap::new());
    static JS_VALUE_OVERLAY: RefCell<BTreeMap<(u32, String), Value>> = RefCell::new(BTreeMap::new());
    static NEXT_JS_OVERLAY_ID: RefCell<u32> = const { RefCell::new(1) };
    static JS_SYMBOL_PROPERTY_KEYS: RefCell<Vec<(u32, JsValue)>> = const { RefCell::new(Vec::new()) };
    static NEXT_JS_SYMBOL_PROPERTY_KEY_ID: RefCell<u32> = const { RefCell::new(1) };
    static VM_JS_HANDLES: RefCell<BTreeMap<u32, VmJsHandle>> = RefCell::new(BTreeMap::new());
    static VM_JS_HANDLE_VALUES: RefCell<Vec<(u32, JsValue)>> = const { RefCell::new(Vec::new()) };
    static NEXT_VM_JS_HANDLE_ID: RefCell<u32> = const { RefCell::new(1) };
    static VM_JS_CALLBACK_INVOKER: RefCell<Option<VmJsCallbackInvoker>> = RefCell::new(None);
    #[cfg(feature = "runtime-profile")]
    static LAST_WASM_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[cfg(feature = "runtime-profile")]
static RUNTIME_PROFILE_PANIC_HOOK: Once = Once::new();

const JS_SYMBOL_PROPERTY_KEY_PREFIX: &str = "__js_vm_symbol_key:";
const VM_THROWN_MARKER_KEY: &str = "__js_vm_thrown";
const VM_THROWN_VALUE_KEY: &str = "value";

type VmJsCallbackInvoker = Rc<dyn Fn(VmJsHandle, JsValue, Vec<Value>) -> Result<JsValue, JsValue>>;

#[derive(Debug, Clone)]
pub(crate) enum VmJsHandle {
    Function(FunctionValue),
    BoundFunction(FunctionValue, Value),
    NativeFunction(NativeFunctionValue),
    BoundNativeFunction(NativeFunctionValue, Value),
    Class(ClassValue),
    Module(ModuleValue),
}

/// 默认 JS 宿主桥。
///
/// `values` 是执行入口传入的 externals 数组。bytecode 中的 `External(slot)` 会直接索引它，
/// 后续属性读取/调用通过 `Reflect` 交给宿主 JS 引擎。
#[derive(Debug, Clone)]
pub struct JsHostBridge {
    values: Rc<RefCell<Vec<JsValue>>>,
}

/// 宿主值最小能力集合。
///
/// 执行器内部有时只需要 JS coercion 结果，而不关心具体环境实现。该 trait 把 display、
/// truthy、number coercion、property key 等操作抽象出来。
pub trait HostValue {
    /// 转成用于日志/错误信息的显示文本。
    fn display(&self) -> String;
    /// 判断 truthy/falsy。
    fn is_truthy(&self) -> bool;
    /// 转成 number。
    fn to_number(&self) -> f64;
    /// 转成属性键字符串。
    fn property_key(&self) -> String;
    /// 返回 `typeof` 名称。
    fn typeof_name(&self) -> &'static str;
}

impl HostValue for JsValue {
    fn display(&self) -> String {
        js_value_display(self)
    }

    fn is_truthy(&self) -> bool {
        js_value_is_truthy(self)
    }

    fn to_number(&self) -> f64 {
        js_value_to_number(self)
    }

    fn property_key(&self) -> String {
        js_value_property_key(self)
    }

    fn typeof_name(&self) -> &'static str {
        js_value_typeof(self)
    }
}

/// 执行器访问宿主环境的统一接口。
///
/// 任何具体环境包只要实现这个 trait，就可以复用同一套 bytecode executor。
/// 该 trait 的边界是“解析外部槽、属性、调用、构造和 JS coercion”，不承载 bytecode 语义。
pub trait HostBridge: Clone {
    /// 如果当前 bridge 本身就是 `JsHostBridge`，返回克隆值。
    ///
    /// VM 函数被包装成 JS callback 时需要重新进入执行器，因此需要拿到 JS bridge。
    fn as_js_host_bridge(&self) -> Option<JsHostBridge> {
        None
    }

    /// 校验运行时传入 externals 数量是否满足 bytecode 需要。
    fn validate_extern_count(&self, expected: usize) -> Result<(), ExecuteError>;
    /// 校验单个 extern slot 是否存在。
    fn validate_slot(&self, slot: u32) -> Result<(), ExecuteError>;
    /// 读取外部根引用。
    fn read_external(&self, reference: &ExternalRefValue) -> Result<Value, ExecuteError>;
    /// 读取外部对象属性。
    fn get(&self, reference: &ExternalRefValue, property: &str) -> Result<Value, ExecuteError>;
    /// 判断外部对象是否有属性。
    fn has_property(
        &self,
        reference: &ExternalRefValue,
        property: &str,
    ) -> Result<bool, ExecuteError>;
    /// 写入外部对象属性。
    fn set(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        value: &Value,
    ) -> Result<(), ExecuteError>;
    /// 直接替换 externals 数组中的某个槽。
    fn set_slot(&self, slot: u32, value: &Value) -> Result<(), ExecuteError>;
    /// 调用外部引用。
    fn call(&self, reference: &ExternalRefValue, args: Vec<Value>) -> Result<Value, ExecuteError>;
    /// 用显式 `this` 调用外部引用。
    fn call_with_this(
        &self,
        reference: &ExternalRefValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    /// 构造调用外部引用。
    fn construct(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    /// 调用已经解析出的 JS 函数值。
    fn call_js_value(
        &self,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    /// 判断当前调用是否是 `Object.defineProperty`。
    ///
    /// 执行器用它识别 accessor/descriptor 相关语义，避免误走普通函数调用路径。
    fn is_object_define_property_function(&self, callee: &JsValue, this_value: &JsValue) -> bool;
    /// 构造调用已经解析出的 JS 函数值。
    fn construct_js_value(
        &self,
        constructor: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    /// 交给宿主处理的二元运算。
    ///
    /// 当 VM 快速路径无法覆盖对象 coercion、BigInt、Symbol 等语义时会回落到这里。
    fn binary_operator(&self, op: &str, left: &Value, right: &Value)
    -> Result<Value, ExecuteError>;
    #[cfg(feature = "string-builtins")]
    /// 字符串内建方法调用。
    fn call_string_method(
        &self,
        value: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    /// 把 VM 函数包装成可在当前宿主中使用的值。
    fn function_value(&self, function: FunctionValue) -> Value;
    /// 把 VM class 包装成可在当前宿主中使用的值。
    fn class_value(&self, class: ClassValue) -> Value;
    /// 把 VM module namespace 包装成宿主值。
    fn module_value(&self, module: ModuleValue) -> Result<Value, ExecuteError>;
}

impl Default for JsHostBridge {
    fn default() -> Self {
        Self::empty()
    }
}

impl JsHostBridge {
    /// 创建不带 externals 的 bridge。
    pub fn empty() -> Self {
        Self {
            values: Rc::new(RefCell::new(Vec::new())),
        }
    }
    /// 从 JS externals 数组创建 bridge。
    ///
    /// 数组顺序必须和 bytecode 的 extern slot 顺序一致；压缩 bytecode 可只记录 slot 数量。
    pub fn from_js_values(values: Vec<JsValue>) -> Self {
        Self {
            values: Rc::new(RefCell::new(values)),
        }
    }

    pub(crate) fn validate_extern_count(&self, expected: usize) -> Result<(), ExecuteError> {
        let actual = self.values.borrow().len();
        if actual != expected {
            return Err(crate::error::runtime_error!(
                "external slot count mismatch: expected {expected}, got {actual}"
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_slot(&self, slot: u32) -> Result<(), ExecuteError> {
        if self.values.borrow().get(slot as usize).is_none() {
            return Err(crate::error::runtime_error!(
                "external slot {slot} is not available"
            ));
        }
        Ok(())
    }

    pub(crate) fn read_external(
        &self,
        reference: &ExternalRefValue,
    ) -> Result<Value, ExecuteError> {
        if let Some(value) = host_overlay_get(&reference.display_path()) {
            return Ok(value);
        }
        if reference.path.is_empty()
            && !is_global_external_root(&reference.root)
            && let Some(value) = global_overlay_get(&reference.root)
        {
            return Ok(value);
        }
        let value = self.resolve_js_value(reference)?;
        if value.is_undefined() && !is_global_external_root(&reference.root) {
            return Err(crate::error::reference_error!(
                "{} is not defined",
                reference.display_path()
            ));
        }
        Ok(Value::JsValue(value))
    }

    pub(crate) fn get(
        &self,
        reference: &ExternalRefValue,
        property: &str,
    ) -> Result<Value, ExecuteError> {
        let property_key = js_reflect_property_key(property);
        self.get_with_property_key(reference, property, &property_key)
    }

    pub(crate) fn get_with_property_key(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        property_key: &JsValue,
    ) -> Result<Value, ExecuteError> {
        let next = reference.member(property);
        if let Some(value) = host_overlay_get(&next.display_path()) {
            return Ok(value);
        }
        if let Some(alias) = global_property_alias(reference, property)
            && let Some(value) = host_overlay_get(&alias)
        {
            return Ok(value);
        }
        let base = self.resolve_js_value(reference)?;
        if !is_js_property_target(&base) && !is_js_boxable_primitive(&base) {
            let base = Value::JsValue(base);
            return get_local_member(&base, property);
        }
        let value = get_js_property_with_key(&base, property_key)?;
        if value.dyn_ref::<JsFunction>().is_some() {
            Ok(Value::BoundJsFunction(value, base))
        } else {
            Ok(Value::JsValue(value))
        }
    }

    pub(crate) fn get_plain_js_member_function_with_key(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        property_key: &JsValue,
    ) -> Result<Option<(JsValue, JsValue)>, ExecuteError> {
        let next = reference.member(property);
        if host_overlay_get(&next.display_path()).is_some() {
            return Ok(None);
        }
        if let Some(alias) = global_property_alias(reference, property)
            && host_overlay_get(&alias).is_some()
        {
            return Ok(None);
        }
        let base = self.resolve_js_value(reference)?;
        if !is_js_property_target(&base) && !is_js_boxable_primitive(&base) {
            return Ok(None);
        }
        let callee = get_js_property_with_key(&base, property_key)?;
        if self.is_cacheable_plain_js_function(&callee, &base) {
            Ok(Some((callee, base)))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn has_property(
        &self,
        reference: &ExternalRefValue,
        property: &str,
    ) -> Result<bool, ExecuteError> {
        let target = self.resolve_js_value(reference)?;
        if !is_js_property_target(&target) && !is_js_boxable_primitive(&target) {
            let base = Value::JsValue(target);
            return Ok(crate::ops::has_property(&base, property));
        }
        Reflect::has(&target, &js_reflect_property_key(property)).map_err(js_error)
    }

    pub(crate) fn set(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        value: &Value,
    ) -> Result<(), ExecuteError> {
        let target = self.resolve_js_value(reference)?;
        if !is_js_property_target(&target) {
            return Err(crate::error::type_error!(
                "cannot set property {property:?} of {}",
                js_value_display(&target)
            ));
        }
        host_overlay_set(reference, property, value);
        if property.starts_with("Symbol.") {
            return Ok(());
        }
        if !can_represent_value_as_js(value) {
            return Ok(());
        }
        Reflect::set(
            &target,
            &js_reflect_property_key(property),
            &value_to_js_value(value, self)?,
        )
        .map_err(js_error)?;
        Ok(())
    }

    pub(crate) fn set_slot(&self, slot: u32, value: &Value) -> Result<(), ExecuteError> {
        let next_value = value_to_js_value(value, self)?;
        let mut values = self.values.borrow_mut();
        let Some(target) = values.get_mut(slot as usize) else {
            return Err(crate::error::runtime_error!(
                "external slot {slot} is not available"
            ));
        };
        *target = next_value;
        Ok(())
    }

    pub(crate) fn call(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let (callee, this_value) = self.resolve_js_callable(reference)?;
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                reference.display_path()
            ));
        };
        trace_host_call(
            "call.begin",
            &format!(
                "{} args={}",
                js_function_name(&callee).unwrap_or_else(|| reference.display_path()),
                args.len()
            ),
        );
        let result = call_js_function(function, &this_value, &args, self)?;
        trace_host_call("call.end", reference.display_path().as_ref());
        Ok(Value::JsValue(result))
    }

    pub(crate) fn call_with_this(
        &self,
        reference: &ExternalRefValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let callee = self.resolve_js_value(reference)?;
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                reference.display_path()
            ));
        };
        let js_this = value_to_js_value(&this_value, self)?;
        trace_host_call(
            "call_this.begin",
            &format!(
                "{} args={}",
                js_function_name(&callee).unwrap_or_else(|| reference.display_path()),
                args.len()
            ),
        );
        let result = call_js_function(function, &js_this, &args, self)?;
        trace_host_call("call_this.end", reference.display_path().as_ref());
        Ok(Value::JsValue(result))
    }
    pub(crate) fn call_js_value(
        &self,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        if js_is_function_prototype_member(&callee, "call") {
            return self.call_function_prototype_call(this_value, args);
        }
        if js_is_function_prototype_member(&callee, "apply") {
            return self.call_function_prototype_apply(this_value, args);
        }
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                js_value_display(&callee)
            ));
        };
        trace_host_call(
            "call_value.begin",
            &format!(
                "{} args={}",
                js_function_name(&callee).unwrap_or_else(|| js_value_display(&callee)),
                args.len()
            ),
        );
        let result = call_js_function(function, &this_value, &args, self)?;
        trace_host_call(
            "call_value.end",
            js_function_name(&callee)
                .unwrap_or_else(|| js_value_display(&callee))
                .as_ref(),
        );
        Ok(Value::JsValue(result))
    }

    pub(crate) fn call_plain_js_value_three(
        &self,
        callee: JsValue,
        this_value: JsValue,
        first: &Value,
        second: &Value,
        third: &Value,
    ) -> Result<Value, ExecuteError> {
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                js_value_display(&callee)
            ));
        };
        trace_host_call(
            "call_value.begin",
            &format!(
                "{} args=3",
                js_function_name(&callee).unwrap_or_else(|| js_value_display(&callee))
            ),
        );
        let result = function.call3(
            &this_value,
            &value_to_js_value(first, self)?,
            &value_to_js_value(second, self)?,
            &value_to_js_value(third, self)?,
        );
        trace_host_call(
            "call_value.end",
            js_function_name(&callee)
                .unwrap_or_else(|| js_value_display(&callee))
                .as_ref(),
        );
        result.map(Value::JsValue).map_err(js_error)
    }

    pub(crate) fn call_plain_js_value_one(
        &self,
        callee: JsValue,
        this_value: JsValue,
        arg: &Value,
    ) -> Result<Value, ExecuteError> {
        self.call_plain_js_value_with_args(callee, this_value, std::slice::from_ref(arg))
    }

    pub(crate) fn call_plain_js_value_with_args(
        &self,
        callee: JsValue,
        this_value: JsValue,
        args: &[Value],
    ) -> Result<Value, ExecuteError> {
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                js_value_display(&callee)
            ));
        };
        call_js_function(function, &this_value, args, self).map(Value::JsValue)
    }

    pub(crate) fn is_cacheable_plain_js_function(
        &self,
        callee: &JsValue,
        this_value: &JsValue,
    ) -> bool {
        callee.dyn_ref::<JsFunction>().is_some()
            && vm_js_handle(callee).is_none()
            && !js_is_function_prototype_member(callee, "call")
            && !js_is_function_prototype_member(callee, "apply")
            && !self.is_object_define_property_function(callee, this_value)
            && {
                #[cfg(feature = "bigint")]
                {
                    js_bigint_builtin_name(callee).is_none()
                }
                #[cfg(not(feature = "bigint"))]
                {
                    true
                }
            }
    }

    fn call_function_prototype_call(
        &self,
        target: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let Some(function) = target.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                js_value_display(&target)
            ));
        };
        let this_arg = args
            .first()
            .map(|value| value_to_js_value(value, self))
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED);
        call_js_function(function, &this_arg, args.get(1..).unwrap_or(&[]), self)
            .map(Value::JsValue)
    }

    fn call_function_prototype_apply(
        &self,
        target: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let Some(function) = target.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                js_value_display(&target)
            ));
        };
        let this_arg = args
            .first()
            .map(|value| value_to_js_value(value, self))
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED);
        let js_args = match args.get(1) {
            None | Some(Value::Null | Value::Undefined) => JsArray::new(),
            Some(value) => {
                let value = value_to_js_value(value, self)?;
                JsArray::from(&value)
            }
        };
        function
            .apply(&this_arg, &js_args)
            .map(Value::JsValue)
            .map_err(js_error)
    }
    pub(crate) fn is_object_define_property_function(
        &self,
        callee: &JsValue,
        this_value: &JsValue,
    ) -> bool {
        let global = JsValue::from(js_sys::global());
        let Ok(object) = get_js_property(&global, "Object") else {
            return false;
        };
        let Ok(define_property) = get_js_property(&object, "defineProperty") else {
            return false;
        };
        js_sys::Object::is(callee, &define_property) && js_sys::Object::is(this_value, &object)
    }
    pub(crate) fn construct_js_value(
        &self,
        constructor: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let Some(function) = constructor.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not constructable",
                js_value_display(&constructor)
            ));
        };
        let js_args = values_to_js_array(&args, self)?;
        trace_host_call(
            "construct_value.begin",
            &format!(
                "{} args={}",
                js_function_name(&constructor).unwrap_or_else(|| js_value_display(&constructor)),
                args.len()
            ),
        );
        Reflect::construct(function, &js_args)
            .map(Value::JsValue)
            .map_err(js_error)
    }
    pub(crate) fn construct(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let constructor = self.resolve_js_value(reference)?;
        let Some(constructor) = constructor.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not constructable",
                reference.display_path()
            ));
        };
        let js_args = values_to_js_array(&args, self)?;
        let result = Reflect::construct(constructor, &js_args).map_err(js_error)?;
        Ok(Value::JsValue(result))
    }
    fn resolve_js_value(&self, reference: &ExternalRefValue) -> Result<JsValue, ExecuteError> {
        self.validate_slot(reference.slot)?;
        let mut value = if is_global_external_root(&reference.root) {
            js_sys::global().into()
        } else {
            self.values.borrow()[reference.slot as usize].clone()
        };
        for property in &reference.path {
            value = get_js_property(&value, property)?;
        }
        Ok(value)
    }
    fn resolve_js_callable(
        &self,
        reference: &ExternalRefValue,
    ) -> Result<(JsValue, JsValue), ExecuteError> {
        self.validate_slot(reference.slot)?;
        let mut this_value = JsValue::UNDEFINED;
        let mut value = if is_global_external_root(&reference.root) {
            js_sys::global().into()
        } else {
            self.values.borrow()[reference.slot as usize].clone()
        };
        if reference.path.is_empty() {
            return Ok((value, this_value));
        }
        for property in &reference.path[..reference.path.len() - 1] {
            value = get_js_property(&value, property)?;
        }
        this_value = value.clone();
        if !is_js_property_target(&this_value) && !is_js_boxable_primitive(&this_value) {
            return Err(crate::error::type_error!(
                "{} is not callable",
                reference.display_path()
            ));
        }
        let Some(property) = reference.path.last() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                reference.display_path()
            ));
        };
        let callee = get_js_property(&this_value, property)?;
        Ok((callee, this_value))
    }
}

impl HostBridge for JsHostBridge {
    fn as_js_host_bridge(&self) -> Option<JsHostBridge> {
        Some(self.clone())
    }

    fn validate_extern_count(&self, expected: usize) -> Result<(), ExecuteError> {
        JsHostBridge::validate_extern_count(self, expected)
    }

    fn validate_slot(&self, slot: u32) -> Result<(), ExecuteError> {
        JsHostBridge::validate_slot(self, slot)
    }

    fn read_external(&self, reference: &ExternalRefValue) -> Result<Value, ExecuteError> {
        JsHostBridge::read_external(self, reference)
    }

    fn get(&self, reference: &ExternalRefValue, property: &str) -> Result<Value, ExecuteError> {
        JsHostBridge::get(self, reference, property)
    }

    fn has_property(
        &self,
        reference: &ExternalRefValue,
        property: &str,
    ) -> Result<bool, ExecuteError> {
        JsHostBridge::has_property(self, reference, property)
    }

    fn set(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        value: &Value,
    ) -> Result<(), ExecuteError> {
        JsHostBridge::set(self, reference, property, value)
    }

    fn set_slot(&self, slot: u32, value: &Value) -> Result<(), ExecuteError> {
        JsHostBridge::set_slot(self, slot, value)
    }

    fn call(&self, reference: &ExternalRefValue, args: Vec<Value>) -> Result<Value, ExecuteError> {
        JsHostBridge::call(self, reference, args)
    }

    fn call_with_this(
        &self,
        reference: &ExternalRefValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        JsHostBridge::call_with_this(self, reference, this_value, args)
    }

    fn construct(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        JsHostBridge::construct(self, reference, args)
    }

    fn call_js_value(
        &self,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        JsHostBridge::call_js_value(self, callee, this_value, args)
    }

    fn is_object_define_property_function(&self, callee: &JsValue, this_value: &JsValue) -> bool {
        JsHostBridge::is_object_define_property_function(self, callee, this_value)
    }

    fn construct_js_value(
        &self,
        constructor: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        JsHostBridge::construct_js_value(self, constructor, args)
    }

    fn binary_operator(
        &self,
        op: &str,
        left: &Value,
        right: &Value,
    ) -> Result<Value, ExecuteError> {
        js_binary_operator(self, op, left, right)
    }

    #[cfg(feature = "string-builtins")]
    fn call_string_method(
        &self,
        value: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        call_js_string_method(self, value, method, args)
    }

    fn function_value(&self, function: FunctionValue) -> Value {
        Value::JsValue(vm_function_to_js_value(function))
    }

    fn class_value(&self, class: ClassValue) -> Value {
        Value::JsValue(vm_class_to_js_value(class))
    }

    fn module_value(&self, module: ModuleValue) -> Result<Value, ExecuteError> {
        vm_module_to_js_value(module, self).map(Value::JsValue)
    }
}

fn js_binary_operator(
    bridge: &JsHostBridge,
    op: &str,
    left: &Value,
    right: &Value,
) -> Result<Value, ExecuteError> {
    let source = match op {
        "+" => "return left + right;",
        "-" => "return left - right;",
        "*" => "return left * right;",
        "/" => "return left / right;",
        "%" => "return left % right;",
        "**" => "return left ** right;",
        "==" => "return left == right;",
        "!=" => "return left != right;",
        "===" => "return left === right;",
        "!==" => "return left !== right;",
        "<" => "return left < right;",
        "<=" => "return left <= right;",
        ">" => "return left > right;",
        ">=" => "return left >= right;",
        "&" => "return left & right;",
        "|" => "return left | right;",
        "^" => "return left ^ right;",
        "<<" => "return left << right;",
        ">>" => "return left >> right;",
        ">>>" => "return left >>> right;",
        _ => return Err(ExecuteError::Unsupported("js binary operator")),
    };
    let function = JsFunction::new_with_args("left, right", source);
    let left = value_to_js_value(left, bridge)?;
    let right = value_to_js_value(right, bridge)?;
    function
        .call2(&JsValue::UNDEFINED, &left, &right)
        .map(Value::JsValue)
        .map_err(js_error)
}

#[cfg(feature = "string-builtins")]
fn call_js_string_method(
    bridge: &JsHostBridge,
    value: &str,
    method: &str,
    args: Vec<Value>,
) -> Result<Value, ExecuteError> {
    let this_value = JsValue::from_str(value);
    let callee = get_js_property(&this_value, method)?;
    let js_args = JsArray::new();
    for (index, arg) in args.iter().enumerate() {
        if index == 0 {
            #[cfg(feature = "regexp")]
            let pattern = match arg {
                Value::String(pattern) => Some(pattern.clone()),
                Value::JsValue(value) => value.as_string(),
                Value::BoundJsFunction(value, _) => value.as_string(),
                _ => None,
            };
            #[cfg(feature = "regexp")]
            if let Some(pattern) = pattern {
                if let Some((body, global)) = crate::ops::regex_string_parts(&pattern) {
                    js_args.push(&js_sys::RegExp::new(body, if global { "g" } else { "" }).into());
                    continue;
                }
            }
        }
        js_args.push(&value_to_js_value(arg, bridge)?);
    }
    let Some(function) = callee.dyn_ref::<JsFunction>() else {
        return Err(crate::error::type_error!("String.{method} is not callable"));
    };
    function
        .apply(&this_value, &js_args)
        .map(Value::JsValue)
        .map_err(js_error)
}

pub(crate) fn values_to_js_array(
    values: &[Value],
    bridge: &JsHostBridge,
) -> Result<JsArray, ExecuteError> {
    let array = JsArray::new();
    for value in values {
        array.push(&value_to_js_value(value, bridge)?);
    }
    Ok(array)
}

fn call_js_function(
    function: &JsFunction,
    this_value: &JsValue,
    args: &[Value],
    bridge: &JsHostBridge,
) -> Result<JsValue, ExecuteError> {
    // Vue/Router 初始化阶段有大量 0/1/2 参数的小函数调用。直接走 call0/call1/call2
    // 可以避开临时 JS Array 分配和 Function.apply 的额外分发成本。
    match args.len() {
        0 => function.call0(this_value).map_err(js_error),
        1 => {
            let arg0 = value_to_js_value(&args[0], bridge)?;
            function.call1(this_value, &arg0).map_err(js_error)
        }
        2 => {
            let arg0 = value_to_js_value(&args[0], bridge)?;
            let arg1 = value_to_js_value(&args[1], bridge)?;
            function.call2(this_value, &arg0, &arg1).map_err(js_error)
        }
        _ => {
            let js_args = values_to_js_array(args, bridge)?;
            function.apply(this_value, &js_args).map_err(js_error)
        }
    }
}

pub fn value_to_js_value(value: &Value, bridge: &JsHostBridge) -> Result<JsValue, ExecuteError> {
    // VM 正在向裸 JsValue 收敛，但函数、class、module、extern ref 仍需要在转换时
    // 注入 handle 或通过 bridge 解析。入口处不直接递归，避免循环对象导致 wasm 栈爆掉。
    value_to_js_value_inner(value, bridge, &mut Vec::new(), 0)
}

fn value_to_js_value_inner(
    value: &Value,
    bridge: &JsHostBridge,
    seen: &mut Vec<(u8, usize, JsValue)>,
    depth: usize,
) -> Result<JsValue, ExecuteError> {
    if depth > 128 {
        return Ok(JsValue::UNDEFINED);
    }
    Ok(match value {
        Value::Number(value) => JsValue::from_f64(*value),
        #[cfg(feature = "bigint")]
        Value::BigInt(value) => JsFunction::new_with_args("value", "return BigInt(value);")
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(value))
            .map_err(js_error)?,
        #[cfg(not(feature = "bigint"))]
        Value::BigInt(_) => JsValue::UNDEFINED,
        Value::String(value) => JsValue::from_str(value),
        Value::Symbol(value) => symbol_value_to_js_value(value)?,
        Value::Bool(value) => JsValue::from_bool(*value),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => value.clone(),
        Value::Null => JsValue::NULL,
        Value::Undefined => JsValue::UNDEFINED,
        Value::Array(items) => {
            let ptr = Rc::as_ptr(items) as usize;
            if let Some((_, _, value)) = seen
                .iter()
                .find(|(kind, seen_ptr, _)| *kind == 0 && *seen_ptr == ptr)
            {
                return Ok(value.clone());
            }
            let array = JsArray::new();
            seen.push((0, ptr, array.clone().into()));
            for item in items.borrow().iter() {
                array.push(&value_to_js_value_inner(item, bridge, seen, depth + 1)?);
            }
            array.into()
        }
        Value::Object(props) => {
            let ptr = Rc::as_ptr(props) as usize;
            if let Some((_, _, value)) = seen
                .iter()
                .find(|(kind, seen_ptr, _)| *kind == 1 && *seen_ptr == ptr)
            {
                return Ok(value.clone());
            }
            let object = js_sys::Object::new();
            seen.push((1, ptr, object.clone().into()));
            for (key, value) in props.borrow().iter() {
                Reflect::set(
                    &object,
                    &js_reflect_property_key(key),
                    &value_to_js_value_inner(value, bridge, seen, depth + 1)?,
                )
                .map_err(js_error)?;
            }
            object.into()
        }
        Value::ExternalRef(reference) => bridge.resolve_js_value(reference)?,
        Value::Function(function) => vm_function_to_js_value(function.clone()),
        Value::BoundFunction(function, this_value) => {
            vm_bound_function_to_js_value(function.clone(), (**this_value).clone())
        }
        Value::NativeFunction(function) => vm_native_function_to_js_value(function.clone()),
        Value::BoundNativeFunction(function, this_value) => {
            vm_bound_native_function_to_js_value(function.clone(), (**this_value).clone())
        }
        Value::Class(class) => vm_class_to_js_value(class.clone()),
        Value::Module(module) => vm_module_to_js_value(module.clone(), bridge)?,
        Value::GeneratorState(_) => JsValue::UNDEFINED,
    })
}

pub(crate) fn vm_function_to_js_value(function: FunctionValue) -> JsValue {
    vm_callable_handle_to_js_value(VmJsHandle::Function(function))
}

pub(crate) fn vm_bound_function_to_js_value(function: FunctionValue, this_value: Value) -> JsValue {
    vm_callable_handle_to_js_value(VmJsHandle::BoundFunction(function, this_value))
}

pub(crate) fn vm_native_function_to_js_value(function: NativeFunctionValue) -> JsValue {
    vm_callable_handle_to_js_value(VmJsHandle::NativeFunction(function))
}

pub(crate) fn vm_bound_native_function_to_js_value(
    function: NativeFunctionValue,
    this_value: Value,
) -> JsValue {
    vm_callable_handle_to_js_value(VmJsHandle::BoundNativeFunction(function, this_value))
}

pub(crate) fn vm_class_to_js_value(class: ClassValue) -> JsValue {
    vm_callable_handle_to_js_value(VmJsHandle::Class(class))
}

pub(crate) fn vm_module_to_js_value(
    module: ModuleValue,
    bridge: &JsHostBridge,
) -> Result<JsValue, ExecuteError> {
    let value = js_sys::Object::new().into();
    set_vm_js_handle(&value, VmJsHandle::Module(module.clone()));
    for (key, export) in &module.exports {
        Reflect::set(
            &value,
            &JsValue::from_str(key),
            &value_to_js_value(export, bridge)?,
        )
        .map_err(js_error)?;
    }
    Ok(value)
}

pub(crate) fn vm_js_handle(value: &JsValue) -> Option<VmJsHandle> {
    let id = vm_js_handle_id(value)?;
    VM_JS_HANDLES.with(|handles| handles.borrow().get(&id).cloned())
}

pub(crate) fn update_vm_js_handle<R>(
    value: &JsValue,
    update: impl FnOnce(&mut VmJsHandle) -> R,
) -> Option<R> {
    let id = vm_js_handle_id(value)?;
    VM_JS_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles.get_mut(&id)?;
        Some(update(handle))
    })
}

pub(crate) fn set_vm_js_callback_invoker(invoker: Option<VmJsCallbackInvoker>) {
    VM_JS_CALLBACK_INVOKER.with(|current| {
        *current.borrow_mut() = invoker;
    });
}

#[cfg(feature = "runtime-profile")]
pub(crate) fn install_runtime_profile_panic_hook() {
    RUNTIME_PROFILE_PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            LAST_WASM_PANIC.with(|slot| {
                *slot.borrow_mut() = Some(runtime_profile_panic_message(info));
            });
        }));
    });
}

#[cfg(feature = "runtime-profile")]
fn runtime_profile_panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string());
    if let Some(location) = info.location() {
        format!("{payload} at {}:{}", location.file(), location.line())
    } else {
        payload
    }
}

#[cfg(feature = "runtime-profile")]
fn take_runtime_profile_panic() -> Option<String> {
    LAST_WASM_PANIC.with(|slot| slot.borrow_mut().take())
}

fn vm_callable_handle_to_js_value(handle: VmJsHandle) -> JsValue {
    // 把 VM 内部可调用对象包装成真正的 JS Function。
    //
    // JS 侧调用该 Function 时，会回到 `VM_JS_CALLBACK_INVOKER`，再由 executor 重新进入
    // 对应 bytecode 函数。这是 DOM 事件、Promise then、Array.map 回调能够调用 VM 函数的关键。
    let callback_handle = handle.clone();
    let captured_invoker = VM_JS_CALLBACK_INVOKER.with(|invoker| invoker.borrow().clone());
    let callback = Closure::wrap(
        Box::new(move |this_value: JsValue, args: JsArray| -> JsValue {
            trace_vm_callback("start", &callback_handle);
            let callback_invoker = captured_invoker
                .clone()
                .or_else(|| VM_JS_CALLBACK_INVOKER.with(|invoker| invoker.borrow().clone()));
            if let Some(invoker) = callback_invoker.as_ref() {
                let args = args.iter().map(Value::JsValue).collect::<Vec<_>>();
                match invoker(callback_handle.clone(), this_value, args) {
                    Ok(value) => {
                        trace_vm_callback("end", &callback_handle);
                        value
                    }
                    Err(error) => {
                        trace_vm_callback_error(&callback_handle, &error);
                        vm_callback_thrown_result(error)
                    }
                }
            } else {
                trace_vm_callback("missing-invoker", &callback_handle);
                JsValue::UNDEFINED
            }
        }) as Box<dyn Fn(JsValue, JsArray) -> JsValue>,
    );
    let value = JsFunction::new_with_args(
        "__invoke",
        "return function(...args){ const r = __invoke(this, args); if (r && r.__js_vm_thrown) throw r.value; return r; };",
    )
    .call1(&JsValue::UNDEFINED, callback.as_ref())
    .unwrap_or_else(|_| callback.as_ref().clone());
    callback.forget();
    set_vm_js_handle(&value, handle);
    value
}

fn vm_callback_thrown_result(error: JsValue) -> JsValue {
    if Reflect::get(&error, &JsValue::from_str(VM_THROWN_MARKER_KEY))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return error;
    }
    let object = js_sys::Object::new();
    let _ = Reflect::set(
        &object,
        &JsValue::from_str(VM_THROWN_MARKER_KEY),
        &JsValue::TRUE,
    );
    let _ = Reflect::set(&object, &JsValue::from_str(VM_THROWN_VALUE_KEY), &error);
    object.into()
}

fn trace_vm_callback(event: &str, handle: &VmJsHandle) {
    let enabled = Reflect::get(
        &js_sys::global(),
        &JsValue::from_str("__jsVmTraceCallbacks"),
    )
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false);
    if !enabled {
        return;
    }
    wasm_host_log(
        "debug",
        &format!("VM_CALLBACK {event} {}", vm_js_handle_label(handle)),
    );
}

fn trace_host_call(event: &str, detail: &str) {
    let enabled = Reflect::get(&js_sys::global(), &JsValue::from_str("__jsVmTraceHost"))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if enabled {
        wasm_host_log("debug", &format!("HOST {event} {detail}"));
    }
}

fn trace_vm_callback_error(handle: &VmJsHandle, error: &JsValue) {
    let enabled = Reflect::get(
        &js_sys::global(),
        &JsValue::from_str("__jsVmTraceCallbacks"),
    )
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false);
    if !enabled {
        return;
    }
    let message = Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| js_value_display(error));
    wasm_host_log(
        "debug",
        &format!(
            "VM_CALLBACK error {} message={message}",
            vm_js_handle_label(handle)
        ),
    );
}

fn vm_js_handle_label(handle: &VmJsHandle) -> String {
    match handle {
        VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => format!(
            "function name={} body={}..{}",
            function.name.as_deref().unwrap_or("<anonymous>"),
            function.body_start,
            function.body_end
        ),
        VmJsHandle::NativeFunction(function) | VmJsHandle::BoundNativeFunction(function, _) => {
            format!("native name={}", function.name)
        }
        VmJsHandle::Class(class) => format!(
            "class name={}",
            class.name.as_deref().unwrap_or("<anonymous>")
        ),
        VmJsHandle::Module(module) => format!("module source={}", module.source),
    }
}

const VM_JS_HANDLE_ID_KEY: &str = "__js_vm_handle_id";

fn vm_js_handle_id(value: &JsValue) -> Option<u32> {
    VM_JS_HANDLE_VALUES.with(|values| {
        values
            .borrow()
            .iter()
            .find_map(|(id, candidate)| js_sys::Object::is(candidate, value).then_some(*id))
    })
}

fn set_vm_js_handle(value: &JsValue, handle: VmJsHandle) {
    let id = NEXT_VM_JS_HANDLE_ID.with(|next| {
        let mut next = next.borrow_mut();
        let id = *next;
        *next = next.saturating_add(1);
        id
    });
    VM_JS_HANDLES.with(|handles| {
        handles.borrow_mut().insert(id, handle);
    });
    VM_JS_HANDLE_VALUES.with(|values| {
        values.borrow_mut().push((id, value.clone()));
    });
    let _ = Reflect::set(
        value,
        &JsValue::from_str(VM_JS_HANDLE_ID_KEY),
        &JsValue::from_f64(id as f64),
    );
}
pub(crate) fn is_global_external_root(root: &str) -> bool {
    matches!(root, "window" | "globalThis" | "self" | "global")
}
pub(crate) fn host_overlay_get(path: &str) -> Option<Value> {
    HOST_VALUE_OVERLAY.with(|overlay| overlay.borrow().get(path).cloned())
}
pub(crate) fn clear_host_overlays() {
    HOST_VALUE_OVERLAY.with(|overlay| overlay.borrow_mut().clear());
    JS_VALUE_OVERLAY.with(|overlay| overlay.borrow_mut().clear());
    JS_SYMBOL_PROPERTY_KEYS.with(|keys| keys.borrow_mut().clear());
    NEXT_JS_SYMBOL_PROPERTY_KEY_ID.with(|next| *next.borrow_mut() = 1);
}
pub(crate) fn host_overlay_set_path(path: &str, value: Value) {
    HOST_VALUE_OVERLAY.with(|overlay| {
        overlay.borrow_mut().insert(path.to_string(), value);
    });
}
pub(crate) fn external_name_overlay_get(name: &str) -> Option<Value> {
    host_overlay_get(name).or_else(|| global_overlay_get(name))
}
pub(crate) fn global_overlay_get(property: &str) -> Option<Value> {
    ["window", "globalThis", "self"]
        .into_iter()
        .find_map(|root| host_overlay_get(&format!("{root}.{property}")))
}
pub(crate) fn host_overlay_set(reference: &ExternalRefValue, property: &str, value: &Value) {
    // 给全局对象写属性时，同时保存短别名。这样 script 模式下的 `window.$ = ...`
    // 后续可以通过顶层 `$` 读取到同一个 VM 内部值。
    let next = reference.member(property);
    HOST_VALUE_OVERLAY.with(|overlay| {
        let mut overlay = overlay.borrow_mut();
        overlay.insert(next.display_path(), value.clone());
        if reference.path.is_empty() {
            overlay.insert(property.to_string(), value.clone());
        } else if let Some(alias) = global_property_alias(reference, property) {
            overlay.insert(alias, value.clone());
        }
    });
}

pub(crate) fn js_overlay_get(target: &JsValue, property: &str) -> Option<Value> {
    let id = js_overlay_id(target, false)?;
    JS_VALUE_OVERLAY.with(|overlay| overlay.borrow().get(&(id, property.to_string())).cloned())
}

pub(crate) fn js_overlay_get_in_prototype_chain(target: &JsValue, property: &str) -> Option<Value> {
    let mut current = target.clone();
    loop {
        if let Some(value) = js_overlay_get(&current, property) {
            return Some(value);
        }
        let prototype = JsFunction::new_with_args("value", "return Object.getPrototypeOf(value);")
            .call1(&JsValue::UNDEFINED, &current)
            .ok()?;
        if prototype.is_null() || prototype.is_undefined() {
            return None;
        }
        current = prototype;
    }
}

pub(crate) fn js_overlay_set(target: &JsValue, property: &str, value: Value) {
    let Some(id) = js_overlay_id(target, true) else {
        return;
    };
    JS_VALUE_OVERLAY.with(|overlay| {
        overlay
            .borrow_mut()
            .insert((id, property.to_string()), value);
    });
}

pub(crate) fn js_typed_array_index_get(target: &JsValue, property: &str) -> Option<Value> {
    js_typed_array_u32_index_get(target, typed_array_index(property)?)
}

pub(crate) fn js_typed_array_u32_index_get(target: &JsValue, index: u32) -> Option<Value> {
    if let Some(array) = target.dyn_ref::<Uint8Array>() {
        return Some(if index < array.length() {
            Value::Number(f64::from(array.get_index(index)))
        } else {
            Value::Undefined
        });
    }
    if let Some(array) = target.dyn_ref::<Uint16Array>() {
        return Some(if index < array.length() {
            Value::Number(f64::from(array.get_index(index)))
        } else {
            Value::Undefined
        });
    }
    if let Some(array) = target.dyn_ref::<Int32Array>() {
        return Some(if index < array.length() {
            Value::Number(f64::from(array.get_index(index)))
        } else {
            Value::Undefined
        });
    }
    if let Some(array) = target.dyn_ref::<Float32Array>() {
        return Some(if index < array.length() {
            Value::Number(f64::from(array.get_index(index)))
        } else {
            Value::Undefined
        });
    }
    None
}

pub(crate) fn js_typed_array_index_set(
    target: &JsValue,
    property: &str,
    value: &Value,
) -> Option<Result<(), ExecuteError>> {
    js_typed_array_u32_index_set(target, typed_array_index(property)?, value)
}

pub(crate) fn js_typed_array_u32_index_set(
    target: &JsValue,
    index: u32,
    value: &Value,
) -> Option<Result<(), ExecuteError>> {
    let number = value.to_number();
    if let Some(array) = target.dyn_ref::<Uint8Array>() {
        if index < array.length() {
            array.set_index(index, number as u8);
        }
        return Some(Ok(()));
    }
    if let Some(array) = target.dyn_ref::<Uint16Array>() {
        if index < array.length() {
            array.set_index(index, number as u16);
        }
        return Some(Ok(()));
    }
    if let Some(array) = target.dyn_ref::<Int32Array>() {
        if index < array.length() {
            array.set_index(index, number as i32);
        }
        return Some(Ok(()));
    }
    if let Some(array) = target.dyn_ref::<Float32Array>() {
        if index < array.length() {
            array.set_index(index, number as f32);
        }
        return Some(Ok(()));
    }
    None
}

fn typed_array_index(property: &str) -> Option<u32> {
    if property.is_empty() {
        return None;
    }
    if property.len() > 1 && property.starts_with('0') {
        return None;
    }
    property.parse::<u32>().ok()
}

fn js_overlay_id(target: &JsValue, create: bool) -> Option<u32> {
    const KEY: &str = "__js_vm_overlay_id";
    if !is_js_property_target(target) {
        return None;
    }
    if let Ok(value) = Reflect::get(target, &JsValue::from_str(KEY))
        && let Some(id) = value.as_f64()
        && id > 0.0
    {
        return Some(id as u32);
    }
    if !create {
        return None;
    }
    let id = NEXT_JS_OVERLAY_ID.with(|next| {
        let mut next = next.borrow_mut();
        let id = *next;
        *next = next.saturating_add(1);
        id
    });
    if Reflect::set(
        target,
        &JsValue::from_str(KEY),
        &JsValue::from_f64(id as f64),
    )
    .is_err()
    {
        return None;
    }
    Some(id)
}

pub(crate) fn global_property_alias(
    reference: &ExternalRefValue,
    property: &str,
) -> Option<String> {
    if is_global_external_root(&reference.root) && reference.path.is_empty() {
        Some(property.to_string())
    } else {
        None
    }
}
pub(crate) fn can_represent_value_as_js(value: &Value) -> bool {
    match value {
        Value::Number(_)
        | Value::String(_)
        | Value::Symbol(_)
        | Value::Bool(_)
        | Value::JsValue(_)
        | Value::BoundJsFunction(_, _)
        | Value::Null
        | Value::Undefined
        | Value::Array(_)
        | Value::Object(_)
        | Value::ExternalRef(_)
        | Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::Class(_)
        | Value::Module(_)
        | Value::GeneratorState(_) => true,
        #[cfg(feature = "bigint")]
        Value::BigInt(_) => true,
        #[cfg(not(feature = "bigint"))]
        Value::BigInt(_) => false,
    }
}
pub(crate) fn is_js_property_target(value: &JsValue) -> bool {
    !value.is_null() && matches!(js_value_typeof(value), "object" | "function")
}
pub(crate) fn is_js_boxable_primitive(value: &JsValue) -> bool {
    let is_bigint = js_value_has_bigint_type(value);
    !value.is_null()
        && !value.is_undefined()
        && (value.as_bool().is_some()
            || value.as_f64().is_some()
            || value.as_string().is_some()
            || is_bigint
            || js_value_is_symbol(value))
}
pub(crate) fn get_js_property(target: &JsValue, property: &str) -> Result<JsValue, ExecuteError> {
    let property_key = js_reflect_property_key(property);
    get_js_property_with_key(target, &property_key)
}

pub(crate) fn get_js_property_with_key(
    target: &JsValue,
    property_key: &JsValue,
) -> Result<JsValue, ExecuteError> {
    // Reflect.get 对 primitive receiver 的处理在不同宿主 API 上容易踩到 Illegal invocation。
    // 这里先尝试原值，失败后用 Object(value) 装箱，尽量贴近 JavaScript 的属性读取语义。
    if target.is_null() || target.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    let result = reflect_get_with_receiver(target, property_key, target);
    if is_js_boxable_primitive(target) {
        if let Ok(value) = result {
            return Ok(value);
        }
        let boxed = js_object(target);
        return reflect_get_with_receiver(&boxed, property_key, &boxed).map_err(js_error);
    } else {
        result.map_err(js_error)
    }
}

pub(crate) fn js_reflect_property_key(property: &str) -> JsValue {
    if let Some(symbol) = js_symbol_property_key_from_token(property) {
        return symbol;
    }
    if let Some(name) = symbol_property_name(property) {
        return JsFunction::new_with_args("name", "return Symbol[name];")
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(name))
            .ok()
            .filter(|value| !value.is_undefined())
            .unwrap_or_else(|| JsValue::from_str(property));
    }
    JsValue::from_str(property)
}

pub(crate) fn symbol_value_to_js_value(value: &str) -> Result<JsValue, ExecuteError> {
    let key = js_reflect_property_key(value);
    if js_value_typeof(&key) == "symbol" {
        return Ok(key);
    }
    JsFunction::new_with_args("description", "return Symbol(description);")
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(value))
        .map_err(js_error)
}

fn symbol_property_name(property: &str) -> Option<&str> {
    if let Some(name) = property.strip_prefix("Symbol.") {
        return Some(name);
    }
    if let Some(name) = property
        .strip_prefix("Symbol(")
        .and_then(|value| value.strip_suffix(")"))
        .and_then(well_known_symbol_name)
    {
        return Some(name);
    }
    None
}

fn js_symbol_property_key_from_token(property: &str) -> Option<JsValue> {
    let id = property
        .strip_prefix(JS_SYMBOL_PROPERTY_KEY_PREFIX)?
        .parse::<u32>()
        .ok()?;
    JS_SYMBOL_PROPERTY_KEYS.with(|keys| {
        keys.borrow()
            .iter()
            .find_map(|(key_id, value)| (*key_id == id).then(|| value.clone()))
    })
}

fn js_symbol_property_key_token(value: &JsValue) -> String {
    JS_SYMBOL_PROPERTY_KEYS.with(|keys| {
        let mut keys = keys.borrow_mut();
        if let Some((id, _)) = keys
            .iter()
            .find(|(_, candidate)| js_values_strict_equal(candidate, value))
        {
            return format!("{JS_SYMBOL_PROPERTY_KEY_PREFIX}{id}");
        }
        let id = NEXT_JS_SYMBOL_PROPERTY_KEY_ID.with(|next| {
            let mut next = next.borrow_mut();
            let id = *next;
            *next = next.saturating_add(1);
            id
        });
        keys.push((id, value.clone()));
        format!("{JS_SYMBOL_PROPERTY_KEY_PREFIX}{id}")
    })
}

fn js_values_strict_equal(left: &JsValue, right: &JsValue) -> bool {
    JsFunction::new_with_args("left, right", "return left === right;")
        .call2(&JsValue::UNDEFINED, left, right)
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn well_known_symbol_name(description: &str) -> Option<&str> {
    match description {
        "asyncIterator" | "hasInstance" | "isConcatSpreadable" | "iterator" | "match"
        | "matchAll" | "replace" | "search" | "species" | "split" | "toPrimitive"
        | "toStringTag" | "unscopables" => Some(description),
        _ => None,
    }
}

#[cfg(feature = "array-builtins")]
pub(crate) fn js_array_prototype_symbol_iterator() -> Option<JsValue> {
    JsFunction::new_no_args(
        "const it = Array.prototype[Symbol.iterator]; return it === Array.prototype.values ? undefined : it;",
    )
        .call0(&JsValue::UNDEFINED)
        .ok()
        .filter(|value| !value.is_undefined() && !value.is_null())
}

#[cfg(feature = "array-builtins")]
pub(crate) fn js_array_prototype_has_symbol_iterator() -> bool {
    JsFunction::new_no_args("return Array.prototype[Symbol.iterator] !== undefined;")
        .call0(&JsValue::UNDEFINED)
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn js_value_is_array_prototype(value: &JsValue) -> bool {
    JsFunction::new_with_args("value", "return value === Array.prototype;")
        .call1(&JsValue::UNDEFINED, value)
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn js_value_display(value: &JsValue) -> String {
    if value.is_undefined() {
        "undefined".to_string()
    } else if value.is_null() {
        "null".to_string()
    } else if let Some(value) = value.as_string() {
        value
    } else if let Some(value) = value.as_f64() {
        value.to_string()
    } else if let Some(value) = value.as_bool() {
        value.to_string()
    } else if js_value_has_bigint_type(value) {
        JsFunction::new_with_args("value", "return String(value);")
            .call1(&JsValue::UNDEFINED, value)
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_else(|| "[bigint]".to_string())
    } else if js_value_is_symbol(value) {
        let description = js_symbol_description(value);
        if description.is_empty() {
            "Symbol()".to_string()
        } else {
            format!("Symbol({description})")
        }
    } else {
        let message = Reflect::get(value, &JsValue::from_str("message"))
            .ok()
            .and_then(|value| value.as_string());
        if let Some(message) = message {
            let name = Reflect::get(value, &JsValue::from_str("name"))
                .ok()
                .and_then(|value| value.as_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Error".to_string());
            return format!("{name}: {message}");
        }
        JsFunction::new_with_args(
            "value",
            "try { return String(value); } catch (_) { return '[object]'; }",
        )
        .call1(&JsValue::UNDEFINED, value)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "[object]".to_string())
    }
}

pub(crate) fn js_value_is_truthy(value: &JsValue) -> bool {
    if value.is_undefined() || value.is_null() {
        false
    } else if let Some(value) = value.as_bool() {
        value
    } else if let Some(value) = value.as_f64() {
        value != 0.0 && !value.is_nan()
    } else if let Some(value) = value.as_string() {
        !value.is_empty()
    } else {
        true
    }
}
pub(crate) fn js_value_to_number(value: &JsValue) -> f64 {
    if value.is_undefined() {
        f64::NAN
    } else if value.is_null() {
        0.0
    } else if let Some(value) = value.as_bool() {
        f64::from(value as u8)
    } else if let Some(value) = value.as_f64() {
        value
    } else if let Some(value) = value.as_string() {
        value.trim().parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}
pub(crate) fn js_value_property_key(value: &JsValue) -> String {
    if let Some(number) = value.as_f64()
        && number.is_finite()
        && number.fract() == 0.0
    {
        return format!("{}", number as i64);
    }
    if js_value_typeof(value) == "symbol" {
        let description = js_symbol_description(value);
        if description.starts_with("Symbol.") {
            return description;
        }
        if let Some(name) = well_known_symbol_name(&description) {
            return format!("Symbol.{name}");
        }
        return js_symbol_property_key_token(value);
    }
    js_value_display(value)
}
pub(crate) fn js_value_typeof(value: &JsValue) -> &'static str {
    if value.is_undefined() {
        "undefined"
    } else if value.is_null() {
        "object"
    } else if value.as_bool().is_some() {
        "boolean"
    } else if value.as_f64().is_some() {
        "number"
    } else if value.as_string().is_some() {
        "string"
    } else if value.dyn_ref::<JsFunction>().is_some() {
        "function"
    } else if js_value_has_bigint_type(value) {
        "bigint"
    } else if js_typeof(value) == "symbol" {
        "symbol"
    } else {
        "object"
    }
}

pub(crate) fn js_value_is_symbol(value: &JsValue) -> bool {
    js_value_typeof(value) == "symbol"
}

fn js_value_has_bigint_type(value: &JsValue) -> bool {
    #[cfg(feature = "bigint")]
    {
        js_typeof(value) == "bigint"
    }
    #[cfg(not(feature = "bigint"))]
    {
        let _ = value;
        false
    }
}

#[cfg(feature = "bigint")]
pub(crate) fn js_value_is_bigint(value: &JsValue) -> bool {
    js_value_has_bigint_type(value)
}

#[cfg(feature = "regexp")]
pub(crate) fn js_value_is_regexp(value: &JsValue) -> bool {
    value.dyn_ref::<js_sys::RegExp>().is_some()
}

#[cfg(feature = "regexp")]
pub(crate) fn js_regexp_test(value: &JsValue, input: &str) -> Option<bool> {
    value
        .dyn_ref::<js_sys::RegExp>()
        .map(|regexp| regexp.test(input))
}

#[cfg(feature = "regexp")]
pub(crate) fn js_regexp_exec(value: &JsValue, input: &str) -> Option<JsValue> {
    let regexp = value.dyn_ref::<js_sys::RegExp>()?;
    Some(
        regexp
            .exec(input)
            .map(JsValue::from)
            .unwrap_or(JsValue::NULL),
    )
}

#[cfg(feature = "bigint")]
pub(crate) fn js_bigint_builtin_name(value: &JsValue) -> Option<&'static str> {
    let name = JsFunction::new_with_args(
        "value",
        "if (value === BigInt) return 'BigInt';\
         if (value === BigInt.asIntN) return 'BigInt.asIntN';\
         if (value === BigInt.asUintN) return 'BigInt.asUintN';\
         if (typeof value === 'function' && value.name === 'BigInt') return 'BigInt';\
         if (typeof value === 'function' && value.name === 'asIntN') return 'BigInt.asIntN';\
         if (typeof value === 'function' && value.name === 'asUintN') return 'BigInt.asUintN';\
         return '';",
    )
    .call1(&JsValue::UNDEFINED, value)
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_default();
    match name.as_str() {
        "BigInt" => Some("BigInt"),
        "BigInt.asIntN" => Some("BigInt.asIntN"),
        "BigInt.asUintN" => Some("BigInt.asUintN"),
        _ => None,
    }
}

pub(crate) fn js_function_name(value: &JsValue) -> Option<String> {
    let name = JsFunction::new_with_args(
        "value",
        "return typeof value === 'function' ? (value.name || '') : '';",
    )
    .call1(&JsValue::UNDEFINED, value)
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_default();
    if name.is_empty() { None } else { Some(name) }
}

fn js_is_function_prototype_member(value: &JsValue, name: &str) -> bool {
    JsFunction::new_with_args(
        "value, name",
        "return typeof Function === 'function' && value === Function.prototype[name];",
    )
    .call2(&JsValue::UNDEFINED, value, &JsValue::from_str(name))
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false)
}

fn js_typeof(value: &JsValue) -> String {
    JsFunction::new_with_args("value", "return typeof value;")
        .call1(&JsValue::UNDEFINED, value)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "object".to_string())
}

fn js_symbol_description(value: &JsValue) -> String {
    JsFunction::new_with_args(
        "value",
        "return value.description === undefined ? '' : String(value.description);",
    )
    .call1(&JsValue::UNDEFINED, value)
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_default()
}
#[cfg(feature = "object-builtins")]
pub(crate) fn js_object_to_string_tag(value: &JsValue) -> String {
    Reflect::get(
        &js_object(&JsValue::UNDEFINED),
        &JsValue::from_str("toString"),
    )
    .ok()
    .and_then(|function| function.dyn_into::<JsFunction>().ok())
    .and_then(|function| function.call0(value).ok())
    .and_then(|value| value.as_string())
    .unwrap_or_else(|| "[object Object]".to_string())
}
#[cfg(feature = "function-builtins")]
pub(crate) fn js_function_source_string(value: &JsValue) -> String {
    Reflect::get(value, &JsValue::from_str("toString"))
        .ok()
        .and_then(|function| function.dyn_into::<JsFunction>().ok())
        .and_then(|function| function.call0(value).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| js_value_display(value))
}
pub(crate) fn js_instance_of(value: &Value, constructor: &JsValue) -> Result<bool, ExecuteError> {
    let js_value = value_to_js_value(value, &JsHostBridge::empty())?;
    js_sys::Function::new_with_args("value, constructor", "return value instanceof constructor;")
        .call2(&JsValue::UNDEFINED, &js_value, constructor)
        .map_err(js_error)
        .map(|value| value.as_bool().unwrap_or(false))
}
pub(crate) fn js_error(value: JsValue) -> ExecuteError {
    if Reflect::get(&value, &JsValue::from_str(VM_THROWN_MARKER_KEY))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        let thrown = Reflect::get(&value, &JsValue::from_str(VM_THROWN_VALUE_KEY))
            .unwrap_or(JsValue::UNDEFINED);
        return ExecuteError::Thrown(Value::JsValue(thrown));
    }
    let name = Reflect::get(&value, &JsValue::from_str("name"))
        .ok()
        .and_then(|value| value.as_string());
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
    #[cfg(feature = "runtime-profile")]
    if name.as_deref() == Some("RuntimeError")
        && let Some(panic) = take_runtime_profile_panic()
    {
        return ExecuteError::Runtime(format!("{message}; wasm panic: {panic}"));
    }
    match name.as_deref() {
        Some("TypeError") => ExecuteError::TypeError(message),
        Some("RangeError") => ExecuteError::RangeError(message),
        Some("ReferenceError") => ExecuteError::ReferenceError(message),
        Some("SyntaxError") => ExecuteError::SyntaxError(message),
        _ => ExecuteError::Thrown(Value::JsValue(value)),
    }
}

pub(crate) fn execute_error_to_js_value(error: &ExecuteError, bridge: &JsHostBridge) -> JsValue {
    match error {
        ExecuteError::Thrown(Value::JsValue(value)) if js_value_is_native_error(value) => {
            value.clone()
        }
        ExecuteError::Thrown(value) => {
            let object = js_sys::Object::new();
            let _ = Reflect::set(
                &object,
                &JsValue::from_str(VM_THROWN_MARKER_KEY),
                &JsValue::TRUE,
            );
            let thrown = value_to_js_value(value, bridge).unwrap_or_else(|_| JsValue::UNDEFINED);
            let _ = Reflect::set(&object, &JsValue::from_str(VM_THROWN_VALUE_KEY), &thrown);
            object.into()
        }
        _ => JsValue::from_str(&error.to_string()),
    }
}

fn js_value_is_native_error(value: &JsValue) -> bool {
    value.dyn_ref::<JsError>().is_some()
}
