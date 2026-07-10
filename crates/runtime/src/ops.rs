use crate::error::ExecuteError;
use crate::host::{
    HostBridge, VmJsHandle, can_represent_value_as_js, get_js_property, is_js_boxable_primitive,
    is_js_property_target, js_error, js_function_source_string, js_instance_of,
    js_object_to_string_tag, js_overlay_get, js_overlay_set, js_value_display,
    js_value_property_key, js_value_to_number, js_value_typeof, update_vm_js_handle,
    value_to_js_value, vm_bound_function_to_js_value, vm_bound_native_function_to_js_value,
    vm_js_handle,
};
use crate::value::{ClassValue, NativeFunctionValue, Value, array_value};
use js_sys::{Array as JsArray, Function as JsFunction, Reflect};
use js_token_core::{
    BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeOp, BytecodeOperand,
};
use wasm_bindgen::{JsCast, JsValue};

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

pub(crate) fn constant_value(module: &BytecodeModule, index: u32) -> Result<Value, ExecuteError> {
    match module.constants.get(index as usize) {
        Some(BytecodeConstant::Number(value)) => Ok(Value::JsValue(JsValue::from_f64(*value))),
        Some(BytecodeConstant::String(value)) => Ok(Value::JsValue(JsValue::from_str(value))),
        Some(BytecodeConstant::Bool(value)) => Ok(Value::JsValue(JsValue::from_bool(*value))),
        Some(BytecodeConstant::Null) => Ok(Value::JsValue(JsValue::NULL)),
        Some(BytecodeConstant::Undefined) => Ok(Value::JsValue(JsValue::UNDEFINED)),
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
    "typeof",
    "void",
    "delete",
    "in",
    "instanceof",
];

pub(crate) fn get_local_member(object: &Value, property: &str) -> Result<Value, ExecuteError> {
    match object {
        Value::Object(props) => match props.borrow().get(property).cloned() {
            Some(Value::Function(function)) => Ok(Value::JsValue(vm_bound_function_to_js_value(
                function,
                object.clone(),
            ))),
            Some(Value::NativeFunction(function)) => Ok(Value::JsValue(
                vm_bound_native_function_to_js_value(function, object.clone()),
            )),
            Some(value) => Ok(value),
            None => object_prototype_member(object, property),
        },
        Value::Function(function) | Value::BoundFunction(function, _) => {
            if let Some(value) = function.props.borrow().get(property).cloned() {
                Ok(bind_member_value(value, object))
            } else if is_function_native_method(property) {
                Ok(bound_native_method_value(
                    format!("Function.{property}"),
                    object,
                ))
            } else {
                object_prototype_member(object, property)
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
        Value::Class(class) => match class.static_props.get(property).cloned() {
            Some(Value::Function(function)) => Ok(Value::JsValue(vm_bound_function_to_js_value(
                function,
                object.clone(),
            ))),
            Some(Value::NativeFunction(function)) => Ok(Value::JsValue(
                vm_bound_native_function_to_js_value(function, object.clone()),
            )),
            Some(value) => Ok(value),
            None => Ok(Value::Undefined),
        },
        Value::ExternalRef(reference) => Ok(Value::ExternalRef(reference.member(property))),
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            if value.is_null() || value.is_undefined() {
                return Err(ExecuteError::TypeError(format!(
                    "cannot read property {property:?} of {}",
                    js_value_display(value)
                )));
            }
            if let Some(member) = get_vm_js_handle_member(value, object, property)? {
                return Ok(member);
            }
            if let Some(text) = value.as_string() {
                if regex_string_parts(&text).is_some() && is_regexp_native_method(property) {
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
        Value::String(value)
            if regex_string_parts(value).is_some() && is_regexp_native_method(property) =>
        {
            Ok(bound_native_method_value(
                format!("RegExp.{property}"),
                object,
            ))
        }
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
            if let Some(value) = function.props.borrow().get(property).cloned() {
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
        VmJsHandle::Class(class) => Ok(Some(class_member_value(class, object, property))),
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
        Some(value) => value,
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
                value_to_js_value(this_value, &HostBridge::empty()).unwrap_or(JsValue::UNDEFINED);
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
    Value::JsValue(vm_bound_native_function_to_js_value(
        NativeFunctionValue { name },
        this_value.clone(),
    ))
}

pub(crate) fn is_object_prototype_method(property: &str) -> bool {
    matches!(property, "hasOwnProperty" | "toString" | "valueOf")
}

pub(crate) fn is_function_native_method(property: &str) -> bool {
    matches!(property, "apply" | "call" | "toString")
}

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

pub(crate) fn is_array_native_method(property: &str) -> bool {
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

pub(crate) fn is_string_native_method(property: &str) -> bool {
    matches!(
        property,
        "charAt"
            | "includes"
            | "indexOf"
            | "slice"
            | "trim"
            | "toLowerCase"
            | "toUpperCase"
            | "split"
            | "concat"
            | "match"
            | "replace"
    )
}

pub(crate) fn is_regexp_native_method(property: &str) -> bool {
    matches!(property, "exec" | "test")
}

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
                props.borrow_mut().insert(
                    "lastIndex".to_string(),
                    Value::Number((index + needle.len()) as f64),
                );
            }
        }
        result
    } else {
        Value::Null
    }
}

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

pub(crate) fn regexp_match_array(match_text: &str, index: usize, input: &str) -> Value {
    let _ = (index, input);
    array_value(vec![Value::String(match_text.to_string())])
}

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

pub(crate) fn pattern_matches_whitespace_tokens(pattern: &str) -> bool {
    pattern.contains("[^\\x20\\t\\r\\n\\f]+")
        || pattern.contains("[^\\s]+")
        || pattern.contains("\\S+")
}

pub(crate) fn regex_string_parts(pattern: &str) -> Option<(&str, bool)> {
    let rest = pattern.strip_prefix('/')?;
    let end = rest.rfind('/')?;
    let body = &rest[..end];
    let flags = &rest[end + 1..];
    Some((body, flags.contains('g')))
}

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
        Value::Bool(_) => "[object Boolean]",
        Value::Null => "[object Null]",
        Value::Undefined => "[object Undefined]",
        Value::Module(_) => "[object Module]",
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

pub(crate) fn native_function_display_name(name: &str) -> &str {
    name.strip_prefix("Object.prototype.")
        .or_else(|| name.strip_prefix("Function."))
        .or_else(|| name.strip_prefix("Array."))
        .or_else(|| name.strip_prefix("String."))
        .unwrap_or(name)
}

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
    match object {
        Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
            "cannot set property {property:?} of {object}"
        ))),
        Value::Object(props) => {
            props.borrow_mut().insert(property.to_string(), value);
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
                .borrow_mut()
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
            if set_vm_js_handle_member(target, property, value.clone())? {
                return Ok(());
            }
            if !can_represent_value_as_js(&value) {
                js_overlay_set(target, property, value);
                return Ok(());
            }
            Reflect::set(
                target,
                &JsValue::from_str(property),
                &value_to_js_value(&value, &HostBridge::empty())?,
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
                    .borrow_mut()
                    .insert(property.to_string(), value);
                Ok(true)
            }
            VmJsHandle::NativeFunction(_) | VmJsHandle::BoundNativeFunction(_, _) => Ok(false),
            VmJsHandle::Class(class) => {
                if property == "constructor" {
                    match value {
                        Value::Function(function) => class.constructor = Some(function),
                        Value::JsValue(_) | Value::BoundJsFunction(_, _) => {
                            class.constructor =
                                js_constructor_function.expect("checked constructor js value")?;
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

pub(crate) fn binary(op: &str, left: Value, right: Value) -> Result<Value, ExecuteError> {
    match op {
        "+" => match (left, right) {
            (Value::String(left), right) => Ok(Value::String(format!("{left}{right}"))),
            (left, Value::String(right)) => Ok(Value::String(format!("{left}{right}"))),
            (Value::JsValue(left), right) if left.as_string().is_some() => Ok(Value::String(
                format!("{}{}", js_value_display(&left), right),
            )),
            (left, Value::JsValue(right)) if right.as_string().is_some() => Ok(Value::String(
                format!("{}{}", left, js_value_display(&right)),
            )),
            (left, right) => Ok(Value::Number(left.to_number() + right.to_number())),
        },
        "-" => Ok(Value::Number(left.to_number() - right.to_number())),
        "*" => Ok(Value::Number(left.to_number() * right.to_number())),
        "/" => Ok(Value::Number(left.to_number() / right.to_number())),
        "%" => Ok(Value::Number(left.to_number() % right.to_number())),
        "**" => Ok(Value::Number(left.to_number().powf(right.to_number()))),
        "==" => Ok(Value::Bool(loose_eq(&left, &right))),
        "!=" => Ok(Value::Bool(!loose_eq(&left, &right))),
        "===" => Ok(Value::Bool(strict_eq(&left, &right))),
        "!==" => Ok(Value::Bool(!strict_eq(&left, &right))),
        "<" => Ok(Value::Bool(left.to_number() < right.to_number())),
        "<=" => Ok(Value::Bool(left.to_number() <= right.to_number())),
        ">" => Ok(Value::Bool(left.to_number() > right.to_number())),
        ">=" => Ok(Value::Bool(left.to_number() >= right.to_number())),
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
        Value::ExternalRef(_) | Value::Class(_) | Value::Module(_) => true,
        Value::Number(_) | Value::Bool(_) | Value::Symbol(_) => is_object_prototype_method(key),
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
        Value::Function(_) | Value::BoundFunction(_, _) => {
            matches!(value, Value::Object(_) | Value::Array(_))
        }
        Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => {
            native_instance_of(value, &function.name)
        }
        Value::ExternalRef(reference) => native_instance_of(value, &reference.display_path()),
        Value::JsValue(constructor) | Value::BoundJsFunction(constructor, _) => {
            js_instance_of(value, constructor).unwrap_or(false)
        }
        _ => false,
    }
}

pub(crate) fn native_instance_of(value: &Value, constructor: &str) -> bool {
    match constructor.rsplit('.').next().unwrap_or(constructor) {
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
    if can_represent_value_as_js(left) && can_represent_value_as_js(right) {
        return js_binary_bool("return left == right;", left, right).unwrap_or(false);
    }
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

fn strict_eq(left: &Value, right: &Value) -> bool {
    if can_represent_value_as_js(left) && can_represent_value_as_js(right) {
        return js_binary_bool("return left === right;", left, right).unwrap_or(false);
    }
    left == right
}

fn js_binary_bool(source: &str, left: &Value, right: &Value) -> Result<bool, ExecuteError> {
    let function = JsFunction::new_with_args("left, right", source);
    let left = value_to_js_value(left, &HostBridge::empty())?;
    let right = value_to_js_value(right, &HostBridge::empty())?;
    function
        .call2(&JsValue::UNDEFINED, &left, &right)
        .map_err(js_error)
        .map(|value| value.as_bool().unwrap_or(false))
}

pub(crate) fn unary(op: &str, arg: Value) -> Result<Value, ExecuteError> {
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
                | Value::ExternalRef(_) => "object",
                Value::Undefined => "undefined",
            }
            .to_string(),
        )),
        _ => Err(ExecuteError::Unsupported("UNARY")),
    }
}
