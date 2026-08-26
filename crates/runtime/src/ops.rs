//! 执行器语义 helper。
//!
//! `executor.rs` 负责 pc 调度和控制流，这个模块集中处理可复用的 JS 语义：
//! 操作数读取、常量转换、成员访问、二元/一元运算、参数展开、try 区间识别和对象/函数辅助逻辑。
//!
//! 优先原则：
//! - 简单 number/string/bool 路径在 VM 内快速处理。
//! - 对象、数组、函数、Symbol、BigInt 等复杂 coercion 尽量下沉到 `JsValue`/HostBridge。

use crate::error::ExecuteError;
#[cfg(feature = "object-builtins")]
use crate::host::js_object_to_string_tag;
use crate::host::{
    JsHostBridge, VmJsHandle, can_represent_value_as_js, get_js_property, host_overlay_set_path,
    is_js_boxable_primitive, is_js_property_target, js_error, js_instance_of, js_overlay_get,
    js_overlay_set, js_reflect_property_key, js_typed_array_index_set, js_value_display,
    js_value_is_array_prototype, js_value_property_key, js_value_typeof, update_vm_js_handle,
    value_to_js_value, vm_bound_function_to_js_value, vm_bound_native_function_to_js_value,
    vm_js_handle,
};
#[cfg(feature = "array-builtins")]
use crate::host::{
    host_overlay_get, js_array_prototype_has_symbol_iterator, js_array_prototype_symbol_iterator,
};
#[cfg(feature = "function-builtins")]
use crate::host::{js_function_source_string, js_value_to_number};
#[cfg(feature = "regexp")]
use crate::value::array_value;
use crate::value::{ClassValue, NativeFunctionValue, Value};
#[cfg(feature = "function-builtins")]
use js_sys::Array as JsArray;
use js_sys::{Function as JsFunction, Reflect};
use js_token_core::{
    BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue};

/// `try/catch/finally` 在扁平 bytecode 中的区间划分。
///
/// Core 当前仍用结构标记指令表示 try 区域，执行器进入 try 时会扫描出 body/catch/finally/end
/// 的 pc 区间，然后按 JS 异常语义调度。
pub(crate) struct TryParts {
    pub(crate) body_start: usize,
    pub(crate) body_end: usize,
    pub(crate) catch_start: usize,
    pub(crate) catch_end: usize,
    pub(crate) catch_param: Option<BytecodeOperand>,
    pub(crate) finally_start: usize,
    pub(crate) finally_end: usize,
    pub(crate) end: usize,
}

pub(crate) fn find_try_parts(
    module: &BytecodeModule,
    try_start: usize,
) -> Result<TryParts, ExecuteError> {
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
            BytecodeOperand::Name(_) | BytecodeOperand::LocalSlot(_) => {
                Ok(Some(operand(&module.instructions[index], 0)?.clone()))
            }
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

pub(crate) struct ScopeMetadata {
    pub(crate) instruction_scope_depths: Vec<usize>,
}

pub(crate) fn collect_scope_metadata(
    module: &BytecodeModule,
) -> Result<ScopeMetadata, ExecuteError> {
    let mut instruction_scope_depths = Vec::with_capacity(module.instructions.len());
    let mut scope_depth = 0usize;
    for instruction in &module.instructions {
        instruction_scope_depths.push(scope_depth);
        match instruction.op {
            BytecodeOp::EnterScope => scope_depth += 1,
            BytecodeOp::LeaveScope => scope_depth = scope_depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(ScopeMetadata {
        instruction_scope_depths,
    })
}

pub(crate) fn operand(
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

pub(crate) fn register(
    instruction: &BytecodeInstruction,
    index: usize,
) -> Result<u32, ExecuteError> {
    match operand(instruction, index)? {
        BytecodeOperand::Register(index) => Ok(*index),
        _ => Err(ExecuteError::InvalidOperand("register")),
    }
}

pub(crate) fn count_operand(
    instruction: &BytecodeInstruction,
    index: usize,
) -> Result<u32, ExecuteError> {
    match operand(instruction, index)? {
        BytecodeOperand::Count(value) => Ok(*value),
        _ => Err(ExecuteError::InvalidOperand("count")),
    }
}

pub(crate) fn local_slot_operand(
    instruction: &BytecodeInstruction,
    index: usize,
) -> Result<u32, ExecuteError> {
    match operand(instruction, index)? {
        BytecodeOperand::LocalSlot(index) => Ok(*index),
        _ => Err(ExecuteError::InvalidOperand("local slot")),
    }
}

pub(crate) fn constant_value(module: &BytecodeModule, index: u32) -> Result<Value, ExecuteError> {
    match module.constants.get(index as usize) {
        Some(BytecodeConstant::Number(value)) => Ok(Value::JsValue(JsValue::from_f64(*value))),
        Some(BytecodeConstant::String(value)) => Ok(Value::JsValue(JsValue::from_str(value))),
        #[cfg(feature = "bigint")]
        Some(BytecodeConstant::BigInt(value)) => Ok(Value::BigInt(normalize_bigint_text(value))),
        #[cfg(not(feature = "bigint"))]
        Some(BytecodeConstant::BigInt(_)) => Err(ExecuteError::Unsupported("BigInt")),
        Some(BytecodeConstant::Bool(value)) => Ok(Value::JsValue(JsValue::from_bool(*value))),
        Some(BytecodeConstant::Null) => Ok(Value::Null),
        Some(BytecodeConstant::Undefined) => Ok(Value::Undefined),
        None => Err(ExecuteError::BadConstant(index)),
    }
}

pub(crate) fn constant_string(module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
    match module.constants.get(index as usize) {
        Some(BytecodeConstant::String(value)) => Ok(value.clone()),
        Some(value) => Ok(value.to_string()),
        None => Err(ExecuteError::BadConstant(index)),
    }
}

pub(crate) fn name_string(module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
    module
        .names
        .get(index as usize)
        .cloned()
        .ok_or(ExecuteError::BadConstant(index))
}

pub(crate) fn external_string(module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
    module
        .extern_slots
        .get(index as usize)
        .cloned()
        .ok_or(ExecuteError::BadConstant(index))
}

pub(crate) fn operator_name(operator: u32) -> Option<&'static str> {
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
    "++",
    "--",
    "typeof",
    "void",
    "delete",
    "in",
    "instanceof",
];

pub(crate) fn get_local_member(object: &Value, property: &str) -> Result<Value, ExecuteError> {
    match object {
        Value::Object(props) => {
            let value = { props.borrow().get(property).cloned() };
            match value {
                Some(Value::Function(function)) => Ok(Value::JsValue(
                    vm_bound_function_to_js_value(function, object.clone()),
                )),
                Some(Value::NativeFunction(function)) => Ok(Value::JsValue(
                    vm_bound_native_function_to_js_value(function, object.clone()),
                )),
                Some(value) => Ok(bind_member_value(value, object)),
                None => object_prototype_member(object, property),
            }
        }
        Value::Function(function) | Value::BoundFunction(function, _) => {
            if property == "name" {
                Ok(Value::String(function.name.clone().unwrap_or_default()))
            } else if property == "length" {
                Ok(Value::Number(function.params.len() as f64))
            } else {
                let value = { function.props.borrow().get(property).cloned() };
                match value {
                    Some(value) => Ok(bind_member_value(value, object)),
                    None if is_function_native_method(property) => Ok(bound_native_method_value(
                        format!("Function.{property}"),
                        object,
                    )),
                    None => object_prototype_member(object, property),
                }
            }
        }
        Value::NativeFunction(_) | Value::BoundNativeFunction(_, _)
            if is_function_native_method(property) =>
        {
            Ok(bound_native_method_value(
                format!("Function.{property}"),
                object,
            ))
        }
        Value::Class(class) => {
            if property == "name" {
                match class.static_props.get(property).cloned() {
                    Some(value) => Ok(bind_member_value(value, object)),
                    None => Ok(Value::String(class.name.clone().unwrap_or_default())),
                }
            } else {
                match class.static_props.get(property).cloned() {
                    Some(Value::Function(function)) => Ok(Value::JsValue(
                        vm_bound_function_to_js_value(function, object.clone()),
                    )),
                    Some(Value::NativeFunction(function)) => Ok(Value::JsValue(
                        vm_bound_native_function_to_js_value(function, object.clone()),
                    )),
                    Some(value) => Ok(bind_member_value(value, object)),
                    None => Ok(Value::Undefined),
                }
            }
        }
        Value::ExternalRef(reference) => Ok(Value::ExternalRef(reference.member(property))),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            if value.is_null() || value.is_undefined() {
                return Err(ExecuteError::TypeError(format!(
                    "cannot read property {property:?} of {}",
                    js_value_display(value)
                )));
            }
            #[cfg(feature = "regexp")]
            if crate::host::js_value_is_regexp(value) && is_regexp_native_method(property) {
                return Ok(bound_native_method_value(
                    format!("RegExp.{property}"),
                    object,
                ));
            }
            if let Some(member) = get_vm_js_handle_member(value, object, property)? {
                return Ok(member);
            }
            if let Some(text) = value.as_string() {
                if is_regexp_literal_method(&text, property) {
                    return Ok(bound_native_method_value(
                        format!("RegExp.{property}"),
                        object,
                    ));
                }
                if matches!(property, "match" | "replace") {
                    return Ok(bound_native_method_value(
                        format!("String.{property}"),
                        object,
                    ));
                }
            }
            if !is_js_property_target(value) && !is_js_boxable_primitive(value) {
                return Ok(Value::Undefined);
            }
            if let Some(member) = js_overlay_get(value, property) {
                return Ok(bind_member_value(member, object));
            }
            let member = get_js_property(value, property)?;
            if member.dyn_ref::<JsFunction>().is_some() {
                Ok(Value::BoundJsFunction(member, value.clone()))
            } else {
                Ok(Value::JsValue(member))
            }
        }
        #[cfg(feature = "array-builtins")]
        Value::Array(_) if property == "Symbol.iterator" => {
            if let Some(value) = host_overlay_get("Array.prototype.Symbol.iterator")
                .or_else(|| js_array_prototype_symbol_iterator().map(Value::JsValue))
            {
                Ok(bind_member_value(value, object))
            } else if js_array_prototype_has_symbol_iterator() {
                Ok(bound_native_method_value(
                    "Array.values".to_string(),
                    object,
                ))
            } else {
                Ok(Value::Undefined)
            }
        }
        Value::Array(items) if property == "length" => {
            Ok(Value::Number(items.borrow().len() as f64))
        }
        Value::Array(_) if is_array_native_method(property) => Ok(bound_native_method_value(
            format!("Array.{property}"),
            object,
        )),
        Value::Array(_) if is_object_prototype_method(property) => {
            object_prototype_member(object, property)
        }
        Value::Array(items) => Ok(property
            .parse::<usize>()
            .ok()
            .and_then(|index| items.borrow().get(index).cloned())
            .unwrap_or(Value::Undefined)),
        Value::String(value) if property == "length" => {
            Ok(Value::Number(value.chars().count() as f64))
        }
        Value::String(value) if is_regexp_literal_method(value, property) => Ok(
            bound_native_method_value(format!("RegExp.{property}"), object),
        ),
        Value::String(_) if is_string_native_method(property) => Ok(bound_native_method_value(
            format!("String.{property}"),
            object,
        )),
        Value::String(_) if is_object_prototype_method(property) => {
            object_prototype_member(object, property)
        }
        Value::String(value) => Ok(property
            .parse::<usize>()
            .ok()
            .and_then(|index| value.chars().nth(index))
            .map(|value| Value::String(value.to_string()))
            .unwrap_or(Value::Undefined)),
        Value::Symbol(_) if is_object_prototype_method(property) => {
            object_prototype_member(object, property)
        }
        #[cfg(feature = "bigint")]
        Value::BigInt(_) if is_bigint_native_method(property) => Ok(bound_native_method_value(
            format!("BigInt.{property}"),
            object,
        )),
        Value::BigInt(_) if is_object_prototype_method(property) => {
            object_prototype_member(object, property)
        }
        Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
            "cannot read property {property:?} of {object}"
        ))),
        _ => Ok(Value::Undefined),
    }
}

