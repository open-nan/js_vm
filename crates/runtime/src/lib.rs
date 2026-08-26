//! JS VM 执行器核心。
//!
//! 该 crate 只实现 bytecode 解释执行、作用域模型、Value 模型和 HostBridge trait。
//! 浏览器/Node 的具体 wasm 绑定位于 `runtime/bin/browser` 和 `runtime/bin/node`。
//!
//! 运行时主流程：
//! 1. `BytecodeModule` 由 Core Layer 解码得到。
//! 2. `Executor` 按 pc 顺序执行指令，并维护寄存器、词法环境和调用栈。
//! 3. 遇到 extern/global/宿主对象时，通过 `HostBridge` 访问真实环境。
//! 4. script 返回最后一个表达式值，module 返回模块 namespace 对象。

mod env;
mod error;
mod executor;
mod host;
mod ops;
mod value;

pub use env::{EnvironmentRecord, LexicalEnv, ScopeFrame, ScopeKind};
pub use error::ExecuteError;
pub use executor::{
    DEFAULT_MAX_CALL_DEPTH, DEFAULT_MAX_EXECUTION_STEPS, DEFAULT_MAX_RECURSIVE_CALL_DEPTH, Executor,
};
#[cfg(feature = "debugger")]
pub use executor::{DebugSnapshot, ExecutorDebugSession};
#[cfg(feature = "runtime-profile")]
pub use executor::{
    RuntimeProfile, RuntimeProfileEntry, RuntimeProfileFunctionEntry, RuntimeProfilePcEntry,
};
pub use host::{HostBridge, HostValue, JsHostBridge, value_to_js_value};
pub use value::{
    ClassValue, ExternalRefValue, FunctionValue, ModuleValue, NativeFunctionValue, Value,
};
