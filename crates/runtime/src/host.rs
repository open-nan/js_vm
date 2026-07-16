use crate::error::ExecuteError;
use crate::ops::get_local_member;
#[cfg(feature = "proxy")]
use crate::value::object_value;
use crate::value::{
    ClassValue, ExternalRefValue, FunctionValue, ModuleValue, NativeFunctionValue, Value,
};
use js_sys::{Array as JsArray, Function as JsFunction, Reflect};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
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
    static HOST_VALUE_OVERLAY: RefCell<BTreeMap<String, Value>> = RefCell::new(BTreeMap::new());
    static JS_VALUE_OVERLAY: RefCell<BTreeMap<(u32, String), Value>> = RefCell::new(BTreeMap::new());
    static NEXT_JS_OVERLAY_ID: RefCell<u32> = const { RefCell::new(1) };
    static VM_JS_HANDLES: RefCell<BTreeMap<u32, VmJsHandle>> = RefCell::new(BTreeMap::new());
    static NEXT_VM_JS_HANDLE_ID: RefCell<u32> = const { RefCell::new(1) };
}

#[derive(Debug, Clone)]
pub(crate) enum VmJsHandle {
    Function(FunctionValue),
    BoundFunction(FunctionValue, Value),
    NativeFunction(NativeFunctionValue),
    BoundNativeFunction(NativeFunctionValue, Value),
    Class(ClassValue),
    Module(ModuleValue),
}

#[derive(Debug, Clone)]
pub struct JsHostBridge {
    values: Rc<RefCell<Vec<JsValue>>>,
}