fn get_vm_js_handle_member(
    value: &JsValue,
    object: &Value,
    property: &str,
) -> Result<Option<Value>, ExecuteError> {
    let Some(handle) = vm_js_handle(value) else {
        return Ok(None);
    };
    match handle {
        VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => {
            if property == "name" {
                return Ok(Some(Value::String(
                    function.name.clone().unwrap_or_default(),
                )));
            }
            if property == "length" {
                return Ok(Some(Value::Number(function.params.len() as f64)));
            }
            let value = { function.props.borrow().get(property).cloned() };
            if let Some(value) = value {
                return Ok(Some(bind_member_value(value, object)));
            }
            if is_function_native_method(property) {
                return Ok(Some(bound_native_method_value(
                    format!("Function.{property}"),
                    object,
                )));
            }
            object_prototype_member(object, property).map(Some)
        }
        VmJsHandle::NativeFunction(_) | VmJsHandle::BoundNativeFunction(_, _) => {
            if is_function_native_method(property) {
                Ok(Some(bound_native_method_value(
                    format!("Function.{property}"),
                    object,
                )))
            } else {
                object_prototype_member(object, property).map(Some)
            }
        }
        VmJsHandle::Class(class) => {
            if property == "name" {
                match class.static_props.get(property).cloned() {
                    Some(value) => Ok(Some(bind_member_value(value, object))),
                    None => Ok(Some(Value::String(class.name.clone().unwrap_or_default()))),
                }
            } else {
                Ok(Some(class_member_value(class, object, property)))
            }
        }
        VmJsHandle::Module(module) => Ok(Some(
            module
                .exports
                .get(property)
                .cloned()
                .unwrap_or(Value::Undefined),
        )),
    }
}

fn class_member_value(class: ClassValue, object: &Value, property: &str) -> Value {
    match class.static_props.get(property).cloned() {
        Some(Value::Function(function)) => {
            Value::JsValue(vm_bound_function_to_js_value(function, object.clone()))
        }
        Some(Value::NativeFunction(function)) => Value::JsValue(
            vm_bound_native_function_to_js_value(function, object.clone()),
        ),
        Some(value) => bind_member_value(value, object),
        None => Value::Undefined,
    }
}

pub(crate) fn bind_member_value(value: Value, this_value: &Value) -> Value {
    match value {
        Value::Function(function) => {
            Value::JsValue(vm_bound_function_to_js_value(function, this_value.clone()))
        }
        Value::NativeFunction(function) => Value::JsValue(vm_bound_native_function_to_js_value(
            function,
            this_value.clone(),
        )),
        Value::JsValue(value)
            if vm_js_handle(&value).is_some_and(|handle| {
                matches!(
                    handle,
                    VmJsHandle::Function(_)
                        | VmJsHandle::BoundFunction(_, _)
                        | VmJsHandle::NativeFunction(_)
                        | VmJsHandle::BoundNativeFunction(_, _)
                )
            }) =>
        {
            let this_value =
                value_to_js_value(this_value, &JsHostBridge::empty()).unwrap_or(JsValue::UNDEFINED);
            Value::BoundJsFunction(value, this_value)
        }
        Value::JsValue(value) if value.dyn_ref::<JsFunction>().is_some() => {
            let this_value =
                value_to_js_value(this_value, &JsHostBridge::empty()).unwrap_or(JsValue::UNDEFINED);
            Value::BoundJsFunction(value, this_value)
        }
        value => value,
    }
}

