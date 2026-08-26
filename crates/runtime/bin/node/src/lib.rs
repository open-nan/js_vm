//! Node 环境运行时 wasm 入口。
//!
//! 该 crate 与 browser 包共享 runtime core，但面向 Node/bundler 场景输出 wasm-bindgen 绑定。
//! CLI 打包 Node 目标时会使用这里的入口加载 `.bin` 并执行 VM bytecode。
//!
//! externals 仍由调用方按数组传入，runtime 只按 slot 访问，不直接假设 `window` 存在。

#[cfg(any(feature = "source-map", feature = "runtime-profile"))]
use js_sys::{Array, Object, Reflect};
use js_token_core::{BytecodeModule, BytecodeModuleKind};
use js_vm_runtime_core::{
    DEFAULT_MAX_CALL_DEPTH, DEFAULT_MAX_EXECUTION_STEPS, DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
    Executor, JsHostBridge, value_to_js_value,
};
#[cfg(feature = "debugger")]
use js_vm_runtime_core::{DebugSnapshot, ExecutorDebugSession};
#[cfg(feature = "runtime-profile")]
use js_vm_runtime_core::{ExecuteError, RuntimeProfile, Value};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
/// 执行 bytecode 并返回字符串化结果。
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
        DEFAULT_MAX_EXECUTION_STEPS,
    )
}

#[wasm_bindgen]
/// 执行 bytecode，并允许设置调用深度限制。
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
        DEFAULT_MAX_EXECUTION_STEPS,
    )
}

#[wasm_bindgen]
/// 执行 bytecode，并允许设置调用深度、递归深度和步数预算。
pub fn js_execute_bytes_with_seed_and_runtime_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
    max_execution_steps: u32,
) -> Result<String, String> {
    execute_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        max_call_depth as usize,
        max_recursive_call_depth as usize,
        max_execution_steps as usize,
    )
}

#[wasm_bindgen]
/// 执行 bytecode，忽略返回值。
pub fn js_execute_void_bytes_with_seed(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<(), String> {
    execute_void_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        DEFAULT_MAX_CALL_DEPTH,
        DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
        DEFAULT_MAX_EXECUTION_STEPS,
    )
}

#[wasm_bindgen]
/// 执行 bytecode，忽略返回值，并使用自定义运行限制。
pub fn js_execute_void_bytes_with_seed_and_runtime_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
    max_execution_steps: u32,
) -> Result<(), String> {
    execute_void_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        max_call_depth as usize,
        max_recursive_call_depth as usize,
        max_execution_steps as usize,
    )
}

#[wasm_bindgen]
/// 执行 ES module bytecode，返回模块 namespace 对象。
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
        DEFAULT_MAX_EXECUTION_STEPS,
    )
}

#[wasm_bindgen]
/// 执行 ES module bytecode，返回模块 namespace 对象，并使用自定义运行限制。
pub fn js_execute_module_bytes_with_seed_and_runtime_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
    max_execution_steps: u32,
) -> Result<JsValue, String> {
    execute_module_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        max_call_depth as usize,
        max_recursive_call_depth as usize,
        max_execution_steps as usize,
    )
}

#[wasm_bindgen]
/// 执行 bytecode，返回原生 `JsValue`。
pub fn js_execute_value_bytes_with_seed(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<JsValue, String> {
    execute_value_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        DEFAULT_MAX_CALL_DEPTH,
        DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
        DEFAULT_MAX_EXECUTION_STEPS,
    )
}

#[wasm_bindgen]
pub fn js_execute_value_bytes_with_seed_and_runtime_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
    max_execution_steps: u32,
) -> Result<JsValue, String> {
    execute_value_bytes_with_seed_and_limits(
        bytes,
        seed,
        externals.into_vec(),
        max_call_depth as usize,
        max_recursive_call_depth as usize,
        max_execution_steps as usize,
    )
}

#[cfg(feature = "runtime-profile")]
#[wasm_bindgen]
/// 执行 bytecode 并返回运行时 profile。
pub fn js_profile_execute_bytes_with_seed(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<JsValue, String> {
    js_profile_execute_bytes_with_seed_and_runtime_limits(
        bytes,
        seed,
        externals,
        DEFAULT_MAX_CALL_DEPTH as u32,
        DEFAULT_MAX_RECURSIVE_CALL_DEPTH as u32,
        DEFAULT_MAX_EXECUTION_STEPS as u32,
    )
}

#[cfg(feature = "runtime-profile")]
#[wasm_bindgen]
/// 执行 bytecode 并返回运行时 profile，可设置调用深度和步数预算。
pub fn js_profile_execute_bytes_with_seed_and_runtime_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
    max_execution_steps: u32,
) -> Result<JsValue, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge =
        JsHostBridge::from_js_values(normalize_js_externals(&module, externals.into_vec()));
    let (result, profile) = Executor::profile_with_host_bridge_and_runtime_limits(
        &module,
        host_bridge,
        max_call_depth as usize,
        max_recursive_call_depth as usize,
        max_execution_steps as usize,
    );
    profile_result_to_js(result, profile)
}

