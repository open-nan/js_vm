use js_token_core::{BytecodeModule, BytecodeModuleKind};
use js_vm_runtime_core::{
    DEFAULT_MAX_CALL_DEPTH, DEFAULT_MAX_RECURSIVE_CALL_DEPTH, Executor, JsHostBridge,
    value_to_js_value,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn js_execute_bytes_with_seed(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<String, String> {
    execute_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        DEFAULT_MAX_CALL_DEPTH,
        DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
    )
}

#[wasm_bindgen]
pub fn js_execute_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
) -> Result<String, String> {
    execute_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        max_call_depth as usize,
        max_recursive_call_depth as usize,
    )
}

#[wasm_bindgen]
pub fn js_execute_module_bytes_with_seed(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<JsValue, String> {
    execute_module_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        DEFAULT_MAX_CALL_DEPTH,
        DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
    )
}

fn execute_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
) -> Result<String, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge = JsHostBridge::from_js_values(normalize_js_externals(&module, externals));
    Executor::run_with_host_bridge_and_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

fn execute_module_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
) -> Result<JsValue, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let is_module = module.kind == BytecodeModuleKind::Module;
    let host_bridge = JsHostBridge::from_js_values(normalize_js_externals(&module, externals));
    let value = Executor::run_with_host_bridge_and_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
    )
    .map_err(|err| err.to_string())?;
    if is_module {
        value_to_js_value(&value, &JsHostBridge::empty()).map_err(|err| err.to_string())
    } else {
        Ok(JsValue::UNDEFINED)
    }
}

fn normalize_js_externals(module: &BytecodeModule, mut externals: Vec<JsValue>) -> Vec<JsValue> {
    for (index, name) in module.extern_slots.iter().enumerate() {
        if index < externals.len() {
            continue;
        }
        externals.push(browser_global_external(name).unwrap_or(JsValue::UNDEFINED));
    }
    externals
}

fn browser_global_external(name: &str) -> Option<JsValue> {
    match name {
        "window" | "globalThis" | "self" => Some(js_sys::global().into()),
        "document" | "console" | "fetch" | "location" | "navigator" => {
            js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str(name)).ok()
        }
        _ => None,
    }
    .filter(|value| !value.is_undefined())
}