pub(crate) fn object_prototype_member(
    object: &Value,
    property: &str,
) -> Result<Value, ExecuteError> {
    if is_object_prototype_method(property) {
        Ok(bound_native_method_value(
            format!("Object.prototype.{property}"),
            object,
        ))
    } else {
        Ok(Value::Undefined)
    }
}

fn bound_native_method_value(name: String, this_value: &Value) -> Value {
    Value::BoundNativeFunction(NativeFunctionValue { name }, Box::new(this_value.clone()))
}

#[cfg(feature = "object-builtins")]
pub(crate) fn is_object_prototype_method(property: &str) -> bool {
    matches!(property, "hasOwnProperty" | "toString" | "valueOf")
}

#[cfg(not(feature = "object-builtins"))]
pub(crate) fn is_object_prototype_method(property: &str) -> bool {
    let _ = property;
    false
}

#[cfg(feature = "function-builtins")]
pub(crate) fn is_function_native_method(property: &str) -> bool {
    matches!(property, "apply" | "call" | "toString")
}

#[cfg(not(feature = "function-builtins"))]
pub(crate) fn is_function_native_method(property: &str) -> bool {
    let _ = property;
    false
}

#[cfg(feature = "bigint")]
fn is_bigint_native_method(property: &str) -> bool {
    matches!(property, "toString" | "valueOf")
}

#[cfg(feature = "function-builtins")]
pub(crate) fn apply_argument_list(value: &Value) -> Result<Vec<Value>, ExecuteError> {
    match value {
        Value::Null | Value::Undefined => Ok(Vec::new()),
        Value::Array(items) => Ok(items.borrow().clone()),
        Value::JsValue(value) | Value::BoundJsFunction(value, _)
            if value.is_null() || value.is_undefined() =>
        {
            Ok(Vec::new())
        }
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            let length = get_js_property(value, "length")
                .map(|value| js_value_to_number(&value))
                .unwrap_or(0.0)
                .max(0.0)
                .trunc() as usize;
            if length == 0 && !JsArray::is_array(value) {
                return Ok(Vec::new());
            }
            Ok((0..length)
                .map(|index| {
                    get_js_property(value, &index.to_string())
                        .map(Value::JsValue)
                        .unwrap_or(Value::Undefined)
                })
                .collect())
        }
        Value::Object(props) => {
            let props = props.borrow();
            let length = props.get("length").map(Value::to_number).unwrap_or(0.0);
            let length = length.max(0.0).trunc() as usize;
            Ok((0..length)
                .map(|index| {
                    props
                        .get(&index.to_string())
                        .cloned()
                        .unwrap_or(Value::Undefined)
                })
                .collect())
        }
        value => Err(ExecuteError::TypeError(format!(
            "Function.apply arguments must be array-like, got {value}"
        ))),
    }
}

#[cfg(feature = "array-builtins")]
pub(crate) fn is_array_native_method(property: &str) -> bool {
    matches!(
        property,
        "push"
            | "fill"
            | "join"
            | "toString"
            | "forEach"
            | "map"
            | "filter"
            | "flatMap"
            | "find"
            | "reduce"
            | "every"
            | "includes"
            | "indexOf"
            | "pop"
            | "shift"
            | "unshift"
            | "reverse"
            | "sort"
            | "splice"
            | "slice"
            | "concat"
            | "flat"
    )
}

#[cfg(not(feature = "array-builtins"))]
pub(crate) fn is_array_native_method(property: &str) -> bool {
    let _ = property;
    false
}

#[cfg(feature = "string-builtins")]
pub(crate) fn is_string_native_method(property: &str) -> bool {
    matches!(
        property,
        "charAt"
            | "charCodeAt"
            | "endsWith"
            | "includes"
            | "indexOf"
            | "slice"
            | "startsWith"
            | "trim"
            | "toLowerCase"
            | "toUpperCase"
            | "split"
            | "concat"
            | "match"
            | "replace"
    )
}

#[cfg(not(feature = "string-builtins"))]
pub(crate) fn is_string_native_method(property: &str) -> bool {
    let _ = property;
    false
}

fn is_regexp_literal_method(value: &str, property: &str) -> bool {
    #[cfg(feature = "regexp")]
    {
        regex_string_parts(value).is_some() && is_regexp_native_method(property)
    }
    #[cfg(not(feature = "regexp"))]
    {
        let _ = (value, property);
        false
    }
}

#[cfg(feature = "regexp")]
pub(crate) fn is_regexp_native_method(property: &str) -> bool {
    matches!(property, "exec" | "test")
}

#[cfg(feature = "regexp")]
pub(crate) fn regexp_exec_value(regexp: &Value, input: &str) -> Value {
    let Some((pattern, global)) = regexp_pattern_flags(regexp) else {
        return whitespace_match_array(input);
    };
    if pattern_matches_whitespace_tokens(&pattern) {
        return whitespace_match_array(input);
    }
    let needle = regex_string_parts(&pattern)
        .map(|(body, _)| body.to_string())
        .unwrap_or(pattern);
    let needle = simplified_regex_needle(&needle);
    if needle.is_empty() {
        return regexp_match_array("", 0, input);
    }
    if let Some(index) = input.find(&needle) {
        let result = regexp_match_array(&needle, index, input);
        if global {
            if let Value::Object(props) = regexp {
                let _ = props.try_borrow_mut().map(|mut props| {
                    props.insert(
                        "lastIndex".to_string(),
                        Value::Number((index + needle.len()) as f64),
                    )
                });
            }
        }
        result
    } else {
        Value::Null
    }
}

#[cfg(feature = "regexp")]
pub(crate) fn regexp_pattern_flags(regexp: &Value) -> Option<(String, bool)> {
    match regexp {
        Value::Object(props) => {
            let props = props.borrow();
            let source = props.get("source")?.to_string();
            let global = props.get("global").is_some_and(Value::is_truthy);
            Some((source, global))
        }
        Value::String(value) => regex_string_parts(value)
            .map(|(body, global)| (body.to_string(), global))
            .or_else(|| Some((value.clone(), false))),
        Value::ExternalRef(_) => None,
        value => Some((value.to_string(), false)),
    }
}

#[cfg(feature = "regexp")]
pub(crate) fn whitespace_match_array(value: &str) -> Value {
    let matches = value
        .split_whitespace()
        .map(|value| Value::String(value.to_string()))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        Value::Null
    } else {
        array_value(matches)
    }
}

#[cfg(feature = "regexp")]
pub(crate) fn regexp_match_array(match_text: &str, index: usize, input: &str) -> Value {
    let _ = (index, input);
    array_value(vec![Value::String(match_text.to_string())])
}

#[cfg(feature = "regexp")]
pub(crate) fn simplified_regex_needle(pattern: &str) -> String {
    pattern
        .trim_start_matches('^')
        .trim_end_matches('$')
        .replace("\\x20", " ")
        .replace("\\t", "\t")
        .replace("\\r", "\r")
        .replace("\\n", "\n")
        .replace("\\f", "\u{c}")
        .replace("\\/", "/")
        .replace("\\.", ".")
        .replace("\\-", "-")
}