#[cfg(feature = "source-map")]
#[wasm_bindgen]
pub fn js_execute_bytes_with_seed_debug(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
) -> Result<JsValue, String> {
    js_execute_bytes_with_seed_debug_and_runtime_limits(
        bytes,
        seed,
        externals,
        DEFAULT_MAX_CALL_DEPTH as u32,
        DEFAULT_MAX_RECURSIVE_CALL_DEPTH as u32,
        DEFAULT_MAX_EXECUTION_STEPS as u32,
    )
}

#[cfg(feature = "source-map")]
#[wasm_bindgen]
pub fn js_execute_bytes_with_seed_debug_and_runtime_limits(
    bytes: &[u8],
    seed: &str,
    externals: Box<[JsValue]>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
    max_execution_steps: u32,
) -> Result<JsValue, String> {
    let module = match BytecodeModule::from_bytes_with_seed(bytes, seed) {
        Ok(module) => module,
        Err(err) => return debug_error_value("decode", &err.to_string()),
    };
    let host_bridge =
        JsHostBridge::from_js_values(normalize_js_externals(&module, externals.into_vec()));
    match Executor::run_with_host_bridge_and_runtime_limits(
        &module,
        host_bridge,
        max_call_depth as usize,
        max_recursive_call_depth as usize,
        max_execution_steps as usize,
    ) {
        Ok(value) => debug_ok_value(&value.to_string()),
        Err(err) => debug_error_value("runtime", &err.to_string()),
    }
}

#[cfg(feature = "debugger")]
#[wasm_bindgen]
pub struct JsVmDebugSession {
    inner: ExecutorDebugSession<JsHostBridge>,
}

#[cfg(feature = "debugger")]
#[wasm_bindgen]
impl JsVmDebugSession {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8], seed: &str, externals: Box<[JsValue]>) -> Result<Self, String> {
        Self::new_with_runtime_limits(
            bytes,
            seed,
            externals,
            DEFAULT_MAX_CALL_DEPTH as u32,
            DEFAULT_MAX_RECURSIVE_CALL_DEPTH as u32,
            DEFAULT_MAX_EXECUTION_STEPS as u32,
        )
    }

    pub fn new_with_runtime_limits(
        bytes: &[u8],
        seed: &str,
        externals: Box<[JsValue]>,
        max_call_depth: u32,
        max_recursive_call_depth: u32,
        max_execution_steps: u32,
    ) -> Result<Self, String> {
        let module =
            BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
        let host_bridge =
            JsHostBridge::from_js_values(normalize_js_externals(&module, externals.into_vec()));
        let inner = Executor::debug_session_with_host_bridge_and_runtime_limits(
            &module,
            host_bridge,
            max_call_depth as usize,
            max_recursive_call_depth as usize,
            max_execution_steps as usize,
        )
        .map_err(|err| err.to_string())?;
        Ok(Self { inner })
    }

    pub fn set_breakpoints(&mut self, pcs: Box<[JsValue]>) -> Result<(), String> {
        self.inner.set_breakpoints(parse_breakpoint_pcs(pcs)?);
        Ok(())
    }

    pub fn clear_breakpoints(&mut self) {
        self.inner.clear_breakpoints();
    }

    pub fn resume(&mut self) -> Result<JsValue, String> {
        debug_snapshot_to_js(self.inner.resume().map_err(|err| err.to_string())?)
    }

    pub fn step(&mut self) -> Result<JsValue, String> {
        debug_snapshot_to_js(self.inner.step().map_err(|err| err.to_string())?)
    }

    pub fn inspect(&self) -> Result<JsValue, String> {
        debug_snapshot_to_js(self.inner.inspect())
    }
}

fn execute_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
    max_execution_steps: usize,
) -> Result<String, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge = JsHostBridge::from_js_values(normalize_js_externals(&module, externals));
    Executor::run_with_host_bridge_and_runtime_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
        max_execution_steps,
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

