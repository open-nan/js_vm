//! 执行错误模型。
//!
//! 默认构建保留详细错误信息，方便定位 pc、callee、property 等问题。
//! 开启 `compact-errors` feature 后，错误会压缩成短错误码，用于减小 runtime wasm 体积。

use crate::value::Value;
use std::fmt;

/// bytecode 执行错误。
///
/// 这里同时承载 VM 内部错误和 JavaScript 语义错误。能被 JS `try/catch` 捕获的错误会在
/// executor 中转成错误对象；真正的解码/运行时结构错误会直接返回给调用方。
#[derive(Debug, Clone, PartialEq)]
pub enum ExecuteError {
    /// 指令缺少指定位置的操作数。
    MissingOperand { op: &'static str, index: usize },
    /// 操作数类型不符合指令预期。
    InvalidOperand(&'static str),
    /// 常量池下标无效。
    BadConstant(u32),
    /// 跳转标签不存在。
    UnknownLabel(String),
    /// 当前构建未启用或尚未实现的语义。
    Unsupported(&'static str),
    /// JavaScript `throw` 抛出的值。
    Thrown(Value),
    /// JavaScript ReferenceError。
    ReferenceError(String),
    /// JavaScript TypeError。
    TypeError(String),
    /// JavaScript RangeError。
    RangeError(String),
    /// JavaScript SyntaxError。
    SyntaxError(String),
    /// VM runtime 错误。
    Runtime(String),
}

#[cfg(feature = "compact-errors")]
/// 紧凑错误模式：保留错误码，丢弃格式化细节以减小 wasm 字符串段。
macro_rules! compact_error_message {
    ($code:literal) => {
        $code.to_string()
    };
    ($code:literal, $($arg:tt)*) => {{
        let _ = stringify!($($arg)*);
        $code.to_string()
    }};
}

#[cfg(not(feature = "compact-errors"))]
/// 默认错误模式：保留完整错误信息。
macro_rules! compact_error_message {
    ($code:literal) => {
        $code.to_string()
    };
    ($code:literal, $($arg:tt)*) => {
        format!($($arg)*)
    };
}

macro_rules! runtime_error {
    ($($arg:tt)*) => {
        $crate::error::ExecuteError::Runtime($crate::error::compact_error_message!("E_RUNTIME", $($arg)*))
    };
}

macro_rules! type_error {
    ($($arg:tt)*) => {
        $crate::error::ExecuteError::TypeError($crate::error::compact_error_message!("E_TYPE", $($arg)*))
    };
}

macro_rules! range_error {
    ($($arg:tt)*) => {
        $crate::error::ExecuteError::RangeError($crate::error::compact_error_message!("E_RANGE", $($arg)*))
    };
}

macro_rules! reference_error {
    ($($arg:tt)*) => {
        $crate::error::ExecuteError::ReferenceError($crate::error::compact_error_message!("E_REFERENCE", $($arg)*))
    };
}

pub(crate) use compact_error_message;
pub(crate) use range_error;
pub(crate) use reference_error;
pub(crate) use runtime_error;
pub(crate) use type_error;

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(feature = "compact-errors")]
        {
            return f.write_str(match self {
                ExecuteError::MissingOperand { .. } => "E_MISSING_OPERAND",
                ExecuteError::InvalidOperand(_) => "E_INVALID_OPERAND",
                ExecuteError::BadConstant(_) => "E_BAD_CONSTANT",
                ExecuteError::UnknownLabel(_) => "E_UNKNOWN_LABEL",
                ExecuteError::Unsupported(_) => "E_UNSUPPORTED",
                ExecuteError::Thrown(_) => "E_THROWN",
                ExecuteError::ReferenceError(_) => "E_REFERENCE",
                ExecuteError::TypeError(_) => "E_TYPE",
                ExecuteError::RangeError(_) => "E_RANGE",
                ExecuteError::SyntaxError(_) => "E_SYNTAX",
                ExecuteError::Runtime(_) => "E_RUNTIME",
            });
        }
        #[cfg(not(feature = "compact-errors"))]
        match self {
            ExecuteError::MissingOperand { op, index } => {
                write!(f, "{op} missing operand {index}")
            }
            ExecuteError::InvalidOperand(kind) => write!(f, "invalid {kind} operand"),
            ExecuteError::BadConstant(index) => write!(f, "bad constant index {index}"),
            ExecuteError::UnknownLabel(label) => write!(f, "unknown label {label}"),
            ExecuteError::Unsupported(op) => write!(f, "unsupported opcode {op}"),
            ExecuteError::Thrown(value) => write!(f, "uncaught exception {value}"),
            ExecuteError::ReferenceError(message) => write!(f, "ReferenceError: {message}"),
            ExecuteError::TypeError(message) => write!(f, "TypeError: {message}"),
            ExecuteError::RangeError(message) => write!(f, "RangeError: {message}"),
            ExecuteError::SyntaxError(message) => write!(f, "SyntaxError: {message}"),
            ExecuteError::Runtime(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ExecuteError {}