#[cfg(feature = "regexp")]
pub(crate) fn pattern_matches_whitespace_tokens(pattern: &str) -> bool {
    pattern.contains("[^\\x20\\t\\r\\n\\f]+")
        || pattern.contains("[^\\s]+")
        || pattern.contains("\\S+")
}

#[cfg(feature = "regexp")]
pub(crate) fn regex_string_parts(pattern: &str) -> Option<(&str, bool)> {
    let rest = pattern.strip_prefix('/')?;
    let end = rest.rfind('/')?;
    let body = &rest[..end];
    let flags = &rest[end + 1..];
    Some((body, flags.contains('g')))
}

#[cfg(feature = "object-builtins")]
pub(crate) fn object_has_own_property(object: &Value, key: &str) -> bool {
    match object {
        Value::Object(props) => props.borrow().contains_key(key),
        Value::Function(function) | Value::BoundFunction(function, _) => {
            function.props.borrow().contains_key(key)
        }
        Value::Array(items) => key
            .parse::<usize>()
            .is_ok_and(|index| index < items.borrow().len()),
        Value::String(value) => key
            .parse::<usize>()
            .is_ok_and(|index| index < value.chars().count()),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            get_js_property(value, key).is_ok_and(|value| !value.is_undefined())
        }
        _ => false,
    }
}

#[cfg(feature = "object-builtins")]
pub(crate) fn object_to_string_tag(object: &Value) -> String {
    match object {
        Value::Array(_) => "[object Array]",
        Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::Class(_) => "[object Function]",
        Value::String(_) => "[object String]",
        Value::Number(_) => "[object Number]",
        Value::BigInt(_) => "[object BigInt]",
        Value::Bool(_) => "[object Boolean]",
        Value::Null => "[object Null]",
        Value::Undefined => "[object Undefined]",
        Value::Module(_) => "[object Module]",
        Value::GeneratorState(_) => "[object Object]",
        Value::Symbol(_) => "[object Symbol]",
        Value::ExternalRef(reference) => match reference.display_path().as_str() {
            "Array" | "Object" | "Function" | "String" | "Number" | "Boolean" => {
                "[object Function]"
            }
            _ => "[object Object]",
        },
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            if let Some(handle) = vm_js_handle(value) {
                return match handle {
                    VmJsHandle::Function(_)
                    | VmJsHandle::BoundFunction(_, _)
                    | VmJsHandle::NativeFunction(_)
                    | VmJsHandle::BoundNativeFunction(_, _)
                    | VmJsHandle::Class(_) => "[object Function]",
                    VmJsHandle::Module(_) => "[object Module]",
                }
                .to_string();
            }
            return js_object_to_string_tag(value);
        }
        Value::Object(_) => "[object Object]",
    }
    .to_string()
}

#[cfg(feature = "function-builtins")]
pub(crate) fn function_source_string(value: &Value) -> String {
    match value {
        Value::Function(function) | Value::BoundFunction(function, _) => format!(
            "function {}() {{ [vm code] }}",
            function.name.as_deref().unwrap_or("")
        ),
        Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => {
            let name = native_function_display_name(&function.name);
            format!("function {name}() {{ [native code] }}")
        }
        Value::ExternalRef(reference) => {
            let path = reference.display_path();
            let name = path.rsplit('.').next().unwrap_or("anonymous");
            format!("function {name}() {{ [native code] }}")
        }
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            if let Some(handle) = vm_js_handle(value) {
                return match handle {
                    VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => {
                        format!(
                            "function {}() {{ [vm code] }}",
                            function.name.as_deref().unwrap_or("")
                        )
                    }
                    VmJsHandle::NativeFunction(function)
                    | VmJsHandle::BoundNativeFunction(function, _) => {
                        let name = native_function_display_name(&function.name);
                        format!("function {name}() {{ [native code] }}")
                    }
                    VmJsHandle::Class(class) => {
                        format!(
                            "class {} {{ [vm code] }}",
                            class.name.as_deref().unwrap_or("")
                        )
                    }
                    VmJsHandle::Module(module) => format!("module {}", module.source),
                };
            }
            js_function_source_string(value)
        }
        _ => "function () { [native code] }".to_string(),
    }
}

#[cfg(feature = "function-builtins")]
pub(crate) fn native_function_display_name(name: &str) -> &str {
    name.strip_prefix("Object.prototype.")
        .or_else(|| name.strip_prefix("Function."))
        .or_else(|| name.strip_prefix("Array."))
        .or_else(|| name.strip_prefix("String."))
        .unwrap_or(name)
}

#[cfg(any(feature = "array-builtins", feature = "string-builtins"))]
pub(crate) fn normalize_index(index: isize, len: isize) -> isize {
    if index < 0 {
        (len + index).clamp(0, len)
    } else {
        index.clamp(0, len)
    }
}

pub(crate) fn property_key(value: &Value) -> String {
    match value {
        Value::Number(value) if value.is_finite() && value.fract() == 0.0 => {
            format!("{}", *value as i64)
        }
        Value::BigInt(value) => value.clone(),
        Value::String(value) | Value::Symbol(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => js_value_property_key(value),
        Value::Null => "null".to_string(),
        Value::Undefined => "undefined".to_string(),
        value => value.to_string(),
    }
}

pub(crate) fn set_member(
    object: &mut Value,
    property: &str,
    value: Value,
) -> Result<(), ExecuteError> {
    thread_local! {
        static SET_MEMBER_DEPTH: Cell<usize> = const { Cell::new(0) };
    }
    SET_MEMBER_DEPTH.with(|depth| {
        let current = depth.get();
        if current >= 128 {
            return Err(ExecuteError::RangeError(format!(
                "maximum set_member recursion exceeded while setting {property:?}"
            )));
        }
        depth.set(current + 1);
        let result = set_member_inner(object, property, value);
        depth.set(current);
        result
    })
}

fn set_member_inner(object: &mut Value, property: &str, value: Value) -> Result<(), ExecuteError> {
    match object {
        Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
            "cannot set property {property:?} of {object}"
        ))),
        Value::Object(props) => {
            props
                .try_borrow_mut()
                .map_err(|_| {
                    ExecuteError::Runtime(format!(
                        "object properties are already borrowed while setting {property:?}"
                    ))
                })?
                .insert(property.to_string(), value);
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
        Value::Function(function) | Value::BoundFunction(function, _) => {
            function
                .props
                .try_borrow_mut()
                .map_err(|_| {
                    ExecuteError::Runtime(format!(
                        "function properties are already borrowed while setting {property:?}"
                    ))
                })?
                .insert(property.to_string(), value);
            Ok(())
        }
        Value::NativeFunction(_) | Value::BoundNativeFunction(_, _) | Value::ExternalRef(_) => {
            Ok(())
        }
        Value::JsValue(target) | Value::BoundJsFunction(target, _) => {
            if !is_js_property_target(target) {
                if is_js_boxable_primitive(target) {
                    return Ok(());
                }
                return Err(ExecuteError::TypeError(format!(
                    "cannot set property {property:?} of {}",
                    js_value_display(target)
                )));
            }
            if let Some(result) = js_typed_array_index_set(target, property, &value) {
                return result;
            }
            if set_vm_js_handle_member(target, property, value.clone())? {
                return Ok(());
            }
            if property.starts_with("Symbol.") {
                if js_value_is_array_prototype(target) {
                    host_overlay_set_path(&format!("Array.prototype.{property}"), value.clone());
                }
                if can_represent_value_as_js(&value) {
                    Reflect::set(
                        target,
                        &js_reflect_property_key(property),
                        &value_to_js_value(&value, &JsHostBridge::empty())?,
                    )
                    .map_err(js_error)?;
                }
                js_overlay_set(target, property, value);
                return Ok(());
            }
            if !can_represent_value_as_js(&value) {
                js_overlay_set(target, property, value);
                return Ok(());
            }
            Reflect::set(
                target,
                &js_reflect_property_key(property),
                &value_to_js_value(&value, &JsHostBridge::empty())?,
            )
            .map_err(js_error)?;
            Ok(())
        }
        Value::Number(_) | Value::String(_) | Value::Bool(_) | Value::Symbol(_) => Ok(()),
        _ => Err(ExecuteError::Runtime(format!(
            "cannot set {property} on {object}"
        ))),
    }
}