fn execute_void_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
    max_execution_steps: usize,
) -> Result<(), String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge = JsHostBridge::from_js_values(normalize_js_externals(&module, externals));
    Executor::run_with_host_bridge_and_runtime_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
        max_execution_steps,
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn execute_module_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
    max_execution_steps: usize,
) -> Result<JsValue, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let is_module = module.kind == BytecodeModuleKind::Module;
    let host_bridge = JsHostBridge::from_js_values(normalize_js_externals(&module, externals));
    let value = Executor::run_with_host_bridge_and_runtime_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
        max_execution_steps,
    )
    .map_err(|err| err.to_string())?;
    if is_module {
        value_to_js_value(&value, &JsHostBridge::empty()).map_err(|err| err.to_string())
    } else {
        Ok(JsValue::UNDEFINED)
    }
}

fn execute_value_bytes_with_seed_and_limits(
    bytes: &[u8],
    seed: &str,
    externals: Vec<JsValue>,
    max_call_depth: usize,
    max_recursive_call_depth: usize,
    max_execution_steps: usize,
) -> Result<JsValue, String> {
    let module =
        BytecodeModule::from_bytes_with_seed(bytes, seed).map_err(|err| err.to_string())?;
    let host_bridge = JsHostBridge::from_js_values(normalize_js_externals(&module, externals));
    let value = Executor::run_with_host_bridge_and_runtime_limits(
        &module,
        host_bridge,
        max_call_depth,
        max_recursive_call_depth,
        max_execution_steps,
    )
    .map_err(|err| err.to_string())?;
    value_to_js_value(&value, &JsHostBridge::empty()).map_err(|err| err.to_string())
}

fn normalize_js_externals(module: &BytecodeModule, mut externals: Vec<JsValue>) -> Vec<JsValue> {
    // Node 包和浏览器包共享 executor，只在这里补 Node 场景常见全局对象。
    // 如果调用方显式传入 externals，则显式值优先，保证打包产物可以接入自定义宿主环境。
    for (index, name) in module.extern_slots.iter().enumerate() {
        if index < externals.len() {
            continue;
        }
        externals.push(node_global_external(name).unwrap_or(JsValue::UNDEFINED));
    }
    externals
}

fn node_global_external(name: &str) -> Option<JsValue> {
    // 这里只解析全局根，不解析具体方法。方法调用始终由 HostBridge 的 Reflect 路径完成。
    match name {
        "global" | "globalThis" => Some(js_sys::global().into()),
        "console" | "process" | "Buffer" | "setTimeout" | "clearTimeout" | "fetch" => {
            js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str(name)).ok()
        }
        _ => None,
    }
    .filter(|value| !value.is_undefined())
}

#[cfg(feature = "runtime-profile")]
fn profile_result_to_js(
    result: Result<Value, ExecuteError>,
    profile: RuntimeProfile,
) -> Result<JsValue, String> {
    let object = Object::new();
    match result {
        Ok(value) => {
            set_profile_prop(&object, "ok", JsValue::TRUE)?;
            set_profile_prop(&object, "value", JsValue::from_str(&value.to_string()))?;
        }
        Err(error) => {
            set_profile_prop(&object, "ok", JsValue::FALSE)?;
            set_profile_prop(&object, "error", JsValue::from_str(&error.to_string()))?;
        }
    }
    set_profile_prop(&object, "profile", runtime_profile_to_js(profile)?)?;
    Ok(object.into())
}

