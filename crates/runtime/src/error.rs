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
    TypeError(String),
    RangeError(String),
    Runtime(String),
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecuteError::MissingOperand { op, index } => {
                write!(f, "{op} missing operand {index}")
            }
            ExecuteError::InvalidOperand(kind) => write!(f, "invalid {kind} operand"),
            ExecuteError::BadConstant(index) => write!(f, "bad constant index {index}"),
            ExecuteError::UnknownLabel(label) => write!(f, "unknown label {label}"),
            ExecuteError::Unsupported(op) => write!(f, "unsupported opcode {op}"),
            ExecuteError::Thrown(value) => write!(f, "uncaught exception {value}"),
            ExecuteError::TypeError(message) => write!(f, "TypeError: {message}"),
            ExecuteError::RangeError(message) => write!(f, "RangeError: {message}"),
            ExecuteError::Runtime(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ExecuteError {}