fn set_vm_js_handle_member(
    target: &JsValue,
    property: &str,
    value: Value,
) -> Result<bool, ExecuteError> {
    let js_constructor_function = if property == "constructor" {
        match &value {
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => match vm_js_handle(value) {
                Some(VmJsHandle::Function(function))
                | Some(VmJsHandle::BoundFunction(function, _)) => Some(Ok(Some(function))),
                Some(_) => Some(Err(ExecuteError::Runtime(format!(
                    "class constructor must be a function, got {}",
                    js_value_display(value)
                )))),
                None => Some(Err(ExecuteError::Runtime(format!(
                    "class constructor must be a function, got {}",
                    js_value_display(value)
                )))),
            },
            _ => None,
        }
    } else {
        None
    };
    update_vm_js_handle(target, |handle| -> Result<bool, ExecuteError> {
        match handle {
            VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => {
                function
                    .props
                    .try_borrow_mut()
                    .map_err(|_| {
                        ExecuteError::Runtime(format!(
                            "function properties are already borrowed while setting {property:?}"
                        ))
                    })?
                    .insert(property.to_string(), value);
                Ok(true)
            }
            VmJsHandle::NativeFunction(_) | VmJsHandle::BoundNativeFunction(_, _) => Ok(false),
            VmJsHandle::Class(class) => {
                if property == "constructor" {
                    match value {
                        Value::Function(function) => class.constructor = Some(function),
                        Value::JsValue(_) | Value::BoundJsFunction(_, _) => {
                            class.constructor = match js_constructor_function {
                                Some(function) => function?,
                                None => {
                                    return Err(ExecuteError::Runtime(
                                        "class constructor must be a function".to_string(),
                                    ));
                                }
                            };
                        }
                        Value::Undefined => class.constructor = None,
                        value => {
                            return Err(ExecuteError::Runtime(format!(
                                "class constructor must be a function, got {value}"
                            )));
                        }
                    }
                } else if let Some(property) = property.strip_prefix("prototype.") {
                    class.instance_props.insert(property.to_string(), value);
                } else {
                    class.static_props.insert(property.to_string(), value);
                }
                Ok(true)
            }
            VmJsHandle::Module(module) => {
                module.exports.insert(property.to_string(), value);
                Ok(true)
            }
        }
    })
    .transpose()
    .map(|updated| updated.unwrap_or(false))
}

#[cfg(feature = "bigint")]
fn normalize_bigint_text(value: &str) -> String {
    js_bigint_to_string(value).unwrap_or_else(|_| {
        parse_bigint(value)
            .map(|value| value.to_string())
            .unwrap_or_else(|| value.trim_end_matches('n').to_string())
    })
}

#[cfg(feature = "bigint")]
fn parse_bigint(value: &str) -> Option<i128> {
    let value = value.trim().trim_end_matches('n').replace('_', "");
    let (sign, value) = if let Some(rest) = value.strip_prefix('-') {
        (-1i128, rest)
    } else if let Some(rest) = value.strip_prefix('+') {
        (1i128, rest)
    } else {
        (1i128, value.as_str())
    };
    let parsed = if let Some(rest) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        i128::from_str_radix(rest, 16).ok()
    } else if let Some(rest) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        i128::from_str_radix(rest, 8).ok()
    } else if let Some(rest) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        i128::from_str_radix(rest, 2).ok()
    } else {
        value.parse::<i128>().ok()
    }?;
    Some(parsed * sign)
}

#[cfg(feature = "bigint")]
fn js_bigint_to_string(value: &str) -> Result<String, ExecuteError> {
    JsFunction::new_with_args(
        "value",
        "return String(BigInt(String(value).replace(/n$/, '')));",
    )
    .call1(&JsValue::UNDEFINED, &JsValue::from_str(value.trim()))
    .map_err(js_error)?
    .as_string()
    .ok_or_else(|| ExecuteError::Runtime("BigInt conversion did not return a string".to_string()))
}

#[cfg(feature = "bigint")]
fn js_bigint_binary(op: &str, left: &str, right: &str) -> Result<Value, ExecuteError> {
    let body = match op {
        "+" => "return String(BigInt(left) + BigInt(right));",
        "-" => "return String(BigInt(left) - BigInt(right));",
        "*" => "return String(BigInt(left) * BigInt(right));",
        "/" => "return String(BigInt(left) / BigInt(right));",
        "%" => "return String(BigInt(left) % BigInt(right));",
        "**" => "return String(BigInt(left) ** BigInt(right));",
        "&" => "return String(BigInt(left) & BigInt(right));",
        "|" => "return String(BigInt(left) | BigInt(right));",
        "^" => "return String(BigInt(left) ^ BigInt(right));",
        "<<" => "return String(BigInt(left) << BigInt(right));",
        ">>" => "return String(BigInt(left) >> BigInt(right));",
        "==" => "return BigInt(left) == BigInt(right);",
        "!=" => "return BigInt(left) != BigInt(right);",
        "===" => "return BigInt(left) === BigInt(right);",
        "!==" => "return BigInt(left) !== BigInt(right);",
        "<" => "return BigInt(left) < BigInt(right);",
        "<=" => "return BigInt(left) <= BigInt(right);",
        ">" => "return BigInt(left) > BigInt(right);",
        ">=" => "return BigInt(left) >= BigInt(right);",
        _ => {
            return Err(ExecuteError::Runtime(format!(
                "unsupported BigInt binary op {op}"
            )));
        }
    };
    let result = JsFunction::new_with_args("left, right", body)
        .call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&js_bigint_to_string(left)?),
            &JsValue::from_str(&js_bigint_to_string(right)?),
        )
        .map_err(js_error)?;
    if let Some(value) = result.as_bool() {
        Ok(Value::Bool(value))
    } else if let Some(value) = result.as_string() {
        Ok(Value::BigInt(value))
    } else {
        Err(ExecuteError::Runtime(
            "BigInt operation returned unsupported value".to_string(),
        ))
    }
}

#[cfg(feature = "bigint")]
fn js_bigint_unary(op: &str, value: &str) -> Result<Value, ExecuteError> {
    let body = match op {
        "-" => "return String(-BigInt(value));",
        "~" => "return String(~BigInt(value));",
        _ => return Err(ExecuteError::Unsupported("BigInt unary op")),
    };
    JsFunction::new_with_args("value", body)
        .call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&js_bigint_to_string(value)?),
        )
        .map_err(js_error)?
        .as_string()
        .map(Value::BigInt)
        .ok_or_else(|| ExecuteError::Runtime("BigInt unary returned unsupported value".to_string()))
}