#[cfg(feature = "runtime-profile")]
fn runtime_profile_to_js(profile: RuntimeProfile) -> Result<JsValue, String> {
    let object = Object::new();
    set_profile_prop(
        &object,
        "instructionCount",
        JsValue::from_f64(profile.instruction_count as f64),
    )?;
    set_profile_prop(
        &object,
        "callbackCount",
        JsValue::from_f64(profile.callback_count as f64),
    )?;
    set_profile_prop(
        &object,
        "callbackFastPathCount",
        JsValue::from_f64(profile.callback_fast_path_count as f64),
    )?;
    set_profile_prop(
        &object,
        "loadNameCacheHitCount",
        JsValue::from_f64(profile.load_name_cache_hit_count as f64),
    )?;
    set_profile_prop(
        &object,
        "loadNameCacheMissCount",
        JsValue::from_f64(profile.load_name_cache_miss_count as f64),
    )?;
    set_profile_prop(
        &object,
        "memberConstCacheHitCount",
        JsValue::from_f64(profile.member_const_cache_hit_count as f64),
    )?;
    set_profile_prop(
        &object,
        "memberConstCacheMissCount",
        JsValue::from_f64(profile.member_const_cache_miss_count as f64),
    )?;
    set_profile_prop(
        &object,
        "callOneCacheHitCount",
        JsValue::from_f64(profile.call_one_cache_hit_count as f64),
    )?;
    set_profile_prop(
        &object,
        "callOneCacheMissCount",
        JsValue::from_f64(profile.call_one_cache_miss_count as f64),
    )?;
    set_profile_prop(
        &object,
        "memberCallCacheHitCount",
        JsValue::from_f64(profile.member_call_cache_hit_count as f64),
    )?;
    set_profile_prop(
        &object,
        "memberCallCacheMissCount",
        JsValue::from_f64(profile.member_call_cache_miss_count as f64),
    )?;
    set_profile_prop(
        &object,
        "fastBinaryRegConstCount",
        JsValue::from_f64(profile.fast_binary_reg_const_count as f64),
    )?;
    set_profile_prop(
        &object,
        "fastBinaryRegRegCount",
        JsValue::from_f64(profile.fast_binary_reg_reg_count as f64),
    )?;
    set_profile_prop(
        &object,
        "fusedBinaryBranchCount",
        JsValue::from_f64(profile.fused_binary_branch_count as f64),
    )?;
    set_profile_prop(
        &object,
        "fusedMoveBranchCount",
        JsValue::from_f64(profile.fused_move_branch_count as f64),
    )?;
    set_profile_prop(
        &object,
        "fusedRegBranchJumpCount",
        JsValue::from_f64(profile.fused_reg_branch_jump_count as f64),
    )?;
    set_profile_prop(
        &object,
        "hostGetCount",
        JsValue::from_f64(profile.host_get_count as f64),
    )?;
    set_profile_prop(
        &object,
        "hostSetCount",
        JsValue::from_f64(profile.host_set_count as f64),
    )?;
    set_profile_prop(
        &object,
        "hostCallCount",
        JsValue::from_f64(profile.host_call_count as f64),
    )?;
    set_profile_prop(
        &object,
        "hostConstructCount",
        JsValue::from_f64(profile.host_construct_count as f64),
    )?;
    set_profile_prop(
        &object,
        "lastPc",
        profile
            .last_pc
            .map(|pc| JsValue::from_f64(pc as f64))
            .unwrap_or(JsValue::NULL),
    )?;

    let callback_stack = Array::new();
    for entry in profile.callback_stack {
        callback_stack.push(&JsValue::from_str(&entry));
    }
    set_profile_prop(&object, "callbackStack", callback_stack.into())?;

    let opcodes = Array::new();
    for entry in profile.opcodes {
        let item = Object::new();
        set_profile_prop(&item, "name", JsValue::from_str(&entry.name))?;
        set_profile_prop(&item, "count", JsValue::from_f64(entry.count as f64))?;
        opcodes.push(&item);
    }
    set_profile_prop(&object, "opcodes", opcodes.into())?;

    let functions = Array::new();
    for entry in profile.functions {
        let item = Object::new();
        set_profile_prop(&item, "name", JsValue::from_str(&entry.name))?;
        set_profile_prop(
            &item,
            "bodyStart",
            JsValue::from_f64(entry.body_start as f64),
        )?;
        set_profile_prop(&item, "bodyEnd", JsValue::from_f64(entry.body_end as f64))?;
        set_profile_prop(
            &item,
            "callCount",
            JsValue::from_f64(entry.call_count as f64),
        )?;
        set_profile_prop(
            &item,
            "instructionCount",
            JsValue::from_f64(entry.instruction_count as f64),
        )?;
        functions.push(&item);
    }
    set_profile_prop(&object, "functions", functions.into())?;

    let callbacks = Array::new();
    for entry in profile.callbacks {
        let item = Object::new();
        set_profile_prop(&item, "label", JsValue::from_str(&entry.label))?;
        set_profile_prop(
            &item,
            "callCount",
            JsValue::from_f64(entry.call_count as f64),
        )?;
        set_profile_prop(
            &item,
            "instructionCount",
            JsValue::from_f64(entry.instruction_count as f64),
        )?;
        callbacks.push(&item);
    }
    set_profile_prop(&object, "callbacks", callbacks.into())?;

    let hot_pcs = Array::new();
    for entry in profile.hot_pcs {
        let item = Object::new();
        set_profile_prop(&item, "pc", JsValue::from_f64(entry.pc as f64))?;
        set_profile_prop(&item, "op", JsValue::from_str(&entry.op))?;
        set_profile_prop(&item, "count", JsValue::from_f64(entry.count as f64))?;
        hot_pcs.push(&item);
    }
    set_profile_prop(&object, "hotPcs", hot_pcs.into())?;
    Ok(object.into())
}

