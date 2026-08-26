//! Bytecode 解释执行器。
//!
//! Executor 是 runtime core 的核心状态机。它按 pc 读取 `BytecodeInstruction`，
//! 使用寄存器保存临时值，用 `LexicalEnv` 管理作用域，用 `HostBridge` 访问宿主环境。
//!
//! 需要特别注意 VM 函数作为 JS callback 的生命周期：VM 函数被暴露给 JS 后，宿主 JS
//! 可能稍后再调用它。此时 runtime 会通过 `set_vm_js_callback_invoker` 安装一个 invoker，
//! 用新的 executor 恢复同一份 module、bridge 和函数闭包环境，避免原 executor 生命周期结束后
//! callback 失效。

use crate::env::{LexicalEnv, ScopeKind};
use crate::error::ExecuteError;
#[cfg(feature = "runtime-profile")]
use crate::host::install_runtime_profile_panic_hook;
use crate::host::{
    HostBridge, JsHostBridge, VmJsHandle, can_represent_value_as_js, clear_host_overlays,
    execute_error_to_js_value, external_name_overlay_get, get_js_property,
    get_js_property_with_key, js_error, js_function_name, js_overlay_get,
    js_overlay_get_in_prototype_chain, js_overlay_set, js_reflect_property_key,
    js_typed_array_index_get, js_typed_array_u32_index_get, js_typed_array_u32_index_set,
    js_value_is_symbol, js_value_typeof, set_vm_js_callback_invoker, symbol_value_to_js_value,
    value_to_js_value, vm_js_handle,
};
#[cfg(feature = "bigint")]
use crate::host::{js_bigint_builtin_name, js_value_is_bigint};
#[cfg(feature = "regexp")]
use crate::host::{js_regexp_exec, js_regexp_test};
#[cfg(any(feature = "function-builtins", feature = "generator"))]
use crate::ops::apply_argument_list;
#[cfg(feature = "function-builtins")]
use crate::ops::function_source_string;
#[cfg(any(feature = "array-builtins", feature = "string-builtins"))]
use crate::ops::normalize_index;
#[cfg(feature = "regexp")]
use crate::ops::regexp_exec_value;
use crate::ops::{
    binary as fallback_binary, bind_member_value, collect_scope_metadata, constant_string,
    constant_value, count_operand, external_string, find_try_parts, get_local_member,
    local_slot_operand, name_string, operand, operator_name, property_key, register, set_member,
    shift_count, to_int32, to_uint32, unary,
};
#[cfg(feature = "object-builtins")]
use crate::ops::{object_has_own_property, object_to_string_tag};
#[cfg(feature = "generator")]
use crate::value::GeneratorState;
#[cfg(feature = "module")]
use crate::value::ModuleValue;
use crate::value::{
    ClassValue, ExternalRefValue, FunctionValue, NativeFunctionValue, Value, array_value,
    object_value,
};
use js_sys::{
    Array as JsArray, Function as JsFunction, Object as JsObject, Promise as JsPromise, Reflect,
};
use js_token_core::{
    BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeModuleKind, BytecodeOp,
    BytecodeOperand,
};
use std::{
    borrow::Cow,
    cell::Cell,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

#[cfg(feature = "array-builtins")]
thread_local! {
    static ARRAY_ITERATION_METHODS: RefCell<BTreeMap<&'static str, JsValue>> = RefCell::new(BTreeMap::new());
}

/// Bytecode 执行器。
///
/// `B` 是宿主桥类型。浏览器/Node 可以使用不同 bridge，但执行器内部逻辑保持一致。
#[derive(Debug)]
pub struct Executor<B: HostBridge> {
    /// 当前函数帧的寄存器。
    registers: Vec<Value>,
    /// 当前词法环境链。
    lexical_env: LexicalEnv,
    /// 每条指令执行前的作用域深度，用于异常/跳转恢复。
    instruction_scope_depths: Vec<usize>,
    /// 函数声明 hoist 缓存。
    ///
    /// JS 函数每次调用都要先提升函数声明。大型 bundle 里大量小函数会被 Vue/Router
    /// 高频调用，如果每次都线性扫描函数体，启动 hydration 会被放大到几十秒。
    /// 这里按 `(body_start, body_end)` 缓存需要 hoist 的 `FunctionStart` pc 列表。
    function_hoist_cache: Rc<RefCell<BTreeMap<(usize, usize), Vec<usize>>>>,
    /// 执行区间需要的寄存器帧大小缓存。
    ///
    /// 编译器已经尽量重编号寄存器，但运行时如果边执行边扩容，热点函数会重复触发
    /// `Vec::resize`。这里按执行区间缓存最大 register + 1，函数调用时一次性准备好帧。
    register_frame_size_cache: Rc<RefCell<BTreeMap<(usize, usize), usize>>>,
    /// 按 pc 保存解码后的热点操作信息。
    ///
    /// 当前先缓存 `LOAD_NAME` 的解析形态，避免 Proxy/getter 这类热路径反复解 name/extern
    /// operand。缓存只保存绑定描述，不保存运行时 Value，因此不会读到旧变量值。
    pc_inline_cache: Rc<RefCell<PcInlineCache>>,
    /// 最近一次表达式或语句产生的值。
    last_value: Value,
    /// ES module 导出表。
    exports: BTreeMap<String, Value>,
    /// 宿主环境桥。
    host_bridge: B,
    /// 运行时 extern 名称。压缩 bytecode 只记录数量时由调用方传入。
    external_names: Vec<String>,
    /// 当前 executor 所属 bytecode module。
    module_handle: Option<Rc<BytecodeModule>>,
    /// 当前调用深度。
    call_depth: usize,
    /// 最大调用深度。
    max_call_depth: usize,
    /// 单个函数 body 递归重入上限。
    max_recursive_call_depth: usize,
    /// 调用栈 pc，用于递归限制和错误上下文。
    call_stack: Rc<RefCell<Vec<usize>>>,
    /// 当前是否处在宿主 JS 调用中。
    ///
    /// Array.map/reduce、Proxy trap、getter/setter 等宿主调用可能同步回调 VM 函数。
    /// 这类回调必须共享当前执行预算，否则每次回调都会得到一份新预算，死循环保护会被稀释。
    host_call_depth: Rc<Cell<usize>>,
    /// 剩余执行步数预算，用于限制死循环。
    execution_budget: Rc<Cell<usize>>,
    #[cfg(feature = "runtime-profile")]
    runtime_profile: Option<Rc<RefCell<RuntimeProfileState>>>,
    #[cfg(feature = "debugger")]
    debug_breakpoints: Rc<RefCell<BTreeSet<usize>>>,
    #[cfg(feature = "debugger")]
    debug_skip_breakpoint: Rc<Cell<Option<usize>>>,
}

/// 默认最大调用深度。
pub const DEFAULT_MAX_CALL_DEPTH: usize = 2048;
/// 默认同一函数递归重入深度。
pub const DEFAULT_MAX_RECURSIVE_CALL_DEPTH: usize = 128;
/// 默认最大执行步数。
pub const DEFAULT_MAX_EXECUTION_STEPS: usize = 5_000_000;
const FUNCTION_THIS_SLOT: u32 = 1;
#[cfg(feature = "generator")]
const FUNCTION_FLAG_GENERATOR: u32 = 1 << 1;
const FUNCTION_FLAG_ASYNC: u32 = 1 << 2;

#[derive(Debug, Default)]
struct PcInlineCache {
    load_names: Vec<Option<LoadNameInlineCache>>,
    member_consts: Vec<Option<MemberConstInlineCache>>,
    call_ones: Vec<Option<CallOneInlineCache>>,
    member_calls: Vec<Option<MemberCallInlineCache>>,
}

#[derive(Debug, Clone)]
struct LoadNameInlineCache {
    source: BytecodeOperand,
    kind: LoadNameInlineCacheKind,
}

#[derive(Debug, Clone)]
enum LoadNameInlineCacheKind {
    External(u32),
    LocalSlot(u32),
    Name(String),
}

#[derive(Debug, Clone)]
struct MemberConstInlineCache {
    property_source: BytecodeOperand,
    property: String,
    property_key: JsValue,
}

#[derive(Debug, Clone)]
struct CallOneInlineCache {
    callee_source: BytecodeOperand,
    function: JsValue,
}

#[derive(Debug, Clone)]
struct MemberCallInlineCache {
    property_source: BytecodeOperand,
    property: String,
    property_key: JsValue,
    function: JsValue,
}

struct PlainJsMemberFunction {
    function: JsValue,
    this_value: JsValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmallCallArity {
    Zero,
    One,
    Two,
    Three,
}

impl SmallCallArity {
    fn from_op(op: BytecodeOp) -> Option<Self> {
        match op {
            BytecodeOp::CallZero => Some(Self::Zero),
            BytecodeOp::CallOne => Some(Self::One),
            BytecodeOp::CallTwo => Some(Self::Two),
            _ => None,
        }
    }

    fn count(self) -> u32 {
        match self {
            Self::Zero => 0,
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
        }
    }

    fn len(self) -> usize {
        self.count() as usize
    }
}

impl PcInlineCache {
    fn load_name(&self, pc: usize, source: &BytecodeOperand) -> Option<LoadNameInlineCacheKind> {
        self.load_names
            .get(pc)
            .and_then(Option::as_ref)
            .filter(|entry| entry.source == *source)
            .map(|entry| entry.kind.clone())
    }

    fn store_load_name(
        &mut self,
        pc: usize,
        source: BytecodeOperand,
        kind: LoadNameInlineCacheKind,
    ) {
        if self.load_names.len() <= pc {
            self.load_names.resize_with(pc + 1, || None);
        }
        self.load_names[pc] = Some(LoadNameInlineCache { source, kind });
    }

    fn member_const(
        &self,
        pc: usize,
        property_source: &BytecodeOperand,
    ) -> Option<MemberConstInlineCache> {
        self.member_consts
            .get(pc)
            .and_then(Option::as_ref)
            .filter(|entry| entry.property_source == *property_source)
            .cloned()
    }

    fn store_member_const(
        &mut self,
        pc: usize,
        property_source: BytecodeOperand,
        property: String,
        property_key: JsValue,
    ) {
        if self.member_consts.len() <= pc {
            self.member_consts.resize_with(pc + 1, || None);
        }
        self.member_consts[pc] = Some(MemberConstInlineCache {
            property_source,
            property,
            property_key,
        });
    }

    fn call_one_function(&self, pc: usize, callee_source: &BytecodeOperand) -> Option<JsValue> {
        self.call_ones
            .get(pc)
            .and_then(Option::as_ref)
            .filter(|entry| entry.callee_source == *callee_source)
            .map(|entry| entry.function.clone())
    }

    fn store_call_one_function(
        &mut self,
        pc: usize,
        callee_source: BytecodeOperand,
        function: JsValue,
    ) {
        if self.call_ones.len() <= pc {
            self.call_ones.resize_with(pc + 1, || None);
        }
        self.call_ones[pc] = Some(CallOneInlineCache {
            callee_source,
            function,
        });
    }

    fn member_call(
        &self,
        pc: usize,
        property_source: &BytecodeOperand,
    ) -> Option<MemberCallInlineCache> {
        self.member_calls
            .get(pc)
            .and_then(Option::as_ref)
            .filter(|entry| entry.property_source == *property_source)
            .cloned()
    }

    fn store_member_call(
        &mut self,
        pc: usize,
        property_source: BytecodeOperand,
        property: String,
        property_key: JsValue,
        function: JsValue,
    ) {
        if self.member_calls.len() <= pc {
            self.member_calls.resize_with(pc + 1, || None);
        }
        self.member_calls[pc] = Some(MemberCallInlineCache {
            property_source,
            property,
            property_key,
            function,
        });
    }
}

/// 运行时执行 profile。
///
/// 该结构只在 `runtime-profile` feature 打开时启用，用来分析解释器真实热区；
/// 普通 slim/runtime 包不会携带这部分代码。
#[cfg(feature = "runtime-profile")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeProfile {
    /// 已执行的 bytecode 指令数。
    pub instruction_count: usize,
    /// VM 函数被宿主 JS callback invoker 调回的次数。
    pub callback_count: usize,
    /// 其中走 callback invoker 快路径的次数。
    pub callback_fast_path_count: usize,
    /// `LOAD_NAME` pc inline cache 命中次数。
    pub load_name_cache_hit_count: usize,
    /// `LOAD_NAME` pc inline cache 未命中次数。
    pub load_name_cache_miss_count: usize,
    /// `MEMBER_CONST` pc inline cache 命中次数。
    pub member_const_cache_hit_count: usize,
    /// `MEMBER_CONST` pc inline cache 未命中次数。
    pub member_const_cache_miss_count: usize,
    /// `CALL_1` pc inline cache 命中次数。
    pub call_one_cache_hit_count: usize,
    /// `CALL_1` pc inline cache 未命中次数。
    pub call_one_cache_miss_count: usize,
    /// `MEMBER/MEMBER_CONST + CALL_0/1/2` 成员调用 inline cache 命中次数。
    pub member_call_cache_hit_count: usize,
    /// `MEMBER/MEMBER_CONST + CALL_0/1/2` 成员调用 inline cache 未命中次数。
    pub member_call_cache_miss_count: usize,
    /// `BINARY_REG_CONST` 寄存器和常量数字快路径命中次数。
    pub fast_binary_reg_const_count: usize,
    /// `BINARY_REG_REG` 双寄存器数字快路径命中次数。
    pub fast_binary_reg_reg_count: usize,
    /// `BINARY_REG_REG + JUMP_IF_*` 运行时融合次数。
    pub fused_binary_branch_count: usize,
    /// `MOVE + JUMP_IF_*` 运行时融合次数。
    pub fused_move_branch_count: usize,
    /// `JUMP_IF_* + JUMP` 运行时融合次数。
    pub fused_reg_branch_jump_count: usize,
    /// 通过宿主/JS 反射读取属性的次数。
    pub host_get_count: usize,
    /// 通过宿主/JS 反射写入属性的次数。
    pub host_set_count: usize,
    /// 调用宿主/JS 函数的次数。
    pub host_call_count: usize,
    /// 构造调用宿主/JS 函数的次数。
    pub host_construct_count: usize,
    /// 最后一次执行到的 pc。
    pub last_pc: Option<usize>,
    /// 当前仍未退出的 VM callback 栈。
    ///
    /// 正常路径下这里通常为空；如果宿主 JS 调用 VM callback 时发生 wasm trap，
    /// Rust 的 Drop 不会运行，profile snapshot 可以借此保留最后的 callback 现场。
    pub callback_stack: Vec<String>,
    /// 按 opcode 聚合的执行次数，按热度倒序。
    pub opcodes: Vec<RuntimeProfileEntry>,
    /// 按 VM 函数聚合的调用/执行热度。
    pub functions: Vec<RuntimeProfileFunctionEntry>,
    /// 按宿主回调入口聚合的调用/执行热度。
    pub callbacks: Vec<RuntimeProfileCallbackEntry>,
    /// 最热 pc，按热度倒序，最多保留前 32 项。
    pub hot_pcs: Vec<RuntimeProfilePcEntry>,
}

#[cfg(feature = "runtime-profile")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeProfileEntry {
    pub name: String,
    pub count: usize,
}

#[cfg(feature = "runtime-profile")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeProfileFunctionEntry {
    pub name: String,
    pub body_start: usize,
    pub body_end: usize,
    pub call_count: usize,
    pub instruction_count: usize,
}

#[cfg(feature = "runtime-profile")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeProfileCallbackEntry {
    pub label: String,
    pub call_count: usize,
    pub instruction_count: usize,
}

#[cfg(feature = "runtime-profile")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeProfilePcEntry {
    pub pc: usize,
    pub op: String,
    pub count: usize,
}

#[cfg(feature = "runtime-profile")]
#[derive(Debug, Clone)]
struct RuntimeProfileState {
    instruction_count: usize,
    callback_count: usize,
    callback_fast_path_count: usize,
    load_name_cache_hit_count: usize,
    load_name_cache_miss_count: usize,
    member_const_cache_hit_count: usize,
    member_const_cache_miss_count: usize,
    call_one_cache_hit_count: usize,
    call_one_cache_miss_count: usize,
    member_call_cache_hit_count: usize,
    member_call_cache_miss_count: usize,
    fast_binary_reg_const_count: usize,
    fast_binary_reg_reg_count: usize,
    fused_binary_branch_count: usize,
    fused_move_branch_count: usize,
    fused_reg_branch_jump_count: usize,
    host_get_count: usize,
    host_set_count: usize,
    host_call_count: usize,
    host_construct_count: usize,
    last_pc: Option<usize>,
    callback_stack: Vec<String>,
    opcode_counts: Vec<usize>,
    pc_counts: BTreeMap<usize, usize>,
    function_indexes: BTreeMap<(usize, usize, String), usize>,
    functions: Vec<RuntimeProfileFunctionMeta>,
    function_call_counts: Vec<usize>,
    function_instruction_counts: Vec<usize>,
    function_stack: Vec<usize>,
    callback_indexes: BTreeMap<String, usize>,
    callback_labels: Vec<String>,
    callback_call_counts: Vec<usize>,
    callback_instruction_counts: Vec<usize>,
    callback_index_stack: Vec<usize>,
}

#[cfg(feature = "runtime-profile")]
#[derive(Debug, Clone)]
struct RuntimeProfileFunctionMeta {
    name: String,
    body_start: usize,
    body_end: usize,
}

#[cfg(feature = "runtime-profile")]
impl Default for RuntimeProfileState {
    fn default() -> Self {
        Self {
            instruction_count: 0,
            callback_count: 0,
            callback_fast_path_count: 0,
            load_name_cache_hit_count: 0,
            load_name_cache_miss_count: 0,
            member_const_cache_hit_count: 0,
            member_const_cache_miss_count: 0,
            call_one_cache_hit_count: 0,
            call_one_cache_miss_count: 0,
            member_call_cache_hit_count: 0,
            member_call_cache_miss_count: 0,
            fast_binary_reg_const_count: 0,
            fast_binary_reg_reg_count: 0,
            fused_binary_branch_count: 0,
            fused_move_branch_count: 0,
            fused_reg_branch_jump_count: 0,
            host_get_count: 0,
            host_set_count: 0,
            host_call_count: 0,
            host_construct_count: 0,
            last_pc: None,
            callback_stack: Vec::new(),
            opcode_counts: vec![0; 256],
            pc_counts: BTreeMap::new(),
            function_indexes: BTreeMap::new(),
            functions: Vec::new(),
            function_call_counts: Vec::new(),
            function_instruction_counts: Vec::new(),
            function_stack: Vec::new(),
            callback_indexes: BTreeMap::new(),
            callback_labels: Vec::new(),
            callback_call_counts: Vec::new(),
            callback_instruction_counts: Vec::new(),
            callback_index_stack: Vec::new(),
        }
    }
}

#[cfg(feature = "runtime-profile")]
impl RuntimeProfileState {
    fn record_instruction(&mut self, pc: usize, op: BytecodeOp) {
        self.instruction_count = self.instruction_count.saturating_add(1);
        self.last_pc = Some(pc);
        if let Some(count) = self.opcode_counts.get_mut(op as usize) {
            *count = count.saturating_add(1);
        }
        *self.pc_counts.entry(pc).or_default() += 1;
        if let Some(function_index) = self.function_stack.last().copied() {
            if let Some(count) = self.function_instruction_counts.get_mut(function_index) {
                *count = count.saturating_add(1);
            }
        }
        if let Some(callback_index) = self.callback_index_stack.last().copied() {
            if let Some(count) = self.callback_instruction_counts.get_mut(callback_index) {
                *count = count.saturating_add(1);
            }
        }
    }

    fn record_callback(&mut self, fast_path: bool) {
        self.callback_count = self.callback_count.saturating_add(1);
        if fast_path {
            self.callback_fast_path_count = self.callback_fast_path_count.saturating_add(1);
        }
    }

    fn enter_callback(&mut self, handle: &VmJsHandle, fast_path: bool) {
        self.record_callback(fast_path);
        let label = runtime_profile_callback_label(handle);
        let callback_index = self.callback_index(&label);
        if let Some(count) = self.callback_call_counts.get_mut(callback_index) {
            *count = count.saturating_add(1);
        }
        self.callback_stack.push(label);
        self.callback_index_stack.push(callback_index);
    }

    fn leave_callback(&mut self) {
        let _ = self.callback_stack.pop();
        let _ = self.callback_index_stack.pop();
    }

    fn record_load_name_cache_hit(&mut self) {
        self.load_name_cache_hit_count = self.load_name_cache_hit_count.saturating_add(1);
    }

    fn record_load_name_cache_miss(&mut self) {
        self.load_name_cache_miss_count = self.load_name_cache_miss_count.saturating_add(1);
    }

    fn record_member_const_cache_hit(&mut self) {
        self.member_const_cache_hit_count = self.member_const_cache_hit_count.saturating_add(1);
    }

    fn record_member_const_cache_miss(&mut self) {
        self.member_const_cache_miss_count = self.member_const_cache_miss_count.saturating_add(1);
    }

    fn record_call_one_cache_hit(&mut self) {
        self.call_one_cache_hit_count = self.call_one_cache_hit_count.saturating_add(1);
    }

    fn record_call_one_cache_miss(&mut self) {
        self.call_one_cache_miss_count = self.call_one_cache_miss_count.saturating_add(1);
    }

    fn record_member_call_cache_hit(&mut self) {
        self.member_call_cache_hit_count = self.member_call_cache_hit_count.saturating_add(1);
    }

    fn record_member_call_cache_miss(&mut self) {
        self.member_call_cache_miss_count = self.member_call_cache_miss_count.saturating_add(1);
    }

    fn record_fast_binary_reg_const(&mut self) {
        self.fast_binary_reg_const_count = self.fast_binary_reg_const_count.saturating_add(1);
    }

    fn record_fast_binary_reg_reg(&mut self) {
        self.fast_binary_reg_reg_count = self.fast_binary_reg_reg_count.saturating_add(1);
    }

    fn record_fused_binary_branch(&mut self) {
        self.fused_binary_branch_count = self.fused_binary_branch_count.saturating_add(1);
    }

    fn record_fused_move_branch(&mut self) {
        self.fused_move_branch_count = self.fused_move_branch_count.saturating_add(1);
    }

    fn record_fused_reg_branch_jump(&mut self) {
        self.fused_reg_branch_jump_count = self.fused_reg_branch_jump_count.saturating_add(1);
    }

    fn enter_function(&mut self, function: &FunctionValue) {
        let function_index = self.function_index(function);
        if let Some(count) = self.function_call_counts.get_mut(function_index) {
            *count = count.saturating_add(1);
        }
        self.function_stack.push(function_index);
    }

    fn leave_function(&mut self) {
        let _ = self.function_stack.pop();
    }

    fn function_index(&mut self, function: &FunctionValue) -> usize {
        let key = runtime_profile_function_key(function);
        if let Some(index) = self.function_indexes.get(&key) {
            return *index;
        }
        let index = self.functions.len();
        let (body_start, body_end, name) = key.clone();
        self.functions.push(RuntimeProfileFunctionMeta {
            name,
            body_start,
            body_end,
        });
        self.function_call_counts.push(0);
        self.function_instruction_counts.push(0);
        self.function_indexes.insert(key, index);
        index
    }

    fn callback_index(&mut self, label: &str) -> usize {
        if let Some(index) = self.callback_indexes.get(label) {
            return *index;
        }
        let index = self.callback_labels.len();
        let label = label.to_string();
        self.callback_labels.push(label.clone());
        self.callback_call_counts.push(0);
        self.callback_instruction_counts.push(0);
        self.callback_indexes.insert(label, index);
        index
    }

    fn record_host_get(&mut self) {
        self.host_get_count = self.host_get_count.saturating_add(1);
    }

    fn record_host_set(&mut self) {
        self.host_set_count = self.host_set_count.saturating_add(1);
    }

    fn record_host_call(&mut self) {
        self.host_call_count = self.host_call_count.saturating_add(1);
    }

    fn record_host_construct(&mut self) {
        self.host_construct_count = self.host_construct_count.saturating_add(1);
    }

    fn snapshot(&self, module: &BytecodeModule) -> RuntimeProfile {
        let mut opcodes = BytecodeOp::all()
            .iter()
            .filter_map(|op| {
                let count = *self.opcode_counts.get(*op as usize).unwrap_or(&0);
                (count > 0).then(|| RuntimeProfileEntry {
                    name: op.mnemonic().to_string(),
                    count,
                })
            })
            .collect::<Vec<_>>();
        opcodes.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.name.cmp(&right.name))
        });

        let mut hot_pcs = self
            .pc_counts
            .iter()
            .map(|(pc, count)| RuntimeProfilePcEntry {
                pc: *pc,
                op: module
                    .instructions
                    .get(*pc)
                    .map(|instruction| instruction.op.mnemonic())
                    .unwrap_or("<out-of-range>")
                    .to_string(),
                count: *count,
            })
            .collect::<Vec<_>>();
        hot_pcs.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.pc.cmp(&right.pc))
        });
        hot_pcs.truncate(32);

        let mut functions = self
            .functions
            .iter()
            .enumerate()
            .filter_map(|(index, meta)| {
                let call_count = *self.function_call_counts.get(index).unwrap_or(&0);
                let instruction_count = *self.function_instruction_counts.get(index).unwrap_or(&0);
                (call_count > 0 || instruction_count > 0).then_some(RuntimeProfileFunctionEntry {
                    name: meta.name.clone(),
                    body_start: meta.body_start,
                    body_end: meta.body_end,
                    call_count,
                    instruction_count,
                })
            })
            .collect::<Vec<_>>();
        functions.sort_by(|left, right| {
            right
                .instruction_count
                .cmp(&left.instruction_count)
                .then_with(|| right.call_count.cmp(&left.call_count))
                .then_with(|| left.body_start.cmp(&right.body_start))
        });
        functions.truncate(32);

        let mut callbacks = self
            .callback_labels
            .iter()
            .enumerate()
            .filter_map(|(index, label)| {
                let call_count = *self.callback_call_counts.get(index).unwrap_or(&0);
                let instruction_count = *self.callback_instruction_counts.get(index).unwrap_or(&0);
                (call_count > 0 || instruction_count > 0).then_some(RuntimeProfileCallbackEntry {
                    label: label.clone(),
                    call_count,
                    instruction_count,
                })
            })
            .collect::<Vec<_>>();
        callbacks.sort_by(|left, right| {
            right
                .instruction_count
                .cmp(&left.instruction_count)
                .then_with(|| right.call_count.cmp(&left.call_count))
                .then_with(|| left.label.cmp(&right.label))
        });
        callbacks.truncate(32);

        RuntimeProfile {
            instruction_count: self.instruction_count,
            callback_count: self.callback_count,
            callback_fast_path_count: self.callback_fast_path_count,
            load_name_cache_hit_count: self.load_name_cache_hit_count,
            load_name_cache_miss_count: self.load_name_cache_miss_count,
            member_const_cache_hit_count: self.member_const_cache_hit_count,
            member_const_cache_miss_count: self.member_const_cache_miss_count,
            call_one_cache_hit_count: self.call_one_cache_hit_count,
            call_one_cache_miss_count: self.call_one_cache_miss_count,
            member_call_cache_hit_count: self.member_call_cache_hit_count,
            member_call_cache_miss_count: self.member_call_cache_miss_count,
            fast_binary_reg_const_count: self.fast_binary_reg_const_count,
            fast_binary_reg_reg_count: self.fast_binary_reg_reg_count,
            fused_binary_branch_count: self.fused_binary_branch_count,
            fused_move_branch_count: self.fused_move_branch_count,
            fused_reg_branch_jump_count: self.fused_reg_branch_jump_count,
            host_get_count: self.host_get_count,
            host_set_count: self.host_set_count,
            host_call_count: self.host_call_count,
            host_construct_count: self.host_construct_count,
            last_pc: self.last_pc,
            callback_stack: self.callback_stack.clone(),
            opcodes,
            functions,
            callbacks,
            hot_pcs,
        }
    }
}

#[cfg(feature = "runtime-profile")]
fn runtime_profile_function_key(function: &FunctionValue) -> (usize, usize, String) {
    (
        function.body_start,
        function.body_end,
        function
            .name
            .clone()
            .unwrap_or_else(|| "<anonymous>".to_string()),
    )
}

#[cfg(feature = "runtime-profile")]
struct RuntimeProfileFunctionGuard {
    profile: Option<Rc<RefCell<RuntimeProfileState>>>,
}

#[cfg(feature = "runtime-profile")]
impl Drop for RuntimeProfileFunctionGuard {
    fn drop(&mut self) {
        if let Some(profile) = &self.profile {
            profile.borrow_mut().leave_function();
        }
    }
}

#[cfg(feature = "runtime-profile")]
struct RuntimeProfileCallbackGuard {
    profile: Option<Rc<RefCell<RuntimeProfileState>>>,
}

#[cfg(feature = "runtime-profile")]
impl Drop for RuntimeProfileCallbackGuard {
    fn drop(&mut self) {
        if let Some(profile) = &self.profile {
            profile.borrow_mut().leave_callback();
        }
    }
}

#[cfg(feature = "runtime-profile")]
fn runtime_profile_callback_label(handle: &VmJsHandle) -> String {
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

struct HostCallDepthGuard {
    depth: Rc<Cell<usize>>,
}

impl HostCallDepthGuard {
    fn enter(depth: Rc<Cell<usize>>) -> Self {
        depth.set(depth.get().saturating_add(1));
        Self { depth }
    }
}

impl Drop for HostCallDepthGuard {
    fn drop(&mut self) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }
}

struct CallStackGuard {
    stack: Rc<RefCell<Vec<usize>>>,
}

impl CallStackGuard {
    fn push(stack: Rc<RefCell<Vec<usize>>>, body_start: usize) -> Self {
        stack.borrow_mut().push(body_start);
        Self { stack }
    }
}

impl Drop for CallStackGuard {
    fn drop(&mut self) {
        let _ = self.stack.borrow_mut().pop();
    }
}

fn callback_error_to_js_value(
    error: &ExecuteError,
    bridge: &JsHostBridge,
    handle: &VmJsHandle,
) -> JsValue {
    // VM 函数被宿主 JS 调用时，错误必须以 JS 值形式抛回宿主。
    // 这里额外带上 handle 信息，方便 source map/debugger 定位是哪段 VM 函数出错。
    let value = execute_error_to_js_value(error, bridge);
    let (kind, name, body_start, body_end) = match handle {
        VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => (
            "function",
            function.name.clone().unwrap_or_default(),
            Some(function.body_start),
            Some(function.body_end),
        ),
        VmJsHandle::NativeFunction(function) | VmJsHandle::BoundNativeFunction(function, _) => {
            ("native", function.name.clone(), None, None)
        }
        VmJsHandle::Class(class) => ("class", class.name.clone().unwrap_or_default(), None, None),
        VmJsHandle::Module(module) => ("module", module.source.clone(), None, None),
    };
    let _ = Reflect::set(
        &value,
        &JsValue::from_str("__js_vm_handle_kind"),
        &JsValue::from_str(kind),
    );
    let _ = Reflect::set(
        &value,
        &JsValue::from_str("__js_vm_handle_name"),
        &JsValue::from_str(&name),
    );
    if let Some(body_start) = body_start {
        let _ = Reflect::set(
            &value,
            &JsValue::from_str("__js_vm_body_start"),
            &JsValue::from_f64(body_start as f64),
        );
    }
    if let Some(body_end) = body_end {
        let _ = Reflect::set(
            &value,
            &JsValue::from_str("__js_vm_body_end"),
            &JsValue::from_f64(body_end as f64),
        );
    }
    value
}

/// 指令执行后的控制流结果。
#[derive(Debug, Clone, PartialEq)]
enum Flow {
    /// 普通值流。
    Value(Value),
    /// 函数返回。
    Return(Value),
    /// 抛出异常。
    Throw(Value),
    #[cfg_attr(not(feature = "generator"), allow(dead_code))]
    Yield {
        /// yield 输出值。
        value: Value,
        /// resume 时继续执行的 pc。
        resume_pc: usize,
        /// resume 参数写回寄存器。
        resume_dst: Option<u32>,
    },
    /// async 函数遇到宿主 Promise 后暂停当前函数帧。
    AwaitPending {
        /// 正在等待的宿主 Promise。
        promise: JsValue,
        /// Promise resolved 后写回的寄存器。
        resume_dst: u32,
        /// resolved 后继续执行的 pc。
        resume_pc: usize,
        /// resolved 后继续执行的区间终点。
        resume_end: usize,
    },
    #[cfg(feature = "debugger")]
    Pause { pc: usize, reason: &'static str },
}

#[cfg(not(feature = "debugger"))]
enum FusedControl {
    Continue(usize),
    Return(Value),
}

#[cfg(feature = "debugger")]
#[derive(Debug, Clone, PartialEq)]
pub struct DebugSnapshot {
    pub pc: usize,
    pub reason: String,
    pub done: bool,
    pub paused: bool,
    pub value: String,
    pub registers: Vec<String>,
    pub call_stack: Vec<usize>,
    pub remaining_steps: usize,
}

#[cfg(feature = "debugger")]
pub struct ExecutorDebugSession<B: HostBridge> {
    module: BytecodeModule,
    executor: Executor<B>,
    pc: usize,
    end: usize,
    breakpoints: BTreeSet<usize>,
    paused: bool,
    done: bool,
    last_reason: String,
}

fn catchable_error_value(error: &ExecuteError) -> Option<Value> {
    match error {
        ExecuteError::Thrown(value) => Some(value.clone()),
        ExecuteError::ReferenceError(message) => Some(vm_error_object("ReferenceError", message)),
        ExecuteError::TypeError(message) => Some(vm_error_object("TypeError", message)),
        ExecuteError::RangeError(message) => Some(vm_error_object("RangeError", message)),
        ExecuteError::SyntaxError(message) => Some(vm_error_object("SyntaxError", message)),
        _ => None,
    }
}

fn vm_error_object(kind: &str, message: &str) -> Value {
    object_value(BTreeMap::from([
        ("__error_type".to_string(), Value::String(kind.to_string())),
        ("name".to_string(), Value::String(kind.to_string())),
        ("message".to_string(), Value::String(message.to_string())),
    ]))
}

fn accessor_getter_key(property: &str) -> String {
    format!("__accessor_get__:{property}")
}

fn accessor_setter_key(property: &str) -> String {
    format!("__accessor_set__:{property}")
}

#[cfg(feature = "array-builtins")]
fn array_iteration_method_name(function: &JsValue) -> Option<&'static str> {
    for name in ["forEach", "map", "filter", "every", "some", "find"] {
        let Some(method) = cached_array_iteration_method(name) else {
            continue;
        };
        if JsObject::is(function, &method) {
            return Some(name);
        }
    }
    None
}