#[cfg(feature = "bigint")]
fn js_bigint_number_compare(
    op: &str,
    bigint: &str,
    number: f64,
    bigint_left: bool,
) -> Option<bool> {
    let body = match (op, bigint_left) {
        ("==", true) => "return BigInt(bigint) == number;",
        ("!=", true) => "return BigInt(bigint) != number;",
        ("<", true) => "return BigInt(bigint) < number;",
        ("<=", true) => "return BigInt(bigint) <= number;",
        (">", true) => "return BigInt(bigint) > number;",
        (">=", true) => "return BigInt(bigint) >= number;",
        ("==", false) => "return number == BigInt(bigint);",
        ("!=", false) => "return number != BigInt(bigint);",
        ("<", false) => "return number < BigInt(bigint);",
        ("<=", false) => "return number <= BigInt(bigint);",
        (">", false) => "return number > BigInt(bigint);",
        (">=", false) => "return number >= BigInt(bigint);",
        _ => return None,
    };
    JsFunction::new_with_args("bigint, number", body)
        .call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&js_bigint_to_string(bigint).ok()?),
            &JsValue::from_f64(number),
        )
        .ok()
        .and_then(|value| value.as_bool())
}

#[cfg(feature = "bigint")]
fn bigint_number_eq(bigint: &str, number: f64) -> bool {
    number.is_finite()
        && number.fract() == 0.0
        && parse_bigint(bigint).is_some_and(|bigint| bigint as f64 == number)
}

#[cfg(feature = "bigint")]
fn bigint_binary(op: &str, left: Value, right: Value) -> Result<Value, ExecuteError> {
    match (left, right) {
        (Value::BigInt(left), Value::BigInt(right)) => {
            if op != ">>>" {
                return js_bigint_binary(op, &left, &right);
            }
            let left = parse_bigint(&left)
                .ok_or_else(|| ExecuteError::TypeError("invalid BigInt value".to_string()))?;
            let right = parse_bigint(&right)
                .ok_or_else(|| ExecuteError::TypeError("invalid BigInt value".to_string()))?;
            match op {
                "+" => Ok(Value::BigInt((left + right).to_string())),
                "-" => Ok(Value::BigInt((left - right).to_string())),
                "*" => Ok(Value::BigInt((left * right).to_string())),
                "/" => {
                    if right == 0 {
                        Err(ExecuteError::RangeError("division by zero".to_string()))
                    } else {
                        Ok(Value::BigInt((left / right).to_string()))
                    }
                }
                "%" => {
                    if right == 0 {
                        Err(ExecuteError::RangeError("division by zero".to_string()))
                    } else {
                        Ok(Value::BigInt((left % right).to_string()))
                    }
                }
                "**" => {
                    if right < 0 {
                        return Err(ExecuteError::RangeError(
                            "BigInt exponent must be positive".to_string(),
                        ));
                    }
                    Ok(Value::BigInt(left.pow(right as u32).to_string()))
                }
                "&" => Ok(Value::BigInt((left & right).to_string())),
                "|" => Ok(Value::BigInt((left | right).to_string())),
                "^" => Ok(Value::BigInt((left ^ right).to_string())),
                "<<" => Ok(Value::BigInt((left << right.max(0) as u32).to_string())),
                ">>" => Ok(Value::BigInt((left >> right.max(0) as u32).to_string())),
                "==" => Ok(Value::Bool(left == right)),
                "!=" => Ok(Value::Bool(left != right)),
                "===" => Ok(Value::Bool(left == right)),
                "!==" => Ok(Value::Bool(left != right)),
                "<" => Ok(Value::Bool(left < right)),
                "<=" => Ok(Value::Bool(left <= right)),
                ">" => Ok(Value::Bool(left > right)),
                ">=" => Ok(Value::Bool(left >= right)),
                _ => Err(ExecuteError::Runtime(format!(
                    "unsupported BigInt binary op {op}"
                ))),
            }
        }
        (Value::BigInt(left), Value::Number(right)) => match op {
            "==" => Ok(Value::Bool(
                js_bigint_number_compare(op, &left, right, true)
                    .unwrap_or_else(|| bigint_number_eq(&left, right)),
            )),
            "!=" => Ok(Value::Bool(
                js_bigint_number_compare(op, &left, right, true)
                    .unwrap_or_else(|| !bigint_number_eq(&left, right)),
            )),
            "===" => Ok(Value::Bool(false)),
            "!==" => Ok(Value::Bool(true)),
            "<" | "<=" | ">" | ">=" => {
                if let Some(result) = js_bigint_number_compare(op, &left, right, true) {
                    return Ok(Value::Bool(result));
                }
                let left = parse_bigint(&left)
                    .ok_or_else(|| ExecuteError::TypeError("invalid BigInt value".to_string()))?
                    as f64;
                binary(op, Value::Number(left), Value::Number(right))
            }
            _ => Err(ExecuteError::TypeError(
                "cannot mix BigInt and other types".to_string(),
            )),
        },
        (Value::Number(left), Value::BigInt(right)) => match op {
            "==" => Ok(Value::Bool(
                js_bigint_number_compare(op, &right, left, false)
                    .unwrap_or_else(|| bigint_number_eq(&right, left)),
            )),
            "!=" => Ok(Value::Bool(
                js_bigint_number_compare(op, &right, left, false)
                    .unwrap_or_else(|| !bigint_number_eq(&right, left)),
            )),
            "===" => Ok(Value::Bool(false)),
            "!==" => Ok(Value::Bool(true)),
            "<" | "<=" | ">" | ">=" => {
                if let Some(result) = js_bigint_number_compare(op, &right, left, false) {
                    return Ok(Value::Bool(result));
                }
                let right = parse_bigint(&right)
                    .ok_or_else(|| ExecuteError::TypeError("invalid BigInt value".to_string()))?
                    as f64;
                binary(op, Value::Number(left), Value::Number(right))
            }
            _ => Err(ExecuteError::TypeError(
                "cannot mix BigInt and other types".to_string(),
            )),
        },
        (left @ Value::BigInt(_), right) | (left, right @ Value::BigInt(_)) => match op {
            "==" => Ok(Value::Bool(
                js_binary_bool("return left == right;", &left, &right).unwrap_or(false),
            )),
            "!=" => Ok(Value::Bool(
                js_binary_bool("return left != right;", &left, &right).unwrap_or(true),
            )),
            "===" => Ok(Value::Bool(false)),
            "!==" => Ok(Value::Bool(true)),
            _ => Err(ExecuteError::TypeError(
                "cannot mix BigInt and other types".to_string(),
            )),
        },
        _ => Err(ExecuteError::TypeError(
            "BigInt operator fallback received non-BigInt operands".to_string(),
        )),
    }
}