pub trait HostValue {
    fn display(&self) -> String;
    fn is_truthy(&self) -> bool;
    fn to_number(&self) -> f64;
    fn property_key(&self) -> String;
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

pub trait HostBridge: Clone {
    fn validate_extern_count(&self, expected: usize) -> Result<(), ExecuteError>;
    fn validate_slot(&self, slot: u32) -> Result<(), ExecuteError>;
    fn read_external(&self, reference: &ExternalRefValue) -> Result<Value, ExecuteError>;
    fn get(&self, reference: &ExternalRefValue, property: &str) -> Result<Value, ExecuteError>;
    fn set(
        &self,
        reference: &ExternalRefValue,
        property: &str,
        value: &Value,
    ) -> Result<(), ExecuteError>;
    fn set_slot(&self, slot: u32, value: &Value) -> Result<(), ExecuteError>;
    fn call(&self, reference: &ExternalRefValue, args: Vec<Value>) -> Result<Value, ExecuteError>;
    fn call_with_this(
        &self,
        reference: &ExternalRefValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    fn construct(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    fn call_js_value(
        &self,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    fn is_object_define_property_function(&self, callee: &JsValue, this_value: &JsValue) -> bool;
    fn construct_js_value(
        &self,
        constructor: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    fn binary_operator(&self, op: &str, left: &Value, right: &Value)
    -> Result<Value, ExecuteError>;
    #[cfg(feature = "string-builtins")]
    fn call_string_method(
        &self,
        value: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError>;
    fn function_value(&self, function: FunctionValue) -> Value;
    fn class_value(&self, class: ClassValue) -> Value;
    fn module_value(&self, module: ModuleValue) -> Result<Value, ExecuteError>;
}

impl Default for JsHostBridge {
    fn default() -> Self {
        Self::empty()
    }
}

impl JsHostBridge {
    pub fn empty() -> Self {
        Self {
            values: Rc::new(RefCell::new(Vec::new())),
        }
    }
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
        let value = get_js_property(&base, property)?;
        if value.dyn_ref::<JsFunction>().is_some() {
            Ok(Value::BoundJsFunction(value, base))
        } else {
            Ok(Value::JsValue(value))
        }
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
        let js_args = values_to_js_array(&args, self)?;
        let result = function.apply(&this_value, &js_args).map_err(js_error)?;
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
        let js_args = values_to_js_array(&args, self)?;
        let result = function.apply(&js_this, &js_args).map_err(js_error)?;
        Ok(Value::JsValue(result))
    }
    pub(crate) fn call_js_value(
        &self,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let Some(function) = callee.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not callable",
                js_value_display(&callee)
            ));
        };
        let js_args = values_to_js_array(&args, self)?;
        let result = function.apply(&this_value, &js_args).map_err(js_error)?;
        Ok(Value::JsValue(result))
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
        #[cfg(feature = "proxy")]
        if is_js_proxy_constructor(&constructor) {
            return Ok(args
                .into_iter()
                .next()
                .unwrap_or_else(|| object_value(BTreeMap::new())));
        }
        let Some(function) = constructor.dyn_ref::<JsFunction>() else {
            return Err(crate::error::type_error!(
                "{} is not constructable",
                js_value_display(&constructor)
            ));
        };
        let js_args = values_to_js_array(&args, self)?;
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
        #[cfg(feature = "proxy")]
        if is_js_proxy_constructor(&constructor) {
            return Ok(args
                .into_iter()
                .next()
                .unwrap_or_else(|| object_value(BTreeMap::new())));
        }
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
        let property = reference.path.last().expect("checked non-empty path");
        let callee = get_js_property(&this_value, property)?;
        Ok((callee, this_value))
    }
}

impl HostBridge for JsHostBridge {
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
        vm_module_to_js_value(module).map(Value::JsValue)
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
pub fn value_to_js_value(value: &Value, bridge: &JsHostBridge) -> Result<JsValue, ExecuteError> {
    Ok(match value {
        Value::Number(value) => JsValue::from_f64(*value),
        #[cfg(feature = "bigint")]
        Value::BigInt(value) => JsFunction::new_with_args("value", "return BigInt(value);")
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(value))
            .map_err(js_error)?,
        #[cfg(not(feature = "bigint"))]
        Value::BigInt(_) => JsValue::UNDEFINED,
        Value::String(value) | Value::Symbol(value) => JsValue::from_str(value),
        Value::Bool(value) => JsValue::from_bool(*value),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => value.clone(),
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
            for (key, value) in props.borrow().iter() {
                Reflect::set(
                    &object,
                    &js_reflect_property_key(key),
                    &value_to_js_value(value, bridge)?,
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
        Value::Module(module) => vm_module_to_js_value(module.clone())?,
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

pub(crate) fn vm_module_to_js_value(module: ModuleValue) -> Result<JsValue, ExecuteError> {
    let value = js_sys::Object::new().into();
    set_vm_js_handle(&value, VmJsHandle::Module(module.clone()));
    for (key, export) in &module.exports {
        Reflect::set(
            &value,
            &JsValue::from_str(key),
            &value_to_js_value(export, &JsHostBridge::empty())?,
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

fn vm_callable_handle_to_js_value(handle: VmJsHandle) -> JsValue {
    let value = JsFunction::new_no_args("").into();
    set_vm_js_handle(&value, handle);
    value
}

const VM_JS_HANDLE_ID_KEY: &str = "__js_vm_handle_id";

fn vm_js_handle_id(value: &JsValue) -> Option<u32> {
    Reflect::get(value, &JsValue::from_str(VM_JS_HANDLE_ID_KEY))
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|id| *id > 0.0)
        .map(|id| id as u32)
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
    !value.is_null()
        && !value.is_undefined()
        && (value.is_object() || value.dyn_ref::<JsFunction>().is_some())
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
    if !is_js_property_target(target) && !is_js_boxable_primitive(target) {
        return Ok(JsValue::UNDEFINED);
    }
    if is_js_property_target(target) {
        reflect_get_with_receiver(target, &js_reflect_property_key(property), target)
            .map_err(js_error)
    } else {
        let boxed = js_object(target);
        reflect_get_with_receiver(&boxed, &js_reflect_property_key(property), &boxed)
            .map_err(js_error)
    }
}

pub(crate) fn js_reflect_property_key(property: &str) -> JsValue {
    if let Some(name) = property.strip_prefix("Symbol.") {
        return JsFunction::new_with_args("name", "return Symbol[name];")
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(name))
            .ok()
            .filter(|value| !value.is_undefined())
            .unwrap_or_else(|| JsValue::from_str(property));
    }
    JsValue::from_str(property)
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
        "[object]".to_string()
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
        return js_value_display(value);
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
#[cfg(feature = "proxy")]
pub(crate) fn is_js_proxy_constructor(value: &JsValue) -> bool {
    Reflect::get(&js_sys::global(), &JsValue::from_str("Proxy"))
        .ok()
        .is_some_and(|proxy| js_sys::Object::is(value, &proxy))
}
pub(crate) fn js_error(value: JsValue) -> ExecuteError {
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
    match name.as_deref() {
        Some("TypeError") => ExecuteError::TypeError(message),
        Some("RangeError") => ExecuteError::RangeError(message),
        Some("ReferenceError") => ExecuteError::ReferenceError(message),
        Some("SyntaxError") => ExecuteError::SyntaxError(message),
        _ => ExecuteError::Runtime(message),
    }
}
