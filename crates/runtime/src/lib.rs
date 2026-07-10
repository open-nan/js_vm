mod env;
mod error;
mod executor;
mod host;
mod ops;
mod value;

use executor::{DEFAULT_MAX_CALL_DEPTH, DEFAULT_MAX_RECURSIVE_CALL_DEPTH};
use host::fallback_global_external;
use js_token_core::BytecodeModule;
use wasm_bindgen::prelude::*;

pub use env::{EnvironmentRecord, LexicalEnv, ScopeFrame, ScopeKind};
pub use error::ExecuteError;
pub use executor::Executor;
pub use host::HostBridge;
pub use value::{
    ClassValue, ExternalRefValue, FunctionValue, ModuleValue, NativeFunctionValue, Value,
};

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

fn execute_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
) -> Result<String, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge = HostBridge::from_js_values(normalize_js_externals(&module, externals));
    Executor::run_with_host_bridge_and_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

fn normalize_js_externals(module: &BytecodeModule, mut externals: Vec<JsValue>) -> Vec<JsValue> {
    for (index, name) in module.extern_slots.iter().enumerate() {
        if index < externals.len() {
            continue;
        }
        externals.push(fallback_global_external(name).unwrap_or(JsValue::UNDEFINED));
    }
    externals
}