pub(crate) fn binary(op: &str, left: Value, right: Value) -> Result<Value, ExecuteError> {
    match op {
        "+" => match (left, right) {
            (Value::String(left), right) => Ok(Value::String(format!("{left}{right}"))),
            (left, Value::String(right)) => Ok(Value::String(format!("{left}{right}"))),
            #[cfg(feature = "bigint")]
            (left @ Value::BigInt(_), right) | (left, right @ Value::BigInt(_)) => {
                bigint_binary(op, left, right)
            }
            #[cfg(not(feature = "bigint"))]
            (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                Err(ExecuteError::Unsupported("BigInt"))
            }
            (Value::JsValue(left), right) if left.as_string().is_some() => Ok(Value::String(
                format!("{}{}", js_value_display(&left), right),
            )),
            (left, Value::JsValue(right)) if right.as_string().is_some() => Ok(Value::String(
                format!("{}{}", left, js_value_display(&right)),
            )),
            (left, right) => Ok(Value::Number(left.to_number() + right.to_number())),
        },
        #[cfg(feature = "bigint")]
        _ if matches!(left, Value::BigInt(_)) || matches!(right, Value::BigInt(_)) => {
            bigint_binary(op, left, right)
        }
        #[cfg(not(feature = "bigint"))]
        _ if matches!(left, Value::BigInt(_)) || matches!(right, Value::BigInt(_)) => {
            Err(ExecuteError::Unsupported("BigInt"))
        }
        "-" => Ok(Value::Number(left.to_number() - right.to_number())),
        "*" => Ok(Value::Number(left.to_number() * right.to_number())),
        "/" => Ok(Value::Number(left.to_number() / right.to_number())),
        "%" => Ok(Value::Number(left.to_number() % right.to_number())),
        "**" => Ok(Value::Number(left.to_number().powf(right.to_number()))),
        "==" => Ok(Value::Bool(loose_eq(&left, &right))),
        "!=" => Ok(Value::Bool(!loose_eq(&left, &right))),
        "===" => Ok(Value::Bool(strict_eq(&left, &right))),
        "!==" => Ok(Value::Bool(!strict_eq(&left, &right))),
        "<" | "<=" | ">" | ">=" => Ok(Value::Bool(relational_compare(op, &left, &right))),
        "&&" => Ok(if left.is_truthy() { right } else { left }),
        "||" => Ok(if left.is_truthy() { left } else { right }),
        "??" => Ok(match left {
            Value::Null | Value::Undefined => right,
            Value::JsValue(value) if value.is_null() || value.is_undefined() => right,
            value => value,
        }),
        "&" => Ok(Value::Number((to_int32(&left) & to_int32(&right)) as f64)),
        "|" => Ok(Value::Number((to_int32(&left) | to_int32(&right)) as f64)),
        "^" => Ok(Value::Number((to_int32(&left) ^ to_int32(&right)) as f64)),
        "<<" => Ok(Value::Number(
            to_int32(&left).wrapping_shl(shift_count(&right)) as f64,
        )),
        ">>" => Ok(Value::Number(
            to_int32(&left).wrapping_shr(shift_count(&right)) as f64,
        )),
        ">>>" => Ok(Value::Number(
            to_uint32(&left).wrapping_shr(shift_count(&right)) as f64,
        )),
        "in" => Ok(Value::Bool(has_property(&right, &property_key(&left)))),
        "instanceof" => Ok(Value::Bool(instance_of(&left, &right))),
        op => Err(ExecuteError::Runtime(format!("unsupported binary op {op}"))),
    }
}

fn relational_compare(op: &str, left: &Value, right: &Value) -> bool {
    match (relational_string(left), relational_string(right)) {
        (Some(left), Some(right)) => match op {
            "<" => left < right,
            "<=" => left <= right,
            ">" => left > right,
            ">=" => left >= right,
            _ => false,
        },
        _ => match op {
            "<" => left.to_number() < right.to_number(),
            "<=" => left.to_number() <= right.to_number(),
            ">" => left.to_number() > right.to_number(),
            ">=" => left.to_number() >= right.to_number(),
            _ => false,
        },
    }
}

fn relational_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => value.as_string(),
        _ => None,
    }
}

pub(crate) fn to_int32(value: &Value) -> i32 {
    to_uint32(value) as i32
}

pub(crate) fn to_uint32(value: &Value) -> u32 {
    let number = value.to_number();
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let modulo = 4_294_967_296.0;
    number.trunc().rem_euclid(modulo) as u32
}

pub(crate) fn shift_count(value: &Value) -> u32 {
    to_uint32(value) & 0x1f
}

pub(crate) fn has_property(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(props) => props.borrow().contains_key(key) || is_object_prototype_method(key),
        Value::Function(function) | Value::BoundFunction(function, _) => {
            function.props.borrow().contains_key(key)
                || is_function_native_method(key)
                || is_object_prototype_method(key)
        }
        Value::NativeFunction(_) | Value::BoundNativeFunction(_, _) => {
            is_function_native_method(key) || is_object_prototype_method(key)
        }
        Value::Array(items) => {
            key == "length"
                || key
                    .parse::<usize>()
                    .is_ok_and(|index| index < items.borrow().len())
                || is_array_native_method(key)
                || is_object_prototype_method(key)
        }
        Value::String(value) => {
            key == "length"
                || key
                    .parse::<usize>()
                    .is_ok_and(|index| index < value.chars().count())
                || is_string_native_method(key)
                || is_object_prototype_method(key)
        }
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            get_js_property(value, key).is_ok_and(|value| !value.is_undefined())
        }
        Value::ExternalRef(_) | Value::Class(_) | Value::Module(_) | Value::GeneratorState(_) => {
            true
        }
        Value::Number(_) | Value::BigInt(_) | Value::Bool(_) | Value::Symbol(_) => {
            is_object_prototype_method(key)
        }
        Value::Null | Value::Undefined => false,
    }
}

pub(crate) fn instance_of(value: &Value, constructor: &Value) -> bool {
    match constructor {
        Value::Class(class) => match value {
            Value::Object(props) => class
                .name
                .as_ref()
                .is_some_and(|name| props.borrow().contains_key(name)),
            _ => false,
        },
        Value::Function(function) | Value::BoundFunction(function, _) => {
            vm_constructed_by(value, function.name.as_deref())
        }
        Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => {
            native_instance_of(value, &function.name)
        }
        Value::ExternalRef(reference) => native_instance_of(value, &reference.display_path()),
        Value::JsValue(constructor) | Value::BoundJsFunction(constructor, _) => {
            if let Some(handle) = vm_js_handle(constructor) {
                return match handle {
                    VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => {
                        vm_constructed_by(value, function.name.as_deref())
                    }
                    VmJsHandle::Class(class) => match value {
                        Value::Object(props) => class
                            .name
                            .as_ref()
                            .is_some_and(|name| props.borrow().contains_key(name)),
                        _ => false,
                    },
                    VmJsHandle::NativeFunction(function)
                    | VmJsHandle::BoundNativeFunction(function, _) => {
                        native_instance_of(value, &function.name)
                    }
                    _ => false,
                };
            }
            if let Some(error_type) = vm_error_type(value)
                && constructor_name(constructor).is_some_and(|name| name == error_type)
            {
                return true;
            }
            js_instance_of(value, constructor).unwrap_or(false)
        }
        _ => false,
    }
}