#[cfg(feature = "runtime-profile")]
fn set_profile_prop(object: &Object, name: &str, value: JsValue) -> Result<(), String> {
    Reflect::set(object, &JsValue::from_str(name), &value)
        .map(|_| ())
        .map_err(|err| format!("cannot set profile result {name}: {err:?}"))
}

#[cfg(feature = "source-map")]
fn debug_ok_value(value: &str) -> Result<JsValue, String> {
    let object = Object::new();
    set_debug_prop(&object, "ok", JsValue::TRUE)?;
    set_debug_prop(&object, "stage", JsValue::from_str("runtime"))?;
    set_debug_prop(&object, "value", JsValue::from_str(value))?;
    set_debug_prop(&object, "stack", Array::new().into())?;
    Ok(object.into())
}

#[cfg(feature = "source-map")]
fn debug_error_value(stage: &str, message: &str) -> Result<JsValue, String> {
    let object = Object::new();
    set_debug_prop(&object, "ok", JsValue::FALSE)?;
    set_debug_prop(&object, "stage", JsValue::from_str(stage))?;
    set_debug_prop(&object, "error", JsValue::from_str(message))?;
    set_debug_prop(&object, "stack", pc_stack_to_js_array(message))?;
    Ok(object.into())
}

#[cfg(feature = "source-map")]
fn pc_stack_to_js_array(message: &str) -> JsValue {
    let array = Array::new();
    for pc in pc_stack_from_message(message) {
        let frame = Object::new();
        let _ = Reflect::set(
            &frame,
            &JsValue::from_str("pc"),
            &JsValue::from_f64(f64::from(pc)),
        );
        array.push(&frame);
    }
    array.into()
}

#[cfg(feature = "source-map")]
fn pc_stack_from_message(message: &str) -> Vec<u32> {
    let mut stack = Vec::new();
    let mut rest = message;
    while let Some(index) = rest.find("pc ") {
        rest = &rest[index + 3..];
        let digits = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if let Ok(pc) = digits.parse::<u32>() {
            stack.push(pc);
        }
    }
    stack
}

#[cfg(feature = "source-map")]
fn set_debug_prop(object: &Object, name: &str, value: JsValue) -> Result<(), String> {
    Reflect::set(object, &JsValue::from_str(name), &value)
        .map(|_| ())
        .map_err(|err| format!("cannot set debug result {name}: {err:?}"))
}

#[cfg(feature = "debugger")]
fn parse_breakpoint_pcs(values: Box<[JsValue]>) -> Result<Vec<usize>, String> {
    values
        .iter()
        .map(|value| {
            let pc = value
                .as_f64()
                .filter(|pc| pc.is_finite() && *pc >= 0.0)
                .ok_or_else(|| format!("invalid breakpoint pc: {value:?}"))?;
            Ok(pc as usize)
        })
        .collect()
}

#[cfg(feature = "debugger")]
fn debug_snapshot_to_js(snapshot: DebugSnapshot) -> Result<JsValue, String> {
    let object = Object::new();
    set_debug_prop(
        &object,
        "type",
        JsValue::from_str(if snapshot.done {
            "done"
        } else if snapshot.paused {
            "paused"
        } else {
            "running"
        }),
    )?;
    set_debug_prop(&object, "pc", JsValue::from_f64(snapshot.pc as f64))?;
    set_debug_prop(&object, "reason", JsValue::from_str(&snapshot.reason))?;
    set_debug_prop(&object, "done", JsValue::from_bool(snapshot.done))?;
    set_debug_prop(&object, "paused", JsValue::from_bool(snapshot.paused))?;
    set_debug_prop(&object, "value", JsValue::from_str(&snapshot.value))?;
    set_debug_prop(
        &object,
        "remainingSteps",
        JsValue::from_f64(snapshot.remaining_steps as f64),
    )?;

    let registers = Array::new();
    for register in snapshot.registers {
        registers.push(&JsValue::from_str(&register));
    }
    set_debug_prop(&object, "registers", registers.into())?;

    let call_stack = Array::new();
    for pc in snapshot.call_stack {
        let frame = Object::new();
        set_debug_prop(&frame, "pc", JsValue::from_f64(pc as f64))?;
        call_stack.push(&frame);
    }
    set_debug_prop(&object, "callStack", call_stack.into())?;
    Ok(object.into())
}
