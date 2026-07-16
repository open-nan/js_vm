use crate::value::Value;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum ExecuteError {
    MissingOperand { op: &'static str, index: usize },
    InvalidOperand(&'static str),
    BadConstant(u32),
    UnknownLabel(String),
    Unsupported(&'static str),
    Thrown(Value),
    ReferenceError(String),
    TypeError(String),
    RangeError(String),
    SyntaxError(String),
    Runtime(String),
}

#[cfg(feature = "compact-errors")]
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