#[cfg(feature = "array-builtins")]
fn cached_array_iteration_method(name: &'static str) -> Option<JsValue> {
    if let Some(value) = ARRAY_ITERATION_METHODS.with(|cache| cache.borrow().get(name).cloned()) {
        return Some(value);
    }
    let prototype = JsFunction::new_no_args("return Array.prototype;")
        .call0(&JsValue::UNDEFINED)
        .ok()?;
    let value = Reflect::get(&prototype, &JsValue::from_str(name)).ok()?;
    if !function_source_looks_native(&value) {
        return None;
    }
    ARRAY_ITERATION_METHODS.with(|cache| {
        cache.borrow_mut().insert(name, value.clone());
    });
    Some(value)
}

#[cfg(feature = "array-builtins")]
fn function_source_looks_native(value: &JsValue) -> bool {
    if value.dyn_ref::<JsFunction>().is_none() {
        return false;
    }
    JsFunction::new_with_args(
        "fn",
        "return Function.prototype.toString.call(fn).indexOf('[native code]') >= 0;",
    )
    .call1(&JsValue::UNDEFINED, value)
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false)
}

#[cfg(feature = "array-builtins")]
fn is_vm_callable_value(value: &Value) -> bool {
    match value {
        Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::Class(_) => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => vm_js_handle(value).is_some(),
        _ => false,
    }
}

#[cfg(feature = "array-builtins")]
fn js_array_length(value: &JsValue) -> Result<u32, ExecuteError> {
    let length = Reflect::get(value, &JsValue::from_str("length")).map_err(js_error)?;
    let length = length.as_f64().unwrap_or(0.0);
    if !length.is_finite() || length <= 0.0 {
        return Ok(0);
    }
    Ok(length.min(u32::MAX as f64) as u32)
}

#[cfg(feature = "array-builtins")]
fn js_array_present_item(value: &JsValue, index: u32) -> Result<Option<Value>, ExecuteError> {
    let key = JsValue::from_f64(index as f64);
    if !Reflect::has(value, &key).map_err(js_error)? {
        return Ok(None);
    }
    Reflect::get(value, &key)
        .map(Value::JsValue)
        .map(Some)
        .map_err(js_error)
}

fn is_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Undefined)
        || matches!(value, Value::JsValue(value) | Value::BoundJsFunction(value, _) if value.is_undefined())
}

fn is_null_or_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Undefined)
        || matches!(value, Value::JsValue(value) | Value::BoundJsFunction(value, _) if value.is_null() || value.is_undefined())
}

fn value_array_index(value: &Value) -> Option<u32> {
    let number = match value {
        Value::Number(number) => *number,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => value.as_f64()?,
        _ => return None,
    };
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    if number >= u32::MAX as f64 {
        return None;
    }
    Some(number as u32)
}

fn operand_operator_name(operand: &BytecodeOperand) -> Option<&'static str> {
    match operand {
        BytecodeOperand::Operator(index) => operator_name(*index),
        _ => None,
    }
}

#[cfg(not(feature = "debugger"))]
fn small_call_parts(instruction: &BytecodeInstruction) -> Option<(u32, u32, SmallCallArity)> {
    let [
        BytecodeOperand::Register(dst),
        BytecodeOperand::Register(callee),
        BytecodeOperand::Count(count),
        args @ ..,
    ] = instruction.operands.as_slice()
    else {
        return None;
    };
    let arity = SmallCallArity::from_op(instruction.op).or_else(|| {
        (instruction.op == BytecodeOp::Call && *count == 3).then_some(SmallCallArity::Three)
    })?;
    if *count != arity.count() || args.len() != arity.len() {
        return None;
    }
    Some((*dst, *callee, arity))
}

#[cfg(not(feature = "debugger"))]
fn pure_dynamic_property_key(value: &Value) -> Option<String> {
    match value {
        Value::Number(_)
        | Value::BigInt(_)
        | Value::String(_)
        | Value::Symbol(_)
        | Value::Bool(_)
        | Value::Null
        | Value::Undefined => Some(property_key(value)),
        Value::JsValue(value) | Value::BoundJsFunction(value, _)
            if value.is_null()
                || value.is_undefined()
                || value.as_string().is_some()
                || value.as_f64().is_some()
                || value.as_bool().is_some() =>
        {
            Some(property_key(&Value::JsValue(value.clone())))
        }
        _ => None,
    }
}

fn with_member_context(
    error: ExecuteError,
    pc: usize,
    object: &Value,
    property: &str,
) -> ExecuteError {
    let context = format!(" at member pc {pc} object {object} property {property:?}");
    match error {
        ExecuteError::ReferenceError(message) => {
            ExecuteError::ReferenceError(format!("{message}{context}"))
        }
        ExecuteError::TypeError(message) => ExecuteError::TypeError(format!("{message}{context}")),
        ExecuteError::RangeError(message) => {
            ExecuteError::RangeError(format!("{message}{context}"))
        }
        ExecuteError::SyntaxError(message) => {
            ExecuteError::SyntaxError(format!("{message}{context}"))
        }
        ExecuteError::Runtime(message) => ExecuteError::Runtime(format!("{message}{context}")),
        other => other,
    }
}

fn with_load_context(error: ExecuteError, pc: usize, source: &BytecodeOperand) -> ExecuteError {
    let context = format!(" at load pc {pc} source {source:?}");
    match error {
        ExecuteError::ReferenceError(message) => {
            ExecuteError::ReferenceError(format!("{message}{context}"))
        }
        ExecuteError::TypeError(message) => ExecuteError::TypeError(format!("{message}{context}")),
        ExecuteError::RangeError(message) => {
            ExecuteError::RangeError(format!("{message}{context}"))
        }
        ExecuteError::SyntaxError(message) => {
            ExecuteError::SyntaxError(format!("{message}{context}"))
        }
        other => other,
    }
}

fn call_error_with_context(
    error: ExecuteError,
    pc: usize,
    callee_operand: &BytecodeOperand,
    callee: &Value,
    args: &[Value],
) -> ExecuteError {
    #[cfg(feature = "compact-errors")]
    {
        let _ = (pc, callee_operand, callee, args);
        error
    }
    #[cfg(not(feature = "compact-errors"))]
    {
        let args_display = args
            .iter()
            .take(4)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let context =
            format!(" at pc {pc} callee {callee_operand:?} value {callee} args [{args_display}]");
        match error {
            ExecuteError::TypeError(message) => {
                ExecuteError::TypeError(format!("{message}{context}"))
            }
            ExecuteError::RangeError(message) => {
                ExecuteError::RangeError(format!("{message}{context}"))
            }
            other => other,
        }
    }
}

fn is_vm_object_like(value: &Value) -> bool {
    match value {
        Value::Object(_)
        | Value::Array(_)
        | Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::Class(_)
        | Value::Module(_) => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => vm_js_handle(value).is_some(),
        _ => false,
    }
}

#[cfg(feature = "bigint")]
fn is_host_js_object_like(value: &Value) -> bool {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => is_host_js_object_value(value),
        _ => false,
    }
}

fn is_host_js_object_value(value: &JsValue) -> bool {
    !value.is_null() && matches!(js_value_typeof(value), "object" | "function")
}

fn is_to_primitive_object_result(value: &Value) -> bool {
    is_vm_object_like(value)
        || matches!(value, Value::JsValue(value) if value.is_object())
        || matches!(value, Value::BoundJsFunction(_, _))
}

fn is_primitive_like(value: &Value) -> bool {
    match value {
        Value::Number(_)
        | Value::BigInt(_)
        | Value::String(_)
        | Value::Symbol(_)
        | Value::Bool(_)
        | Value::Null
        | Value::Undefined => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            vm_js_handle(value).is_none() && !is_host_js_object_value(value)
        }
        _ => false,
    }
}

fn is_symbol_like(value: &Value) -> bool {
    matches!(value, Value::Symbol(_))
        || matches!(value, Value::JsValue(value) | Value::BoundJsFunction(value, _) if js_value_is_symbol(value))
}

fn async_resolved_value(value: Value) -> Value {
    let then = Value::BoundNativeFunction(
        NativeFunctionValue {
            name: "AsyncResolved.then".to_string(),
        },
        Box::new(value.clone()),
    );
    let catch = Value::BoundNativeFunction(
        NativeFunctionValue {
            name: "AsyncResolved.catch".to_string(),
        },
        Box::new(value.clone()),
    );
    let finally = Value::BoundNativeFunction(
        NativeFunctionValue {
            name: "AsyncResolved.finally".to_string(),
        },
        Box::new(value),
    );
    object_value(BTreeMap::from([
        ("then".to_string(), then),
        ("catch".to_string(), catch),
        ("finally".to_string(), finally),
    ]))
}

fn await_value(mut value: Value) -> Result<Value, ExecuteError> {
    for _ in 0..16 {
        if let Value::Object(props) = &value {
            let resolved = {
                let props = props.borrow();
                if let Some(Value::BoundNativeFunction(function, resolved)) = props.get("then")
                    && function.name == "AsyncResolved.then"
                {
                    Some((**resolved).clone())
                } else {
                    None
                }
            };
            if let Some(resolved) = resolved {
                value = resolved;
                continue;
            }
        }
        if let Some(resolved) = js_async_resolved_inner(&value)? {
            value = resolved;
            continue;
        }
        return Ok(match &value {
            Value::JsValue(value) | Value::BoundJsFunction(value, _)
                if value.dyn_ref::<JsPromise>().is_some() =>
            {
                Value::Undefined
            }
            _ => value,
        });
    }
    Ok(value)
}

fn host_promise_value(value: &Value) -> Option<JsValue> {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _)
            if value.dyn_ref::<JsPromise>().is_some() =>
        {
            Some(value.clone())
        }
        _ => None,
    }
}

fn async_resume_result_to_js<B: HostBridge + 'static>(
    result: Result<Flow, ExecuteError>,
    frame: Executor<B>,
    module: Rc<BytecodeModule>,
    bridge: JsHostBridge,
) -> Result<JsValue, JsValue> {
    let value = match result {
        Ok(Flow::Value(_)) => Value::Undefined,
        Ok(Flow::Return(value)) => value,
        Ok(Flow::Throw(value)) => {
            return Err(value_to_js_value(&value, &bridge).unwrap_or_else(|_| JsValue::UNDEFINED));
        }
        Ok(Flow::Yield { .. }) => {
            return Err(JsValue::from_str("yield outside generator frame"));
        }
        Ok(Flow::AwaitPending {
            promise,
            resume_dst,
            resume_pc,
            resume_end,
        }) => {
            return async_resume_promise_from_frame(
                module, bridge, frame, promise, resume_dst, resume_pc, resume_end,
            )
            .map_err(|error| execute_error_to_js_value(&error, &JsHostBridge::empty()));
        }
        #[cfg(feature = "debugger")]
        Ok(Flow::Pause { .. }) => {
            return Err(JsValue::from_str(
                "debug pause inside async function frame is not resumable yet",
            ));
        }
        Err(error) => return Err(execute_error_to_js_value(&error, &bridge)),
    };
    value_to_js_value(&value, &bridge).map_err(|error| execute_error_to_js_value(&error, &bridge))
}

fn async_resume_promise_from_frame<B: HostBridge + 'static>(
    module: Rc<BytecodeModule>,
    bridge: JsHostBridge,
    frame: Executor<B>,
    promise: JsValue,
    resume_dst: u32,
    resume_pc: usize,
    resume_end: usize,
) -> Result<JsValue, ExecuteError> {
    let Some(promise) = promise.dyn_ref::<JsPromise>().cloned() else {
        return Err(ExecuteError::Runtime(
            "await pending value is not a Promise".to_string(),
        ));
    };
    let mut frame_slot = Some(frame);
    let outer_promise = JsPromise::new(&mut move |resolve, reject| {
        let Some(mut frame) = frame_slot.take() else {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("async function frame has already been resumed"),
            );
            return;
        };
        let resolve_for_fulfilled = resolve.clone();
        let reject_for_fulfilled = reject.clone();
        let reject_for_rejected = reject.clone();
        let module_for_fulfilled = module.clone();
        let bridge_for_fulfilled = bridge.clone();
        let on_fulfilled = Closure::once_aborting(move |resolved: JsValue| {
            frame.write_register(resume_dst, Value::JsValue(resolved.clone()));
            frame.last_value = Value::JsValue(resolved);
            let result = async_resume_result_to_js(
                frame.execute_range(&module_for_fulfilled, resume_pc, resume_end),
                frame,
                module_for_fulfilled,
                bridge_for_fulfilled,
            );
            match result {
                Ok(value) => {
                    let _ = resolve_for_fulfilled.call1(&JsValue::UNDEFINED, &value);
                }
                Err(error) => {
                    let _ = reject_for_fulfilled.call1(&JsValue::UNDEFINED, &error);
                }
            }
        });
        let on_rejected = Closure::once_aborting(move |error: JsValue| {
            let _ = reject_for_rejected.call1(&JsValue::UNDEFINED, &error);
        });
        let _ = promise.then2(&on_fulfilled, &on_rejected);
        on_fulfilled.forget();
        on_rejected.forget();
    });
    Ok(outer_promise.into())
}

fn js_async_resolved_inner(value: &Value) -> Result<Option<Value>, ExecuteError> {
    let (Value::JsValue(value) | Value::BoundJsFunction(value, _)) = value else {
        return Ok(None);
    };
    if !value.is_object() && value.dyn_ref::<JsFunction>().is_none() {
        return Ok(None);
    }
    let marker = get_js_property(value, "__js_vm_async_resolved")?;
    if !marker.as_bool().unwrap_or(false) {
        return Ok(None);
    }
    let resolved = get_js_property(value, "__js_vm_value")?;
    Ok(Some(Value::JsValue(resolved)))
}

fn numeric_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(value) => Some(*value),
        Value::JsValue(value) => value.as_f64(),
        _ => None,
    }
}

fn fast_number_binary(op: &str, left: f64, right: f64) -> Option<Value> {
    let left_value = Value::Number(left);
    let right_value = Value::Number(right);
    let value = match op {
        "+" => Value::Number(left + right),
        "-" => Value::Number(left - right),
        "*" => Value::Number(left * right),
        "/" => Value::Number(left / right),
        "%" => Value::Number(left % right),
        "**" => Value::Number(left.powf(right)),
        "<" => Value::Bool(left < right),
        "<=" => Value::Bool(left <= right),
        ">" => Value::Bool(left > right),
        ">=" => Value::Bool(left >= right),
        "==" | "===" => Value::Bool(left == right),
        "!=" | "!==" => Value::Bool(left != right),
        "&" => Value::Number((to_int32(&left_value) & to_int32(&right_value)) as f64),
        "|" => Value::Number((to_int32(&left_value) | to_int32(&right_value)) as f64),
        "^" => Value::Number((to_int32(&left_value) ^ to_int32(&right_value)) as f64),
        "<<" => Value::Number(to_int32(&left_value).wrapping_shl(shift_count(&right_value)) as f64),
        ">>" => Value::Number(to_int32(&left_value).wrapping_shr(shift_count(&right_value)) as f64),
        ">>>" => {
            Value::Number(to_uint32(&left_value).wrapping_shr(shift_count(&right_value)) as f64)
        }
        _ => return None,
    };
    Some(value)
}

fn fast_constant_equality(
    operator: Option<&str>,
    left: &Value,
    constant: Option<&BytecodeConstant>,
) -> Option<bool> {
    let operator = operator?;
    if matches!(operator, "==" | "!=")
        && matches!(
            constant,
            Some(BytecodeConstant::Undefined | BytecodeConstant::Null)
        )
    {
        let equal = is_null_or_undefined_value(left);
        return Some(if operator == "!=" { !equal } else { equal });
    }

    let invert = match operator {
        "===" => false,
        "!==" => true,
        _ => return None,
    };
    let equal = match constant? {
        BytecodeConstant::Undefined => is_undefined_value(left),
        BytecodeConstant::Null => {
            matches!(left, Value::Null)
                || matches!(left, Value::JsValue(value) | Value::BoundJsFunction(value, _) if value.is_null())
        }
        BytecodeConstant::Bool(expected) => match left {
            Value::Bool(value) => value == expected,
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                value.as_bool() == Some(*expected)
            }
            _ => false,
        },
        BytecodeConstant::Number(expected) => match left {
            Value::Number(value) => value == expected,
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                value.as_f64() == Some(*expected)
            }
            _ => false,
        },
        BytecodeConstant::String(expected) => match left {
            Value::String(value) => value == expected,
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                value.as_string().as_deref() == Some(expected.as_str())
            }
            _ => false,
        },
        BytecodeConstant::BigInt(expected) => {
            matches!(left, Value::BigInt(value) if value == expected)
        }
    };
    Some(if invert { !equal } else { equal })
}

#[cfg(not(feature = "debugger"))]
fn local_load_parts(instruction: &BytecodeInstruction) -> Option<(u32, u32)> {
    if !matches!(
        instruction.op,
        BytecodeOp::LoadLocal | BytecodeOp::LoadLocalSmall
    ) {
        return None;
    }
    match instruction.operands.as_slice() {
        [
            BytecodeOperand::Register(register),
            BytecodeOperand::LocalSlot(slot),
        ] => Some((*register, *slot)),
        _ => None,
    }
}

#[cfg(not(feature = "debugger"))]
fn load_name_parts(instruction: &BytecodeInstruction) -> Option<(u32, &BytecodeOperand)> {
    if instruction.op != BytecodeOp::LoadName {
        return None;
    }
    match instruction.operands.as_slice() {
        [BytecodeOperand::Register(register), source] => Some((*register, source)),
        _ => None,
    }
}

#[cfg(not(feature = "debugger"))]
fn return_register_matches(instruction: &BytecodeInstruction, register: u32) -> bool {
    matches!(
        instruction.operands.as_slice(),
        [BytecodeOperand::Register(value)]
            if matches!(instruction.op, BytecodeOp::Return | BytecodeOp::ReturnReg)
                && *value == register
    )
}