fn vm_error_type(value: &Value) -> Option<String> {
    match value {
        Value::Object(props) => match props.borrow().get("__error_type") {
            Some(Value::String(value)) => Some(value.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn vm_constructed_by(value: &Value, constructor_name: Option<&str>) -> bool {
    let Some(constructor_name) = constructor_name else {
        return false;
    };
    match value {
        Value::Object(props) => match props.borrow().get("__constructor_name") {
            Some(Value::String(value)) => value == constructor_name,
            _ => false,
        },
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            get_js_property(value, "__constructor_name")
                .ok()
                .and_then(|value| value.as_string())
                .is_some_and(|value| value == constructor_name)
        }
        _ => false,
    }
}

fn constructor_name(constructor: &JsValue) -> Option<String> {
    get_js_property(constructor, "name")
        .ok()
        .and_then(|value| value.as_string())
}

pub(crate) fn native_instance_of(value: &Value, constructor: &str) -> bool {
    match constructor.rsplit('.').next().unwrap_or(constructor) {
        name @ ("ReferenceError" | "TypeError" | "RangeError" | "SyntaxError" | "Error") => {
            vm_error_type(value).is_some_and(|error_type| error_type == name)
        }
        "Array" => matches!(value, Value::Array(_)),
        "Object" => matches!(
            value,
            Value::Object(_)
                | Value::Array(_)
                | Value::Function(_)
                | Value::BoundFunction(_, _)
                | Value::NativeFunction(_)
                | Value::BoundNativeFunction(_, _)
                | Value::Class(_)
                | Value::Module(_)
        ),
        "String" => matches!(value, Value::String(_)),
        "Number" => matches!(value, Value::Number(_)),
        "Boolean" => matches!(value, Value::Bool(_)),
        "Function" => matches!(
            value,
            Value::Function(_)
                | Value::BoundFunction(_, _)
                | Value::NativeFunction(_)
                | Value::BoundNativeFunction(_, _)
                | Value::Class(_)
        ),
        "RegExp" => {
            matches!(value, Value::Object(props) if props.borrow().contains_key("source") && props.borrow().contains_key("exec"))
        }
        _ => false,
    }
}

pub(crate) fn loose_eq(left: &Value, right: &Value) -> bool {
    if let Some(equal) = same_reference(left, right) {
        return equal;
    }
    if can_represent_value_as_js(left) && can_represent_value_as_js(right) {
        return js_binary_bool("return left == right;", left, right).unwrap_or(false);
    }
    match (left, right) {
        #[cfg(feature = "bigint")]
        (Value::BigInt(left), Value::BigInt(right)) => left == right,
        #[cfg(feature = "bigint")]
        (Value::BigInt(left), Value::String(right)) => parse_bigint(right)
            .is_some_and(|right| parse_bigint(left).is_some_and(|left| left == right)),
        #[cfg(feature = "bigint")]
        (Value::String(left), Value::BigInt(right)) => parse_bigint(left)
            .is_some_and(|left| parse_bigint(right).is_some_and(|right| left == right)),
        #[cfg(feature = "bigint")]
        (Value::BigInt(left), Value::Number(right)) => bigint_number_eq(left, *right),
        #[cfg(feature = "bigint")]
        (Value::Number(left), Value::BigInt(right)) => bigint_number_eq(right, *left),
        #[cfg(not(feature = "bigint"))]
        (Value::BigInt(_), _) | (_, Value::BigInt(_)) => false,
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

fn strict_eq(left: &Value, right: &Value) -> bool {
    if let Some(equal) = same_reference(left, right) {
        return equal;
    }
    if can_represent_value_as_js(left) && can_represent_value_as_js(right) {
        return js_binary_bool("return left === right;", left, right).unwrap_or(false);
    }
    left == right
}

fn same_reference(left: &Value, right: &Value) -> Option<bool> {
    match (left, right) {
        #[cfg(feature = "bigint")]
        (Value::BigInt(left), Value::BigInt(right)) => Some(left == right),
        (Value::Array(left), Value::Array(right)) => Some(Rc::ptr_eq(left, right)),
        (Value::Object(left), Value::Object(right)) => Some(Rc::ptr_eq(left, right)),
        (Value::Function(left), Value::Function(right)) => {
            Some(Rc::ptr_eq(&left.props, &right.props))
        }
        (Value::BoundFunction(left, left_this), Value::BoundFunction(right, right_this)) => {
            Some(Rc::ptr_eq(&left.props, &right.props) && strict_eq(left_this, right_this))
        }
        (Value::NativeFunction(left), Value::NativeFunction(right)) => {
            Some(left.name == right.name)
        }
        (
            Value::BoundNativeFunction(left, left_this),
            Value::BoundNativeFunction(right, right_this),
        ) => Some(left.name == right.name && strict_eq(left_this, right_this)),
        (
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
            | Value::NativeFunction(_)
            | Value::BoundNativeFunction(_, _),
            Value::Array(_)
            | Value::Object(_)
            | Value::Function(_)
            | Value::BoundFunction(_, _)
            | Value::NativeFunction(_)
            | Value::BoundNativeFunction(_, _),
        ) => Some(false),
        _ => None,
    }
}

fn js_binary_bool(source: &str, left: &Value, right: &Value) -> Result<bool, ExecuteError> {
    let function = JsFunction::new_with_args("left, right", source);
    let left = value_to_js_value(left, &JsHostBridge::empty())?;
    let right = value_to_js_value(right, &JsHostBridge::empty())?;
    function
        .call2(&JsValue::UNDEFINED, &left, &right)
        .map_err(js_error)
        .map(|value| value.as_bool().unwrap_or(false))
}

pub(crate) fn unary(op: &str, arg: Value) -> Result<Value, ExecuteError> {
    match op {
        "-" => match arg {
            #[cfg(feature = "bigint")]
            Value::BigInt(value) => js_bigint_unary("-", &value),
            #[cfg(not(feature = "bigint"))]
            Value::BigInt(_) => Err(ExecuteError::Unsupported("BigInt")),
            arg => Ok(Value::Number(-arg.to_number())),
        },
        #[cfg(feature = "bigint")]
        "+" if matches!(arg, Value::BigInt(_)) => Err(ExecuteError::TypeError(
            "cannot convert a BigInt value to a number".to_string(),
        )),
        #[cfg(not(feature = "bigint"))]
        "+" if matches!(arg, Value::BigInt(_)) => Err(ExecuteError::Unsupported("BigInt")),
        "+" => Ok(Value::Number(arg.to_number())),
        "!" => Ok(Value::Bool(!arg.is_truthy())),
        "~" => match arg {
            #[cfg(feature = "bigint")]
            Value::BigInt(value) => js_bigint_unary("~", &value),
            #[cfg(not(feature = "bigint"))]
            Value::BigInt(_) => Err(ExecuteError::Unsupported("BigInt")),
            arg => Ok(Value::Number((!(arg.to_number() as i32)) as f64)),
        },
        "++" => match arg {
            #[cfg(feature = "bigint")]
            Value::BigInt(value) => js_bigint_binary("+", &value, "1"),
            #[cfg(not(feature = "bigint"))]
            Value::BigInt(_) => Err(ExecuteError::Unsupported("BigInt")),
            arg => Ok(Value::Number(arg.to_number() + 1.0)),
        },
        "--" => match arg {
            #[cfg(feature = "bigint")]
            Value::BigInt(value) => js_bigint_binary("-", &value, "1"),
            #[cfg(not(feature = "bigint"))]
            Value::BigInt(_) => Err(ExecuteError::Unsupported("BigInt")),
            arg => Ok(Value::Number(arg.to_number() - 1.0)),
        },
        "delete" => Ok(Value::Bool(true)),
        "void" => Ok(Value::Undefined),
        "typeof" => Ok(Value::String(
            match arg {
                Value::Number(_) => "number",
                Value::BigInt(_) => "bigint",
                Value::String(_) => "string",
                Value::Symbol(_) => "symbol",
                Value::Bool(_) => "boolean",
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                    return Ok(Value::String(js_value_typeof(&value).to_string()));
                }
                Value::Function(_)
                | Value::BoundFunction(_, _)
                | Value::NativeFunction(_)
                | Value::BoundNativeFunction(_, _) => "function",
                Value::Null => "object",
                Value::Array(_)
                | Value::Object(_)
                | Value::Class(_)
                | Value::Module(_)
                | Value::GeneratorState(_) => "object",
                Value::ExternalRef(reference)
                    if !reference.path.is_empty()
                        || matches!(
                            reference.root.as_str(),
                            "BigInt"
                                | "Symbol"
                                | "Object"
                                | "Function"
                                | "Array"
                                | "String"
                                | "Number"
                                | "Boolean"
                                | "Date"
                                | "RegExp"
                                | "Error"
                                | "TypeError"
                                | "RangeError"
                                | "ReferenceError"
                                | "SyntaxError"
                        ) =>
                {
                    "function"
                }
                Value::ExternalRef(_) => "object",
                Value::Undefined => "undefined",
            }
            .to_string(),
        )),
        _ => Err(ExecuteError::Unsupported("UNARY")),
    }
}