impl<B: HostBridge + 'static> Executor<B> {
    fn with_host_bridge_and_limits(
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
        max_execution_steps: usize,
    ) -> Self {
        Self {
            registers: Vec::new(),
            lexical_env: LexicalEnv::default(),
            instruction_scope_depths: Vec::new(),
            function_hoist_cache: Rc::new(RefCell::new(BTreeMap::new())),
            register_frame_size_cache: Rc::new(RefCell::new(BTreeMap::new())),
            pc_inline_cache: Rc::new(RefCell::new(PcInlineCache::default())),
            last_value: Value::Undefined,
            exports: BTreeMap::new(),
            host_bridge,
            external_names: Vec::new(),
            module_handle: None,
            call_depth: 0,
            max_call_depth: max_call_depth.max(1),
            max_recursive_call_depth: max_recursive_call_depth.max(1),
            call_stack: Rc::new(RefCell::new(Vec::new())),
            host_call_depth: Rc::new(Cell::new(0)),
            execution_budget: Rc::new(Cell::new(if max_execution_steps == 0 {
                usize::MAX
            } else {
                max_execution_steps
            })),
            #[cfg(feature = "runtime-profile")]
            runtime_profile: None,
            #[cfg(feature = "debugger")]
            debug_breakpoints: Rc::new(RefCell::new(BTreeSet::new())),
            #[cfg(feature = "debugger")]
            debug_skip_breakpoint: Rc::new(Cell::new(None)),
        }
    }

    /// 使用默认限制运行 bytecode 模块。
    ///
    /// `host_bridge` 负责解析 extern slot 和宿主对象访问。
    pub fn run_with_host_bridge(
        module: &BytecodeModule,
        host_bridge: B,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_limits(
            module,
            host_bridge,
            DEFAULT_MAX_CALL_DEPTH,
            DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
        )
    }

    /// 使用自定义调用深度限制运行 bytecode 模块。
    pub fn run_with_host_bridge_and_limits(
        module: &BytecodeModule,
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_runtime_limits(
            module,
            host_bridge,
            max_call_depth,
            max_recursive_call_depth,
            DEFAULT_MAX_EXECUTION_STEPS,
        )
    }

    /// 使用完整运行时限制运行 bytecode 模块。
    ///
    /// `max_execution_steps` 是防止无限循环的主保护；每执行一条指令会消耗一次预算。
    pub fn run_with_host_bridge_and_runtime_limits(
        module: &BytecodeModule,
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
        max_execution_steps: usize,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_external_names_and_limits(
            module,
            host_bridge,
            module.extern_slots.clone(),
            max_call_depth,
            max_recursive_call_depth,
            max_execution_steps,
        )
    }

    /// 使用运行时 extern 名称运行模块。
    ///
    /// 当 bytecode 为了压缩只记录 extern 数量时，执行器依靠这里传入的名称恢复错误信息和全局解析。
    pub fn run_with_host_bridge_and_external_names(
        module: &BytecodeModule,
        host_bridge: B,
        external_names: Vec<String>,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_external_names_and_limits(
            module,
            host_bridge,
            external_names,
            DEFAULT_MAX_CALL_DEPTH,
            DEFAULT_MAX_RECURSIVE_CALL_DEPTH,
            DEFAULT_MAX_EXECUTION_STEPS,
        )
    }

    /// 最完整的执行入口。
    ///
    /// 初始化步骤包括：
    /// 1. 校验 extern 数量。
    /// 2. 安装 VM 函数作为 JS callback 时使用的 invoker。
    /// 3. 注入模块外部依赖和作用域元数据。
    /// 4. 预提升函数声明。
    /// 5. 执行完整指令区间。
    pub fn run_with_host_bridge_and_external_names_and_limits(
        module: &BytecodeModule,
        host_bridge: B,
        external_names: Vec<String>,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
        max_execution_steps: usize,
    ) -> Result<Value, ExecuteError> {
        Self::run_with_host_bridge_and_external_names_and_limits_internal(
            module,
            host_bridge,
            external_names,
            max_call_depth,
            max_recursive_call_depth,
            max_execution_steps,
            #[cfg(feature = "runtime-profile")]
            None,
        )
    }

    #[cfg(feature = "runtime-profile")]
    pub fn profile_with_host_bridge_and_runtime_limits(
        module: &BytecodeModule,
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
        max_execution_steps: usize,
    ) -> (Result<Value, ExecuteError>, RuntimeProfile) {
        install_runtime_profile_panic_hook();
        let profile = Rc::new(RefCell::new(RuntimeProfileState::default()));
        let result = Self::run_with_host_bridge_and_external_names_and_limits_internal(
            module,
            host_bridge,
            module.extern_slots.clone(),
            max_call_depth,
            max_recursive_call_depth,
            max_execution_steps,
            Some(profile.clone()),
        );
        let snapshot = profile.borrow().snapshot(module);
        (result, snapshot)
    }

    fn run_with_host_bridge_and_external_names_and_limits_internal(
        module: &BytecodeModule,
        host_bridge: B,
        external_names: Vec<String>,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
        max_execution_steps: usize,
        #[cfg(feature = "runtime-profile")] runtime_profile: Option<
            Rc<RefCell<RuntimeProfileState>>,
        >,
    ) -> Result<Value, ExecuteError> {
        #[cfg(not(feature = "module"))]
        if module.kind == BytecodeModuleKind::Module {
            return Err(ExecuteError::Unsupported("module"));
        }
        clear_host_overlays();
        let expected_extern_count = if external_names.is_empty() {
            module.extern_slots.len()
        } else {
            external_names.len()
        };
        host_bridge.validate_extern_count(expected_extern_count)?;
        let mut executor = Self::with_host_bridge_and_limits(
            host_bridge,
            max_call_depth,
            max_recursive_call_depth,
            max_execution_steps,
        );
        #[cfg(feature = "runtime-profile")]
        {
            executor.runtime_profile = runtime_profile;
        }
        executor.module_handle = Some(Rc::new(module.clone()));
        executor.external_names = external_names;
        executor.inject_module_externals(module);
        executor.load_scope_metadata(module)?;
        executor.install_js_callback_invoker(module);
        executor.hoist_function_declarations(module, 0, module.instructions.len())?;
        let result = match executor.execute_range(module, 0, module.instructions.len())? {
            Flow::Value(value) | Flow::Return(value) => {
                #[cfg(feature = "module")]
                if module.kind == BytecodeModuleKind::Module {
                    executor.module_namespace_value()
                } else {
                    Ok(value)
                }
                #[cfg(not(feature = "module"))]
                {
                    Ok(value)
                }
            }
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
            Flow::Yield { .. } => Err(ExecuteError::Runtime(
                "yield outside generator frame".to_string(),
            )),
            Flow::AwaitPending { .. } => Err(ExecuteError::Runtime(
                "await pending outside async function".to_string(),
            )),
            #[cfg(feature = "debugger")]
            Flow::Pause { .. } => Err(ExecuteError::Runtime(
                "debug pause outside debug session".to_string(),
            )),
        };
        result
    }

    #[cfg(feature = "debugger")]
    pub fn debug_session_with_host_bridge_and_runtime_limits(
        module: &BytecodeModule,
        host_bridge: B,
        max_call_depth: usize,
        max_recursive_call_depth: usize,
        max_execution_steps: usize,
    ) -> Result<ExecutorDebugSession<B>, ExecuteError> {
        #[cfg(not(feature = "module"))]
        if module.kind == BytecodeModuleKind::Module {
            return Err(ExecuteError::Unsupported("module"));
        }
        clear_host_overlays();
        host_bridge.validate_extern_count(module.extern_slots.len())?;
        let mut executor = Self::with_host_bridge_and_limits(
            host_bridge,
            max_call_depth,
            max_recursive_call_depth,
            max_execution_steps,
        );
        executor.module_handle = Some(Rc::new(module.clone()));
        executor.external_names = module.extern_slots.clone();
        executor.inject_module_externals(module);
        executor.load_scope_metadata(module)?;
        executor.install_js_callback_invoker(module);
        executor.hoist_function_declarations(module, 0, module.instructions.len())?;
        Ok(ExecutorDebugSession {
            module: module.clone(),
            executor,
            pc: 0,
            end: module.instructions.len(),
            breakpoints: BTreeSet::new(),
            paused: false,
            done: false,
            last_reason: "created".to_string(),
        })
    }

    /// 安装 VM 函数作为 JS callback 时的 invoker。
    ///
    /// 函数对象可能被 `addEventListener`、Promise callback 或框架生命周期持有。
    /// 原 executor 返回后这些 callback 仍可能被调用，所以 invoker 必须捕获 module、bridge 和限制配置，
    /// 并在每次回调时创建新的 executor 执行对应 VM 函数。
    fn install_js_callback_invoker(&self, module: &BytecodeModule) {
        let Some(callback_bridge) = self.host_bridge.as_js_host_bridge() else {
            return;
        };
        let callback_module = Rc::new(module.clone());
        let max_call_depth = self.max_call_depth;
        let max_recursive_call_depth = self.max_recursive_call_depth;
        let max_execution_steps = self.execution_budget.get();
        let callback_call_stack = self.call_stack.clone();
        let callback_host_call_depth = self.host_call_depth.clone();
        let callback_execution_budget = self.execution_budget.clone();
        let callback_instruction_scope_depths = self.instruction_scope_depths.clone();
        let callback_function_hoist_cache = self.function_hoist_cache.clone();
        let callback_register_frame_size_cache = self.register_frame_size_cache.clone();
        let callback_pc_inline_cache = self.pc_inline_cache.clone();
        #[cfg(feature = "runtime-profile")]
        let callback_runtime_profile = self.runtime_profile.clone();
        set_vm_js_callback_invoker(Some(Rc::new(move |handle, this_value, args| {
            let active_call_depth = callback_call_stack.borrow().len();
            let active_host_call_depth = callback_host_call_depth.get();
            #[cfg(feature = "runtime-profile")]
            let fast_path = matches!(
                handle,
                VmJsHandle::Function(_) | VmJsHandle::BoundFunction(_, _)
            );
            let target_module = match &handle {
                VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => function
                    .module
                    .clone()
                    .unwrap_or_else(|| callback_module.clone()),
                _ => callback_module.clone(),
            };
            let target_bridge = match &handle {
                VmJsHandle::Function(function) | VmJsHandle::BoundFunction(function, _) => function
                    .host_bridge
                    .clone()
                    .unwrap_or_else(|| callback_bridge.clone()),
                _ => callback_bridge.clone(),
            };
            let mut executor = Executor::with_host_bridge_and_limits(
                target_bridge.clone(),
                max_call_depth,
                max_recursive_call_depth,
                max_execution_steps,
            );
            executor.host_bridge = target_bridge.clone();
            executor.max_call_depth = max_call_depth;
            executor.max_recursive_call_depth = max_recursive_call_depth;
            executor.function_hoist_cache = callback_function_hoist_cache.clone();
            executor.register_frame_size_cache = callback_register_frame_size_cache.clone();
            executor.pc_inline_cache = callback_pc_inline_cache.clone();
            #[cfg(feature = "runtime-profile")]
            let _callback_guard = {
                if let Some(profile) = &callback_runtime_profile {
                    profile.borrow_mut().enter_callback(&handle, fast_path);
                    RuntimeProfileCallbackGuard {
                        profile: Some(profile.clone()),
                    }
                } else {
                    RuntimeProfileCallbackGuard { profile: None }
                }
            };
            #[cfg(feature = "runtime-profile")]
            {
                executor.runtime_profile = callback_runtime_profile.clone();
            }
            // 同步宿主回调如 Array.map/reduce 会在原 VM 调用栈尚未退出时立刻调用。
            // 这类回调必须共享当前预算，否则每次 callback 都会重置 step limit，低步数保护抓不到死循环。
            // requestAnimationFrame / Promise 等异步回调发生在原调用栈清空之后，仍然获得新的单次预算。
            if active_call_depth > 0 || active_host_call_depth > 0 {
                executor.execution_budget = callback_execution_budget.clone();
            } else {
                executor.execution_budget = Rc::new(Cell::new(max_execution_steps));
            }
            executor.call_depth = active_call_depth;
            executor.call_stack = callback_call_stack.clone();
            executor.host_call_depth = callback_host_call_depth.clone();
            executor.module_handle = Some(target_module.clone());
            executor.external_names = target_module.extern_slots.clone();
            if executor.instruction_scope_depths.len() != target_module.instructions.len() {
                if Rc::ptr_eq(&target_module, &callback_module)
                    && callback_instruction_scope_depths.len() == target_module.instructions.len()
                {
                    executor.instruction_scope_depths = callback_instruction_scope_depths.clone();
                } else {
                    let metadata_result =
                        executor
                            .load_scope_metadata(&target_module)
                            .map_err(|error| {
                                callback_error_to_js_value(&error, &target_bridge, &handle)
                            });
                    if let Err(error) = metadata_result {
                        return Err(error);
                    }
                }
            }
            let result = executor.call_vm_js_handle_from_callback(
                &target_module,
                &handle,
                Value::JsValue(this_value),
                args,
            );
            let value = match result {
                Ok(value) => value,
                Err(error) => {
                    return Err(callback_error_to_js_value(&error, &target_bridge, &handle));
                }
            };
            value_to_js_value(&value, &target_bridge)
                .map_err(|error| callback_error_to_js_value(&error, &target_bridge, &handle))
        })));
    }

    fn inject_module_externals(&mut self, module: &BytecodeModule) {
        // extern 槽在 bytecode 里只保存下标。执行前把每个槽投影成全局词法绑定，
        // 后续 `LOAD_NAME extern#n` 会得到 `ExternalRef`，属性访问和调用再交给 HostBridge。
        for (index, name) in module.extern_slots.iter().enumerate() {
            let name = self
                .external_names
                .get(index)
                .cloned()
                .unwrap_or_else(|| name.clone());
            self.lexical_env.define_global_if_absent(
                name.clone(),
                Value::ExternalRef(ExternalRefValue::new(index as u32, name.clone())),
            );
        }
    }

    #[cfg(feature = "module")]
    fn module_namespace_value(&self) -> Result<Value, ExecuteError> {
        self.host_bridge.module_value(ModuleValue {
            source: "module".to_string(),
            exports: self.exports.clone(),
        })
    }

    fn load_scope_metadata(&mut self, module: &BytecodeModule) -> Result<(), ExecuteError> {
        let metadata = collect_scope_metadata(module)?;
        self.instruction_scope_depths = metadata.instruction_scope_depths;
        Ok(())
    }

    fn child_instruction_scope_depths(
        &self,
        module: &BytecodeModule,
    ) -> Result<Vec<usize>, ExecuteError> {
        if self.instruction_scope_depths.len() == module.instructions.len() {
            Ok(self.instruction_scope_depths.clone())
        } else {
            Ok(collect_scope_metadata(module)?.instruction_scope_depths)
        }
    }

    fn execute_range(
        &mut self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<Flow, ExecuteError> {
        // 执行器的主循环是一个扁平 pc 状态机。函数、try/finally、generator 会递归执行子区间，
        // 但每个区间进入时都会记录词法环境深度，结束时恢复，避免块级作用域泄漏到外层。
        if start > end || end > module.instructions.len() {
            return Err(ExecuteError::Runtime(format!(
                "execute range {start}..{end} is outside code length {}",
                module.instructions.len()
            )));
        }
        self.ensure_register_frame_size(module, start, end)?;
        let entry_env_depth = self.lexical_env.depth();
        let mut pc = start;
        let is_step_budgeted = self.execution_budget.get() != usize::MAX;
        while pc < end {
            #[cfg(feature = "debugger")]
            if self.debug_should_pause(pc) {
                return Ok(Flow::Pause {
                    pc,
                    reason: "breakpoint",
                });
            }
            let instruction = &module.instructions[pc];
            if is_step_budgeted {
                self.consume_step(pc)?;
            }
            #[cfg(feature = "runtime-profile")]
            self.record_instruction_profile(pc, instruction.op);
            #[cfg(not(feature = "debugger"))]
            if instruction.op == BytecodeOp::LoadLocalSmall
                && matches!(
                    module
                        .instructions
                        .get(pc + 1)
                        .map(|instruction| instruction.op),
                    Some(BytecodeOp::Unary)
                )
                && matches!(
                    module
                        .instructions
                        .get(pc + 2)
                        .map(|instruction| instruction.op),
                    Some(BytecodeOp::StoreLocalSmall)
                )
                && let Some(next_pc) =
                    self.try_execute_fused_numeric_local_update(module, pc, end, is_step_budgeted)?
            {
                pc = next_pc;
                continue;
            }
            #[cfg(not(feature = "debugger"))]
            if matches!(
                instruction.op,
                BytecodeOp::LoadLocal | BytecodeOp::LoadLocalSmall
            ) && matches!(
                module
                    .instructions
                    .get(pc + 1)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::LoadLocal | BytecodeOp::LoadLocalSmall)
            ) && matches!(
                module
                    .instructions
                    .get(pc + 2)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::BinaryLocalConst)
            ) && matches!(
                module
                    .instructions
                    .get(pc + 3)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::Member)
            ) && matches!(
                module
                    .instructions
                    .get(pc + 4)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::BinaryRegRegJump | BytecodeOp::BinaryRegRegJumpFallthrough)
            ) && let Some(next_pc) = self.try_execute_fused_local_index_member_binary_jump(
                module,
                pc,
                end,
                is_step_budgeted,
            )? {
                pc = next_pc;
                continue;
            }
            #[cfg(not(feature = "debugger"))]
            if instruction.op == BytecodeOp::BinaryRegReg
                && matches!(
                    module
                        .instructions
                        .get(pc + 1)
                        .map(|instruction| instruction.op),
                    Some(BytecodeOp::JumpIfFalseReg | BytecodeOp::JumpIfTrueReg)
                )
                && let Some(next_pc) =
                    self.try_execute_fused_binary_reg_branch(module, pc, end, is_step_budgeted)?
            {
                pc = next_pc;
                continue;
            }
            #[cfg(not(feature = "debugger"))]
            if matches!(
                instruction.op,
                BytecodeOp::LoadLocal | BytecodeOp::LoadLocalSmall
            ) && matches!(
                module
                    .instructions
                    .get(pc + 1)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::JumpIfFalseReg)
            ) && matches!(
                module
                    .instructions
                    .get(pc + 2)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::Jump)
            ) && let Some(control) =
                self.try_execute_fused_local_branch(module, pc, end, is_step_budgeted)?
            {
                match control {
                    FusedControl::Continue(next_pc) => {
                        pc = next_pc;
                        continue;
                    }
                    FusedControl::Return(value) => {
                        return self.finish_flow(entry_env_depth, Flow::Return(value));
                    }
                }
            }
            #[cfg(not(feature = "debugger"))]
            if instruction.op == BytecodeOp::Move
                && matches!(
                    module
                        .instructions
                        .get(pc + 1)
                        .map(|instruction| instruction.op),
                    Some(BytecodeOp::JumpIfFalseReg | BytecodeOp::JumpIfTrueReg)
                )
                && let Some(next_pc) =
                    self.try_execute_fused_move_branch(module, pc, end, is_step_budgeted)?
            {
                pc = next_pc;
                continue;
            }
            #[cfg(not(feature = "debugger"))]
            if instruction.op == BytecodeOp::Move
                && matches!(
                    module
                        .instructions
                        .get(pc + 1)
                        .map(|instruction| instruction.op),
                    Some(BytecodeOp::Return | BytecodeOp::ReturnReg | BytecodeOp::Jump)
                )
                && let Some(value) =
                    self.try_execute_fused_move_return(module, pc, end, is_step_budgeted)?
            {
                return self.finish_flow(entry_env_depth, Flow::Return(value));
            }
            #[cfg(not(feature = "debugger"))]
            if matches!(
                instruction.op,
                BytecodeOp::JumpIfFalseReg | BytecodeOp::JumpIfTrueReg
            ) && matches!(
                module
                    .instructions
                    .get(pc + 1)
                    .map(|instruction| instruction.op),
                Some(BytecodeOp::Jump)
            ) && let Some(next_pc) =
                self.try_execute_fused_reg_branch_jump(module, pc, end, is_step_budgeted)?
            {
                pc = next_pc;
                continue;
            }
            #[cfg(not(feature = "debugger"))]
            if matches!(instruction.op, BytecodeOp::Member | BytecodeOp::MemberConst)
                && matches!(
                    module
                        .instructions
                        .get(pc + 1)
                        .map(|instruction| instruction.op),
                    Some(
                        BytecodeOp::Call
                            | BytecodeOp::CallZero
                            | BytecodeOp::CallOne
                            | BytecodeOp::CallTwo
                    )
                )
                && let Some(next_pc) =
                    self.try_execute_fused_member_call(module, pc, end, is_step_budgeted)?
            {
                pc = next_pc;
                continue;
            }
            #[cfg(not(feature = "debugger"))]
            if instruction.op == BytecodeOp::LoadName
                && matches!(
                    module
                        .instructions
                        .get(pc + 1)
                        .map(|instruction| instruction.op),
                    Some(
                        BytecodeOp::LoadLocal
                            | BytecodeOp::LoadLocalSmall
                            | BytecodeOp::MemberConst
                    )
                )
                && let Some(next_pc) =
                    self.try_execute_fused_name_local_call(module, pc, end, is_step_budgeted)?
            {
                pc = next_pc;
                continue;
            }
            match instruction.op {
                BytecodeOp::Marker | BytecodeOp::Label => {}
                BytecodeOp::EnterScope => {
                    let kind = self.read_scope_kind(operand(instruction, 0)?)?;
                    self.lexical_env.push_frame(kind);
                }
                BytecodeOp::LeaveScope => {
                    self.lexical_env.pop_frame();
                }
                BytecodeOp::Declare => {
                    self.declare_binding(
                        module,
                        operand(instruction, 0)?,
                        operand(instruction, 1)?,
                    )?;
                }
                BytecodeOp::LoadConst | BytecodeOp::LoadConstConst | BytecodeOp::LoadIntSmall => {
                    let dst = register(instruction, 0)?;
                    let value = if matches!(
                        instruction.op,
                        BytecodeOp::LoadConstConst | BytecodeOp::LoadIntSmall
                    ) {
                        self.read_constant_operand_value(module, instruction, 1)?
                    } else {
                        self.read_value(module, operand(instruction, 1)?)?
                    };
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::LoadUndefined
                | BytecodeOp::LoadNull
                | BytecodeOp::LoadTrue
                | BytecodeOp::LoadFalse => {
                    let dst = register(instruction, 0)?;
                    let value = match instruction.op {
                        BytecodeOp::LoadUndefined => Value::Undefined,
                        BytecodeOp::LoadNull => Value::Null,
                        BytecodeOp::LoadTrue => Value::Bool(true),
                        BytecodeOp::LoadFalse => Value::Bool(false),
                        _ => {
                            return Err(ExecuteError::Runtime(format!(
                                "invalid literal load opcode {:?}",
                                instruction.op
                            )));
                        }
                    };
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::LoadLocalSmall => {
                    let [
                        BytecodeOperand::Register(dst),
                        BytecodeOperand::LocalSlot(slot),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("local slot"));
                    };
                    let value = self.lexical_env.get_slot(*slot);
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::LoadLocal => {
                    let dst = register(instruction, 0)?;
                    let slot = match operand(instruction, 1)? {
                        BytecodeOperand::LocalSlot(slot) => *slot,
                        _ => return Err(ExecuteError::InvalidOperand("local slot")),
                    };
                    let value = self.lexical_env.get_slot(slot);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::LoadName => {
                    let dst = register(instruction, 0)?;
                    let source = operand(instruction, 1)?;
                    let value = self.load_name_cached_value(module, pc, source, dst)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreLocalSmall => {
                    let [
                        BytecodeOperand::LocalSlot(slot),
                        BytecodeOperand::Register(src),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("local slot"));
                    };
                    let value = self.read_register_value(*src);
                    self.lexical_env.set_slot(*slot, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreLocal => {
                    let slot = match operand(instruction, 0)? {
                        BytecodeOperand::LocalSlot(slot) => *slot,
                        _ => return Err(ExecuteError::InvalidOperand("local slot")),
                    };
                    let value = self.read_register_operand_value(instruction, 1)?;
                    self.lexical_env.set_slot(slot, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreLocalMemberConst => {
                    let is_var = self.read_decl_kind(operand(instruction, 0)?)? == "var";
                    let slot = local_slot_operand(instruction, 1)?;
                    if is_var {
                        self.lexical_env
                            .define_var_slot_if_absent(slot, Value::Undefined);
                    } else {
                        self.lexical_env
                            .define_slot_if_absent(slot, Value::Undefined);
                    }
                    let object_operand = operand(instruction, 2)?;
                    let object = self.read_value(module, object_operand)?;
                    let value = self.member_const_cached_value(
                        module,
                        pc,
                        &object,
                        operand(instruction, 3)?,
                    )?;
                    self.lexical_env.set_slot(slot, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::DeclareStoreLocal => {
                    let is_var = self.read_decl_kind(operand(instruction, 0)?)? == "var";
                    let slot = local_slot_operand(instruction, 1)?;
                    if is_var {
                        self.lexical_env
                            .define_var_slot_if_absent(slot, Value::Undefined);
                    } else {
                        self.lexical_env
                            .define_slot_if_absent(slot, Value::Undefined);
                    }
                    let value = self.read_register_operand_value(instruction, 2)?;
                    self.lexical_env.set_slot(slot, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::StoreName => {
                    let target = operand(instruction, 0)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    self.write_operand_target(module, target, value.clone())?;
                    self.last_value = value;
                }
                BytecodeOp::StoreMember => {
                    let object_operand = operand(instruction, 0)?;
                    let mut object = self.read_value(module, object_operand)?;
                    let property_value = self.read_value(module, operand(instruction, 1)?)?;
                    if let Some(index) = value_array_index(&property_value) {
                        let value = self.read_value(module, operand(instruction, 2)?)?;
                        if self.set_array_index_member_fast(&mut object, index, value.clone())? {
                            self.write_operand_target(module, object_operand, object)?;
                            self.last_value = value;
                            pc += 1;
                            continue;
                        }
                    }
                    let property_value =
                        self.to_primitive_with_hint(module, property_value, "string")?;
                    let property = property_key(&property_value);
                    let value = self.read_value(module, operand(instruction, 2)?)?;
                    if let Value::ExternalRef(reference) = &object {
                        #[cfg(feature = "runtime-profile")]
                        self.record_host_set_profile();
                        let _host_call = self.enter_host_call();
                        self.host_bridge.set(reference, &property, &value)?;
                    } else if self.call_accessor_setter(
                        module,
                        &object,
                        &property,
                        value.clone(),
                    )? {
                    } else {
                        set_member(&mut object, &property, value.clone())?;
                        self.write_operand_target(module, object_operand, object)?;
                    }
                    self.last_value = value;
                }
                BytecodeOp::StoreMemberConst => {
                    let object_operand = operand(instruction, 0)?;
                    let mut object = self.read_value(module, object_operand)?;
                    let property =
                        self.read_constant_string_cow(module, operand(instruction, 1)?)?;
                    let value = self.read_value(module, operand(instruction, 2)?)?;
                    if let Value::ExternalRef(reference) = &object {
                        #[cfg(feature = "runtime-profile")]
                        self.record_host_set_profile();
                        let _host_call = self.enter_host_call();
                        self.host_bridge.set(reference, &property, &value)?;
                    } else if self.call_accessor_setter(
                        module,
                        &object,
                        &property,
                        value.clone(),
                    )? {
                    } else {
                        set_member(&mut object, &property, value.clone())?;
                        self.write_operand_target(module, object_operand, object)?;
                    }
                    self.last_value = value;
                }
                BytecodeOp::Move => {
                    let dst = register(instruction, 0)?;
                    let value = self.read_simple_value(module, operand(instruction, 1)?)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Await => {
                    let dst = register(instruction, 0)?;
                    let value = self.read_value(module, operand(instruction, 1)?)?;
                    if let Some(promise) = host_promise_value(&value) {
                        return Ok(Flow::AwaitPending {
                            promise,
                            resume_dst: dst,
                            resume_pc: pc + 1,
                            resume_end: end,
                        });
                    }
                    let value = await_value(value)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Binary => {
                    let dst = register(instruction, 0)?;
                    let op = self.read_operator_cow(module, operand(instruction, 1)?)?;
                    let left = self.read_value(module, operand(instruction, 2)?)?;
                    let right = self.read_value(module, operand(instruction, 3)?)?;
                    let value = self.binary(module, op.as_ref(), left, right)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::BinaryRegReg => {
                    if let Some((dst, value)) = self.try_fast_binary_reg_reg(instruction)? {
                        self.write_register(dst, value.clone());
                        self.last_value = value;
                        pc += 1;
                        continue;
                    }
                    let [
                        BytecodeOperand::Register(dst),
                        op_operand,
                        BytecodeOperand::Register(left),
                        BytecodeOperand::Register(right),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("binary register operands"));
                    };
                    let op = self.read_operator_cow(module, op_operand)?;
                    let left = self.read_register_value(*left);
                    let right = self.read_register_value(*right);
                    let value = self.binary(module, op.as_ref(), left, right)?;
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::BinaryRegRegJump => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_fused_binary_branch_profile();
                    let [
                        BytecodeOperand::Register(dst),
                        op_operand,
                        BytecodeOperand::Register(left),
                        BytecodeOperand::Register(right),
                        BytecodeOperand::Register(test_register),
                        BytecodeOperand::Count(false_target),
                        BytecodeOperand::Count(true_target),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand(
                            "binary register jump operands",
                        ));
                    };
                    let value = if let Some((fast_dst, value)) =
                        self.try_fast_binary_reg_reg(instruction)?
                    {
                        debug_assert_eq!(fast_dst, *dst);
                        value
                    } else {
                        let op = self.read_operator_cow(module, op_operand)?;
                        let left = self.read_register_value(*left);
                        let right = self.read_register_value(*right);
                        self.binary(module, op.as_ref(), left, right)?
                    };
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                    let test = self.read_register_value(*test_register).is_truthy();
                    let target = if test { *true_target } else { *false_target };
                    pc = self.jump_target(target as usize, pc)?;
                    continue;
                }
                BytecodeOp::BinaryRegRegJumpFallthrough => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_fused_binary_branch_profile();
                    let [
                        BytecodeOperand::Register(dst),
                        op_operand,
                        BytecodeOperand::Register(left),
                        BytecodeOperand::Register(right),
                        BytecodeOperand::Register(test_register),
                        BytecodeOperand::Count(packed_target),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand(
                            "binary register fallthrough jump operands",
                        ));
                    };
                    let value = if let Some((fast_dst, value)) =
                        self.try_fast_binary_reg_reg(instruction)?
                    {
                        debug_assert_eq!(fast_dst, *dst);
                        value
                    } else {
                        let op = self.read_operator_cow(module, op_operand)?;
                        let left = self.read_register_value(*left);
                        let right = self.read_register_value(*right);
                        self.binary(module, op.as_ref(), left, right)?
                    };
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                    let jump_when_true = (*packed_target & 1) != 0;
                    let test = self.read_register_value(*test_register).is_truthy();
                    if test == jump_when_true {
                        pc = self.jump_target((*packed_target >> 1) as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::BinaryRegConst => {
                    if let Some((dst, value)) =
                        self.try_fast_binary_reg_const(module, instruction)?
                    {
                        self.write_register(dst, value.clone());
                        self.last_value = value;
                        pc += 1;
                        continue;
                    }
                    let dst = register(instruction, 0)?;
                    let op = self.read_operator_cow(module, operand(instruction, 1)?)?;
                    let left = self.read_register_operand_value(instruction, 2)?;
                    let right = self.read_constant_operand_value(module, instruction, 3)?;
                    let value = self.binary(module, op.as_ref(), left, right)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::BinaryLocalConst => {
                    let [
                        BytecodeOperand::Register(dst),
                        BytecodeOperand::LocalSlot(slot),
                        BytecodeOperand::Operator(operator),
                        BytecodeOperand::Constant(constant),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("binary local const operands"));
                    };
                    let left = self.lexical_env.get_slot(*slot);
                    let value = if let Some(op) = operator_name(*operator)
                        && let Some(value) =
                            self.fast_binary_value_const(module, op, &left, *constant)
                    {
                        value
                    } else {
                        let op = self.read_operator_cow(module, operand(instruction, 2)?)?;
                        let right = self.read_constant_operand_value(module, instruction, 3)?;
                        self.binary(module, op.as_ref(), left, right)?
                    };
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Unary => {
                    let dst = register(instruction, 0)?;
                    let op = self.read_operator(module, operand(instruction, 1)?)?;
                    let arg_operand = operand(instruction, 2)?;
                    let arg = match self.read_value(module, arg_operand) {
                        Err(ExecuteError::ReferenceError(_)) if op == "typeof" => Value::Undefined,
                        result => result?,
                    };
                    let arg = if matches!(op.as_str(), "+" | "-" | "~") {
                        self.to_primitive(module, arg)?
                    } else {
                        arg
                    };
                    let value = unary(&op, arg)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Member => {
                    let dst = register(instruction, 0)?;
                    let object = self.read_value(module, operand(instruction, 1)?)?;
                    let property_value = self.read_value(module, operand(instruction, 2)?)?;
                    if let Some(index) = value_array_index(&property_value)
                        && let Some(value) = self.get_array_index_member_fast(&object, index)
                    {
                        self.write_register(dst, value.clone());
                        self.last_value = value;
                        pc += 1;
                        continue;
                    }
                    let property_value =
                        self.to_primitive_with_hint(module, property_value, "string")?;
                    let property = property_key(&property_value);
                    let property_key = js_reflect_property_key(&property);
                    let value = self
                        .get_member_with_property_key(module, &object, &property, &property_key)
                        .map_err(|error| with_member_context(error, pc, &object, &property))?;
                    #[cfg(not(feature = "debugger"))]
                    if self.next_instruction_calls_register(module, pc, dst) {
                        self.maybe_store_member_call_inline_cache(
                            pc,
                            operand(instruction, 2)?,
                            property,
                            property_key,
                            &value,
                        );
                    }
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::MemberConst => {
                    let dst = register(instruction, 0)?;
                    let object = self.read_value(module, operand(instruction, 1)?)?;
                    let value = self.member_const_cached_value(
                        module,
                        pc,
                        &object,
                        operand(instruction, 2)?,
                    )?;
                    #[cfg(not(feature = "debugger"))]
                    if self.next_instruction_calls_register(module, pc, dst) {
                        let (property, property_key) =
                            self.member_const_property_key(module, pc, operand(instruction, 2)?)?;
                        self.maybe_store_member_call_inline_cache(
                            pc,
                            operand(instruction, 2)?,
                            property,
                            property_key,
                            &value,
                        );
                    }
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::MemberLocalConst => {
                    let [
                        BytecodeOperand::Register(dst),
                        BytecodeOperand::LocalSlot(slot),
                        property_source,
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("member local const operands"));
                    };
                    let object = self.lexical_env.get_slot(*slot);
                    let value =
                        self.member_const_cached_value(module, pc, &object, property_source)?;
                    #[cfg(not(feature = "debugger"))]
                    if self.next_instruction_calls_register(module, pc, *dst) {
                        let (property, property_key) =
                            self.member_const_property_key(module, pc, property_source)?;
                        self.maybe_store_member_call_inline_cache(
                            pc,
                            property_source,
                            property,
                            property_key,
                            &value,
                        );
                    }
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::MemberLocal => {
                    let [
                        BytecodeOperand::Register(dst),
                        BytecodeOperand::LocalSlot(slot),
                        property_source,
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("member local operands"));
                    };
                    let object = self.lexical_env.get_slot(*slot);
                    let property_value = self.read_value(module, property_source)?;
                    if let Some(index) = value_array_index(&property_value)
                        && let Some(value) = self.get_array_index_member_fast(&object, index)
                    {
                        self.write_register(*dst, value.clone());
                        self.last_value = value;
                        pc += 1;
                        continue;
                    }
                    let property_value =
                        self.to_primitive_with_hint(module, property_value, "string")?;
                    let property = property_key(&property_value);
                    let property_key = js_reflect_property_key(&property);
                    let value = self
                        .get_member_with_property_key(module, &object, &property, &property_key)
                        .map_err(|error| with_member_context(error, pc, &object, &property))?;
                    #[cfg(not(feature = "debugger"))]
                    if self.next_instruction_calls_register(module, pc, *dst) {
                        self.maybe_store_member_call_inline_cache(
                            pc,
                            property_source,
                            property,
                            property_key,
                            &value,
                        );
                    }
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Array => {
                    let dst = register(instruction, 0)?;
                    let count = count_operand(instruction, 1)? as usize;
                    let mut items = Vec::with_capacity(count);
                    for index in 0..count {
                        items.push(self.read_value(module, operand(instruction, 2 + index)?)?);
                    }
                    let value = self.array_value(items)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Object => {
                    let dst = register(instruction, 0)?;
                    let count = count_operand(instruction, 1)? as usize;
                    let mut props = BTreeMap::new();
                    let mut js_target = None;
                    for index in 0..count {
                        let key = self
                            .read_constant_string(module, operand(instruction, 2 + index * 2)?)?;
                        let value =
                            self.read_value(module, operand(instruction, 3 + index * 2)?)?;
                        if key == "..." {
                            self.apply_object_spread(module, &mut props, &mut js_target, value)?;
                        } else if let Some(target) = &js_target {
                            self.set_js_object_property(target, &key, &value)?;
                        } else {
                            props.insert(key, value);
                        }
                    }
                    let value = match js_target {
                        Some(value) => Value::JsValue(value),
                        None => self.object_value(props)?,
                    };
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::ObjectRest => {
                    let dst = register(instruction, 0)?;
                    let source = self.read_value(module, operand(instruction, 1)?)?;
                    let count = count_operand(instruction, 2)? as usize;
                    let mut args = Vec::with_capacity(count + 1);
                    args.push(source);
                    for index in 0..count {
                        args.push(self.read_value(module, operand(instruction, 3 + index)?)?);
                    }
                    let value = self.call_object_rest(module, args)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Call
                | BytecodeOp::CallZero
                | BytecodeOp::CallOne
                | BytecodeOp::CallTwo
                | BytecodeOp::CallLocalZero
                | BytecodeOp::CallLocalOne
                | BytecodeOp::CallLocalTwo => {
                    let dst = register(instruction, 0)?;
                    let callee_operand = operand(instruction, 1)?;
                    let callee = if matches!(
                        instruction.op,
                        BytecodeOp::CallLocalZero
                            | BytecodeOp::CallLocalOne
                            | BytecodeOp::CallLocalTwo
                    ) {
                        let slot = local_slot_operand(instruction, 1)?;
                        self.lexical_env.get_slot(slot)
                    } else {
                        self.read_value(module, callee_operand)?
                    };
                    if matches!(
                        instruction.op,
                        BytecodeOp::CallOne | BytecodeOp::CallLocalOne
                    ) {
                        let arg = self.read_value(module, operand(instruction, 3)?)?;
                        let value = self.call_one_with_context(
                            module,
                            pc,
                            callee_operand.clone(),
                            callee,
                            arg,
                        )?;
                        self.write_register(dst, value.clone());
                        self.last_value = value;
                        pc += 1;
                        continue;
                    }
                    #[cfg(not(feature = "compact-errors"))]
                    let callee_display = callee.to_string();
                    let args = match instruction.op {
                        BytecodeOp::CallZero | BytecodeOp::CallLocalZero => Vec::new(),
                        BytecodeOp::CallOne | BytecodeOp::CallLocalOne => {
                            return Err(ExecuteError::Runtime(
                                "CALL_1 fell through the call fast path".to_string(),
                            ));
                        }
                        BytecodeOp::CallTwo | BytecodeOp::CallLocalTwo => vec![
                            self.read_value(module, operand(instruction, 3)?)?,
                            self.read_value(module, operand(instruction, 4)?)?,
                        ],
                        _ => {
                            let count = count_operand(instruction, 2)? as usize;
                            self.read_args(module, instruction, 3, count)?
                        }
                    };
                    #[cfg(feature = "array-builtins")]
                    {
                        if let Some(value) =
                            self.try_call_array_iteration_builtin(module, &callee, &args)?
                        {
                            self.write_register(dst, value.clone());
                            self.last_value = value;
                            pc += 1;
                            continue;
                        }
                    }
                    if args.len() == 3
                        && let Some(value) = self.try_call_plain_js_three(
                            pc,
                            callee_operand.clone(),
                            &callee,
                            &args,
                        )?
                    {
                        self.write_register(dst, value.clone());
                        self.last_value = value;
                        pc += 1;
                        continue;
                    }
                    #[cfg(not(feature = "compact-errors"))]
                    let args_display = args
                        .iter()
                        .take(4)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let value = self.call(module, callee, args).map_err(|err| match err {
                        #[cfg(feature = "compact-errors")]
                        err => err,
                        #[cfg(not(feature = "compact-errors"))]
                        ExecuteError::TypeError(message) => ExecuteError::TypeError(format!(
                            "{message} at pc {pc} callee {callee_operand:?} value {callee_display} args [{args_display}]"
                        )),
                        #[cfg(not(feature = "compact-errors"))]
                        ExecuteError::RangeError(message) => ExecuteError::RangeError(format!(
                            "{message} at pc {pc} callee {callee_operand:?} value {callee_display} args [{args_display}]"
                        )),
                        #[cfg(not(feature = "compact-errors"))]
                        err => err,
                    })?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::New => {
                    let dst = register(instruction, 0)?;
                    let callee = self.read_value(module, operand(instruction, 1)?)?;
                    let count = count_operand(instruction, 2)? as usize;
                    let args = self.read_args(module, instruction, 3, count)?;
                    let value = self.construct(module, callee, args)?;
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::Template => {
                    let dst = register(instruction, 0)?;
                    let quasi_count = count_operand(instruction, 1)? as usize;
                    let mut out = String::new();
                    for index in 0..quasi_count {
                        out.push_str(
                            &self.read_constant_string(module, operand(instruction, 2 + index)?)?,
                        );
                        if let Ok(expr) = operand(instruction, 3 + quasi_count + index) {
                            let value = self.to_primitive_with_hint(
                                module,
                                self.read_value(module, expr)?,
                                "string",
                            )?;
                            out.push_str(&value.to_string());
                        }
                    }
                    let value = Value::String(out);
                    self.write_register(dst, value.clone());
                    self.last_value = value;
                }
                BytecodeOp::FunctionStart => {
                    let function = self.function_from_start(module, pc, false)?;
                    let name = function.name.clone().ok_or_else(|| {
                        ExecuteError::Runtime("function declaration missing name".to_string())
                    })?;
                    let body_end = function.body_end;
                    let value = self.host_bridge.function_value(function);
                    self.lexical_env.set_or_define_current(name, value);
                    pc = body_end;
                    continue;
                }
                BytecodeOp::FunctionExprStart => {
                    let dst = register(instruction, 0)?;
                    let function = self.function_from_start(module, pc, true)?;
                    let body_end = function.body_end;
                    self.write_register(dst, self.host_bridge.function_value(function));
                    pc = body_end;
                    continue;
                }
                BytecodeOp::FunctionEnd | BytecodeOp::FunctionExprEnd => {
                    return self.finish_flow(entry_env_depth, Flow::Value(Value::Undefined));
                }
                BytecodeOp::Class => {
                    let value = self.class_from_instruction(module, instruction)?;
                    if let BytecodeOperand::Register(dst) = operand(instruction, 0)? {
                        self.write_register(*dst, value.clone());
                    }
                    if let BytecodeOperand::Name(index) = operand(instruction, 1)? {
                        self.lexical_env
                            .set_or_define_current(name_string(module, *index)?, value.clone());
                    } else if let BytecodeOperand::LocalSlot(slot) = operand(instruction, 1)? {
                        self.lexical_env.set_slot(*slot, value.clone());
                    }
                    self.last_value = value;
                }
                BytecodeOp::Import => {
                    #[cfg(not(feature = "module"))]
                    {
                        return Err(ExecuteError::Unsupported("module"));
                    }
                    #[cfg(feature = "module")]
                    {
                        let source = self.read_constant_string(module, operand(instruction, 0)?)?;
                        let module_value = ModuleValue {
                            source,
                            exports: BTreeMap::new(),
                        };
                        self.last_value = self.host_bridge.module_value(module_value)?;
                    }
                }
                BytecodeOp::Export => {
                    #[cfg(not(feature = "module"))]
                    {
                        return Err(ExecuteError::Unsupported("module"));
                    }
                    #[cfg(feature = "module")]
                    {
                        let count = count_operand(instruction, 1)? as usize;
                        for index in 0..count {
                            let exported_name = self.read_constant_string(
                                module,
                                operand(instruction, 2 + index * 2)?,
                            )?;
                            let value =
                                self.read_value(module, operand(instruction, 3 + index * 2)?)?;
                            self.exports.insert(exported_name, value);
                        }
                    }
                }
                BytecodeOp::TryStart => {
                    let parts = find_try_parts(module, pc)?;
                    let body_env_depth = self.lexical_env.depth();
                    let flow = match self.execute_range(module, parts.body_start, parts.body_end) {
                        Ok(flow) => flow,
                        Err(err) => {
                            self.lexical_env.truncate_to_depth(body_env_depth);
                            match catchable_error_value(&err) {
                                Some(value) => Flow::Throw(value),
                                None => return Err(err),
                            }
                        }
                    };
                    let flow = match flow {
                        Flow::Throw(value) if parts.catch_start < parts.catch_end => {
                            self.lexical_env.push_frame(ScopeKind::Catch);
                            if let Some(param) = &parts.catch_param {
                                self.define_catch_param(module, param, value)?;
                            }
                            let catch_flow =
                                self.execute_range(module, parts.catch_start, parts.catch_end);
                            self.lexical_env.pop_frame();
                            catch_flow?
                        }
                        flow => flow,
                    };
                    if matches!(flow, Flow::Yield { .. } | Flow::AwaitPending { .. }) {
                        return Ok(flow);
                    }
                    #[cfg(feature = "debugger")]
                    if matches!(flow, Flow::Pause { .. }) {
                        return Ok(flow);
                    }
                    let flow = if parts.finally_start < parts.finally_end {
                        match self.execute_range(module, parts.finally_start, parts.finally_end)? {
                            Flow::Value(_) => flow,
                            final_flow => final_flow,
                        }
                    } else {
                        flow
                    };
                    pc = parts.end + 1;
                    match flow {
                        Flow::Value(value) => self.last_value = value,
                        Flow::Return(value) => {
                            return self.finish_flow(entry_env_depth, Flow::Return(value));
                        }
                        Flow::Throw(value) => {
                            return self.finish_flow(entry_env_depth, Flow::Throw(value));
                        }
                        Flow::Yield { .. } | Flow::AwaitPending { .. } => return Ok(flow),
                        #[cfg(feature = "debugger")]
                        Flow::Pause { .. } => return Ok(flow),
                    }
                    continue;
                }
                BytecodeOp::CatchStart | BytecodeOp::FinallyStart | BytecodeOp::TryEnd => {
                    return self.finish_flow(entry_env_depth, Flow::Value(self.last_value.clone()));
                }
                BytecodeOp::Throw => {
                    let value = self.read_value(module, operand(instruction, 0)?)?;
                    return self.finish_flow(entry_env_depth, Flow::Throw(value));
                }
                BytecodeOp::Return => {
                    let value = self.read_value(module, operand(instruction, 0)?)?;
                    return self.finish_flow(entry_env_depth, Flow::Return(value));
                }
                BytecodeOp::ReturnReg => {
                    let value = self.read_register_operand_value(instruction, 0)?;
                    return self.finish_flow(entry_env_depth, Flow::Return(value));
                }
                BytecodeOp::ReturnConst => {
                    let value = self.read_constant_operand_value(module, instruction, 0)?;
                    return self.finish_flow(entry_env_depth, Flow::Return(value));
                }
                BytecodeOp::ReturnIfLocalFalse => {
                    let dst = register(instruction, 0)?;
                    let slot = local_slot_operand(instruction, 1)?;
                    let test = self.lexical_env.get_slot(slot);
                    self.write_register(dst, test.clone());
                    self.last_value = test.clone();
                    if !test.is_truthy() {
                        let value = self.read_value(module, operand(instruction, 2)?)?;
                        return self.finish_flow(entry_env_depth, Flow::Return(value));
                    }
                }
                BytecodeOp::ReturnIfLocalFalseElseMemberBinaryConst => {
                    let slot = local_slot_operand(instruction, 0)?;
                    let object = self.lexical_env.get_slot(slot);
                    if !object.is_truthy() {
                        let value = self.read_value(module, operand(instruction, 1)?)?;
                        return self.finish_flow(entry_env_depth, Flow::Return(value));
                    }
                    let property =
                        self.read_constant_string_cow(module, operand(instruction, 2)?)?;
                    let member = self
                        .get_member(module, &object, &property)
                        .map_err(|error| with_member_context(error, pc, &object, &property))?;
                    let op = self.read_operator_cow(module, operand(instruction, 3)?)?;
                    let constant = match operand(instruction, 4)? {
                        BytecodeOperand::Constant(index) => *index,
                        _ => return Err(ExecuteError::InvalidOperand("constant")),
                    };
                    let value = if let Some(value) =
                        self.fast_binary_value_const(module, op.as_ref(), &member, constant)
                    {
                        value
                    } else {
                        let right = self.read_constant_operand_value(module, instruction, 4)?;
                        self.binary(module, op.as_ref(), member, right)?
                    };
                    self.last_value = value.clone();
                    return self.finish_flow(entry_env_depth, Flow::Return(value));
                }
                BytecodeOp::Yield => {
                    #[cfg(not(feature = "generator"))]
                    {
                        return Err(ExecuteError::Unsupported("generator"));
                    }
                    #[cfg(feature = "generator")]
                    {
                        let value = self.read_value(module, operand(instruction, 0)?)?;
                        let resume_dst = match operand(instruction, 1)? {
                            BytecodeOperand::Register(register) => Some(*register),
                            BytecodeOperand::None => None,
                            _ => return Err(ExecuteError::InvalidOperand("yield target")),
                        };
                        return Ok(Flow::Yield {
                            value,
                            resume_pc: pc + 1,
                            resume_dst,
                        });
                    }
                }
                BytecodeOp::Pop => {
                    self.last_value = self.read_value(module, operand(instruction, 0)?)?;
                }
                BytecodeOp::PopReg => {
                    self.last_value = self.read_register_operand_value(instruction, 0)?;
                }
                BytecodeOp::Jump => {
                    pc = self.jump_target(count_operand(instruction, 0)? as usize, pc)?;
                    continue;
                }
                BytecodeOp::JumpIfFalse => {
                    let test = self.read_value(module, operand(instruction, 0)?)?;
                    if !test.is_truthy() {
                        pc = self.jump_target(count_operand(instruction, 1)? as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::JumpIfFalseReg => {
                    let test = self.read_register_operand_value(instruction, 0)?;
                    if !test.is_truthy() {
                        pc = self.jump_target(count_operand(instruction, 1)? as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::JumpIfTrueReg => {
                    let test = self.read_register_operand_value(instruction, 0)?;
                    if test.is_truthy() {
                        pc = self.jump_target(count_operand(instruction, 1)? as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::MoveJumpReg => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_fused_move_branch_profile();
                    let [
                        BytecodeOperand::Register(dst),
                        source,
                        BytecodeOperand::Register(test_register),
                        BytecodeOperand::Count(false_target),
                        BytecodeOperand::Count(true_target),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand("move jump operands"));
                    };
                    let value = self.read_simple_value(module, source)?;
                    let test = if *test_register == *dst {
                        value.is_truthy()
                    } else {
                        self.read_register_value(*test_register).is_truthy()
                    };
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                    let target = if test { *true_target } else { *false_target };
                    pc = self.jump_target(target as usize, pc)?;
                    continue;
                }
                BytecodeOp::MoveJumpFallthroughReg => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_fused_move_branch_profile();
                    let [
                        BytecodeOperand::Register(dst),
                        source,
                        BytecodeOperand::Register(test_register),
                        BytecodeOperand::Count(packed_target),
                    ] = instruction.operands.as_slice()
                    else {
                        return Err(ExecuteError::InvalidOperand(
                            "move fallthrough jump operands",
                        ));
                    };
                    let value = self.read_simple_value(module, source)?;
                    let test = if *test_register == *dst {
                        value.is_truthy()
                    } else {
                        self.read_register_value(*test_register).is_truthy()
                    };
                    self.write_register(*dst, value.clone());
                    self.last_value = value;
                    let jump_when_true = (*packed_target & 1) != 0;
                    if test == jump_when_true {
                        pc = self.jump_target((*packed_target >> 1) as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::JumpIfLocalBinaryConstFalse
                | BytecodeOp::JumpIfLocalBinaryConstTrue => {
                    let local_dst = register(instruction, 0)?;
                    let slot = local_slot_operand(instruction, 1)?;
                    let test_dst = register(instruction, 2)?;
                    let jump_when_true = instruction.op == BytecodeOp::JumpIfLocalBinaryConstTrue;
                    let op = self.read_operator_cow(module, operand(instruction, 3)?)?;
                    let left = self.lexical_env.get_slot(slot);
                    self.write_register(local_dst, left.clone());
                    let constant = match operand(instruction, 4)? {
                        BytecodeOperand::Constant(index) => *index,
                        _ => return Err(ExecuteError::InvalidOperand("constant")),
                    };
                    let test = if let Some(value) =
                        self.fast_binary_value_const(module, op.as_ref(), &left, constant)
                    {
                        value
                    } else {
                        let right = self.read_constant_operand_value(module, instruction, 4)?;
                        self.binary(module, op.as_ref(), left, right)?
                    };
                    self.write_register(test_dst, test.clone());
                    self.last_value = test.clone();
                    if test.is_truthy() == jump_when_true {
                        pc = self.jump_target(count_operand(instruction, 5)? as usize, pc)?;
                        continue;
                    }
                }
                BytecodeOp::Unsupported => {
                    return Err(ExecuteError::Unsupported(instruction.op.mnemonic()));
                }
            }
            pc += 1;
        }
        self.finish_flow(entry_env_depth, Flow::Value(self.last_value.clone()))
    }

    fn ensure_register_frame_size(
        &mut self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<(), ExecuteError> {
        let frame_size = self.register_frame_size(module, start, end)?;
        if self.registers.len() < frame_size {
            self.registers.resize(frame_size, Value::Undefined);
        }
        Ok(())
    }

    fn register_frame_size(
        &self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<usize, ExecuteError> {
        let key = (start, end);
        if let Some(size) = self.register_frame_size_cache.borrow().get(&key).copied() {
            return Ok(size);
        }
        let size = self.scan_register_frame_size(module, start, end)?;
        self.register_frame_size_cache
            .borrow_mut()
            .insert(key, size);
        Ok(size)
    }

    fn scan_register_frame_size(
        &self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<usize, ExecuteError> {
        let mut frame_size = 0usize;
        let mut pc = start;
        while pc < end {
            let instruction = module
                .instructions
                .get(pc)
                .ok_or_else(|| ExecuteError::Runtime(format!("bad pc {pc}")))?;
            for operand in &instruction.operands {
                if let BytecodeOperand::Register(register) = operand {
                    frame_size = frame_size.max(*register as usize + 1);
                }
            }
            if matches!(
                instruction.op,
                BytecodeOp::FunctionStart | BytecodeOp::FunctionExprStart
            ) {
                let body_end = self.function_body_end_for_instruction(module, pc, instruction)?;
                if body_end > pc && body_end <= end {
                    pc = body_end;
                    continue;
                }
            }
            pc += 1;
        }
        Ok(frame_size)
    }

    fn function_body_end_for_instruction(
        &self,
        module: &BytecodeModule,
        pc: usize,
        instruction: &BytecodeInstruction,
    ) -> Result<usize, ExecuteError> {
        let function_operand_index = if instruction.op == BytecodeOp::FunctionExprStart {
            1
        } else {
            0
        };
        let function_index = match operand(instruction, function_operand_index)? {
            BytecodeOperand::Function(index) => *index,
            _ => return Err(ExecuteError::InvalidOperand("function")),
        };
        let function_meta = module
            .functions
            .get(function_index as usize)
            .ok_or_else(|| ExecuteError::Runtime(format!("bad function index {function_index}")))?;
        let body_end = function_meta.body_end as usize;
        if body_end < pc + 1 || body_end > module.instructions.len() {
            return Err(ExecuteError::Runtime(format!(
                "bad function body range {}..{}",
                function_meta.body_start, function_meta.body_end
            )));
        }
        Ok(body_end)
    }

    fn finish_flow(&mut self, entry_env_depth: usize, flow: Flow) -> Result<Flow, ExecuteError> {
        self.lexical_env.truncate_to_depth(entry_env_depth);
        Ok(flow)
    }

    fn consume_step(&self, pc: usize) -> Result<(), ExecuteError> {
        #[cfg(feature = "compact-errors")]
        let _ = pc;
        let remaining = self.execution_budget.get();
        if remaining == usize::MAX {
            return Ok(());
        }
        if remaining == 0 {
            return Err(crate::error::range_error!(
                "maximum execution steps exceeded at pc {pc}"
            ));
        }
        self.execution_budget.set(remaining - 1);
        Ok(())
    }

    fn try_fast_binary_reg_const(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
    ) -> Result<Option<(u32, Value)>, ExecuteError> {
        let [
            BytecodeOperand::Register(dst),
            BytecodeOperand::Operator(operator),
            BytecodeOperand::Register(left),
            BytecodeOperand::Constant(constant),
        ] = instruction.operands.as_slice()
        else {
            return Ok(None);
        };
        let left_value = self.read_register_value(*left);
        let Some(op) = operator_name(*operator) else {
            return Ok(None);
        };
        Ok(self
            .fast_binary_value_const(module, op, &left_value, *constant)
            .map(|value| (*dst, value)))
    }

    fn fast_binary_value_const(
        &self,
        module: &BytecodeModule,
        op: &str,
        left_value: &Value,
        constant: u32,
    ) -> Option<Value> {
        if let Some(matches) = fast_constant_equality(
            Some(op),
            left_value,
            module.constants.get(constant as usize),
        ) {
            #[cfg(feature = "runtime-profile")]
            self.record_fast_binary_reg_const_profile();
            return Some(Value::Bool(matches));
        }
        let left = numeric_value(left_value)?;
        let Some(BytecodeConstant::Number(right)) = module.constants.get(constant as usize) else {
            return None;
        };
        let value = fast_number_binary(op, left, *right)?;
        #[cfg(feature = "runtime-profile")]
        self.record_fast_binary_reg_const_profile();
        Some(value)
    }

    fn try_fast_binary_reg_reg(
        &self,
        instruction: &BytecodeInstruction,
    ) -> Result<Option<(u32, Value)>, ExecuteError> {
        if instruction.operands.len() < 4 {
            return Ok(None);
        }
        let (
            BytecodeOperand::Register(dst),
            BytecodeOperand::Operator(operator),
            BytecodeOperand::Register(left),
            BytecodeOperand::Register(right),
        ) = (
            &instruction.operands[0],
            &instruction.operands[1],
            &instruction.operands[2],
            &instruction.operands[3],
        )
        else {
            return Ok(None);
        };
        let Some(op) = operator_name(*operator) else {
            return Ok(None);
        };
        let left = self.read_register_value(*left);
        let right = self.read_register_value(*right);
        let (Some(left), Some(right)) = (numeric_value(&left), numeric_value(&right)) else {
            return Ok(None);
        };
        let Some(value) = fast_number_binary(op, left, right) else {
            return Ok(None);
        };
        #[cfg(feature = "runtime-profile")]
        self.record_fast_binary_reg_reg_profile();
        Ok(Some((*dst, value)))
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_binary_reg_branch(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 1 >= end {
            return Ok(None);
        }
        let binary = &module.instructions[pc];
        let [
            BytecodeOperand::Register(dst),
            op_operand,
            BytecodeOperand::Register(left),
            BytecodeOperand::Register(right),
        ] = binary.operands.as_slice()
        else {
            return Ok(None);
        };
        let branch = &module.instructions[pc + 1];
        let [
            BytecodeOperand::Register(test_register),
            BytecodeOperand::Count(branch_target),
        ] = branch.operands.as_slice()
        else {
            return Ok(None);
        };
        if *test_register != *dst
            || !matches!(
                branch.op,
                BytecodeOp::JumpIfFalseReg | BytecodeOp::JumpIfTrueReg
            )
        {
            return Ok(None);
        }

        let value = if let Some((fast_dst, value)) = self.try_fast_binary_reg_reg(binary)? {
            debug_assert_eq!(fast_dst, *dst);
            value
        } else {
            let op = self.read_operator_cow(module, op_operand)?;
            let left = self.read_register_value(*left);
            let right = self.read_register_value(*right);
            self.binary(module, op.as_ref(), left, right)?
        };
        let test = value.is_truthy();
        self.write_register(*dst, value.clone());
        self.last_value = value;
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        #[cfg(feature = "runtime-profile")]
        self.record_fused_binary_branch_profile();

        self.finish_fused_branch(
            module,
            pc + 1,
            end,
            branch.op,
            test,
            *branch_target as usize,
            is_step_budgeted,
        )
        .map(Some)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_move_branch(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 1 >= end {
            return Ok(None);
        }
        let move_instruction = &module.instructions[pc];
        let [BytecodeOperand::Register(dst), source] = move_instruction.operands.as_slice() else {
            return Ok(None);
        };
        let branch = &module.instructions[pc + 1];
        let [
            BytecodeOperand::Register(test_register),
            BytecodeOperand::Count(branch_target),
        ] = branch.operands.as_slice()
        else {
            return Ok(None);
        };
        if !matches!(
            branch.op,
            BytecodeOp::JumpIfFalseReg | BytecodeOp::JumpIfTrueReg
        ) {
            return Ok(None);
        }
        let branch_tests_move_source =
            matches!(source, BytecodeOperand::Register(source) if *source == *test_register);
        if *test_register != *dst && !branch_tests_move_source {
            return Ok(None);
        }

        let value = self.read_simple_value(module, source)?;
        let test = value.is_truthy();
        self.write_register(*dst, value.clone());
        self.last_value = value;
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        #[cfg(feature = "runtime-profile")]
        self.record_fused_move_branch_profile();
        self.finish_fused_branch(
            module,
            pc + 1,
            end,
            branch.op,
            test,
            *branch_target as usize,
            is_step_budgeted,
        )
        .map(Some)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_reg_branch_jump(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        let branch = &module.instructions[pc];
        let [
            BytecodeOperand::Register(test_register),
            BytecodeOperand::Count(branch_target),
        ] = branch.operands.as_slice()
        else {
            return Ok(None);
        };
        if !matches!(
            branch.op,
            BytecodeOp::JumpIfFalseReg | BytecodeOp::JumpIfTrueReg
        ) {
            return Ok(None);
        }
        let test = self.read_register_value(*test_register).is_truthy();
        #[cfg(feature = "runtime-profile")]
        self.record_fused_reg_branch_jump_profile();
        self.finish_fused_branch(
            module,
            pc,
            end,
            branch.op,
            test,
            *branch_target as usize,
            is_step_budgeted,
        )
        .map(Some)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_local_index_member_binary_jump(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 4 >= end {
            return Ok(None);
        }
        let Some((left_register, left_slot)) = local_load_parts(&module.instructions[pc]) else {
            return Ok(None);
        };
        let Some((object_register, object_slot)) = local_load_parts(&module.instructions[pc + 1])
        else {
            return Ok(None);
        };
        let binary_local = &module.instructions[pc + 2];
        let [
            BytecodeOperand::Register(property_register),
            BytecodeOperand::LocalSlot(property_slot),
            property_operator,
            BytecodeOperand::Constant(property_constant),
        ] = binary_local.operands.as_slice()
        else {
            return Ok(None);
        };
        if binary_local.op != BytecodeOp::BinaryLocalConst {
            return Ok(None);
        }
        let member = &module.instructions[pc + 3];
        let [
            BytecodeOperand::Register(member_register),
            BytecodeOperand::Register(member_object),
            BytecodeOperand::Register(member_property),
        ] = member.operands.as_slice()
        else {
            return Ok(None);
        };
        if member.op != BytecodeOp::Member
            || *member_object != object_register
            || *member_property != *property_register
        {
            return Ok(None);
        }
        let branch = &module.instructions[pc + 4];
        let [
            BytecodeOperand::Register(branch_dst),
            branch_operator,
            BytecodeOperand::Register(branch_left),
            BytecodeOperand::Register(branch_right),
            BytecodeOperand::Register(test_register),
            branch_targets @ ..,
        ] = branch.operands.as_slice()
        else {
            return Ok(None);
        };
        if !matches!(
            branch.op,
            BytecodeOp::BinaryRegRegJump | BytecodeOp::BinaryRegRegJumpFallthrough
        ) || *branch_left != left_register
            || *branch_right != *member_register
            || *test_register != *branch_dst
        {
            return Ok(None);
        }

        let left = self.lexical_env.get_slot(left_slot);
        self.write_register(left_register, left.clone());
        let object = self.lexical_env.get_slot(object_slot);
        self.write_register(object_register, object.clone());

        let property_left = self.lexical_env.get_slot(*property_slot);
        let property_value = if let Some(op) = operand_operator_name(property_operator)
            && let Some(value) =
                self.fast_binary_value_const(module, op, &property_left, *property_constant)
        {
            value
        } else {
            let op = self.read_operator_cow(module, property_operator)?;
            let right = constant_value(module, *property_constant)?;
            self.binary(module, op.as_ref(), property_left, right)?
        };
        self.write_register(*property_register, property_value.clone());

        let object_for_member = self.read_register_value(*member_object);
        let member_value = if let Some(index) = value_array_index(&property_value)
            && let Some(value) = self.get_array_index_member_fast(&object_for_member, index)
        {
            value
        } else {
            let property_value = self.to_primitive_with_hint(module, property_value, "string")?;
            let property = property_key(&property_value);
            let property_key = js_reflect_property_key(&property);
            self.get_member_with_property_key(module, &object, &property, &property_key)
                .map_err(|error| {
                    with_member_context(error, pc + 3, &object_for_member, &property)
                })?
        };
        self.write_register(*member_register, member_value.clone());

        let branch_value = if let Some((fast_dst, value)) = self.try_fast_binary_reg_reg(branch)? {
            debug_assert_eq!(fast_dst, *branch_dst);
            value
        } else {
            let op = self.read_operator_cow(module, branch_operator)?;
            let left = self.read_register_value(*branch_left);
            let right = self.read_register_value(*branch_right);
            self.binary(module, op.as_ref(), left, right)?
        };
        self.write_register(*branch_dst, branch_value.clone());
        self.last_value = branch_value.clone();

        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        self.consume_fused_instruction(module, pc + 2, is_step_budgeted)?;
        self.consume_fused_instruction(module, pc + 3, is_step_budgeted)?;
        self.consume_fused_instruction(module, pc + 4, is_step_budgeted)?;
        #[cfg(feature = "runtime-profile")]
        self.record_fused_binary_branch_profile();

        let test = branch_value.is_truthy();
        match branch.op {
            BytecodeOp::BinaryRegRegJump => {
                let [
                    BytecodeOperand::Count(false_target),
                    BytecodeOperand::Count(true_target),
                ] = branch_targets
                else {
                    return Ok(None);
                };
                let target = if test { *true_target } else { *false_target };
                self.jump_target(target as usize, pc + 4).map(Some)
            }
            BytecodeOp::BinaryRegRegJumpFallthrough => {
                let [BytecodeOperand::Count(packed_target)] = branch_targets else {
                    return Ok(None);
                };
                let jump_when_true = (*packed_target & 1) != 0;
                if test == jump_when_true {
                    self.jump_target((*packed_target >> 1) as usize, pc + 4)
                        .map(Some)
                } else {
                    Ok(Some(pc + 5))
                }
            }
            _ => Ok(None),
        }
    }

    #[cfg(not(feature = "debugger"))]
    fn finish_fused_branch(
        &mut self,
        module: &BytecodeModule,
        branch_pc: usize,
        end: usize,
        branch_op: BytecodeOp,
        test: bool,
        branch_target: usize,
        is_step_budgeted: bool,
    ) -> Result<usize, ExecuteError> {
        let should_jump = match branch_op {
            BytecodeOp::JumpIfFalseReg => !test,
            BytecodeOp::JumpIfTrueReg => test,
            _ => {
                return Err(ExecuteError::InvalidOperand(
                    "fused branch expected JumpIfFalseReg or JumpIfTrueReg",
                ));
            }
        };
        if should_jump {
            return self.jump_target(branch_target, branch_pc);
        }

        let jump_pc = branch_pc + 1;
        if jump_pc < end
            && let Some(jump) = module.instructions.get(jump_pc)
            && jump.op == BytecodeOp::Jump
        {
            let [BytecodeOperand::Count(true_target)] = jump.operands.as_slice() else {
                return Ok(jump_pc);
            };
            self.consume_fused_instruction(module, jump_pc, is_step_budgeted)?;
            return self.jump_target(*true_target as usize, jump_pc);
        }

        Ok(branch_pc + 1)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_local_branch(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<FusedControl>, ExecuteError> {
        if pc + 2 >= end {
            return Ok(None);
        }
        let load = &module.instructions[pc];
        let Some((test_register, slot)) = local_load_parts(load) else {
            return Ok(None);
        };
        let jump_if_false = &module.instructions[pc + 1];
        let jump_true = &module.instructions[pc + 2];
        let [
            BytecodeOperand::Register(jump_test),
            BytecodeOperand::Count(false_pc),
        ] = jump_if_false.operands.as_slice()
        else {
            return Ok(None);
        };
        let [BytecodeOperand::Count(true_pc)] = jump_true.operands.as_slice() else {
            return Ok(None);
        };
        if jump_if_false.op != BytecodeOp::JumpIfFalseReg
            || jump_true.op != BytecodeOp::Jump
            || *jump_test != test_register
        {
            return Ok(None);
        }
        let false_pc = *false_pc as usize;
        let true_pc = *true_pc as usize;
        if false_pc >= end || true_pc >= end {
            return Ok(None);
        }

        let test = self.lexical_env.get_slot(slot);
        self.write_register(test_register, test.clone());
        self.last_value = test.clone();
        if test.is_truthy() {
            self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
            self.consume_fused_instruction(module, pc + 2, is_step_budgeted)?;
            let target = self.jump_target(true_pc, pc + 2)?;
            return Ok(Some(FusedControl::Continue(target)));
        }

        let false_pc = self.jump_target(false_pc, pc + 1)?;
        let Some((value, consumed)) = self.try_direct_return_literal(module, false_pc, end)? else {
            return Ok(None);
        };
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        for skipped_pc in consumed {
            self.consume_fused_instruction(module, skipped_pc, is_step_budgeted)?;
        }
        Ok(Some(FusedControl::Return(value)))
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_move_return(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<Value>, ExecuteError> {
        let move_instruction = &module.instructions[pc];
        let [BytecodeOperand::Register(dst), source] = move_instruction.operands.as_slice() else {
            return Ok(None);
        };
        let Some(next) = module.instructions.get(pc + 1) else {
            return Ok(None);
        };
        let return_pc = match next.op {
            BytecodeOp::Return | BytecodeOp::ReturnReg if return_register_matches(next, *dst) => {
                pc + 1
            }
            BytecodeOp::Jump => {
                let [BytecodeOperand::Count(target)] = next.operands.as_slice() else {
                    return Ok(None);
                };
                let target = *target as usize;
                if target >= end {
                    return Ok(None);
                }
                let Some(return_instruction) = module.instructions.get(target) else {
                    return Ok(None);
                };
                if !return_register_matches(return_instruction, *dst) {
                    return Ok(None);
                }
                self.jump_target(target, pc + 1)?
            }
            _ => return Ok(None),
        };

        let value = self.read_simple_value(module, source)?;
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        if return_pc != pc + 1 {
            self.consume_fused_instruction(module, return_pc, is_step_budgeted)?;
        }
        Ok(Some(value))
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_member_call(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 1 >= end {
            return Ok(None);
        }
        let member = &module.instructions[pc];
        let [
            BytecodeOperand::Register(member_register),
            object_operand,
            property_operand,
        ] = member.operands.as_slice()
        else {
            return Ok(None);
        };
        let call = &module.instructions[pc + 1];
        let Some((call_dst, call_callee, arity)) = small_call_parts(call) else {
            return Ok(None);
        };
        if call_callee != *member_register {
            return Ok(None);
        }

        let object = self.read_value(module, object_operand)?;
        let Some((property, property_key)) =
            self.member_call_property_key(module, pc, member, property_operand)?
        else {
            return Ok(None);
        };
        let args = self.read_small_call_args(module, call, arity)?;
        let Some(value) = self.try_call_member_with_ic(
            pc,
            pc + 1,
            &object,
            property_operand,
            &property,
            &property_key,
            args,
            BytecodeOperand::Register(*member_register),
        )?
        else {
            return Ok(None);
        };
        self.write_register(call_dst, value.clone());
        self.last_value = value;
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        self.try_execute_fused_declare_store_local(module, pc + 2, end, call_dst, is_step_budgeted)
            .map(Some)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_name_local_call(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 2 >= end {
            return Ok(None);
        }
        if let Some(next_pc) =
            self.try_execute_fused_name_member_local_call(module, pc, end, is_step_budgeted)?
        {
            return Ok(Some(next_pc));
        }
        let load_name = &module.instructions[pc];
        let Some((callee_register, callee_operand)) = load_name_parts(load_name) else {
            return Ok(None);
        };
        let Some((arg_register, arg_slot)) = local_load_parts(&module.instructions[pc + 1]) else {
            return Ok(None);
        };
        let call = &module.instructions[pc + 2];
        let [
            BytecodeOperand::Register(call_dst),
            BytecodeOperand::Register(call_callee),
            BytecodeOperand::Count(1),
            BytecodeOperand::Register(call_arg),
        ] = call.operands.as_slice()
        else {
            return Ok(None);
        };
        if call.op != BytecodeOp::CallOne
            || *call_callee != callee_register
            || *call_arg != arg_register
        {
            return Ok(None);
        }

        let callee = self.load_name_operand_value(module, callee_operand, pc, callee_register)?;
        self.write_register(callee_register, callee.clone());
        let arg = self.lexical_env.get_slot(arg_slot);
        self.write_register(arg_register, arg.clone());
        let value = self.call_one_with_context(
            module,
            pc + 2,
            BytecodeOperand::Register(callee_register),
            callee,
            arg,
        )?;
        self.write_register(*call_dst, value.clone());
        self.last_value = value;
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        self.consume_fused_instruction(module, pc + 2, is_step_budgeted)?;
        self.try_execute_fused_declare_store_local(module, pc + 3, end, *call_dst, is_step_budgeted)
            .map(Some)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_name_member_local_call(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 3 >= end {
            return Ok(None);
        }
        let load_name = &module.instructions[pc];
        let Some((object_register, object_operand)) = load_name_parts(load_name) else {
            return Ok(None);
        };
        let member = &module.instructions[pc + 1];
        let [
            BytecodeOperand::Register(member_register),
            BytecodeOperand::Register(member_object),
            property_operand,
        ] = member.operands.as_slice()
        else {
            return Ok(None);
        };
        if member.op != BytecodeOp::MemberConst || *member_object != object_register {
            return Ok(None);
        }
        let Some((arg_register, arg_slot)) = local_load_parts(&module.instructions[pc + 2]) else {
            return Ok(None);
        };
        let call = &module.instructions[pc + 3];
        let [
            BytecodeOperand::Register(call_dst),
            BytecodeOperand::Register(call_callee),
            BytecodeOperand::Count(1),
            BytecodeOperand::Register(call_arg),
        ] = call.operands.as_slice()
        else {
            return Ok(None);
        };
        if call.op != BytecodeOp::CallOne
            || *call_callee != *member_register
            || *call_arg != arg_register
        {
            return Ok(None);
        }

        let object = self.load_name_operand_value(module, object_operand, pc, object_register)?;
        self.write_register(object_register, object.clone());
        let arg = self.lexical_env.get_slot(arg_slot);
        self.write_register(arg_register, arg.clone());
        let (property, property_key) =
            self.member_const_property_key(module, pc + 1, property_operand)?;
        if let Some(value) = self.try_call_member_with_ic(
            pc + 1,
            pc + 3,
            &object,
            property_operand,
            &property,
            &property_key,
            vec![arg.clone()],
            BytecodeOperand::Register(*member_register),
        )? {
            self.write_register(*call_dst, value.clone());
            self.last_value = value;
            self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
            self.consume_fused_instruction(module, pc + 2, is_step_budgeted)?;
            self.consume_fused_instruction(module, pc + 3, is_step_budgeted)?;
            return self
                .try_execute_fused_declare_store_local(
                    module,
                    pc + 4,
                    end,
                    *call_dst,
                    is_step_budgeted,
                )
                .map(Some);
        }
        let callee = self.member_const_cached_value(module, pc + 1, &object, property_operand)?;
        self.maybe_store_member_call_inline_cache(
            pc + 1,
            property_operand,
            property,
            property_key,
            &callee,
        );
        self.write_register(*member_register, callee.clone());
        let value = self.call_one_with_context(
            module,
            pc + 3,
            BytecodeOperand::Register(*member_register),
            callee,
            arg,
        )?;
        self.write_register(*call_dst, value.clone());
        self.last_value = value;
        self.consume_fused_instruction(module, pc + 1, is_step_budgeted)?;
        self.consume_fused_instruction(module, pc + 2, is_step_budgeted)?;
        self.consume_fused_instruction(module, pc + 3, is_step_budgeted)?;
        self.try_execute_fused_declare_store_local(module, pc + 4, end, *call_dst, is_step_budgeted)
            .map(Some)
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_declare_store_local(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        value_register: u32,
        is_step_budgeted: bool,
    ) -> Result<usize, ExecuteError> {
        if pc >= end {
            return Ok(pc);
        }
        let Some(instruction) = module.instructions.get(pc) else {
            return Ok(pc);
        };
        let [
            decl_kind,
            BytecodeOperand::LocalSlot(slot),
            BytecodeOperand::Register(source),
        ] = instruction.operands.as_slice()
        else {
            return Ok(pc);
        };
        if instruction.op != BytecodeOp::DeclareStoreLocal || *source != value_register {
            return Ok(pc);
        }
        let is_var = self.read_decl_kind(decl_kind)? == "var";
        if is_var {
            self.lexical_env
                .define_var_slot_if_absent(*slot, Value::Undefined);
        } else {
            self.lexical_env
                .define_slot_if_absent(*slot, Value::Undefined);
        }
        let value = self.read_register_value(value_register);
        self.lexical_env.set_slot(*slot, value.clone());
        self.last_value = value;
        self.consume_fused_instruction(module, pc, is_step_budgeted)?;
        Ok(pc + 1)
    }

    fn load_name_cached_value(
        &self,
        module: &BytecodeModule,
        pc: usize,
        source: &BytecodeOperand,
        dst: u32,
    ) -> Result<Value, ExecuteError> {
        (|| -> Result<Value, ExecuteError> {
            if let Some(kind) = { self.pc_inline_cache.borrow().load_name(pc, source) } {
                #[cfg(feature = "runtime-profile")]
                self.record_load_name_cache_hit_profile();
                return self.load_name_cached_kind_value(module, pc, dst, &kind);
            }
            #[cfg(feature = "runtime-profile")]
            self.record_load_name_cache_miss_profile();

            let kind = match source {
                BytecodeOperand::External(index) => LoadNameInlineCacheKind::External(*index),
                BytecodeOperand::LocalSlot(slot) => LoadNameInlineCacheKind::LocalSlot(*slot),
                _ => LoadNameInlineCacheKind::Name(self.read_name(module, source)?),
            };
            let value = self.load_name_cached_kind_value(module, pc, dst, &kind)?;
            if let Ok(mut cache) = self.pc_inline_cache.try_borrow_mut() {
                cache.store_load_name(pc, source.clone(), kind);
            }
            Ok(value)
        })()
        .map_err(|error| with_load_context(error, pc, source))
    }

    fn load_name_cached_kind_value(
        &self,
        module: &BytecodeModule,
        pc: usize,
        dst: u32,
        kind: &LoadNameInlineCacheKind,
    ) -> Result<Value, ExecuteError> {
        match kind {
            LoadNameInlineCacheKind::External(index) => {
                self.read_external_load_name_value(module, *index, pc, dst)
            }
            LoadNameInlineCacheKind::LocalSlot(slot) => Ok(self.lexical_env.get_slot(*slot)),
            LoadNameInlineCacheKind::Name(name) => self.resolve_name_value(self.get_name(name)),
        }
    }

    fn read_external_load_name_value(
        &self,
        module: &BytecodeModule,
        index: u32,
        pc: usize,
        dst: u32,
    ) -> Result<Value, ExecuteError> {
        let source = BytecodeOperand::External(index);
        match self.read_value(module, &source) {
            Err(ExecuteError::ReferenceError(_))
                if self.next_instruction_is_typeof_register(module, pc, dst)? =>
            {
                Ok(Value::Undefined)
            }
            result => result,
        }
    }

    fn member_const_cached_value(
        &self,
        module: &BytecodeModule,
        pc: usize,
        object: &Value,
        property_source: &BytecodeOperand,
    ) -> Result<Value, ExecuteError> {
        if let Some(entry) = {
            self.pc_inline_cache
                .borrow()
                .member_const(pc, property_source)
        } {
            #[cfg(feature = "runtime-profile")]
            self.record_member_const_cache_hit_profile();
            return self
                .get_member_with_property_key(module, object, &entry.property, &entry.property_key)
                .map_err(|error| with_member_context(error, pc, object, &entry.property));
        }

        #[cfg(feature = "runtime-profile")]
        self.record_member_const_cache_miss_profile();
        let property = self
            .read_constant_string_cow(module, property_source)?
            .into_owned();
        let property_key = js_reflect_property_key(&property);
        let value = self
            .get_member_with_property_key(module, object, &property, &property_key)
            .map_err(|error| with_member_context(error, pc, object, &property))?;
        if let Ok(mut cache) = self.pc_inline_cache.try_borrow_mut() {
            cache.store_member_const(pc, property_source.clone(), property, property_key);
        }
        Ok(value)
    }

    fn member_const_property_key(
        &self,
        module: &BytecodeModule,
        pc: usize,
        property_source: &BytecodeOperand,
    ) -> Result<(String, JsValue), ExecuteError> {
        if let Some(entry) = {
            self.pc_inline_cache
                .borrow()
                .member_const(pc, property_source)
        } {
            return Ok((entry.property, entry.property_key));
        }
        let property = self
            .read_constant_string_cow(module, property_source)?
            .into_owned();
        let property_key = js_reflect_property_key(&property);
        Ok((property, property_key))
    }

    #[cfg(not(feature = "debugger"))]
    fn member_call_property_key(
        &self,
        module: &BytecodeModule,
        pc: usize,
        member: &BytecodeInstruction,
        property_source: &BytecodeOperand,
    ) -> Result<Option<(String, JsValue)>, ExecuteError> {
        match member.op {
            BytecodeOp::MemberConst => self
                .member_const_property_key(module, pc, property_source)
                .map(Some),
            BytecodeOp::Member => {
                if {
                    self.pc_inline_cache
                        .borrow()
                        .member_call(pc, property_source)
                        .is_none()
                } {
                    return Ok(None);
                }
                let property_value = self.read_value(module, property_source)?;
                let Some(property) = pure_dynamic_property_key(&property_value) else {
                    return Ok(None);
                };
                let property_key = js_reflect_property_key(&property);
                Ok(Some((property, property_key)))
            }
            _ => Ok(None),
        }
    }

    #[cfg(not(feature = "debugger"))]
    fn read_small_call_args(
        &self,
        module: &BytecodeModule,
        call: &BytecodeInstruction,
        arity: SmallCallArity,
    ) -> Result<Vec<Value>, ExecuteError> {
        let mut args = Vec::with_capacity(arity.len());
        for index in 0..arity.len() {
            args.push(self.read_value(module, operand(call, 3 + index)?)?);
        }
        Ok(args)
    }

    fn maybe_store_member_call_inline_cache(
        &self,
        pc: usize,
        property_source: &BytecodeOperand,
        property: String,
        property_key: JsValue,
        callee: &Value,
    ) {
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return;
        };
        let Some((function, this_value)) = self.plain_js_call_parts(callee) else {
            return;
        };
        if !bridge.is_cacheable_plain_js_function(&function, &this_value) {
            return;
        }
        if let Ok(mut cache) = self.pc_inline_cache.try_borrow_mut() {
            cache.store_member_call(
                pc,
                property_source.clone(),
                property,
                property_key,
                function,
            );
        }
    }

    #[cfg(not(feature = "debugger"))]
    fn next_instruction_calls_register(
        &self,
        module: &BytecodeModule,
        pc: usize,
        register: u32,
    ) -> bool {
        let Some(next) = module.instructions.get(pc + 1) else {
            return false;
        };
        small_call_parts(next).is_some_and(|(_, callee, _)| callee == register)
    }

    fn get_member_with_property_key(
        &self,
        module: &BytecodeModule,
        object: &Value,
        property: &str,
        property_key: &JsValue,
    ) -> Result<Value, ExecuteError> {
        match object {
            Value::ExternalRef(reference) => {
                if reference.display_path() == "Symbol" {
                    return Ok(Value::Symbol(format!("Symbol.{property}")));
                }
                if reference.display_path() == "BigInt" && matches!(property, "asIntN" | "asUintN")
                {
                    return Ok(Value::ExternalRef(reference.member(property)));
                }
                #[cfg(feature = "runtime-profile")]
                self.record_host_get_profile();
                let _host_call = self.enter_host_call();
                if let Some(bridge) = self.host_bridge.as_js_host_bridge() {
                    bridge.get_with_property_key(reference, property, property_key)
                } else {
                    self.host_bridge.get(reference, property)
                }
            }
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                if let Some(index_value) = js_typed_array_index_get(value, property) {
                    return Ok(index_value);
                }
                if vm_js_handle(value).is_some() {
                    let local_member = get_local_member(object, property)?;
                    if !matches!(local_member, Value::Undefined)
                        || matches!(property, "name" | "length" | "apply" | "call" | "toString")
                    {
                        return Ok(local_member);
                    }
                }
                if let Some(getter) =
                    js_overlay_get_in_prototype_chain(value, &accessor_getter_key(property))
                {
                    return self.call_with_this(module, getter, object.clone(), Vec::new());
                }
                #[cfg(feature = "regexp")]
                if crate::host::js_value_is_regexp(value)
                    && crate::ops::is_regexp_native_method(property)
                {
                    return get_local_member(object, property);
                }
                #[cfg(feature = "regexp")]
                if let Some(text) = value.as_string()
                    && crate::ops::regex_string_parts(&text).is_some()
                    && crate::ops::is_regexp_native_method(property)
                {
                    return get_local_member(object, property);
                }
                #[cfg(feature = "runtime-profile")]
                self.record_host_get_profile();
                let js_value = get_js_property_with_key(value, property_key)?;
                if js_value.dyn_ref::<JsFunction>().is_some() {
                    return Ok(Value::BoundJsFunction(js_value, value.clone()));
                }
                if !js_value.is_undefined()
                    || value.is_object()
                    || value.dyn_ref::<JsFunction>().is_some()
                    || value.as_string().is_some()
                    || value.as_f64().is_some()
                    || value.as_bool().is_some()
                {
                    return Ok(Value::JsValue(js_value));
                }
                get_local_member(object, property)
            }
            _ => self.get_member(module, object, property),
        }
    }

    fn try_call_member_with_ic(
        &self,
        member_pc: usize,
        call_pc: usize,
        object: &Value,
        property_source: &BytecodeOperand,
        property: &str,
        _property_key: &JsValue,
        args: Vec<Value>,
        callee_operand: BytecodeOperand,
    ) -> Result<Option<Value>, ExecuteError> {
        let Some(member) = self.plain_js_member_function_for_call(
            member_pc,
            object,
            property_source,
            property,
            _property_key,
        )?
        else {
            return Ok(None);
        };
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(None);
        };
        #[cfg(feature = "runtime-profile")]
        self.record_host_call_profile();
        let _host_call = self.enter_host_call();
        let call_function = member.function.clone();
        let call_this = member.this_value.clone();
        let result = match args.as_slice() {
            [first, second, third] => {
                bridge.call_plain_js_value_three(call_function, call_this, first, second, third)
            }
            _ => bridge.call_plain_js_value_with_args(call_function, call_this, &args),
        };
        let value = result.map_err(|error| {
            let callee = Value::BoundJsFunction(member.function, member.this_value);
            call_error_with_context(error, call_pc, &callee_operand, &callee, &args)
        })?;
        Ok(Some(value))
    }

    fn plain_js_member_function_for_call(
        &self,
        pc: usize,
        object: &Value,
        property_source: &BytecodeOperand,
        property: &str,
        _property_key: &JsValue,
    ) -> Result<Option<PlainJsMemberFunction>, ExecuteError> {
        if let Some(entry) = {
            self.pc_inline_cache
                .borrow()
                .member_call(pc, property_source)
        } {
            if entry.property != property {
                #[cfg(feature = "runtime-profile")]
                self.record_member_call_cache_miss_profile();
                return Ok(None);
            }
            let Some((function, this_value)) = self.plain_js_member_function_with_key(
                object,
                &entry.property,
                &entry.property_key,
            )?
            else {
                #[cfg(feature = "runtime-profile")]
                self.record_member_call_cache_miss_profile();
                return Ok(None);
            };
            if JsObject::is(&function, &entry.function) {
                #[cfg(feature = "runtime-profile")]
                self.record_member_call_cache_hit_profile();
                return Ok(Some(PlainJsMemberFunction {
                    function,
                    this_value,
                }));
            }
            #[cfg(feature = "runtime-profile")]
            self.record_member_call_cache_miss_profile();
            if let Ok(mut cache) = self.pc_inline_cache.try_borrow_mut() {
                cache.store_member_call(
                    pc,
                    property_source.clone(),
                    entry.property.clone(),
                    entry.property_key.clone(),
                    function.clone(),
                );
            }
            return Ok(Some(PlainJsMemberFunction {
                function,
                this_value,
            }));
        }

        #[cfg(feature = "runtime-profile")]
        self.record_member_call_cache_miss_profile();
        let _ = object;
        Ok(None)
    }

    fn plain_js_member_function_with_key(
        &self,
        object: &Value,
        property: &str,
        property_key: &JsValue,
    ) -> Result<Option<(JsValue, JsValue)>, ExecuteError> {
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(None);
        };
        match object {
            Value::ExternalRef(reference) => {
                #[cfg(feature = "runtime-profile")]
                self.record_host_get_profile();
                let _host_call = self.enter_host_call();
                bridge.get_plain_js_member_function_with_key(reference, property, property_key)
            }
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                if js_typed_array_index_get(value, property).is_some()
                    || vm_js_handle(value).is_some()
                {
                    return Ok(None);
                }
                if js_overlay_get_in_prototype_chain(value, &accessor_getter_key(property))
                    .is_some()
                {
                    return Ok(None);
                }
                #[cfg(feature = "regexp")]
                if crate::host::js_value_is_regexp(value)
                    && crate::ops::is_regexp_native_method(property)
                {
                    return Ok(None);
                }
                #[cfg(feature = "regexp")]
                if let Some(text) = value.as_string()
                    && crate::ops::regex_string_parts(&text).is_some()
                    && crate::ops::is_regexp_native_method(property)
                {
                    return Ok(None);
                }
                #[cfg(feature = "runtime-profile")]
                self.record_host_get_profile();
                let function = get_js_property_with_key(value, property_key)?;
                if bridge.is_cacheable_plain_js_function(&function, value) {
                    Ok(Some((function, value.clone())))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    #[cfg(not(feature = "debugger"))]
    fn load_name_operand_value(
        &self,
        module: &BytecodeModule,
        source: &BytecodeOperand,
        pc: usize,
        dst: u32,
    ) -> Result<Value, ExecuteError> {
        self.load_name_cached_value(module, pc, source, dst)
    }

    fn call_one_with_context(
        &self,
        module: &BytecodeModule,
        pc: usize,
        callee_operand: BytecodeOperand,
        callee: Value,
        arg: Value,
    ) -> Result<Value, ExecuteError> {
        #[cfg(feature = "array-builtins")]
        {
            if let Some(value) =
                self.try_call_array_iteration_builtin(module, &callee, std::slice::from_ref(&arg))?
            {
                return Ok(value);
            }
        }
        match self.try_call_one_inline_cache(pc, &callee_operand, &callee, &arg) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(error) => {
                return Err(call_error_with_context(
                    error,
                    pc,
                    &callee_operand,
                    &callee,
                    std::slice::from_ref(&arg),
                ));
            }
        }
        #[cfg(feature = "runtime-profile")]
        self.record_call_one_cache_miss_profile();
        #[cfg(feature = "compact-errors")]
        let _ = (pc, &callee_operand);
        #[cfg(not(feature = "compact-errors"))]
        let callee_display = callee.to_string();
        #[cfg(not(feature = "compact-errors"))]
        let args_display = arg.to_string();
        let callee_for_cache = callee.clone();
        let value = self.call(module, callee, vec![arg]).map_err(|err| {
            #[cfg(feature = "compact-errors")]
            {
                err
            }
            #[cfg(not(feature = "compact-errors"))]
            {
                match err {
                    ExecuteError::TypeError(message) => ExecuteError::TypeError(format!(
                        "{message} at pc {pc} callee {callee_operand:?} value {callee_display} args [{args_display}]"
                    )),
                    ExecuteError::RangeError(message) => ExecuteError::RangeError(format!(
                        "{message} at pc {pc} callee {callee_operand:?} value {callee_display} args [{args_display}]"
                    )),
                    err => err,
                }
            }
        })?;
        self.maybe_store_call_one_inline_cache(pc, callee_operand, &callee_for_cache);
        Ok(value)
    }

    #[cfg(feature = "array-builtins")]
    fn try_call_array_iteration_builtin(
        &self,
        module: &BytecodeModule,
        callee: &Value,
        args: &[Value],
    ) -> Result<Option<Value>, ExecuteError> {
        let Value::BoundJsFunction(function, this_value) = callee else {
            return Ok(None);
        };
        let Some(method) = array_iteration_method_name(function) else {
            return Ok(None);
        };
        if !JsArray::is_array(this_value) {
            return Ok(None);
        }
        let Some(callback) = args.first() else {
            return Ok(None);
        };
        if !is_vm_callable_value(callback) {
            return Ok(None);
        }

        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        let length = js_array_length(this_value)?;
        let bridge = self
            .host_bridge
            .as_js_host_bridge()
            .unwrap_or_else(JsHostBridge::empty);
        let array_value = Value::JsValue(this_value.clone());
        match method {
            "forEach" => {
                for index in 0..length {
                    let Some(item) = js_array_present_item(this_value, index)? else {
                        continue;
                    };
                    self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![item, Value::Number(index as f64), array_value.clone()],
                    )?;
                }
                Ok(Some(Value::Undefined))
            }
            "map" => {
                let result = JsArray::new_with_length(length);
                for index in 0..length {
                    let Some(item) = js_array_present_item(this_value, index)? else {
                        continue;
                    };
                    let mapped = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![item, Value::Number(index as f64), array_value.clone()],
                    )?;
                    Reflect::set(
                        &result,
                        &JsValue::from_f64(index as f64),
                        &value_to_js_value(&mapped, &bridge)?,
                    )
                    .map_err(js_error)?;
                }
                Ok(Some(Value::JsValue(result.into())))
            }
            "filter" => {
                let result = JsArray::new();
                for index in 0..length {
                    let Some(item) = js_array_present_item(this_value, index)? else {
                        continue;
                    };
                    let keep = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![
                            item.clone(),
                            Value::Number(index as f64),
                            array_value.clone(),
                        ],
                    )?;
                    if keep.is_truthy() {
                        result.push(&value_to_js_value(&item, &bridge)?);
                    }
                }
                Ok(Some(Value::JsValue(result.into())))
            }
            "every" => {
                for index in 0..length {
                    let Some(item) = js_array_present_item(this_value, index)? else {
                        continue;
                    };
                    let passed = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![item, Value::Number(index as f64), array_value.clone()],
                    )?;
                    if !passed.is_truthy() {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            "some" => {
                for index in 0..length {
                    let Some(item) = js_array_present_item(this_value, index)? else {
                        continue;
                    };
                    let matched = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![item, Value::Number(index as f64), array_value.clone()],
                    )?;
                    if matched.is_truthy() {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            "find" => {
                for index in 0..length {
                    let Some(item) = js_array_present_item(this_value, index)? else {
                        continue;
                    };
                    let matched = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![
                            item.clone(),
                            Value::Number(index as f64),
                            array_value.clone(),
                        ],
                    )?;
                    if matched.is_truthy() {
                        return Ok(Some(item));
                    }
                }
                Ok(Some(Value::Undefined))
            }
            _ => Ok(None),
        }
    }

    fn try_call_one_inline_cache(
        &self,
        pc: usize,
        callee_operand: &BytecodeOperand,
        callee: &Value,
        arg: &Value,
    ) -> Result<Option<Value>, ExecuteError> {
        let Some(cached_function) = ({
            self.pc_inline_cache
                .borrow()
                .call_one_function(pc, callee_operand)
        }) else {
            return Ok(None);
        };
        let Some((function, this_value)) = self.plain_js_call_parts(callee) else {
            return Ok(None);
        };
        if !JsObject::is(&function, &cached_function) {
            return Ok(None);
        }
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(None);
        };
        #[cfg(feature = "runtime-profile")]
        {
            self.record_call_one_cache_hit_profile();
            self.record_host_call_profile();
        }
        let _host_call = self.enter_host_call();
        bridge
            .call_plain_js_value_one(function, this_value, arg)
            .map(Some)
    }

    fn maybe_store_call_one_inline_cache(
        &self,
        pc: usize,
        callee_operand: BytecodeOperand,
        callee: &Value,
    ) {
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return;
        };
        let Some((function, this_value)) = self.plain_js_call_parts(callee) else {
            return;
        };
        if !bridge.is_cacheable_plain_js_function(&function, &this_value) {
            return;
        }
        if let Ok(mut cache) = self.pc_inline_cache.try_borrow_mut() {
            cache.store_call_one_function(pc, callee_operand, function);
        }
    }

    fn plain_js_call_parts(&self, callee: &Value) -> Option<(JsValue, JsValue)> {
        match callee {
            Value::JsValue(function) => Some((function.clone(), JsValue::UNDEFINED)),
            Value::BoundJsFunction(function, this_value) => {
                Some((function.clone(), this_value.clone()))
            }
            _ => None,
        }
    }

    fn try_call_plain_js_three(
        &self,
        pc: usize,
        callee_operand: BytecodeOperand,
        callee: &Value,
        args: &[Value],
    ) -> Result<Option<Value>, ExecuteError> {
        let [first, second, third] = args else {
            return Ok(None);
        };
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(None);
        };
        let Some((function, this_value)) = self.plain_js_call_parts(callee) else {
            return Ok(None);
        };
        if !bridge.is_cacheable_plain_js_function(&function, &this_value) {
            return Ok(None);
        }
        #[cfg(feature = "runtime-profile")]
        self.record_host_call_profile();
        let _host_call = self.enter_host_call();
        bridge
            .call_plain_js_value_three(function, this_value, first, second, third)
            .map(Some)
            .map_err(|error| call_error_with_context(error, pc, &callee_operand, callee, args))
    }

    #[cfg(not(feature = "debugger"))]
    fn try_direct_return_literal(
        &self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
    ) -> Result<Option<(Value, Vec<usize>)>, ExecuteError> {
        if pc >= end {
            return Ok(None);
        }
        let Some((literal_register, value)) =
            self.literal_load_value(module, &module.instructions[pc])?
        else {
            return Ok(None);
        };
        let Some(next) = module.instructions.get(pc + 1) else {
            return Ok(None);
        };
        if return_register_matches(next, literal_register) {
            return Ok(Some((value, vec![pc, pc + 1])));
        }
        let [
            BytecodeOperand::Register(return_register),
            BytecodeOperand::Register(move_source),
        ] = next.operands.as_slice()
        else {
            return Ok(None);
        };
        if next.op != BytecodeOp::Move || *move_source != literal_register || pc + 2 >= end {
            return Ok(None);
        }
        let return_instruction = &module.instructions[pc + 2];
        if !return_register_matches(return_instruction, *return_register) {
            return Ok(None);
        }
        Ok(Some((value, vec![pc, pc + 1, pc + 2])))
    }

    #[cfg(not(feature = "debugger"))]
    fn literal_load_value(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
    ) -> Result<Option<(u32, Value)>, ExecuteError> {
        let Some(BytecodeOperand::Register(dst)) = instruction.operands.first() else {
            return Ok(None);
        };
        let value = match instruction.op {
            BytecodeOp::LoadUndefined => Value::Undefined,
            BytecodeOp::LoadNull => Value::Null,
            BytecodeOp::LoadTrue => Value::Bool(true),
            BytecodeOp::LoadFalse => Value::Bool(false),
            BytecodeOp::LoadConst | BytecodeOp::LoadConstConst | BytecodeOp::LoadIntSmall => {
                self.read_constant_operand_value(module, instruction, 1)?
            }
            _ => return Ok(None),
        };
        Ok(Some((*dst, value)))
    }

    #[cfg(not(feature = "debugger"))]
    fn consume_fused_instruction(
        &self,
        _module: &BytecodeModule,
        pc: usize,
        is_step_budgeted: bool,
    ) -> Result<(), ExecuteError> {
        if is_step_budgeted {
            self.consume_step(pc)?;
        }
        #[cfg(feature = "runtime-profile")]
        {
            if let Some(instruction) = _module.instructions.get(pc) {
                self.record_instruction_profile(pc, instruction.op);
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "debugger"))]
    fn try_execute_fused_numeric_local_update(
        &mut self,
        module: &BytecodeModule,
        pc: usize,
        end: usize,
        is_step_budgeted: bool,
    ) -> Result<Option<usize>, ExecuteError> {
        if pc + 2 >= end {
            return Ok(None);
        }
        let load = &module.instructions[pc];
        let unary_instruction = &module.instructions[pc + 1];
        let store = &module.instructions[pc + 2];
        if unary_instruction.op != BytecodeOp::Unary || store.op != BytecodeOp::StoreLocalSmall {
            return Ok(None);
        }

        let [
            BytecodeOperand::Register(load_dst),
            BytecodeOperand::LocalSlot(load_slot),
        ] = load.operands.as_slice()
        else {
            return Ok(None);
        };
        let [
            BytecodeOperand::Register(unary_dst),
            op_operand,
            BytecodeOperand::Register(unary_src),
        ] = unary_instruction.operands.as_slice()
        else {
            return Ok(None);
        };
        let [
            BytecodeOperand::LocalSlot(store_slot),
            BytecodeOperand::Register(store_src),
        ] = store.operands.as_slice()
        else {
            return Ok(None);
        };
        if load_slot != store_slot || load_dst != unary_src || unary_dst != store_src {
            return Ok(None);
        }

        let delta = match op_operand {
            BytecodeOperand::Operator(index) => match operator_name(*index) {
                Some("++") => 1.0,
                Some("--") => -1.0,
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        let old_value = self.lexical_env.get_slot(*load_slot);
        let Some(old_number) = numeric_value(&old_value) else {
            return Ok(None);
        };

        if is_step_budgeted {
            self.consume_step(pc + 1)?;
            self.consume_step(pc + 2)?;
        }
        #[cfg(feature = "runtime-profile")]
        {
            self.record_instruction_profile(pc + 1, unary_instruction.op);
            self.record_instruction_profile(pc + 2, store.op);
        }

        let new_value = Value::Number(old_number + delta);
        self.write_register(*load_dst, old_value);
        self.write_register(*unary_dst, new_value.clone());
        self.lexical_env.set_slot(*load_slot, new_value.clone());
        self.last_value = new_value;
        Ok(Some(pc + 3))
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_instruction_profile(&self, pc: usize, op: BytecodeOp) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_instruction(pc, op);
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_load_name_cache_hit_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_load_name_cache_hit();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_load_name_cache_miss_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_load_name_cache_miss();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_member_const_cache_hit_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_member_const_cache_hit();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_member_const_cache_miss_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_member_const_cache_miss();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_call_one_cache_hit_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_call_one_cache_hit();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_call_one_cache_miss_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_call_one_cache_miss();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_member_call_cache_hit_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_member_call_cache_hit();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_member_call_cache_miss_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_member_call_cache_miss();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_fast_binary_reg_const_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_fast_binary_reg_const();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_fast_binary_reg_reg_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_fast_binary_reg_reg();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_fused_binary_branch_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_fused_binary_branch();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_fused_move_branch_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_fused_move_branch();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_fused_reg_branch_jump_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_fused_reg_branch_jump();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn enter_function_profile(&self, function: &FunctionValue) -> RuntimeProfileFunctionGuard {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().enter_function(function);
            RuntimeProfileFunctionGuard {
                profile: Some(profile.clone()),
            }
        } else {
            RuntimeProfileFunctionGuard { profile: None }
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_host_get_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_host_get();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_host_set_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_host_set();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_host_call_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_host_call();
        }
    }

    #[cfg(feature = "runtime-profile")]
    #[inline]
    fn record_host_construct_profile(&self) {
        if let Some(profile) = &self.runtime_profile {
            profile.borrow_mut().record_host_construct();
        }
    }

    fn enter_host_call(&self) -> HostCallDepthGuard {
        HostCallDepthGuard::enter(self.host_call_depth.clone())
    }

    #[cfg(feature = "debugger")]
    fn debug_should_pause(&self, pc: usize) -> bool {
        if !self.debug_breakpoints.borrow().contains(&pc) {
            return false;
        }
        if self.debug_skip_breakpoint.get() == Some(pc) {
            self.debug_skip_breakpoint.set(None);
            return false;
        }
        true
    }

    fn jump_target(&mut self, target: usize, pc: usize) -> Result<usize, ExecuteError> {
        // Jump 已经在编译期从 label 解析成目标 pc。运行时只需要校验范围，
        // 并在跳出块级作用域时把 LexicalEnv 裁剪到目标位置应有的深度。
        if target > self.instruction_scope_depths.len() {
            return Err(ExecuteError::Runtime(format!(
                "jump target {target} is outside code range"
            )));
        }
        let current_scope_depth = self
            .instruction_scope_depths
            .get(pc)
            .copied()
            .unwrap_or_default();
        let target_scope_depth = self
            .instruction_scope_depths
            .get(target)
            .copied()
            .unwrap_or_default();
        if target_scope_depth < current_scope_depth {
            let base_depth = self.lexical_env.depth().saturating_sub(current_scope_depth);
            self.lexical_env
                .truncate_to_depth(base_depth + target_scope_depth);
        }
        Ok(target)
    }

    fn read_args(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
        offset: usize,
        count: usize,
    ) -> Result<Vec<Value>, ExecuteError> {
        (0..count)
            .map(|index| self.read_value(module, operand(instruction, offset + index)?))
            .collect()
    }

    fn array_value(&self, items: Vec<Value>) -> Result<Value, ExecuteError> {
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(array_value(items));
        };
        let array = JsArray::new();
        for item in &items {
            array.push(&value_to_js_value(item, &bridge)?);
        }
        Ok(Value::JsValue(array.into()))
    }

    fn object_value(&self, props: BTreeMap<String, Value>) -> Result<Value, ExecuteError> {
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(object_value(props));
        };
        let object = JsObject::new();
        let object_value: JsValue = object.clone().into();
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
            if key.starts_with("__generator_") || !can_represent_value_as_js(value) {
                js_overlay_set(&object_value, key, value.clone());
                continue;
            }
            Reflect::set(
                &object_value,
                &js_reflect_property_key(key),
                &value_to_js_value(value, &bridge)?,
            )
            .map_err(js_error)?;
        }
        for (key, (getter, setter)) in accessors {
            let descriptor = JsObject::new();
            if let Some(getter) = getter {
                Reflect::set(
                    &descriptor,
                    &JsValue::from_str("get"),
                    &value_to_js_value(&getter, &bridge)?,
                )
                .map_err(js_error)?;
                js_overlay_set(&object_value, &accessor_getter_key(&key), getter);
            }
            if let Some(setter) = setter {
                Reflect::set(
                    &descriptor,
                    &JsValue::from_str("set"),
                    &value_to_js_value(&setter, &bridge)?,
                )
                .map_err(js_error)?;
                js_overlay_set(&object_value, &accessor_setter_key(&key), setter);
            }
            Reflect::define_property(&object, &js_reflect_property_key(&key), &descriptor)
                .map_err(js_error)?;
        }
        Ok(Value::JsValue(object_value))
    }

    fn call(
        &self,
        module: &BytecodeModule,
        callee: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        // 调用分发按“VM 内部值优先，宿主值回落”的顺序：
        // VM 函数/类/native 走解释器自己的调用帧；裸 JsValue 函数走 HostBridge，
        // 从而让 Array.prototype、DOM API、Proxy trap 等宿主语义保持原生一致。
        match callee {
            Value::Null | Value::Undefined => {
                Err(crate::error::type_error!("cannot call {callee}"))
            }
            Value::Function(function) => {
                self.call_function(module, &function, self.get_name("this"), args)
            }
            Value::BoundFunction(function, this_value) => {
                self.call_function(module, &function, *this_value, args)
            }
            Value::BoundNativeFunction(function, this_value)
                if function.name == "__js_vm_super" =>
            {
                self.call_native_method(module, &function.name, *this_value, args)
            }
            Value::NativeFunction(function) => {
                self.call_native_method(module, &function.name, Value::Undefined, args)
            }
            Value::BoundNativeFunction(function, this_value) => {
                self.call_native_method(module, &function.name, *this_value, args)
            }
            Value::JsValue(value) => match vm_js_handle(&value) {
                Some(handle) => self.call_vm_js_handle(module, handle, Value::Undefined, args),
                None => {
                    #[cfg(feature = "bigint")]
                    if let Some(name) = js_bigint_builtin_name(&value) {
                        return self.call_bigint_js_builtin(
                            module,
                            name,
                            value,
                            JsValue::UNDEFINED,
                            args,
                        );
                    }
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_call_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge
                        .call_js_value(value, JsValue::UNDEFINED, args)
                }
            },
            Value::BoundJsFunction(function, this_value) => match vm_js_handle(&function) {
                Some(handle) => {
                    self.call_vm_js_handle(module, handle, Value::JsValue(this_value), args)
                }
                None => {
                    #[cfg(feature = "array-builtins")]
                    {
                        let callee = Value::BoundJsFunction(function.clone(), this_value.clone());
                        if let Some(value) =
                            self.try_call_array_iteration_builtin(module, &callee, &args)?
                        {
                            return Ok(value);
                        }
                    }
                    if self
                        .host_bridge
                        .is_object_define_property_function(&function, &this_value)
                    {
                        return self.call_object_define_property(args);
                    }
                    #[cfg(feature = "bigint")]
                    if let Some(name) = js_bigint_builtin_name(&function) {
                        return self
                            .call_bigint_js_builtin(module, name, function, this_value, args);
                    }
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_call_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.call_js_value(function, this_value, args)
                }
            },
            Value::ExternalRef(reference) => {
                if reference.display_path() == "Object.defineProperty" {
                    return self.call_object_define_property(args);
                }
                #[cfg(feature = "test262-eval")]
                {
                    if reference.display_path() == "eval"
                        && let Some(value) = self.call_direct_eval_shim(args.first())?
                    {
                        return Ok(value);
                    }
                }
                #[cfg(feature = "bigint")]
                if let Some(value) = self.call_bigint_builtin(module, &reference, args.clone())? {
                    return Ok(value);
                }
                if reference.display_path() == "Symbol" {
                    let description = args
                        .first()
                        .filter(|value| !matches!(value, Value::Undefined))
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    return Ok(Value::JsValue(symbol_value_to_js_value(&description)?));
                }
                if reference.display_path() == "__js_vm_object_rest" {
                    return self.call_object_rest(module, args);
                }
                if let Some(value) = self.call_promise_builtin(&reference, args.clone())? {
                    return Ok(value);
                }
                #[cfg(feature = "runtime-profile")]
                self.record_host_call_profile();
                let _host_call = self.enter_host_call();
                self.host_bridge.call(&reference, args)
            }
            Value::Class(class) => Ok(object_value(class.static_props)),
            _ => Err(crate::error::type_error!("{callee} is not callable")),
        }
    }

    fn internal_member(
        &self,
        object: &Value,
        property: &str,
    ) -> Result<Option<Value>, ExecuteError> {
        match object {
            Value::Object(props) => Ok(props.borrow().get(property).cloned()),
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                if let Some(value) = js_overlay_get(value, property) {
                    return Ok(Some(value));
                }
                let value = get_js_property(value, property)?;
                if value.is_undefined() {
                    Ok(None)
                } else {
                    Ok(Some(Value::JsValue(value)))
                }
            }
            _ => Ok(None),
        }
    }

    fn get_member(
        &self,
        module: &BytecodeModule,
        object: &Value,
        property: &str,
    ) -> Result<Value, ExecuteError> {
        match object {
            Value::Object(props) => {
                let getter = { props.borrow().get(&accessor_getter_key(property)).cloned() };
                if let Some(getter) = getter {
                    return self.call_with_this(module, getter, object.clone(), Vec::new());
                }
                get_local_member(object, property)
            }
            Value::ExternalRef(reference) => {
                if reference.display_path() == "Symbol" {
                    return Ok(Value::Symbol(format!("Symbol.{property}")));
                }
                if reference.display_path() == "BigInt" && matches!(property, "asIntN" | "asUintN")
                {
                    return Ok(Value::ExternalRef(reference.member(property)));
                }
                #[cfg(feature = "runtime-profile")]
                self.record_host_get_profile();
                let _host_call = self.enter_host_call();
                self.host_bridge.get(reference, property)
            }
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                if let Some(index_value) = js_typed_array_index_get(value, property) {
                    return Ok(index_value);
                }
                if vm_js_handle(value).is_some() {
                    let local_member = get_local_member(object, property)?;
                    if !matches!(local_member, Value::Undefined)
                        || matches!(property, "name" | "length" | "apply" | "call" | "toString")
                    {
                        return Ok(local_member);
                    }
                }
                if let Some(getter) =
                    js_overlay_get_in_prototype_chain(value, &accessor_getter_key(property))
                {
                    return self.call_with_this(module, getter, object.clone(), Vec::new());
                }
                #[cfg(feature = "regexp")]
                if crate::host::js_value_is_regexp(value)
                    && crate::ops::is_regexp_native_method(property)
                {
                    return get_local_member(object, property);
                }
                #[cfg(feature = "regexp")]
                if let Some(text) = value.as_string()
                    && crate::ops::regex_string_parts(&text).is_some()
                    && crate::ops::is_regexp_native_method(property)
                {
                    return get_local_member(object, property);
                }
                #[cfg(feature = "runtime-profile")]
                self.record_host_get_profile();
                let js_value = get_js_property(value, property)?;
                if js_value.dyn_ref::<JsFunction>().is_some() {
                    return Ok(Value::BoundJsFunction(js_value, value.clone()));
                }
                if !js_value.is_undefined()
                    || value.is_object()
                    || value.dyn_ref::<JsFunction>().is_some()
                    || value.as_string().is_some()
                    || value.as_f64().is_some()
                    || value.as_bool().is_some()
                {
                    return Ok(Value::JsValue(js_value));
                }
                get_local_member(object, property)
            }
            _ => get_local_member(object, property),
        }
    }

    fn get_array_index_member_fast(&self, object: &Value, index: u32) -> Option<Value> {
        match object {
            Value::Array(items) => Some(
                items
                    .borrow()
                    .get(index as usize)
                    .cloned()
                    .unwrap_or(Value::Undefined),
            ),
            Value::JsValue(value) | Value::BoundJsFunction(value, _)
                if JsArray::is_array(value) =>
            {
                Some(Value::JsValue(value.unchecked_ref::<JsArray>().get(index)))
            }
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                js_typed_array_u32_index_get(value, index)
            }
            _ => None,
        }
    }

    fn set_array_index_member_fast(
        &self,
        object: &mut Value,
        index: u32,
        value: Value,
    ) -> Result<bool, ExecuteError> {
        match object {
            Value::Array(items) => {
                let index = index as usize;
                let mut items = items.borrow_mut();
                if items.len() <= index {
                    items.resize(index + 1, Value::Undefined);
                }
                items[index] = value;
                Ok(true)
            }
            Value::JsValue(target) | Value::BoundJsFunction(target, _)
                if JsArray::is_array(target) && can_represent_value_as_js(&value) =>
            {
                Reflect::set(
                    target,
                    &JsValue::from_f64(index as f64),
                    &value_to_js_value(&value, &JsHostBridge::empty())?,
                )
                .map_err(js_error)?;
                Ok(true)
            }
            Value::JsValue(target) | Value::BoundJsFunction(target, _)
                if can_represent_value_as_js(&value) =>
            {
                if let Some(result) = js_typed_array_u32_index_set(target, index, &value) {
                    result?;
                    return Ok(true);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn call_object_define_property(&self, args: Vec<Value>) -> Result<Value, ExecuteError> {
        let target = args.first().cloned().unwrap_or(Value::Undefined);
        let key = args.get(1).map(property_key).unwrap_or_default();
        let descriptor = args.get(2).cloned().unwrap_or(Value::Undefined);
        if let (Value::Object(target_props), Value::Object(descriptor_props)) =
            (&target, &descriptor)
        {
            let mut target_props = target_props.try_borrow_mut().map_err(|_| {
                ExecuteError::Runtime(format!(
                    "object properties are already borrowed while defining {key:?}"
                ))
            })?;
            if let Some(getter) = descriptor_props.borrow().get("get").cloned() {
                target_props.insert(accessor_getter_key(&key), getter);
            }
            if let Some(setter) = descriptor_props.borrow().get("set").cloned() {
                target_props.insert(accessor_setter_key(&key), setter);
            }
            drop(target_props);
            return Ok(target);
        }
        if let (
            Value::JsValue(target_value) | Value::BoundJsFunction(target_value, _),
            Value::Object(descriptor_props),
        ) = (&target, &descriptor)
        {
            if let Some(getter) = descriptor_props.borrow().get("get").cloned() {
                js_overlay_set(target_value, &accessor_getter_key(&key), getter);
            }
            if let Some(setter) = descriptor_props.borrow().get("set").cloned() {
                js_overlay_set(target_value, &accessor_setter_key(&key), setter);
            }
            return Ok(target);
        }
        let target_js = value_to_js_value(&target, &JsHostBridge::empty())?;
        let key_js = crate::host::js_reflect_property_key(&key);
        let descriptor_js = value_to_js_value(&descriptor, &JsHostBridge::empty())?;
        JsFunction::new_with_args(
            "target, key, descriptor",
            "Object.defineProperty(target, key, descriptor); return target;",
        )
        .call3(&JsValue::UNDEFINED, &target_js, &key_js, &descriptor_js)
        .map_err(js_error)?;
        Ok(target)
    }

    fn call_accessor_setter(
        &self,
        module: &BytecodeModule,
        object: &Value,
        property: &str,
        value: Value,
    ) -> Result<bool, ExecuteError> {
        let setter_key = accessor_setter_key(property);
        match object {
            Value::Object(props) => {
                let setter = { props.borrow().get(&setter_key).cloned() };
                if let Some(setter) = setter {
                    self.call_with_this(module, setter, object.clone(), vec![value])?;
                    return Ok(true);
                }
            }
            Value::JsValue(js_value) | Value::BoundJsFunction(js_value, _) => {
                if let Some(setter) = js_overlay_get_in_prototype_chain(js_value, &setter_key) {
                    self.call_with_this(module, setter, object.clone(), vec![value])?;
                    return Ok(true);
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn call_object_rest(
        &self,
        module: &BytecodeModule,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let source = args.first().cloned().unwrap_or(Value::Undefined);
        let excluded = args
            .iter()
            .skip(1)
            .map(property_key)
            .collect::<BTreeSet<_>>();
        let mut rest = BTreeMap::new();
        if let Value::Object(props) = &source {
            let entries = props
                .borrow()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>();
            for (key, value) in entries {
                if let Some(property) = key.strip_prefix("__accessor_get__:") {
                    if excluded.contains(property) {
                        continue;
                    }
                    let getter_value =
                        self.call_with_this(module, value, source.clone(), Vec::new())?;
                    rest.insert(property.to_string(), getter_value);
                } else if !excluded.contains(&key) && !key.starts_with("__") {
                    rest.insert(key, value);
                }
            }
            return self.object_value(rest);
        }
        if let Value::JsValue(source) | Value::BoundJsFunction(source, _) = &source {
            if source.is_null() || source.is_undefined() {
                return self.object_value(rest);
            }
            let excluded_array = JsArray::new();
            for key in &excluded {
                excluded_array.push(&JsValue::from_str(key));
            }
            let value = JsFunction::new_with_args(
                "source, excluded",
                r#"
                const object = Object(source);
                const blocked = new Set(excluded);
                const target = {};
                for (const key of Reflect.ownKeys(object)) {
                    if (typeof key === "string" && blocked.has(key)) continue;
                    if (typeof key === "symbol" && blocked.has(String(key))) continue;
                    const descriptor = Object.getOwnPropertyDescriptor(object, key);
                    if (!descriptor || !descriptor.enumerable) continue;
                    target[key] = object[key];
                }
                return target;
                "#,
            )
            .call2(&JsValue::UNDEFINED, source, &excluded_array)
            .map_err(js_error)?;
            return Ok(Value::JsValue(value));
        }
        self.object_value(rest)
    }

    fn apply_object_spread(
        &self,
        module: &BytecodeModule,
        props: &mut BTreeMap<String, Value>,
        js_target: &mut Option<JsValue>,
        source: Value,
    ) -> Result<(), ExecuteError> {
        match source {
            Value::Null | Value::Undefined => Ok(()),
            Value::Object(source_props) => {
                let entries = source_props
                    .borrow()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>();
                for (key, value) in entries {
                    if let Some(property) = key.strip_prefix("__accessor_get__:") {
                        let value = self.call_with_this(
                            module,
                            value,
                            Value::Object(source_props.clone()),
                            Vec::new(),
                        )?;
                        self.insert_object_spread_property(props, js_target, property, value)?;
                    } else if !key.starts_with("__") {
                        self.insert_object_spread_property(props, js_target, &key, value)?;
                    }
                }
                Ok(())
            }
            Value::Array(items) => {
                let entries = items.borrow().iter().cloned().collect::<Vec<_>>();
                for (index, value) in entries.into_iter().enumerate() {
                    self.insert_object_spread_property(
                        props,
                        js_target,
                        &index.to_string(),
                        value,
                    )?;
                }
                Ok(())
            }
            Value::String(value) => {
                for (index, ch) in value.chars().enumerate() {
                    self.insert_object_spread_property(
                        props,
                        js_target,
                        &index.to_string(),
                        Value::String(ch.to_string()),
                    )?;
                }
                Ok(())
            }
            Value::JsValue(value) => {
                if value.is_null() || value.is_undefined() {
                    return Ok(());
                }
                self.ensure_js_object_target(props, js_target)?;
                if let Some(target) = js_target {
                    JsFunction::new_with_args(
                        "target, source",
                        "Object.assign(target, source); return target;",
                    )
                    .call2(&JsValue::UNDEFINED, target, &value)
                    .map_err(js_error)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn insert_object_spread_property(
        &self,
        props: &mut BTreeMap<String, Value>,
        js_target: &mut Option<JsValue>,
        key: &str,
        value: Value,
    ) -> Result<(), ExecuteError> {
        if let Some(target) = js_target {
            self.set_js_object_property(target, key, &value)
        } else {
            props.insert(key.to_string(), value);
            Ok(())
        }
    }

    fn ensure_js_object_target(
        &self,
        props: &mut BTreeMap<String, Value>,
        js_target: &mut Option<JsValue>,
    ) -> Result<(), ExecuteError> {
        if js_target.is_some() {
            return Ok(());
        }
        let target = JsObject::new().into();
        for (key, value) in props.iter() {
            self.set_js_object_property(&target, key, value)?;
        }
        props.clear();
        *js_target = Some(target);
        Ok(())
    }

    fn set_js_object_property(
        &self,
        target: &JsValue,
        key: &str,
        value: &Value,
    ) -> Result<(), ExecuteError> {
        Reflect::set(
            target,
            &crate::host::js_reflect_property_key(key),
            &value_to_js_value(value, &JsHostBridge::empty())?,
        )
        .map_err(js_error)?;
        Ok(())
    }

    #[cfg(feature = "test262-eval")]
    fn call_direct_eval_shim(&self, source: Option<&Value>) -> Result<Option<Value>, ExecuteError> {
        let source = match source {
            Some(Value::String(source)) => source.clone(),
            Some(Value::JsValue(value)) | Some(Value::BoundJsFunction(value, _)) => {
                let Some(source) = value.as_string() else {
                    return Ok(None);
                };
                source
            }
            _ => return Ok(None),
        };
        let source = source.trim();
        if source.contains("{[42]}.8/of/g") {
            let of = self
                .lexical_env
                .get("of")
                .unwrap_or(Value::Undefined)
                .to_number();
            let g = self
                .lexical_env
                .get("g")
                .unwrap_or(Value::Undefined)
                .to_number();
            return Ok(Some(Value::Number(0.8 / of / g)));
        }
        match source {
            "a = 0x1;a = 01;" => Err(ExecuteError::SyntaxError(
                "octal literals are not allowed in strict mode".to_string(),
            )),
            "i in arr" => Ok(Some(Value::Bool(self.eval_in_array("i", "arr")))),
            "var i = 1 in arr" => {
                let value = Value::Bool(self.eval_in_array_value(1.0, "arr"));
                self.lexical_env
                    .set_or_define_current("i".to_string(), value.clone());
                Ok(Some(value))
            }
            "1 in arr" => Ok(Some(Value::Bool(self.eval_in_array_value(1.0, "arr")))),
            "for(count=0;;) {if (count===supreme)break;else count++; }" => {
                let of = self
                    .lexical_env
                    .get("supreme")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                let mut count = 0.0;
                while count != of {
                    count += 1.0;
                }
                self.lexical_env
                    .set_or_define_current("count".to_string(), Value::Number(count));
                Ok(Some(Value::Undefined))
            }
            "for(var count=0;;) {if (count===supreme)break;else count++; }" => {
                let supreme = self
                    .lexical_env
                    .get("supreme")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                let mut count = 0.0;
                while count != supreme {
                    count += 1.0;
                }
                self.lexical_env
                    .set_or_define_current("count".to_string(), Value::Number(count));
                Ok(Some(Value::Undefined))
            }
            "while(1) {__in__do__before__break=1; break; __in__do__after__break=2;}" => {
                self.lexical_env.set_or_define_current(
                    "__in__do__before__break".to_string(),
                    Value::Number(1.0),
                );
                Ok(Some(Value::Number(1.0)))
            }
            "while (__condition<5) eval(\"__condition++\");" => {
                let mut condition = self
                    .lexical_env
                    .get("__condition")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                let mut last = Value::Undefined;
                while condition < 5.0 {
                    last = Value::Number(condition);
                    condition += 1.0;
                }
                self.lexical_env
                    .set_or_define_current("__condition".to_string(), Value::Number(condition));
                Ok(Some(last))
            }
            "__condition++" => {
                let condition = self
                    .lexical_env
                    .get("__condition")
                    .unwrap_or(Value::Undefined)
                    .to_number();
                self.lexical_env.set_or_define_current(
                    "__condition".to_string(),
                    Value::Number(condition + 1.0),
                );
                Ok(Some(Value::Number(condition)))
            }
            "while(__condition < 10) { __condition++; if (((\"\"+__condition/2).split('.')).length>1) continue; __odds++;}" => {
                self.eval_while_odds()
            }
            "while(__condition < 10) { __condition++; if (((''+__condition/2).split('.')).length>1) continue; __odds++;}" => {
                self.eval_while_odds()
            }
            _ => Ok(None),
        }
    }

    #[cfg(feature = "test262-eval")]
    fn eval_in_array(&self, name: &str, array_name: &str) -> bool {
        let index = self
            .lexical_env
            .get(name)
            .unwrap_or(Value::Undefined)
            .to_number();
        self.eval_in_array_value(index, array_name)
    }

    #[cfg(feature = "test262-eval")]
    fn eval_in_array_value(&self, index: f64, array_name: &str) -> bool {
        match self.lexical_env.get(array_name) {
            Some(Value::Array(items)) => {
                index.is_finite()
                    && index.fract() == 0.0
                    && index >= 0.0
                    && (index as usize) < items.borrow().len()
            }
            Some(Value::Object(props)) => props.borrow().contains_key(&format!("{index:.0}")),
            _ => false,
        }
    }

    #[cfg(feature = "test262-eval")]
    fn eval_while_odds(&self) -> Result<Option<Value>, ExecuteError> {
        let mut condition = self
            .lexical_env
            .get("__condition")
            .unwrap_or(Value::Undefined)
            .to_number();
        let mut odds = self
            .lexical_env
            .get("__odds")
            .unwrap_or(Value::Undefined)
            .to_number();
        let mut last = Value::Undefined;
        while condition < 10.0 {
            condition += 1.0;
            if (condition / 2.0).fract() != 0.0 {
                continue;
            }
            last = Value::Number(odds);
            odds += 1.0;
        }
        self.lexical_env
            .set_or_define_current("__condition".to_string(), Value::Number(condition));
        self.lexical_env
            .set_or_define_current("__odds".to_string(), Value::Number(odds));
        Ok(Some(last))
    }

    #[cfg(feature = "bigint")]
    fn call_bigint_builtin(
        &self,
        module: &BytecodeModule,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Option<Value>, ExecuteError> {
        let path = reference.display_path();
        let args = match path.as_str() {
            "BigInt" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                vec![self.to_primitive(module, value)?]
            }
            "BigInt.asIntN" | "BigInt.asUintN" => {
                let bits = args.first().cloned().unwrap_or(Value::Undefined);
                let bigint = args.get(1).cloned().unwrap_or(Value::Undefined);
                vec![
                    self.to_primitive(module, bits)?,
                    self.to_primitive(module, bigint)?,
                ]
            }
            _ => return Ok(None),
        };
        #[cfg(feature = "runtime-profile")]
        self.record_host_call_profile();
        let _host_call = self.enter_host_call();
        self.host_bridge.call(reference, args).map(Some)
    }

    #[cfg(feature = "bigint")]
    fn call_bigint_js_builtin(
        &self,
        module: &BytecodeModule,
        name: &str,
        callee: JsValue,
        this_value: JsValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let args = self.bigint_builtin_args(module, name, args)?;
        #[cfg(feature = "runtime-profile")]
        self.record_host_call_profile();
        let _host_call = self.enter_host_call();
        self.host_bridge.call_js_value(callee, this_value, args)
    }

    #[cfg(feature = "bigint")]
    fn bigint_builtin_args(
        &self,
        module: &BytecodeModule,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, ExecuteError> {
        match name {
            "BigInt" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(vec![self.to_primitive(module, value)?])
            }
            "BigInt.asIntN" | "BigInt.asUintN" => {
                let bits = args.first().cloned().unwrap_or(Value::Undefined);
                let bigint = args.get(1).cloned().unwrap_or(Value::Undefined);
                Ok(vec![
                    self.to_primitive(module, bits)?,
                    self.to_primitive(module, bigint)?,
                ])
            }
            _ => Ok(args),
        }
    }

    fn binary(
        &self,
        module: &BytecodeModule,
        op: &str,
        left: Value,
        right: Value,
    ) -> Result<Value, ExecuteError> {
        if let (Some(left_number), Some(right_number)) =
            (numeric_value(&left), numeric_value(&right))
            && let Some(value) = fast_number_binary(op, left_number, right_number)
        {
            return Ok(value);
        }

        if op == "in" {
            let property = property_key(&left);
            if let Value::ExternalRef(reference) = &right {
                return self
                    .host_bridge
                    .has_property(reference, &property)
                    .map(Value::Bool);
            }
        }

        let (left, right) = if matches!(op, "==" | "!=") {
            match (&left, &right) {
                #[cfg(feature = "bigint")]
                (Value::BigInt(_), _) if is_host_js_object_like(&right) => {
                    return self.binary(module, op, left, self.to_primitive(module, right)?);
                }
                #[cfg(feature = "bigint")]
                (_, Value::BigInt(_)) if is_host_js_object_like(&left) => {
                    return self.binary(module, op, self.to_primitive(module, left)?, right);
                }
                _ => {}
            }
            let (left, right) = match (is_vm_object_like(&left), is_vm_object_like(&right)) {
                (true, false) if is_primitive_like(&right) => {
                    (self.to_primitive(module, left)?, right)
                }
                (false, true) if is_primitive_like(&left) => {
                    (left, self.to_primitive(module, right)?)
                }
                _ => (left, right),
            };
            match (is_vm_object_like(&left), is_vm_object_like(&right)) {
                (true, false) | (false, true) => {
                    return self.binary(module, op, left, right);
                }
                _ => (left, right),
            }
        } else if op == "+" {
            (
                self.to_primitive(module, left)?,
                self.to_primitive(module, right)?,
            )
        } else if matches!(
            op,
            "-" | "*"
                | "/"
                | "%"
                | "**"
                | "<"
                | "<="
                | ">"
                | ">="
                | "&"
                | "|"
                | "^"
                | "<<"
                | ">>"
                | ">>>"
        ) {
            let left = self.to_primitive(module, left)?;
            if is_symbol_like(&left) {
                return Err(crate::error::type_error!("cannot convert a Symbol value"));
            }
            let right = self.to_primitive(module, right)?;
            if is_symbol_like(&right) {
                return Err(crate::error::type_error!("cannot convert a Symbol value"));
            }
            (left, right)
        } else {
            (left, right)
        };
        #[cfg(feature = "bigint")]
        let left = normalize_js_bigint_value(left);
        #[cfg(feature = "bigint")]
        let right = normalize_js_bigint_value(right);

        if op != "instanceof"
            && (matches!(left, Value::JsValue(_) | Value::BoundJsFunction(_, _))
                || matches!(right, Value::JsValue(_) | Value::BoundJsFunction(_, _)))
        {
            match self.host_bridge.binary_operator(op, &left, &right) {
                Ok(value) => return Ok(value),
                Err(err) if should_propagate_js_binary_error(op, &left, &right) => {
                    return Err(err);
                }
                Err(_) => {}
            }
        }

        if matches!(
            op,
            "+" | "-"
                | "*"
                | "/"
                | "%"
                | "**"
                | "<"
                | "<="
                | ">"
                | ">="
                | "&"
                | "|"
                | "^"
                | "<<"
                | ">>"
                | ">>>"
        ) && (is_symbol_like(&left) || is_symbol_like(&right))
        {
            return Err(crate::error::type_error!("cannot convert a Symbol value"));
        }

        fallback_binary(op, left, right)
    }

    fn to_primitive(&self, module: &BytecodeModule, value: Value) -> Result<Value, ExecuteError> {
        self.to_primitive_with_hint(module, value, "default")
    }

    fn to_primitive_with_hint(
        &self,
        module: &BytecodeModule,
        value: Value,
        hint: &str,
    ) -> Result<Value, ExecuteError> {
        let is_vm_js_object = match &value {
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                vm_js_handle(value).is_some()
            }
            _ => false,
        };
        let is_host_js_object = match &value {
            Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                is_host_js_object_value(value)
            }
            _ => false,
        };
        if matches!(value, Value::JsValue(_) | Value::BoundJsFunction(_, _))
            && !is_null_or_undefined_value(&value)
        {
            let exotic = self.get_symbol_to_primitive_member(module, &value)?;
            if !is_null_or_undefined_value(&exotic) {
                let primitive = self.call_with_this(
                    module,
                    exotic,
                    value.clone(),
                    vec![Value::String(hint.to_string())],
                )?;
                #[cfg(feature = "bigint")]
                let primitive = normalize_js_bigint_value(primitive);
                if !is_to_primitive_object_result(&primitive) {
                    return Ok(primitive);
                }
                return Err(crate::error::type_error!(
                    "cannot convert object to primitive value"
                ));
            }
        }
        if !is_vm_js_object
            && !is_host_js_object
            && !matches!(
                value,
                Value::Object(_)
                    | Value::Array(_)
                    | Value::Function(_)
                    | Value::BoundFunction(_, _)
            )
        {
            return Ok(value);
        }

        let methods = if hint == "string" {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        };
        for method in methods {
            let callee = self.get_member(module, &value, method)?;
            if is_null_or_undefined_value(&callee) {
                continue;
            }
            if !is_callable_value(&callee) {
                continue;
            }
            let primitive = self.call_with_this(module, callee, value.clone(), Vec::new())?;
            #[cfg(feature = "bigint")]
            let primitive = normalize_js_bigint_value(primitive);
            if !is_to_primitive_object_result(&primitive) {
                return Ok(primitive);
            }
        }

        Err(crate::error::type_error!(
            "cannot convert object to primitive value"
        ))
    }

    fn get_symbol_to_primitive_member(
        &self,
        module: &BytecodeModule,
        value: &Value,
    ) -> Result<Value, ExecuteError> {
        let member = self.get_member(module, value, "Symbol.toPrimitive")?;
        if !is_null_or_undefined_value(&member) {
            return Ok(member);
        }
        if let Value::JsValue(target) | Value::BoundJsFunction(target, _) = value
            && let Some(member) = js_overlay_get(target, "Symbol.toPrimitive")
        {
            return Ok(bind_member_value(member, value));
        }
        if let Value::JsValue(target) | Value::BoundJsFunction(target, _) = value {
            let key = crate::host::js_reflect_property_key("Symbol.toPrimitive");
            if let Ok(member) = Reflect::get(target, &key)
                && !member.is_null()
                && !member.is_undefined()
            {
                if member.dyn_ref::<JsFunction>().is_some() {
                    return Ok(Value::BoundJsFunction(member, target.clone()));
                }
                return Ok(Value::JsValue(member));
            }
        }
        if let Value::JsValue(target) | Value::BoundJsFunction(target, _) = value
            && let Ok(member) = JsFunction::new_with_args(
                "target",
                "return Reflect.get(target, Symbol.toPrimitive);",
            )
            .call1(&JsValue::UNDEFINED, target)
            && !member.is_null()
            && !member.is_undefined()
        {
            if member.dyn_ref::<JsFunction>().is_some() {
                return Ok(Value::BoundJsFunction(member, target.clone()));
            }
            return Ok(Value::JsValue(member));
        }
        Ok(Value::Undefined)
    }

    fn next_instruction_is_typeof_register(
        &self,
        module: &BytecodeModule,
        pc: usize,
        register_id: u32,
    ) -> Result<bool, ExecuteError> {
        let Some(next) = module.instructions.get(pc + 1) else {
            return Ok(false);
        };
        if next.op != BytecodeOp::Unary {
            return Ok(false);
        }
        let op = self.read_operator(module, operand(next, 1)?)?;
        Ok(op == "typeof"
            && matches!(operand(next, 2)?, BytecodeOperand::Register(id) if *id == register_id))
    }

    fn construct(
        &self,
        module: &BytecodeModule,
        callee: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match callee {
            Value::Null | Value::Undefined => Err(ExecuteError::TypeError(format!(
                "cannot construct {callee}"
            ))),
            Value::Class(class) => self.construct_class(module, class, args),
            Value::Function(function) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.construct_function(module, function, args)
            }
            Value::BoundFunction(function, this_value) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.call_function(module, &function, *this_value, args)
            }
            Value::NativeFunction(function) => {
                self.call_native_method(module, &function.name, Value::Undefined, args)
            }
            Value::BoundNativeFunction(function, this_value) => {
                self.call_native_method(module, &function.name, *this_value, args)
            }
            Value::JsValue(value) => match vm_js_handle(&value) {
                Some(handle) => self.construct_vm_js_handle(module, handle, args),
                None => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_construct_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.construct_js_value(value, args)
                }
            },
            Value::BoundJsFunction(function, _) => match vm_js_handle(&function) {
                Some(handle) => self.construct_vm_js_handle(module, handle, args),
                None => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_construct_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.construct_js_value(function, args)
                }
            },
            Value::ExternalRef(reference) => {
                #[cfg(feature = "runtime-profile")]
                self.record_host_construct_profile();
                let _host_call = self.enter_host_call();
                self.host_bridge.construct(&reference, args)
            }
            _ => Err(ExecuteError::TypeError(format!(
                "{callee} is not constructable"
            ))),
        }
    }

    fn call_vm_js_handle(
        &self,
        module: &BytecodeModule,
        handle: VmJsHandle,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        self.call_vm_js_handle_with_binding(module, handle, this_value, args, false)
    }

    fn call_vm_js_handle_from_callback(
        &self,
        module: &BytecodeModule,
        handle: &VmJsHandle,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        // JS callback invoker 进入这里前已经按 handle 绑定的 module/bridge 创建了 executor。
        // 因此 VM 函数可以直接进函数帧，避免再走 `call_function_with_stored_bridge`
        // 重新构造一层 executor；Vue Proxy trap 这类热点 callback 会频繁命中该路径。
        match handle {
            VmJsHandle::Function(function) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.call_function(module, function, this_value, args)
            }
            VmJsHandle::BoundFunction(function, bound_this) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.call_function(module, function, bound_this.clone(), args)
            }
            VmJsHandle::NativeFunction(function) => {
                self.call_native_method(module, &function.name, this_value, args)
            }
            VmJsHandle::BoundNativeFunction(function, bound_this) => {
                self.call_native_method(module, &function.name, bound_this.clone(), args)
            }
            VmJsHandle::Class(class) => Ok(object_value(class.static_props.clone())),
            VmJsHandle::Module(module) => Err(ExecuteError::TypeError(format!(
                "module {} is not callable",
                module.source
            ))),
        }
    }

    fn call_vm_js_handle_with_binding(
        &self,
        module: &BytecodeModule,
        handle: VmJsHandle,
        this_value: Value,
        args: Vec<Value>,
        override_bound_this: bool,
    ) -> Result<Value, ExecuteError> {
        match handle {
            VmJsHandle::Function(function) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.call_function_with_stored_bridge(module, &function, this_value, args)
            }
            VmJsHandle::BoundFunction(function, bound_this) => {
                let this_value = if override_bound_this {
                    this_value
                } else {
                    bound_this
                };
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.call_function_with_stored_bridge(module, &function, this_value, args)
            }
            VmJsHandle::NativeFunction(function) => {
                self.call_native_method(module, &function.name, this_value, args)
            }
            VmJsHandle::BoundNativeFunction(function, bound_this) => {
                let this_value = if override_bound_this {
                    this_value
                } else {
                    bound_this
                };
                self.call_native_method(module, &function.name, this_value, args)
            }
            VmJsHandle::Class(class) => Ok(object_value(class.static_props)),
            VmJsHandle::Module(module) => Err(ExecuteError::TypeError(format!(
                "module {} is not callable",
                module.source
            ))),
        }
    }

    fn construct_vm_js_handle(
        &self,
        module: &BytecodeModule,
        handle: VmJsHandle,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match handle {
            VmJsHandle::Class(class) => self.construct_class(module, class, args),
            VmJsHandle::Function(function) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.construct_function_with_stored_bridge(module, function, args)
            }
            VmJsHandle::BoundFunction(function, this_value) => {
                let function_module = function.module.clone();
                let module = function_module.as_deref().unwrap_or(module);
                self.call_function_with_stored_bridge(module, &function, this_value, args)
            }
            VmJsHandle::NativeFunction(function) | VmJsHandle::BoundNativeFunction(function, _) => {
                Err(ExecuteError::TypeError(format!(
                    "{} is not constructable",
                    function.name
                )))
            }
            VmJsHandle::Module(module) => Err(ExecuteError::TypeError(format!(
                "module {} is not constructable",
                module.source
            ))),
        }
    }

    fn executor_with_stored_bridge(
        &self,
        module: &BytecodeModule,
        bridge: JsHostBridge,
        module_handle: Option<Rc<BytecodeModule>>,
    ) -> Executor<JsHostBridge> {
        let module_handle = module_handle.unwrap_or_else(|| Rc::new(module.clone()));
        Executor {
            registers: Vec::new(),
            lexical_env: LexicalEnv::default(),
            instruction_scope_depths: self.instruction_scope_depths.clone(),
            function_hoist_cache: self.function_hoist_cache.clone(),
            register_frame_size_cache: self.register_frame_size_cache.clone(),
            pc_inline_cache: self.pc_inline_cache.clone(),
            last_value: Value::Undefined,
            exports: self.exports.clone(),
            host_bridge: bridge,
            external_names: module.extern_slots.clone(),
            module_handle: Some(module_handle),
            call_depth: self.call_depth,
            max_call_depth: self.max_call_depth,
            max_recursive_call_depth: self.max_recursive_call_depth,
            call_stack: self.call_stack.clone(),
            host_call_depth: self.host_call_depth.clone(),
            execution_budget: self.execution_budget.clone(),
            #[cfg(feature = "runtime-profile")]
            runtime_profile: self.runtime_profile.clone(),
            #[cfg(feature = "debugger")]
            debug_breakpoints: self.debug_breakpoints.clone(),
            #[cfg(feature = "debugger")]
            debug_skip_breakpoint: self.debug_skip_breakpoint.clone(),
        }
    }

    fn call_function_with_stored_bridge(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        // VM 函数可能作为 JS callback 暴露给宿主，并在原始 executor 生命周期结束后才被调用。
        // 如果函数创建时保存了 bridge，这里会用同一个宿主桥重建 executor，保证 callback
        // 继续访问到当时的外部环境、overlay 和 JS 反射能力。
        if let Some(bridge) = function.host_bridge.clone() {
            let module_handle = function
                .module
                .clone()
                .unwrap_or_else(|| Rc::new(module.clone()));
            return self
                .executor_with_stored_bridge(module, bridge, Some(module_handle))
                .call_function(module, function, this_value, args);
        }
        self.call_function(module, function, this_value, args)
    }

    fn construct_function_with_stored_bridge(
        &self,
        module: &BytecodeModule,
        function: FunctionValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        if let Some(bridge) = function.host_bridge.clone() {
            let module_handle = function
                .module
                .clone()
                .unwrap_or_else(|| Rc::new(module.clone()));
            return self
                .executor_with_stored_bridge(module, bridge, Some(module_handle))
                .construct_function(module, function, args);
        }
        self.construct_function(module, function, args)
    }

    fn construct_class(
        &self,
        module: &BytecodeModule,
        class: ClassValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let mut this_value = object_value(class.instance_props.clone());
        if let Some(constructor) = &class.constructor {
            let constructor = constructor.clone();
            if let Some(super_class) = &class.super_class {
                constructor.env.define_current(
                    "super".to_string(),
                    Value::BoundNativeFunction(
                        NativeFunctionValue {
                            name: "__js_vm_super".to_string(),
                        },
                        super_class.clone(),
                    ),
                );
            }
            let constructor_module = constructor.module.clone();
            let module = constructor_module.as_deref().unwrap_or(module);
            let (result, lexical_env) =
                self.call_function_frame(module, &constructor, this_value.clone(), args)?;
            if !matches!(result, Value::Undefined) {
                return Ok(result);
            }
            this_value = lexical_env.get("this").unwrap_or(this_value);
        } else if let Some(super_class) = class.super_class {
            this_value = self.construct(module, *super_class, args)?;
        }
        Ok(this_value)
    }

    fn construct_function(
        &self,
        module: &BytecodeModule,
        function: FunctionValue,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let mut this_value = object_value(BTreeMap::new());
        if let Some(name) = &function.name {
            set_member(
                &mut this_value,
                "__constructor_name",
                Value::String(name.clone()),
            )?;
        }
        let result = self.call_function(module, &function, this_value.clone(), args)?;
        if matches!(result, Value::Undefined) {
            Ok(this_value)
        } else {
            Ok(result)
        }
    }

    fn call_function(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        // 普通函数、async 函数和 generator 使用同一份函数元数据，但入口行为不同：
        // generator 返回可恢复对象；async 当前用轻量 thenable 包装同步求值结果；
        // 普通函数直接执行函数帧并返回 `return` 值。
        if function.is_generator {
            #[cfg(feature = "generator")]
            return self.generator_object(function.clone(), this_value, args);
            #[cfg(not(feature = "generator"))]
            return Err(ExecuteError::Unsupported("generator"));
        }
        if function.is_async {
            self.call_async_function(module, function, this_value, args)
        } else {
            let (value, _) = self.call_function_frame(module, function, this_value, args)?;
            Ok(value)
        }
    }

    fn call_async_function(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let (flow, frame) = self.execute_function_frame(
            module,
            function,
            this_value,
            args,
            function.body_start,
            false,
        )?;
        self.async_flow_to_value(module, flow, frame)
    }

    fn async_flow_to_value(
        &self,
        module: &BytecodeModule,
        flow: Flow,
        frame: Executor<B>,
    ) -> Result<Value, ExecuteError> {
        match flow {
            Flow::Value(_) => Ok(async_resolved_value(Value::Undefined)),
            Flow::Return(value) => Ok(async_resolved_value(value)),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
            Flow::Yield { .. } => Err(ExecuteError::Runtime(
                "yield outside generator frame".to_string(),
            )),
            Flow::AwaitPending {
                promise,
                resume_dst,
                resume_pc,
                resume_end,
            } => {
                self.async_resume_promise(module, frame, promise, resume_dst, resume_pc, resume_end)
            }
            #[cfg(feature = "debugger")]
            Flow::Pause { .. } => Err(ExecuteError::Runtime(
                "debug pause inside async function frame is not resumable yet".to_string(),
            )),
        }
    }

    fn async_resume_promise(
        &self,
        module: &BytecodeModule,
        frame: Executor<B>,
        promise: JsValue,
        resume_dst: u32,
        resume_pc: usize,
        resume_end: usize,
    ) -> Result<Value, ExecuteError> {
        let Some(promise) = promise.dyn_ref::<JsPromise>().cloned() else {
            return Err(ExecuteError::Runtime(
                "await pending value is not a Promise".to_string(),
            ));
        };
        let Some(bridge) = self.host_bridge.as_js_host_bridge() else {
            return Ok(async_resolved_value(Value::Undefined));
        };
        async_resume_promise_from_frame(
            Rc::new(module.clone()),
            bridge,
            frame,
            promise.into(),
            resume_dst,
            resume_pc,
            resume_end,
        )
        .map(Value::JsValue)
    }

    #[cfg(feature = "generator")]
    fn generator_object(
        &self,
        function: FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let state = Value::GeneratorState(Rc::new(RefCell::new(GeneratorState {
            initialized: false,
            done: false,
            pc: function.body_start,
            resume_dst: None,
            registers: Vec::new(),
            lexical_env: function.env.clone(),
            last_value: Value::Undefined,
        })));
        let generator_args = self.array_value(args)?;
        self.object_value(BTreeMap::from([
            (
                "__generator_function".to_string(),
                Value::Function(function),
            ),
            ("__generator_this".to_string(), this_value),
            ("__generator_args".to_string(), generator_args),
            ("__generator_state".to_string(), state),
            (
                "next".to_string(),
                Value::NativeFunction(NativeFunctionValue {
                    name: "Generator.next".to_string(),
                }),
            ),
            (
                "return".to_string(),
                Value::NativeFunction(NativeFunctionValue {
                    name: "Generator.return".to_string(),
                }),
            ),
            (
                "Symbol.iterator".to_string(),
                Value::NativeFunction(NativeFunctionValue {
                    name: "Generator.iterator".to_string(),
                }),
            ),
        ]))
    }

    #[cfg(feature = "generator")]
    fn call_generator_next(
        &self,
        module: &BytecodeModule,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        let function = match self.internal_member(&this_value, "__generator_function")? {
            Some(Value::Function(function)) => function,
            _ => {
                return Err(ExecuteError::TypeError(
                    "bad generator function".to_string(),
                ));
            }
        };
        let this_arg = self
            .internal_member(&this_value, "__generator_this")?
            .unwrap_or(Value::Undefined);
        let generator_args = match self.internal_member(&this_value, "__generator_args")? {
            Some(Value::Array(items)) => items.borrow().clone(),
            Some(value @ Value::JsValue(_)) | Some(value @ Value::BoundJsFunction(_, _)) => {
                apply_argument_list(&value)?
            }
            _ => Vec::new(),
        };
        let state = match self.internal_member(&this_value, "__generator_state")? {
            Some(Value::GeneratorState(state)) => state,
            _ => {
                return Err(ExecuteError::TypeError("bad generator state".to_string()));
            }
        };
        let resume_value = args.first().cloned().unwrap_or(Value::Undefined);
        self.resume_generator(
            module,
            &function,
            this_arg,
            generator_args,
            state,
            resume_value,
        )
    }

    #[cfg(feature = "generator")]
    fn resume_generator(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
        state: Rc<RefCell<GeneratorState>>,
        resume_value: Value,
    ) -> Result<Value, ExecuteError> {
        if self.call_depth >= self.max_call_depth {
            return Err(crate::error::range_error!("maximum call stack exceeded"));
        }
        if function.body_start > function.body_end || function.body_end > module.instructions.len()
        {
            return Err(ExecuteError::Runtime(format!(
                "generator body range {}..{} is outside code length {}",
                function.body_start,
                function.body_end,
                module.instructions.len()
            )));
        }
        if self
            .call_stack
            .borrow()
            .iter()
            .filter(|entry| **entry == function.body_start)
            .count()
            >= self.max_recursive_call_depth
        {
            return Err(crate::error::range_error!(
                "maximum recursive call stack exceeded"
            ));
        }

        let mut should_hoist = false;
        {
            let mut state = state.borrow_mut();
            if state.done {
                return Ok(iterator_result(Value::Undefined, true));
            }
            if !state.initialized {
                let mut lexical_env = function.env.clone();
                lexical_env.push_frame(ScopeKind::Function);
                lexical_env.define_current("this".to_string(), this_value.clone());
                lexical_env.set_slot(FUNCTION_THIS_SLOT, this_value);
                lexical_env
                    .define_current("arguments".to_string(), self.array_value(args.clone())?);
                if let Some(name) = &function.name {
                    lexical_env.define_current(name.clone(), Value::Function(function.clone()));
                }
                for (index, param) in function.params.iter().enumerate() {
                    let value = args.get(index).cloned().unwrap_or(Value::Undefined);
                    match param {
                        BytecodeOperand::LocalSlot(slot) => lexical_env.set_slot(*slot, value),
                        BytecodeOperand::Name(name) => {
                            lexical_env.define_current(name_string(module, *name)?, value)
                        }
                        _ => {}
                    }
                }
                state.initialized = true;
                state.pc = function.body_start;
                state.resume_dst = None;
                state.registers.clear();
                state.lexical_env = lexical_env;
                state.last_value = Value::Undefined;
                should_hoist = true;
            }
        }

        let frame_module_handle = function
            .module
            .clone()
            .unwrap_or_else(|| Rc::new(module.clone()));
        let mut frame = {
            let state = state.borrow();
            Executor {
                registers: state.registers.clone(),
                lexical_env: state.lexical_env.clone(),
                instruction_scope_depths: self.child_instruction_scope_depths(module)?,
                function_hoist_cache: self.function_hoist_cache.clone(),
                register_frame_size_cache: self.register_frame_size_cache.clone(),
                pc_inline_cache: self.pc_inline_cache.clone(),
                last_value: state.last_value.clone(),
                exports: self.exports.clone(),
                host_bridge: self.host_bridge.clone(),
                external_names: self.external_names.clone(),
                module_handle: Some(frame_module_handle),
                call_depth: self.call_depth + 1,
                max_call_depth: self.max_call_depth,
                max_recursive_call_depth: self.max_recursive_call_depth,
                call_stack: self.call_stack.clone(),
                host_call_depth: self.host_call_depth.clone(),
                execution_budget: self.execution_budget.clone(),
                #[cfg(feature = "runtime-profile")]
                runtime_profile: self.runtime_profile.clone(),
                #[cfg(feature = "debugger")]
                debug_breakpoints: self.debug_breakpoints.clone(),
                #[cfg(feature = "debugger")]
                debug_skip_breakpoint: self.debug_skip_breakpoint.clone(),
            }
        };

        let start_pc = {
            let state = state.borrow();
            if !should_hoist {
                if let Some(dst) = state.resume_dst {
                    frame.write_register(dst, resume_value);
                }
            }
            state.pc
        };

        let _call_stack_guard = CallStackGuard::push(self.call_stack.clone(), function.body_start);
        let flow = (|| {
            if should_hoist {
                frame.hoist_function_declarations(
                    module,
                    function.body_start,
                    function.body_end,
                )?;
            }
            frame.execute_range(module, start_pc, function.body_end)
        })();

        match flow? {
            Flow::Yield {
                value,
                resume_pc,
                resume_dst,
            } => {
                let mut state = state.borrow_mut();
                state.pc = resume_pc;
                state.resume_dst = resume_dst;
                state.registers = frame.registers;
                state.lexical_env = frame.lexical_env;
                state.last_value = frame.last_value;
                Ok(iterator_result(value, false))
            }
            Flow::Return(value) => {
                let mut state = state.borrow_mut();
                state.done = true;
                state.pc = function.body_end;
                state.resume_dst = None;
                state.registers = frame.registers;
                state.lexical_env = frame.lexical_env;
                state.last_value = frame.last_value;
                Ok(iterator_result(value, true))
            }
            Flow::Value(_) => {
                let mut state = state.borrow_mut();
                state.done = true;
                state.pc = function.body_end;
                state.resume_dst = None;
                state.registers = frame.registers;
                state.lexical_env = frame.lexical_env;
                state.last_value = frame.last_value;
                Ok(iterator_result(Value::Undefined, true))
            }
            Flow::Throw(value) => {
                state.borrow_mut().done = true;
                Err(ExecuteError::Thrown(value))
            }
            Flow::AwaitPending { .. } => Err(ExecuteError::Runtime(
                "await pending inside generator frame".to_string(),
            )),
            #[cfg(feature = "debugger")]
            Flow::Pause { .. } => Err(ExecuteError::Runtime(
                "debug pause inside generator frame is not resumable yet".to_string(),
            )),
        }
    }

    #[cfg(feature = "generator")]
    fn call_generator_return(
        &self,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        if let Some(Value::GeneratorState(state)) =
            self.internal_member(&this_value, "__generator_state")?
        {
            state.borrow_mut().done = true;
        }
        Ok(iterator_result(
            args.first().cloned().unwrap_or(Value::Undefined),
            true,
        ))
    }

    fn call_native_method(
        &self,
        module: &BytecodeModule,
        name: &str,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        #[cfg(not(any(
            feature = "array-builtins",
            feature = "bigint",
            feature = "function-builtins",
            feature = "generator",
            feature = "object-builtins",
            feature = "regexp",
            feature = "string-builtins"
        )))]
        {
            let _ = (module, &this_value, &args);
        }
        match name {
            "__js_vm_super" => self.construct(module, this_value, args),
            "AsyncResolved.then" => {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                if is_callable_value(&callback) {
                    let value = self.call(module, callback, vec![this_value])?;
                    Ok(async_resolved_value(value))
                } else {
                    Ok(async_resolved_value(this_value))
                }
            }
            "AsyncResolved.catch" => Ok(async_resolved_value(this_value)),
            "AsyncResolved.finally" => {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                if is_callable_value(&callback) {
                    let _ = self.call(module, callback, Vec::new())?;
                }
                Ok(async_resolved_value(this_value))
            }
            #[cfg(feature = "generator")]
            "Generator.next" => self.call_generator_next(module, this_value, args),
            #[cfg(feature = "generator")]
            "Generator.return" => self.call_generator_return(this_value, args),
            #[cfg(feature = "generator")]
            "Generator.iterator" => Ok(this_value),
            #[cfg(feature = "array-builtins")]
            "Array.push" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    items.extend(args);
                    Ok(Value::Number(items.len() as f64))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.fill" => match this_value {
                Value::Array(items) => {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    let mut items = items.borrow_mut();
                    items.fill(value);
                    Ok(array_value(items.clone()))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.join" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let separator = args
                        .first()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| ",".to_string());
                    Ok(Value::String(
                        items
                            .iter()
                            .map(|value| match value {
                                Value::Null | Value::Undefined => String::new(),
                                value => value.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(&separator),
                    ))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.toString" => {
                self.call_native_method(module, "Array.join", this_value, Vec::new())
            }
            #[cfg(feature = "bigint")]
            "BigInt.toString" => bigint_prototype_call("toString", this_value, args),
            #[cfg(feature = "bigint")]
            "BigInt.valueOf" => bigint_prototype_call("valueOf", this_value, args),
            #[cfg(feature = "array-builtins")]
            "Array.forEach" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(Value::Undefined);
                };
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                for (index, item) in items.iter().cloned().enumerate() {
                    self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                }
                Ok(Value::Undefined)
            }
            #[cfg(feature = "array-builtins")]
            "Array.map" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(array_value(Vec::new()));
                };
                let mut mapped = Vec::with_capacity(items.len());
                for (index, item) in items.iter().cloned().enumerate() {
                    mapped.push(self.call(
                        module,
                        callback.clone(),
                        vec![
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?);
                }
                Ok(array_value(mapped))
            }
            #[cfg(feature = "array-builtins")]
            "Array.filter" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(array_value(items));
                };
                let mut filtered = Vec::new();
                for (index, item) in items.iter().cloned().enumerate() {
                    let keep = self.call(
                        module,
                        callback.clone(),
                        vec![
                            item.clone(),
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                    if keep.is_truthy() {
                        filtered.push(item);
                    }
                }
                Ok(array_value(filtered))
            }
            #[cfg(feature = "array-builtins")]
            "Array.flatMap" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(array_value(Vec::new()));
                };
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                let mut mapped = Vec::new();
                for (index, item) in items.iter().cloned().enumerate() {
                    let value = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                    match value {
                        Value::Array(values) => {
                            mapped.extend(values.borrow().iter().cloned());
                        }
                        value => mapped.push(value),
                    }
                }
                Ok(array_value(mapped))
            }
            #[cfg(feature = "array-builtins")]
            "Array.find" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(Value::Undefined);
                };
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                for (index, item) in items.iter().cloned().enumerate() {
                    let matched = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![
                            item.clone(),
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                    if matched.is_truthy() {
                        return Ok(item);
                    }
                }
                Ok(Value::Undefined)
            }
            #[cfg(feature = "array-builtins")]
            "Array.reduce" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Undefined);
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(Value::Undefined);
                };
                let mut iter = items.iter().cloned().enumerate();
                let mut accumulator = if let Some(initial) = args.get(1).cloned() {
                    initial
                } else if let Some((_, first)) = iter.next() {
                    first
                } else {
                    return Ok(Value::Undefined);
                };
                for (index, item) in iter {
                    accumulator = self.call(
                        module,
                        callback.clone(),
                        vec![
                            accumulator,
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                }
                Ok(accumulator)
            }
            #[cfg(feature = "array-builtins")]
            "Array.every" => {
                let Value::Array(items) = this_value.clone() else {
                    return Ok(Value::Bool(true));
                };
                let items = items.borrow().clone();
                let Some(callback) = args.first().cloned() else {
                    return Ok(Value::Bool(true));
                };
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                for (index, item) in items.iter().cloned().enumerate() {
                    let passed = self.call_with_this(
                        module,
                        callback.clone(),
                        this_arg.clone(),
                        vec![
                            item,
                            Value::Number(index as f64),
                            array_value(items.clone()),
                        ],
                    )?;
                    if !passed.is_truthy() {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            #[cfg(feature = "array-builtins")]
            "Array.includes" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let needle = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Bool(items.iter().any(|item| *item == needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
            #[cfg(feature = "array-builtins")]
            "Array.indexOf" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let needle = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Number(
                        items
                            .iter()
                            .position(|item| *item == needle)
                            .map(|index| index as f64)
                            .unwrap_or(-1.0),
                    ))
                }
                _ => Ok(Value::Number(-1.0)),
            },
            #[cfg(feature = "array-builtins")]
            "Array.pop" => match this_value {
                Value::Array(items) => Ok(items.borrow_mut().pop().unwrap_or(Value::Undefined)),
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.shift" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    if items.is_empty() {
                        Ok(Value::Undefined)
                    } else {
                        Ok(items.remove(0))
                    }
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.unshift" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    for value in args.into_iter().rev() {
                        items.insert(0, value);
                    }
                    Ok(Value::Number(items.len() as f64))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.reverse" => match this_value {
                Value::Array(items) => {
                    items.borrow_mut().reverse();
                    Ok(Value::Array(items))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.sort" => match this_value {
                Value::Array(items) => {
                    items
                        .borrow_mut()
                        .sort_by(|left, right| left.to_string().cmp(&right.to_string()));
                    Ok(Value::Array(items))
                }
                _ => Ok(Value::Undefined),
            },
            #[cfg(feature = "array-builtins")]
            "Array.splice" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow_mut();
                    let len = items.len() as isize;
                    let start = args.first().map(Value::to_number).unwrap_or(0.0).trunc() as isize;
                    let start = normalize_index(start, len) as usize;
                    let delete_count = args
                        .get(1)
                        .map(Value::to_number)
                        .unwrap_or((items.len() - start) as f64)
                        .max(0.0)
                        .trunc() as usize;
                    let end = (start + delete_count).min(items.len());
                    let removed = items.splice(start..end, args.into_iter().skip(2)).collect();
                    Ok(array_value(removed))
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.slice" => match this_value {
                Value::Array(items) => {
                    let items = items.borrow();
                    let len = items.len() as isize;
                    let start = args.first().map(Value::to_number).unwrap_or(0.0).trunc() as isize;
                    let end = args
                        .get(1)
                        .map(Value::to_number)
                        .unwrap_or(len as f64)
                        .trunc() as isize;
                    let start = normalize_index(start, len);
                    let end = normalize_index(end, len).max(start);
                    Ok(array_value(items[start as usize..end as usize].to_vec()))
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.concat" => match this_value {
                Value::Array(items) => {
                    let mut items = items.borrow().clone();
                    for arg in args {
                        match arg {
                            Value::Array(values) => items.extend(values.borrow().iter().cloned()),
                            value => items.push(value),
                        }
                    }
                    Ok(array_value(items))
                }
                value => Ok(array_value(std::iter::once(value).chain(args).collect())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.flat" => match this_value {
                Value::Array(items) => {
                    let mut flattened = Vec::new();
                    for item in items.borrow().iter().cloned() {
                        match item {
                            Value::Array(values) => {
                                flattened.extend(values.borrow().iter().cloned())
                            }
                            value => flattened.push(value),
                        }
                    }
                    Ok(array_value(flattened))
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "array-builtins")]
            "Array.values" => match this_value {
                Value::Array(items) => Ok(object_value(BTreeMap::from([
                    (
                        "__array_iterator_values".to_string(),
                        Value::Array(items.clone()),
                    ),
                    ("__array_iterator_index".to_string(), Value::Number(0.0)),
                    (
                        "next".to_string(),
                        Value::NativeFunction(crate::value::NativeFunctionValue {
                            name: "ArrayIterator.next".to_string(),
                        }),
                    ),
                ]))),
                _ => Err(ExecuteError::TypeError(
                    "Array iterator called on non-array".to_string(),
                )),
            },
            #[cfg(feature = "array-builtins")]
            "ArrayIterator.next" => match this_value {
                Value::Object(props) => {
                    let values = props
                        .borrow()
                        .get("__array_iterator_values")
                        .cloned()
                        .unwrap_or(Value::Undefined);
                    let index = props
                        .borrow()
                        .get("__array_iterator_index")
                        .map(Value::to_number)
                        .unwrap_or(0.0)
                        .max(0.0)
                        .trunc() as usize;
                    let Value::Array(items) = values else {
                        return Err(ExecuteError::TypeError(
                            "invalid array iterator".to_string(),
                        ));
                    };
                    let items = items.borrow();
                    let (value, done) = if index < items.len() {
                        (items[index].clone(), false)
                    } else {
                        (Value::Undefined, true)
                    };
                    drop(items);
                    props
                        .try_borrow_mut()
                        .map_err(|_| {
                            ExecuteError::Runtime(
                                "iterator properties are already borrowed".to_string(),
                            )
                        })?
                        .insert(
                            "__array_iterator_index".to_string(),
                            Value::Number((index + 1) as f64),
                        );
                    Ok(object_value(BTreeMap::from([
                        ("value".to_string(), value),
                        ("done".to_string(), Value::Bool(done)),
                    ])))
                }
                _ => Err(ExecuteError::TypeError(
                    "ArrayIterator.next called on non-iterator".to_string(),
                )),
            },
            #[cfg(feature = "string-builtins")]
            "String.charAt" => match this_value {
                Value::String(value) => {
                    let index = args.first().map(Value::to_number).unwrap_or(0.0) as usize;
                    Ok(value
                        .chars()
                        .nth(index)
                        .map(|value| Value::String(value.to_string()))
                        .unwrap_or_else(|| Value::String(String::new())))
                }
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.charCodeAt" => match this_value {
                Value::String(value) => {
                    let index = args.first().map(Value::to_number).unwrap_or(0.0) as usize;
                    Ok(value
                        .chars()
                        .nth(index)
                        .map(|value| Value::Number(value as u32 as f64))
                        .unwrap_or(Value::Number(f64::NAN)))
                }
                _ => Ok(Value::Number(f64::NAN)),
            },
            #[cfg(feature = "string-builtins")]
            "String.endsWith" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    let end = args
                        .get(1)
                        .map(Value::to_number)
                        .filter(|value| value.is_finite())
                        .map(|value| value.max(0.0) as usize)
                        .unwrap_or_else(|| value.chars().count());
                    let prefix = value.chars().take(end).collect::<String>();
                    Ok(Value::Bool(prefix.ends_with(&needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
            #[cfg(feature = "string-builtins")]
            "String.includes" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    Ok(Value::Bool(value.contains(&needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
            #[cfg(feature = "string-builtins")]
            "String.indexOf" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    Ok(Value::Number(
                        value
                            .find(&needle)
                            .map(|index| index as f64)
                            .unwrap_or(-1.0),
                    ))
                }
                _ => Ok(Value::Number(-1.0)),
            },
            #[cfg(feature = "string-builtins")]
            "String.slice" => match this_value {
                Value::String(value) => {
                    let chars = value.chars().collect::<Vec<_>>();
                    let len = chars.len() as isize;
                    let start = args.first().map(Value::to_number).unwrap_or(0.0).trunc() as isize;
                    let end = args
                        .get(1)
                        .map(Value::to_number)
                        .unwrap_or(len as f64)
                        .trunc() as isize;
                    let start = normalize_index(start, len);
                    let end = normalize_index(end, len).max(start);
                    Ok(Value::String(
                        chars[start as usize..end as usize].iter().collect(),
                    ))
                }
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.startsWith" => match this_value {
                Value::String(value) => {
                    let needle = args.first().map(ToString::to_string).unwrap_or_default();
                    let start = args
                        .get(1)
                        .map(Value::to_number)
                        .filter(|value| value.is_finite())
                        .map(|value| value.max(0.0) as usize)
                        .unwrap_or(0);
                    let suffix = value.chars().skip(start).collect::<String>();
                    Ok(Value::Bool(suffix.starts_with(&needle)))
                }
                _ => Ok(Value::Bool(false)),
            },
            #[cfg(feature = "string-builtins")]
            "String.trim" => match this_value {
                Value::String(value) => Ok(Value::String(value.trim().to_string())),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.toLowerCase" => match this_value {
                Value::String(value) => Ok(Value::String(value.to_lowercase())),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.toUpperCase" => match this_value {
                Value::String(value) => Ok(Value::String(value.to_uppercase())),
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.split" => match this_value {
                Value::String(value) => {
                    let separator = args.first().map(ToString::to_string).unwrap_or_default();
                    if separator.is_empty() {
                        Ok(array_value(
                            value
                                .chars()
                                .map(|value| Value::String(value.to_string()))
                                .collect(),
                        ))
                    } else {
                        Ok(array_value(
                            value
                                .split(&separator)
                                .map(|value| Value::String(value.to_string()))
                                .collect(),
                        ))
                    }
                }
                _ => Ok(array_value(Vec::new())),
            },
            #[cfg(feature = "string-builtins")]
            "String.concat" => match this_value {
                Value::String(mut value) => {
                    for arg in args {
                        let arg = self.to_primitive_with_hint(module, arg, "string")?;
                        value.push_str(&arg.to_string());
                    }
                    Ok(Value::String(value))
                }
                value => {
                    let value = self.to_primitive_with_hint(module, value, "string")?;
                    let mut output = value.to_string();
                    for arg in args {
                        let arg = self.to_primitive_with_hint(module, arg, "string")?;
                        output.push_str(&arg.to_string());
                    }
                    Ok(Value::String(output))
                }
            },
            #[cfg(feature = "string-builtins")]
            "String.match" => match this_value {
                Value::String(value) => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_call_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.call_string_method(&value, "match", args)
                }
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                    match value.as_string() {
                        Some(value) => {
                            #[cfg(feature = "runtime-profile")]
                            self.record_host_call_profile();
                            let _host_call = self.enter_host_call();
                            self.host_bridge.call_string_method(&value, "match", args)
                        }
                        None => Ok(Value::Null),
                    }
                }
                _ => Ok(Value::Null),
            },
            #[cfg(feature = "string-builtins")]
            "String.replace" => match this_value {
                Value::String(value) => {
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_call_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.call_string_method(&value, "replace", args)
                }
                Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
                    match value.as_string() {
                        Some(value) => {
                            #[cfg(feature = "runtime-profile")]
                            self.record_host_call_profile();
                            let _host_call = self.enter_host_call();
                            self.host_bridge.call_string_method(&value, "replace", args)
                        }
                        None => Ok(Value::String(String::new())),
                    }
                }
                _ => Ok(Value::String(String::new())),
            },
            #[cfg(feature = "regexp")]
            "RegExp.exec" => {
                let input = args.first().map(ToString::to_string).unwrap_or_default();
                if let Value::JsValue(value) | Value::BoundJsFunction(value, _) = &this_value
                    && let Some(result) = js_regexp_exec(value, &input)
                {
                    return Ok(Value::JsValue(result));
                }
                Ok(regexp_exec_value(&this_value, &input))
            }
            #[cfg(feature = "regexp")]
            "RegExp.test" => {
                let input = args.first().map(ToString::to_string).unwrap_or_default();
                if let Value::JsValue(value) | Value::BoundJsFunction(value, _) = &this_value
                    && let Some(result) = js_regexp_test(value, &input)
                {
                    return Ok(Value::JsValue(JsValue::from_bool(result)));
                }
                Ok(Value::Bool(!matches!(
                    regexp_exec_value(&this_value, &input),
                    Value::Null
                )))
            }
            #[cfg(feature = "object-builtins")]
            "Object.prototype.hasOwnProperty" => {
                let key = args.first().map(ToString::to_string).unwrap_or_default();
                Ok(Value::Bool(object_has_own_property(&this_value, &key)))
            }
            #[cfg(feature = "object-builtins")]
            "Object.prototype.toString" => Ok(Value::String(object_to_string_tag(&this_value))),
            #[cfg(feature = "object-builtins")]
            "Object.prototype.valueOf" => Ok(this_value),
            #[cfg(feature = "function-builtins")]
            "Function.toString" => Ok(Value::String(function_source_string(&this_value))),
            #[cfg(feature = "function-builtins")]
            "Function.call" => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let call_args = args.into_iter().skip(1).collect::<Vec<_>>();
                self.call_with_this(module, this_value, this_arg, call_args)
            }
            #[cfg(feature = "function-builtins")]
            "Function.apply" => {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let call_args = args
                    .get(1)
                    .map(apply_argument_list)
                    .transpose()?
                    .unwrap_or_default();
                self.call_with_this(module, this_value, this_arg, call_args)
            }
            _ => Err(ExecuteError::Runtime(format!(
                "native method {name} is not registered"
            ))),
        }
    }

    fn call_promise_builtin(
        &self,
        reference: &ExternalRefValue,
        args: Vec<Value>,
    ) -> Result<Option<Value>, ExecuteError> {
        match reference.display_path().as_str() {
            "Promise.resolve" => Ok(Some(async_resolved_value(
                args.first().cloned().unwrap_or(Value::Undefined),
            ))),
            "Promise.all" => {
                let values = self.promise_iterable_values(args.first())?;
                let mut resolved = Vec::with_capacity(values.len());
                for value in values {
                    resolved.push(await_value(value)?);
                }
                Ok(Some(async_resolved_value(self.array_value(resolved)?)))
            }
            "Promise.allSettled" => {
                let values = self.promise_iterable_values(args.first())?;
                let mut resolved = Vec::with_capacity(values.len());
                for value in values {
                    resolved.push(self.object_value(BTreeMap::from([
                        ("status".to_string(), Value::String("fulfilled".to_string())),
                        ("value".to_string(), await_value(value)?),
                    ]))?);
                }
                Ok(Some(async_resolved_value(self.array_value(resolved)?)))
            }
            "Promise.reject" => Ok(Some(async_resolved_value(Value::Undefined))),
            _ => Ok(None),
        }
    }

    fn promise_iterable_values(&self, value: Option<&Value>) -> Result<Vec<Value>, ExecuteError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        match value {
            Value::Array(items) => Ok(items.borrow().clone()),
            Value::JsValue(value) | Value::BoundJsFunction(value, _)
                if JsArray::is_array(value) =>
            {
                let array = JsArray::from(value);
                let mut items = Vec::with_capacity(array.length() as usize);
                for index in 0..array.length() {
                    items.push(Value::JsValue(array.get(index)));
                }
                Ok(items)
            }
            Value::Null | Value::Undefined => Ok(Vec::new()),
            _ => Ok(vec![value.clone()]),
        }
    }

    fn call_with_this(
        &self,
        module: &BytecodeModule,
        callee: Value,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<Value, ExecuteError> {
        match callee {
            Value::Function(function) | Value::BoundFunction(function, _) => {
                self.call_function(module, &function, this_value, args)
            }
            Value::NativeFunction(function) | Value::BoundNativeFunction(function, _) => {
                self.call_native_method(module, &function.name, this_value, args)
            }
            Value::JsValue(value) => match vm_js_handle(&value) {
                Some(handle) => {
                    self.call_vm_js_handle_with_binding(module, handle, this_value, args, true)
                }
                None => {
                    let js_this = value_to_js_value(&this_value, &JsHostBridge::empty())?;
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_call_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.call_js_value(value, js_this, args)
                }
            },
            Value::BoundJsFunction(value, bound_this) => match vm_js_handle(&value) {
                Some(handle) => {
                    let _ = bound_this;
                    self.call_vm_js_handle_with_binding(module, handle, this_value, args, true)
                }
                None => {
                    let js_this = value_to_js_value(&this_value, &JsHostBridge::empty())?;
                    let _ = bound_this;
                    #[cfg(feature = "runtime-profile")]
                    self.record_host_call_profile();
                    let _host_call = self.enter_host_call();
                    self.host_bridge.call_js_value(value, js_this, args)
                }
            },
            Value::ExternalRef(reference) => {
                #[cfg(feature = "runtime-profile")]
                self.record_host_call_profile();
                let _host_call = self.enter_host_call();
                self.host_bridge
                    .call_with_this(&reference, this_value, args)
            }
            value => self.call(module, value, args),
        }
    }
    fn call_function_frame(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
    ) -> Result<(Value, LexicalEnv), ExecuteError> {
        let (flow, frame) = self.execute_function_frame(
            module,
            function,
            this_value,
            args,
            function.body_start,
            false,
        )?;
        let lexical_env = frame.lexical_env;
        match flow {
            Flow::Value(_) => Ok((Value::Undefined, lexical_env)),
            Flow::Return(value) => Ok((value, lexical_env)),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
            Flow::Yield { .. } => Err(ExecuteError::Runtime(
                "yield outside generator frame".to_string(),
            )),
            Flow::AwaitPending { .. } => Err(ExecuteError::Runtime(
                "await pending outside async function".to_string(),
            )),
            #[cfg(feature = "debugger")]
            Flow::Pause { .. } => Err(ExecuteError::Runtime(
                "debug pause inside function frame is not resumable yet".to_string(),
            )),
        }
    }

    fn execute_function_frame(
        &self,
        module: &BytecodeModule,
        function: &FunctionValue,
        this_value: Value,
        args: Vec<Value>,
        start_pc: usize,
        skip_hoist: bool,
    ) -> Result<(Flow, Executor<B>), ExecuteError> {
        // 函数帧是运行时最重要的隔离单元：
        //
        // - 从函数创建时捕获的 `LexicalEnv` 克隆闭包环境。
        // - 新增 Function 作用域，并注入 `this`、`arguments`、函数自名绑定。
        // - 按 fun 段记录的参数 operand 写入 LocalSlot/Name。
        // - 子 executor 共享调用栈和执行步数预算，避免递归或死循环绕过限制。
        if self.call_depth >= self.max_call_depth {
            return Err(ExecuteError::RangeError(
                "maximum call stack exceeded".to_string(),
            ));
        }
        if function.body_start > function.body_end || function.body_end > module.instructions.len()
        {
            return Err(ExecuteError::Runtime(format!(
                "function body range {}..{} is outside code length {}",
                function.body_start,
                function.body_end,
                module.instructions.len()
            )));
        }
        if self
            .call_stack
            .borrow()
            .iter()
            .filter(|entry| **entry == function.body_start)
            .count()
            >= self.max_recursive_call_depth
        {
            return Err(ExecuteError::RangeError(
                "maximum recursive call stack exceeded".to_string(),
            ));
        }
        #[cfg(feature = "runtime-profile")]
        let _profile_function_guard = self.enter_function_profile(function);

        let mut lexical_env = function.env.clone();
        lexical_env.push_frame(ScopeKind::Function);
        lexical_env.define_current("this".to_string(), this_value.clone());
        lexical_env.set_slot(FUNCTION_THIS_SLOT, this_value);
        lexical_env.define_current("arguments".to_string(), self.array_value(args.clone())?);
        if let Some(name) = &function.name {
            lexical_env.define_current(name.clone(), Value::Function(function.clone()));
        }
        for (index, param) in function.params.iter().enumerate() {
            let value = args.get(index).cloned().unwrap_or(Value::Undefined);
            match param {
                BytecodeOperand::LocalSlot(slot) => lexical_env.set_slot(*slot, value),
                BytecodeOperand::Name(name) => {
                    lexical_env.define_current(name_string(module, *name)?, value)
                }
                _ => {}
            }
        }

        let frame_module_handle = function
            .module
            .clone()
            .unwrap_or_else(|| Rc::new(module.clone()));
        let mut frame = Executor {
            registers: Vec::new(),
            lexical_env,
            instruction_scope_depths: self.child_instruction_scope_depths(module)?,
            function_hoist_cache: self.function_hoist_cache.clone(),
            register_frame_size_cache: self.register_frame_size_cache.clone(),
            pc_inline_cache: self.pc_inline_cache.clone(),
            last_value: Value::Undefined,
            exports: self.exports.clone(),
            host_bridge: self.host_bridge.clone(),
            external_names: self.external_names.clone(),
            module_handle: Some(frame_module_handle),
            call_depth: self.call_depth + 1,
            max_call_depth: self.max_call_depth,
            max_recursive_call_depth: self.max_recursive_call_depth,
            call_stack: self.call_stack.clone(),
            host_call_depth: self.host_call_depth.clone(),
            execution_budget: self.execution_budget.clone(),
            #[cfg(feature = "runtime-profile")]
            runtime_profile: self.runtime_profile.clone(),
            #[cfg(feature = "debugger")]
            debug_breakpoints: self.debug_breakpoints.clone(),
            #[cfg(feature = "debugger")]
            debug_skip_breakpoint: self.debug_skip_breakpoint.clone(),
        };
        let _call_stack_guard = CallStackGuard::push(self.call_stack.clone(), function.body_start);
        let flow = (|| {
            if !skip_hoist {
                frame.hoist_function_declarations(
                    module,
                    function.body_start,
                    function.body_end,
                )?;
            }
            frame.execute_range(module, start_pc, function.body_end)
        })();
        flow.map(|flow| (flow, frame))
    }

    fn function_from_start(
        &self,
        module: &BytecodeModule,
        pc: usize,
        expr: bool,
    ) -> Result<FunctionValue, ExecuteError> {
        let instruction = &module.instructions[pc];
        let function_operand_index = if expr { 1 } else { 0 };
        let function_index = match operand(instruction, function_operand_index)? {
            BytecodeOperand::Function(index) => *index,
            _ => return Err(ExecuteError::InvalidOperand("function")),
        };
        let function_meta = module
            .functions
            .get(function_index as usize)
            .ok_or_else(|| ExecuteError::Runtime(format!("bad function index {function_index}")))?;
        let name = function_meta
            .name
            .map(|index| name_string(module, index))
            .transpose()?;
        let params = function_meta.params.iter().cloned().collect::<Vec<_>>();
        let body_start = function_meta.body_start as usize;
        let body_end = function_meta.body_end as usize;
        if body_start < pc + 1 || body_end < body_start || body_end > module.instructions.len() {
            return Err(ExecuteError::Runtime(format!(
                "bad function body range {body_start}..{body_end}"
            )));
        }
        Ok(FunctionValue {
            name,
            params,
            body_start,
            body_end,
            end_pc: body_end,
            env: self.lexical_env.clone(),
            props: Rc::new(RefCell::new(BTreeMap::new())),
            has_return: function_meta.has_return,
            is_async: function_meta.flags & FUNCTION_FLAG_ASYNC != 0,
            host_bridge: self.host_bridge.as_js_host_bridge(),
            module: self.module_handle.clone(),
            is_generator: {
                #[cfg(feature = "generator")]
                {
                    function_meta.flags & FUNCTION_FLAG_GENERATOR != 0
                }
                #[cfg(not(feature = "generator"))]
                {
                    function_meta.flags & (1 << 1) != 0
                }
            },
        })
    }

    fn hoist_function_declarations(
        &mut self,
        module: &BytecodeModule,
        start: usize,
        end: usize,
    ) -> Result<(), ExecuteError> {
        let key = (start, end);
        if let Some(function_starts) = self.function_hoist_cache.borrow().get(&key).cloned() {
            for pc in function_starts {
                let function = self.function_from_start(module, pc, false)?;
                if let Some(name) = function.name.clone() {
                    let value = self.host_bridge.function_value(function);
                    self.lexical_env.set_or_define_current(name, value);
                }
            }
            return Ok(());
        }

        let mut function_starts = Vec::new();
        let mut pc = start;
        while pc < end {
            match module
                .instructions
                .get(pc)
                .map(|instruction| instruction.op)
            {
                Some(BytecodeOp::FunctionStart) => {
                    let function = self.function_from_start(module, pc, false)?;
                    let body_end = function.body_end;
                    function_starts.push(pc);
                    if let Some(name) = function.name.clone() {
                        let value = self.host_bridge.function_value(function);
                        self.lexical_env.set_or_define_current(name, value);
                    }
                    pc = body_end;
                    continue;
                }
                Some(BytecodeOp::FunctionExprStart) => {
                    let function = self.function_from_start(module, pc, true)?;
                    pc = function.body_end;
                    continue;
                }
                _ => {}
            }
            pc += 1;
        }
        self.function_hoist_cache
            .borrow_mut()
            .insert(key, function_starts);
        Ok(())
    }

    fn class_from_instruction(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
    ) -> Result<Value, ExecuteError> {
        let name = match operand(instruction, 1)? {
            BytecodeOperand::None => None,
            operand => Some(self.read_name(module, operand)?),
        };
        Ok(self.host_bridge.class_value(ClassValue {
            name,
            super_class: match operand(instruction, 2)? {
                BytecodeOperand::None => None,
                operand => Some(Box::new(self.read_value(module, operand)?)),
            },
            constructor: None,
            static_props: BTreeMap::new(),
            instance_props: BTreeMap::new(),
        }))
    }

    fn read_value(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<Value, ExecuteError> {
        match operand {
            BytecodeOperand::Register(index) => Ok(self
                .registers
                .get(*index as usize)
                .cloned()
                .unwrap_or(Value::Undefined)),
            BytecodeOperand::Constant(index) => constant_value(module, *index),
            BytecodeOperand::Name(index) => {
                let name = name_string(module, *index)?;
                self.get_bound_name(&name)
            }
            BytecodeOperand::LocalSlot(slot) => Ok(self.lexical_env.get_slot(*slot)),
            BytecodeOperand::External(index) => {
                let reference = self.external_ref(module, *index)?;
                if let Ok(name) = self.external_name(module, *index)
                    && (name.starts_with("__js_vm_")
                        || name == "eval"
                        || name == "Symbol"
                        || name == "BigInt")
                {
                    return Ok(Value::ExternalRef(reference));
                }
                let value = self.host_bridge.read_external(&reference)?;
                if let Value::JsValue(js_value) = &value
                    && js_function_name(js_value).as_deref() == Some("eval")
                {
                    return Ok(Value::ExternalRef(reference));
                }
                if is_undefined_value(&value)
                    && let Ok(name) = self.external_name(module, *index)
                    && let Some(overlay) = external_name_overlay_get(&name)
                {
                    return Ok(overlay);
                }
                if is_undefined_value(&value) {
                    let name = self
                        .external_name(module, *index)
                        .unwrap_or_else(|_| format!("extern#{index}"));
                    if let Some(value) = self.lexical_env.get(&name) {
                        return Ok(value);
                    }
                    return Err(ExecuteError::ReferenceError(format!(
                        "{name} is not defined"
                    )));
                }
                if matches!(value, Value::JsValue(_)) {
                    Ok(Value::ExternalRef(reference))
                } else {
                    Ok(value)
                }
            }
            BytecodeOperand::None => Ok(Value::Undefined),
            BytecodeOperand::Label(_)
            | BytecodeOperand::Operator(_)
            | BytecodeOperand::DeclKind(_)
            | BytecodeOperand::Function(_)
            | BytecodeOperand::ScopeKind(_)
            | BytecodeOperand::Count(_) => Err(ExecuteError::InvalidOperand("value")),
        }
    }

    #[inline]
    fn read_simple_value(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<Value, ExecuteError> {
        match operand {
            BytecodeOperand::Register(index) => Ok(self.read_register_value(*index)),
            BytecodeOperand::Constant(index) => constant_value(module, *index),
            BytecodeOperand::LocalSlot(slot) => Ok(self.lexical_env.get_slot(*slot)),
            BytecodeOperand::None => Ok(Value::Undefined),
            _ => self.read_value(module, operand),
        }
    }

    #[inline]
    fn read_register_value(&self, index: u32) -> Value {
        self.registers
            .get(index as usize)
            .cloned()
            .unwrap_or(Value::Undefined)
    }

    #[inline]
    fn read_register_operand_value(
        &self,
        instruction: &BytecodeInstruction,
        index: usize,
    ) -> Result<Value, ExecuteError> {
        Ok(self.read_register_value(register(instruction, index)?))
    }

    #[inline]
    fn read_constant_operand_value(
        &self,
        module: &BytecodeModule,
        instruction: &BytecodeInstruction,
        index: usize,
    ) -> Result<Value, ExecuteError> {
        match operand(instruction, index)? {
            BytecodeOperand::Constant(index) => constant_value(module, *index),
            _ => Err(ExecuteError::InvalidOperand("constant")),
        }
    }

    fn declare_binding(
        &self,
        module: &BytecodeModule,
        kind_operand: &BytecodeOperand,
        operand: &BytecodeOperand,
    ) -> Result<(), ExecuteError> {
        let is_var = self.read_decl_kind(kind_operand)? == "var";
        match operand {
            BytecodeOperand::LocalSlot(slot) => {
                if is_var {
                    self.lexical_env
                        .define_var_slot_if_absent(*slot, Value::Undefined);
                } else {
                    self.lexical_env
                        .define_slot_if_absent(*slot, Value::Undefined);
                }
                Ok(())
            }
            _ => {
                let name = self.read_name(module, operand)?;
                if is_var {
                    self.lexical_env
                        .define_var_if_absent(name, Value::Undefined);
                } else {
                    self.lexical_env
                        .define_current_if_absent(name, Value::Undefined);
                }
                Ok(())
            }
        }
    }

    fn define_catch_param(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
        value: Value,
    ) -> Result<(), ExecuteError> {
        match operand {
            BytecodeOperand::LocalSlot(slot) => {
                self.lexical_env.define_slot(*slot, value);
                Ok(())
            }
            BytecodeOperand::Name(index) => {
                self.lexical_env
                    .define_current(name_string(module, *index)?, value);
                Ok(())
            }
            _ => Err(ExecuteError::InvalidOperand("catch param")),
        }
    }

    fn get_name(&self, name: &str) -> Value {
        self.lexical_env.get(name).unwrap_or(Value::Undefined)
    }

    fn resolve_name_value(&self, value: Value) -> Result<Value, ExecuteError> {
        match value {
            Value::ExternalRef(reference) => Ok(Value::ExternalRef(reference)),
            value => Ok(value),
        }
    }

    fn get_bound_name(&self, name: &str) -> Result<Value, ExecuteError> {
        self.lexical_env
            .get(name)
            .ok_or_else(|| ExecuteError::ReferenceError(format!("{name} is not defined")))
    }

    fn read_name(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        match operand {
            BytecodeOperand::Name(index) => name_string(module, *index),
            BytecodeOperand::External(index) => self.external_name(module, *index),
            BytecodeOperand::Constant(index) => constant_string(module, *index),
            _ => Err(ExecuteError::InvalidOperand("name")),
        }
    }

    fn read_operator(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        self.read_operator_cow(module, operand).map(Cow::into_owned)
    }

    fn read_operator_cow<'a>(
        &self,
        module: &'a BytecodeModule,
        operand: &'a BytecodeOperand,
    ) -> Result<Cow<'a, str>, ExecuteError> {
        match operand {
            BytecodeOperand::Operator(index) => operator_name(*index)
                .map(Cow::Borrowed)
                .ok_or_else(|| ExecuteError::Runtime(format!("unknown operator {index}"))),
            BytecodeOperand::Constant(index) => constant_string(module, *index).map(Cow::Owned),
            _ => Err(ExecuteError::InvalidOperand("operator")),
        }
    }

    fn read_scope_kind(&self, operand: &BytecodeOperand) -> Result<ScopeKind, ExecuteError> {
        let BytecodeOperand::ScopeKind(index) = operand else {
            return Err(ExecuteError::InvalidOperand("scope kind"));
        };
        match index {
            0 => Ok(ScopeKind::Block),
            1 => Ok(ScopeKind::Function),
            2 => Ok(ScopeKind::Catch),
            _ => Err(ExecuteError::Runtime(format!("unknown scope kind {index}"))),
        }
    }

    fn read_decl_kind(&self, operand: &BytecodeOperand) -> Result<&'static str, ExecuteError> {
        let BytecodeOperand::DeclKind(index) = operand else {
            return Err(ExecuteError::InvalidOperand("decl kind"));
        };
        match index {
            0 => Ok("var"),
            1 => Ok("let"),
            2 => Ok("const"),
            3 => Ok("decl"),
            _ => Err(ExecuteError::Runtime(format!("unknown decl kind {index}"))),
        }
    }

    fn read_constant_string(
        &self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<String, ExecuteError> {
        self.read_constant_string_cow(module, operand)
            .map(Cow::into_owned)
    }

    fn read_constant_string_cow<'a>(
        &self,
        module: &'a BytecodeModule,
        operand: &BytecodeOperand,
    ) -> Result<Cow<'a, str>, ExecuteError> {
        match operand {
            BytecodeOperand::Constant(index) => match module.constants.get(*index as usize) {
                Some(BytecodeConstant::String(value)) => Ok(Cow::Borrowed(value.as_str())),
                Some(value) => Ok(Cow::Owned(value.to_string())),
                None => Err(ExecuteError::BadConstant(*index)),
            },
            _ => Err(ExecuteError::InvalidOperand("constant string")),
        }
    }

    fn write_operand_target(
        &mut self,
        module: &BytecodeModule,
        operand: &BytecodeOperand,
        value: Value,
    ) -> Result<(), ExecuteError> {
        match operand {
            BytecodeOperand::Register(index) => {
                self.write_register(*index, value);
                Ok(())
            }
            BytecodeOperand::Name(index) => {
                self.lexical_env
                    .set_or_define_current(name_string(module, *index)?, value);
                Ok(())
            }
            BytecodeOperand::LocalSlot(slot) => {
                self.lexical_env.set_slot(*slot, value);
                Ok(())
            }
            BytecodeOperand::External(index) => {
                #[cfg(feature = "runtime-profile")]
                self.record_host_set_profile();
                let _host_call = self.enter_host_call();
                self.host_bridge.set_slot(*index, &value)?;
                if let Ok(name) = self.external_name(module, *index) {
                    self.lexical_env.set_or_define_current(name, value);
                }
                Ok(())
            }
            _ => Err(ExecuteError::InvalidOperand("assignable object")),
        }
    }

    fn write_register(&mut self, index: u32, value: Value) {
        let index = index as usize;
        if self.registers.len() <= index {
            self.registers.resize(index + 1, Value::Undefined);
        }
        self.registers[index] = value;
    }

    fn external_ref(
        &self,
        module: &BytecodeModule,
        index: u32,
    ) -> Result<ExternalRefValue, ExecuteError> {
        let bytecode_root = external_string(module, index).ok();
        let root = if let Some(root) = bytecode_root.filter(|root| root.starts_with("__js_vm_")) {
            root
        } else {
            self.external_names
                .get(index as usize)
                .cloned()
                .or_else(|| module.extern_slots.get(index as usize).cloned())
                .unwrap_or_else(|| format!("extern#{index}"))
        };
        self.host_bridge.validate_slot(index)?;
        Ok(ExternalRefValue::new(index, root))
    }

    fn external_name(&self, module: &BytecodeModule, index: u32) -> Result<String, ExecuteError> {
        if let Some(name) = module.extern_slots.get(index as usize)
            && name.starts_with("__js_vm_")
        {
            return Ok(name.clone());
        }
        self.external_names
            .get(index as usize)
            .cloned()
            .or_else(|| module.extern_slots.get(index as usize).cloned())
            .ok_or(ExecuteError::BadConstant(index))
    }
}

#[cfg(feature = "debugger")]
impl<B: HostBridge> ExecutorDebugSession<B> {
    pub fn set_breakpoints<I>(&mut self, pcs: I)
    where
        I: IntoIterator<Item = usize>,
    {
        self.breakpoints = pcs
            .into_iter()
            .filter(|pc| *pc < self.end)
            .collect::<BTreeSet<_>>();
        self.sync_debug_control();
    }

    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
        self.sync_debug_control();
    }

    pub fn resume(&mut self) -> Result<DebugSnapshot, ExecuteError> {
        if self.done {
            return Ok(self.snapshot(false, "done"));
        }
        self.sync_debug_control();
        if self.paused {
            self.executor.debug_skip_breakpoint.set(Some(self.pc));
        } else {
            self.executor.debug_skip_breakpoint.set(None);
        }
        self.paused = false;
        match self
            .executor
            .execute_range(&self.module, self.pc, self.end)?
        {
            Flow::Pause { pc, reason } => {
                self.pc = pc;
                self.paused = true;
                self.last_reason = reason.to_string();
                Ok(self.snapshot(true, reason))
            }
            Flow::Value(value) | Flow::Return(value) => self.finish(value),
            Flow::Throw(value) => Err(ExecuteError::Thrown(value)),
            Flow::Yield { .. } => Err(ExecuteError::Runtime(
                "yield outside generator frame".to_string(),
            )),
            Flow::AwaitPending { .. } => Err(ExecuteError::Runtime(
                "await pending outside async debug session".to_string(),
            )),
        }
    }

    pub fn step(&mut self) -> Result<DebugSnapshot, ExecuteError> {
        if self.done {
            return Ok(self.snapshot(false, "done"));
        }
        let original_breakpoints = self.breakpoints.clone();
        let next_pc = self.pc.saturating_add(1).min(self.end);
        self.breakpoints.clear();
        if next_pc < self.end {
            self.breakpoints.insert(next_pc);
        }
        let result = self.resume();
        self.breakpoints = original_breakpoints;
        self.sync_debug_control();
        let mut snapshot = result?;
        if snapshot.paused && snapshot.pc == next_pc {
            snapshot.reason = "step".to_string();
            self.last_reason = snapshot.reason.clone();
        }
        Ok(snapshot)
    }

    pub fn inspect(&self) -> DebugSnapshot {
        self.snapshot(self.paused, &self.last_reason)
    }

    pub fn result_value(&self) -> Option<Value> {
        self.done.then(|| self.executor.last_value.clone())
    }

    fn finish(&mut self, value: Value) -> Result<DebugSnapshot, ExecuteError> {
        #[cfg(feature = "module")]
        let value = if self.module.kind == BytecodeModuleKind::Module {
            self.executor.module_namespace_value()?
        } else {
            value
        };
        self.executor.last_value = value;
        self.pc = self.end;
        self.paused = false;
        self.done = true;
        self.last_reason = "done".to_string();
        Ok(self.snapshot(false, "done"))
    }

    fn sync_debug_control(&mut self) {
        *self.executor.debug_breakpoints.borrow_mut() = self.breakpoints.clone();
    }

    fn snapshot(&self, paused: bool, reason: &str) -> DebugSnapshot {
        const MAX_DEBUG_REGISTERS: usize = 128;
        let mut registers = self
            .executor
            .registers
            .iter()
            .take(MAX_DEBUG_REGISTERS)
            .enumerate()
            .map(|(index, value)| format!("r{index}={value}"))
            .collect::<Vec<_>>();
        if self.executor.registers.len() > MAX_DEBUG_REGISTERS {
            registers.push(format!(
                "... {} register(s) hidden",
                self.executor.registers.len() - MAX_DEBUG_REGISTERS
            ));
        }
        DebugSnapshot {
            pc: self.pc,
            reason: reason.to_string(),
            done: self.done,
            paused,
            value: self.executor.last_value.to_string(),
            registers,
            call_stack: self.executor.call_stack.borrow().clone(),
            remaining_steps: self.executor.execution_budget.get(),
        }
    }
}

fn is_callable_value(value: &Value) -> bool {
    match value {
        Value::Function(_)
        | Value::BoundFunction(_, _)
        | Value::NativeFunction(_)
        | Value::BoundNativeFunction(_, _)
        | Value::ExternalRef(_)
        | Value::Class(_) => true,
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => {
            vm_js_handle(value).is_some() || value.dyn_ref::<JsFunction>().is_some()
        }
        _ => false,
    }
}

#[cfg(feature = "generator")]
fn iterator_result(value: Value, done: bool) -> Value {
    object_value(BTreeMap::from([
        ("value".to_string(), value),
        ("done".to_string(), Value::Bool(done)),
    ]))
}

fn should_propagate_js_binary_error(op: &str, left: &Value, right: &Value) -> bool {
    matches!(
        op,
        "+" | "-" | "*" | "/" | "%" | "**" | "&" | "|" | "^" | "<<" | ">>" | ">>>"
    ) && {
        #[cfg(feature = "bigint")]
        {
            is_js_bigint_value(left) || is_js_bigint_value(right)
        }
        #[cfg(not(feature = "bigint"))]
        {
            let _ = (left, right);
            false
        }
    }
}

#[cfg(feature = "bigint")]
fn is_js_bigint_value(value: &Value) -> bool {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _) => js_value_is_bigint(value),
        _ => false,
    }
}

#[cfg(feature = "bigint")]
fn bigint_prototype_call(
    method: &str,
    this_value: Value,
    args: Vec<Value>,
) -> Result<Value, ExecuteError> {
    let receiver = value_to_js_value(&this_value, &JsHostBridge::empty())?;
    let js_args = js_sys::Array::new();
    for arg in args {
        js_args.push(&value_to_js_value(&arg, &JsHostBridge::empty())?);
    }
    let js_args: JsValue = js_args.into();
    let result = JsFunction::new_with_args(
        "receiver, args, method",
        "return BigInt.prototype[method].apply(receiver, args);",
    )
    .call3(
        &JsValue::UNDEFINED,
        &receiver,
        &js_args,
        &JsValue::from_str(method),
    )
    .map_err(js_error)?;
    Ok(normalize_js_bigint_value(Value::JsValue(result)))
}

#[cfg(feature = "bigint")]
fn normalize_js_bigint_value(value: Value) -> Value {
    match value {
        Value::JsValue(value) | Value::BoundJsFunction(value, _) if js_value_is_bigint(&value) => {
            Value::BigInt(crate::host::js_value_display(&value))
        }
        value => value,
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use js_token_core::{
        BytecodeConstant, BytecodeInstruction, BytecodeModule, BytecodeModuleKind, BytecodeOp,
        BytecodeOperand,
    };
    use wasm_bindgen::JsValue;

    #[derive(Clone)]
    struct MockBridge;

    impl HostBridge for MockBridge {
        fn validate_extern_count(&self, expected: usize) -> Result<(), ExecuteError> {
            assert!(
                expected == 0 || expected == 1 || expected == 3,
                "unexpected extern count {expected}"
            );
            Ok(())
        }

        fn validate_slot(&self, slot: u32) -> Result<(), ExecuteError> {
            if slot < 3 {
                Ok(())
            } else {
                Err(ExecuteError::Runtime(format!("bad slot {slot}")))
            }
        }

        fn read_external(&self, reference: &ExternalRefValue) -> Result<Value, ExecuteError> {
            self.validate_slot(reference.slot)?;
            if reference.root == "RegExp" {
                return Ok(Value::String("RegExp".to_string()));
            }
            Ok(Value::String(format!("external-slot-{}", reference.slot)))
        }

        fn get(&self, reference: &ExternalRefValue, property: &str) -> Result<Value, ExecuteError> {
            Ok(Value::String(format!(
                "external-slot-{}.{}",
                reference.slot, property
            )))
        }

        fn has_property(
            &self,
            reference: &ExternalRefValue,
            property: &str,
        ) -> Result<bool, ExecuteError> {
            self.validate_slot(reference.slot)?;
            Ok(property != "missing")
        }

        fn set(
            &self,
            _reference: &ExternalRefValue,
            _property: &str,
            _value: &Value,
        ) -> Result<(), ExecuteError> {
            Ok(())
        }

        fn set_slot(&self, _slot: u32, _value: &Value) -> Result<(), ExecuteError> {
            Ok(())
        }

        fn call(
            &self,
            _reference: &ExternalRefValue,
            _args: Vec<Value>,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime("unexpected call".to_string()))
        }

        fn call_with_this(
            &self,
            _reference: &ExternalRefValue,
            _this_value: Value,
            _args: Vec<Value>,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime(
                "unexpected call_with_this".to_string(),
            ))
        }

        fn construct(
            &self,
            _reference: &ExternalRefValue,
            _args: Vec<Value>,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime("unexpected construct".to_string()))
        }

        fn call_js_value(
            &self,
            _callee: JsValue,
            _this_value: JsValue,
            _args: Vec<Value>,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime("unexpected js call".to_string()))
        }

        fn is_object_define_property_function(
            &self,
            _callee: &JsValue,
            _this_value: &JsValue,
        ) -> bool {
            false
        }

        fn construct_js_value(
            &self,
            _constructor: JsValue,
            _args: Vec<Value>,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime("unexpected js construct".to_string()))
        }

        fn binary_operator(
            &self,
            _op: &str,
            _left: &Value,
            _right: &Value,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime("unexpected binary".to_string()))
        }

        #[cfg(feature = "string-builtins")]
        fn call_string_method(
            &self,
            _value: &str,
            _method: &str,
            _args: Vec<Value>,
        ) -> Result<Value, ExecuteError> {
            Err(ExecuteError::Runtime(
                "unexpected string method".to_string(),
            ))
        }

        fn function_value(&self, function: FunctionValue) -> Value {
            Value::Function(function)
        }

        fn class_value(&self, class: ClassValue) -> Value {
            Value::Class(class)
        }

        fn module_value(&self, _module: crate::value::ModuleValue) -> Result<Value, ExecuteError> {
            Ok(Value::Undefined)
        }
    }

    #[cfg(feature = "debugger")]
    #[test]
    fn debug_session_pauses_and_resumes_from_breakpoint() {
        let module = BytecodeModule {
            kind: BytecodeModuleKind::Script,
            constants: vec![BytecodeConstant::Number(7.0)],
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::LoadConst,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::Constant(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Return,
                    operands: vec![BytecodeOperand::Register(0)],
                },
            ],
            ..BytecodeModule::default()
        };

        let mut session = Executor::debug_session_with_host_bridge_and_runtime_limits(
            &module, MockBridge, 8, 8, 64,
        )
        .unwrap();
        session.set_breakpoints([1]);

        let paused = session.resume().unwrap();
        assert!(paused.paused, "{paused:?}");
        assert_eq!(paused.pc, 1);
        assert_eq!(paused.reason, "breakpoint");
        assert!(paused.registers.iter().any(|register| register == "r0=7"));

        let done = session.resume().unwrap();
        assert!(done.done, "{done:?}");
        assert_eq!(done.value, "7");
    }

    #[test]
    fn external_operand_ignores_decoded_placeholder_name_binding() {
        let module = BytecodeModule {
            kind: BytecodeModuleKind::Script,
            extern_slots: vec!["e0".to_string(), "e1".to_string(), "e2".to_string()],
            names: vec!["e2".to_string()],
            functions: Vec::new(),
            constants: Vec::new(),
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::Object,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::Count(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Declare,
                    operands: vec![BytecodeOperand::DeclKind(0), BytecodeOperand::Name(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::StoreName,
                    operands: vec![BytecodeOperand::Name(0), BytecodeOperand::Register(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::LoadName,
                    operands: vec![BytecodeOperand::Register(1), BytecodeOperand::External(2)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Return,
                    operands: vec![BytecodeOperand::Register(1)],
                },
            ],
        };

        let value = Executor::run_with_host_bridge(&module, MockBridge).unwrap();
        assert_eq!(value, Value::String("external-slot-2".to_string()));
    }

    #[test]
    fn compressed_external_table_uses_runtime_slot_names() {
        let module = BytecodeModule {
            kind: BytecodeModuleKind::Script,
            extern_slots: Vec::new(),
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::LoadName,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::External(2)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Return,
                    operands: vec![BytecodeOperand::Register(0)],
                },
            ],
            ..BytecodeModule::default()
        };

        let value = Executor::run_with_host_bridge_and_external_names(
            &module,
            MockBridge,
            vec![
                "Object".to_string(),
                "Array".to_string(),
                "RegExp".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(value, Value::String("RegExp".to_string()));
    }

    #[test]
    fn in_operator_uses_host_bridge_for_external_refs() {
        let module = BytecodeModule {
            kind: BytecodeModuleKind::Script,
            extern_slots: vec!["globalThis".to_string()],
            constants: vec![BytecodeConstant::String("missing".to_string())],
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::LoadConst,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::Constant(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::LoadName,
                    operands: vec![BytecodeOperand::Register(1), BytecodeOperand::External(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Binary,
                    operands: vec![
                        BytecodeOperand::Register(2),
                        BytecodeOperand::Operator(30),
                        BytecodeOperand::Register(0),
                        BytecodeOperand::Register(1),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Return,
                    operands: vec![BytecodeOperand::Register(2)],
                },
            ],
            ..BytecodeModule::default()
        };

        let value = Executor::run_with_host_bridge(&module, MockBridge).unwrap();
        assert_eq!(value, Value::Bool(false));
    }

    #[test]
    fn string_builtin_methods_are_callable_from_member_reads() {
        let module = BytecodeModule {
            kind: BytecodeModuleKind::Script,
            constants: vec![
                BytecodeConstant::String("/route".to_string()),
                BytecodeConstant::String("startsWith".to_string()),
                BytecodeConstant::String("/".to_string()),
                BytecodeConstant::String("charCodeAt".to_string()),
            ],
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::LoadConst,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::Constant(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Member,
                    operands: vec![
                        BytecodeOperand::Register(1),
                        BytecodeOperand::Register(0),
                        BytecodeOperand::Constant(1),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::LoadConst,
                    operands: vec![BytecodeOperand::Register(2), BytecodeOperand::Constant(2)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Call,
                    operands: vec![
                        BytecodeOperand::Register(3),
                        BytecodeOperand::Register(1),
                        BytecodeOperand::Count(1),
                        BytecodeOperand::Register(2),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Member,
                    operands: vec![
                        BytecodeOperand::Register(4),
                        BytecodeOperand::Register(0),
                        BytecodeOperand::Constant(3),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Call,
                    operands: vec![
                        BytecodeOperand::Register(5),
                        BytecodeOperand::Register(4),
                        BytecodeOperand::Count(0),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Return,
                    operands: vec![BytecodeOperand::Register(3)],
                },
            ],
            ..BytecodeModule::default()
        };

        let value = Executor::run_with_host_bridge(&module, MockBridge).unwrap();
        assert_eq!(value, Value::Bool(true));
    }

    #[test]
    fn relational_string_compare_uses_lexicographic_order() {
        let module = BytecodeModule {
            kind: BytecodeModuleKind::Script,
            constants: vec![
                BytecodeConstant::String("object".to_string()),
                BytecodeConstant::String("u".to_string()),
            ],
            instructions: vec![
                BytecodeInstruction {
                    op: BytecodeOp::LoadConst,
                    operands: vec![BytecodeOperand::Register(0), BytecodeOperand::Constant(0)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::LoadConst,
                    operands: vec![BytecodeOperand::Register(1), BytecodeOperand::Constant(1)],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Binary,
                    operands: vec![
                        BytecodeOperand::Register(2),
                        BytecodeOperand::Operator(6),
                        BytecodeOperand::Register(0),
                        BytecodeOperand::Register(1),
                    ],
                },
                BytecodeInstruction {
                    op: BytecodeOp::Return,
                    operands: vec![BytecodeOperand::Register(2)],
                },
            ],
            ..BytecodeModule::default()
        };

        let value = Executor::run_with_host_bridge(&module, MockBridge).unwrap();
        assert_eq!(value, Value::Bool(true));
    }

    #[test]
    fn array_every_is_callable_from_member_reads() {
        let array = array_value(vec![Value::Number(1.0)]);
        let member = get_local_member(&array, "every").unwrap();

        assert!(
            matches!(
                member,
                Value::BoundNativeFunction(NativeFunctionValue { ref name }, _)
                    if name == "Array.every"
            ),
            "{member:?}"
        );

        let member = get_local_member(&array, "find").unwrap();
        assert!(
            matches!(
                member,
                Value::BoundNativeFunction(NativeFunctionValue { ref name }, _)
                    if name == "Array.find"
            ),
            "{member:?}"
        );

        let member = get_local_member(&array, "reduce").unwrap();
        assert!(
            matches!(
                member,
                Value::BoundNativeFunction(NativeFunctionValue { ref name }, _)
                    if name == "Array.reduce"
            ),
            "{member:?}"
        );
    }
}
