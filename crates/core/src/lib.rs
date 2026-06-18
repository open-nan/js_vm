use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::{self, Write},
};

#[derive(Debug, Clone, PartialEq)]
pub enum IrValue {
    Register(String),
    Name(String),
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Undefined,
}

impl fmt::Display for IrValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrValue::Register(value) => write!(f, "%{value}"),
            IrValue::Name(value) => write!(f, "{value}"),
            IrValue::Number(value) => write!(f, "{value}"),
            IrValue::String(value) => write!(f, "{value:?}"),
            IrValue::Bool(value) => write!(f, "{value}"),
            IrValue::Null => write!(f, "null"),
            IrValue::Undefined => write!(f, "undefined"),
        }
    }
}

/// 中间表示(IR)指令枚举，定义了编译器中间表示的各种操作指令
#[derive(Debug, Clone, PartialEq)]
pub enum IrInstruction {
    /// 标记指令，用于代码生成过程中的调试或临时标记
    Marker(String),
    /// 标签指令，用于跳转目标位置的标记
    Label(String),
    /// 声明指令，用于声明变量、函数等
    Declare {
        /// 声明类型（如"var"、"let"、"const"等）
        kind: String,
        /// 声明的名称
        name: String,
    },
    /// 加载常量指令，将常量值加载到目标寄存器
    LoadConst {
        /// 目标寄存器名称
        dst: String,
        /// 要加载的常量值
        value: IrValue,
    },
    /// 加载变量指令，从变量名加载值到目标寄存器
    LoadName {
        /// 目标寄存器名称
        dst: String,
        /// 要加载的变量名称
        name: String,
    },
    /// 存储变量指令，将源值存储到指定变量名
    StoreName {
        /// 目标变量名称
        name: String,
        /// 要存储的源值
        src: IrValue,
    },
    /// 存储成员指令，将源值存储到对象的指定属性
    StoreMember {
        /// 目标对象
        object: IrValue,
        /// 目标属性名称
        property: String,
        /// 要存储的源值
        src: IrValue,
    },
    /// 移动指令，将源值移动到目标寄存器
    Move {
        /// 目标寄存器名称
        dst: String,
        /// 要移动的源值
        src: IrValue,
    },
    /// 二元运算指令，执行二元运算并将结果存储到目标寄存器
    Binary {
        /// 目标寄存器名称
        dst: String,
        /// 运算操作符（如"+"、"-"、"*"、"/"等）
        op: String,
        /// 左操作数
        left: IrValue,
        /// 右操作数
        right: IrValue,
    },
    /// 一元运算指令，执行一元运算并将结果存储到目标寄存器
    Unary {
        /// 目标寄存器名称
        dst: String,
        /// 运算操作符（如"!"、"-"、"typeof"等）
        op: String,
        /// 操作数
        arg: IrValue,
    },
    /// 成员访问指令，获取对象属性值并存储到目标寄存器
    Member {
        /// 目标寄存器名称
        dst: String,
        /// 目标对象
        object: IrValue,
        /// 属性名称
        property: String,
    },
    /// 数组创建指令，创建数组并将结果存储到目标寄存器
    Array {
        /// 目标寄存器名称
        dst: String,
        /// 数组元素列表
        items: Vec<IrValue>,
    },
    /// 对象创建指令，创建对象并将结果存储到目标寄存器
    Object {
        /// 目标寄存器名称
        dst: String,
        /// 对象属性列表（键值对）
        props: Vec<(String, IrValue)>,
    },
    /// 函数调用指令，调用函数并将返回值存储到目标寄存器
    Call {
        /// 目标寄存器名称
        dst: String,
        /// 被调用的函数
        callee: IrValue,
        /// 函数参数列表
        args: Vec<IrValue>,
    },
    /// 构造函数调用指令，使用new关键字调用构造函数并将实例存储到目标寄存器
    New {
        /// 目标寄存器名称
        dst: String,
        /// 被调用的构造函数
        callee: IrValue,
        /// 构造函数参数列表
        args: Vec<IrValue>,
    },
    /// 模板字符串指令，处理模板字符串并将结果存储到目标寄存器
    Template {
        /// 目标寄存器名称
        dst: String,
        /// 模板字符串的静态部分
        quasis: Vec<String>,
        /// 模板字符串中的表达式部分
        exprs: Vec<IrValue>,
    },
    /// 函数定义指令，定义命名函数
    Function {
        /// 函数名称
        name: String,
        /// 函数参数列表
        params: Vec<String>,
        /// 函数体指令列表
        body: Vec<IrInstruction>,
    },
    /// 函数表达式指令，定义函数表达式并将其存储到目标寄存器
    FunctionExpr {
        /// 目标寄存器名称
        dst: String,
        /// 函数名称（可选，匿名函数为None）
        name: Option<String>,
        /// 函数参数列表
        params: Vec<String>,
        /// 函数体指令列表
        body: Vec<IrInstruction>,
    },
    /// 类定义指令，定义类
    Class {
        /// 目标寄存器名称（可选，若需将类存储到变量则指定）
        dst: Option<String>,
        /// 类名称（可选，匿名类为None）
        name: Option<String>,
        /// 父类（可选，无继承时为None）
        super_class: Option<IrValue>,
        /// 类成员列表（方法/属性的IR表示）
        members: Vec<String>,
    },
    /// 导入指令，处理模块导入
    Import {
        /// 导入源路径
        source: String,
        /// 导入的标识符列表
        specifiers: Vec<String>,
    },
    /// 导出指令，处理模块导出
    Export {
        /// 导出类型（如"named"、"default"、"all"等）
        kind: String,
        /// 导出的标识符列表
        names: Vec<String>,
    },
    /// 抛出异常指令，抛出指定值作为异常
    Throw(IrValue),
    /// 异常处理指令，处理try-catch-finally结构
    Try {
        /// try块的指令列表
        body: Vec<IrInstruction>,
        /// catch块的参数名称（可选，无参数时为None）
        catch_param: Option<String>,
        /// catch块的指令列表
        catch_body: Vec<IrInstruction>,
        /// finally块的指令列表
        finally_body: Vec<IrInstruction>,
    },
    /// 返回指令，从函数返回值（可选，无返回值时为None）
    Return(Option<IrValue>),
    /// 弹出指令，弹出栈顶值（通常用于处理无副作用的表达式结果）
    Pop(IrValue),
    /// 无条件跳转指令，跳转到指定标签
    Jump(String),
    /// 条件跳转指令，当测试值为false时跳转到指定标签
    JumpIfFalse {
        /// 测试条件值
        test: IrValue,
        /// 跳转目标标签
        label: String,
    },
    /// 不支持的指令，用于标记编译器暂不支持的语法结构
    Unsupported(String),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IrModule {
    pub extern_slots: Vec<String>,
    pub instructions: Vec<IrInstruction>,
}
impl IrModule {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if !self.extern_slots.is_empty() {
            let _ = writeln!(out, ".externs");
            for (index, name) in self.extern_slots.iter().enumerate() {
                let _ = writeln!(out, "  e{index} = {name}");
            }
        }
        for instruction in &self.instructions {
            instruction.write_text(&mut out, 0);
        }
        out
    }

    pub fn to_bytecode(&self) -> BytecodeModule {
        BytecodeBuilder::default().compile_module(self)
    }
}

impl IrInstruction {
    fn write_text(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        match self {
            IrInstruction::Marker(message) => {
                let _ = writeln!(out, "{pad}{message}");
            }
            IrInstruction::Label(label) => {
                let _ = writeln!(out, "{pad}{label}:");
            }
            IrInstruction::Declare { kind, name } => {
                let _ = writeln!(out, "{pad}declare {kind} {name}");
            }
            IrInstruction::LoadConst { dst, value } => {
                let _ = writeln!(out, "{pad}%{dst} = const {value}");
            }
            IrInstruction::LoadName { dst, name } => {
                let _ = writeln!(out, "{pad}%{dst} = load {name}");
            }
            IrInstruction::StoreName { name, src } => {
                let _ = writeln!(out, "{pad}store {name}, {src}");
            }
            IrInstruction::StoreMember {
                object,
                property,
                src,
            } => {
                let _ = writeln!(out, "{pad}store_member {object}, {property}, {src}");
            }
            IrInstruction::Move { dst, src } => {
                let _ = writeln!(out, "{pad}%{dst} = move {src}");
            }
            IrInstruction::Binary {
                dst,
                op,
                left,
                right,
            } => {
                let _ = writeln!(out, "{pad}%{dst} = binary {op}, {left}, {right}");
            }
            IrInstruction::Unary { dst, op, arg } => {
                let _ = writeln!(out, "{pad}%{dst} = unary {op}, {arg}");
            }
            IrInstruction::Member {
                dst,
                object,
                property,
            } => {
                let _ = writeln!(out, "{pad}%{dst} = member {object}, {property}");
            }
            IrInstruction::Array { dst, items } => {
                let items = items
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "{pad}%{dst} = array [{items}]");
            }
            IrInstruction::Object { dst, props } => {
                let props = props
                    .iter()
                    .map(|(key, value)| format!("{key}: {value}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "{pad}%{dst} = object {{{props}}}");
            }
            IrInstruction::Call { dst, callee, args } => {
                let args = args
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "{pad}%{dst} = call {callee}({args})");
            }
            IrInstruction::New { dst, callee, args } => {
                let args = args
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "{pad}%{dst} = new {callee}({args})");
            }
            IrInstruction::Template { dst, quasis, exprs } => {
                let quasis = quasis
                    .iter()
                    .map(|part| format!("{part:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let exprs = exprs
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "{pad}%{dst} = template [{quasis}] [{exprs}]");
            }
            IrInstruction::Function { name, params, body } => {
                let _ = writeln!(out, "{pad}function {name}({}) {{", params.join(", "));
                for instruction in body {
                    instruction.write_text(out, indent + 1);
                }
                let _ = writeln!(out, "{pad}}}");
            }
            IrInstruction::FunctionExpr {
                dst,
                name,
                params,
                body,
            } => {
                let name = name.as_deref().unwrap_or("<anonymous>");
                let _ = writeln!(
                    out,
                    "{pad}%{dst} = function {name}({}) {{",
                    params.join(", ")
                );
                for instruction in body {
                    instruction.write_text(out, indent + 1);
                }
                let _ = writeln!(out, "{pad}}}");
            }
            IrInstruction::Class {
                dst,
                name,
                super_class,
                members,
            } => {
                let target = dst
                    .as_ref()
                    .map(|dst| format!("%{dst} = "))
                    .unwrap_or_default();
                let name = name.as_deref().unwrap_or("<anonymous>");
                let extends = super_class
                    .as_ref()
                    .map(|value| format!(" extends {value}"))
                    .unwrap_or_default();
                let _ = writeln!(out, "{pad}{target}class {name}{extends} {{");
                for member in members {
                    let _ = writeln!(out, "{pad}  {member}");
                }
                let _ = writeln!(out, "{pad}}}");
            }
            IrInstruction::Import { source, specifiers } => {
                let _ = writeln!(
                    out,
                    "{pad}import [{}] from {source:?}",
                    specifiers.join(", ")
                );
            }
            IrInstruction::Export { kind, names } => {
                let _ = writeln!(out, "{pad}export {kind} [{}]", names.join(", "));
            }
            IrInstruction::Throw(value) => {
                let _ = writeln!(out, "{pad}throw {value}");
            }
            IrInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                let _ = writeln!(out, "{pad}try {{");
                for instruction in body {
                    instruction.write_text(out, indent + 1);
                }
                let _ = writeln!(out, "{pad}}}");
                if !catch_body.is_empty() {
                    let param = catch_param.as_deref().unwrap_or("");
                    let _ = writeln!(out, "{pad}catch {param} {{");
                    for instruction in catch_body {
                        instruction.write_text(out, indent + 1);
                    }
                    let _ = writeln!(out, "{pad}}}");
                }
                if !finally_body.is_empty() {
                    let _ = writeln!(out, "{pad}finally {{");
                    for instruction in finally_body {
                        instruction.write_text(out, indent + 1);
                    }
                    let _ = writeln!(out, "{pad}}}");
                }
            }
            IrInstruction::Return(Some(value)) => {
                let _ = writeln!(out, "{pad}return {value}");
            }
            IrInstruction::Return(None) => {
                let _ = writeln!(out, "{pad}return");
            }
            IrInstruction::Pop(value) => {
                let _ = writeln!(out, "{pad}pop {value}");
            }
            IrInstruction::Jump(label) => {
                let _ = writeln!(out, "{pad}jump {label}");
            }
            IrInstruction::JumpIfFalse { test, label } => {
                let _ = writeln!(out, "{pad}jump_if_false {test}, {label}");
            }
            IrInstruction::Unsupported(message) => {
                let _ = writeln!(out, "{pad}unsupported {message}");
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BytecodeConstant {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Undefined,
}
impl fmt::Display for BytecodeConstant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BytecodeConstant::Number(value) => write!(f, "{value}"),
            BytecodeConstant::String(value) => write!(f, "{value:?}"),
            BytecodeConstant::Bool(value) => write!(f, "{value}"),
            BytecodeConstant::Null => write!(f, "null"),
            BytecodeConstant::Undefined => write!(f, "undefined"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BytecodeOp {
    Marker = 0,
    Label = 1,
    Declare = 2,
    LoadConst = 3,
    LoadName = 4,
    StoreName = 5,
    StoreMember = 6,
    Move = 7,
    Binary = 8,
    Unary = 9,
    Member = 10,
    Array = 11,
    Object = 12,
    Call = 13,
    New = 14,
    Template = 15,
    FunctionStart = 16,
    FunctionEnd = 17,
    FunctionExprStart = 18,
    FunctionExprEnd = 19,
    Class = 20,
    Import = 21,
    Export = 22,
    Throw = 23,
    TryStart = 24,
    CatchStart = 25,
    FinallyStart = 26,
    TryEnd = 27,
    Return = 28,
    Pop = 29,
    Jump = 30,
    JumpIfFalse = 31,
    Unsupported = 32,
    LoadConstConst = 33,
    PopReg = 34,
    CallOne = 35,
}

impl BytecodeOp {
    pub fn all() -> &'static [BytecodeOp] {
        &[
            BytecodeOp::Marker,
            BytecodeOp::Label,
            BytecodeOp::Declare,
            BytecodeOp::LoadConst,
            BytecodeOp::LoadName,
            BytecodeOp::StoreName,
            BytecodeOp::StoreMember,
            BytecodeOp::Move,
            BytecodeOp::Binary,
            BytecodeOp::Unary,
            BytecodeOp::Member,
            BytecodeOp::Array,
            BytecodeOp::Object,
            BytecodeOp::Call,
            BytecodeOp::New,
            BytecodeOp::Template,
            BytecodeOp::FunctionStart,
            BytecodeOp::FunctionEnd,
            BytecodeOp::FunctionExprStart,
            BytecodeOp::FunctionExprEnd,
            BytecodeOp::Class,
            BytecodeOp::Import,
            BytecodeOp::Export,
            BytecodeOp::Throw,
            BytecodeOp::TryStart,
            BytecodeOp::CatchStart,
            BytecodeOp::FinallyStart,
            BytecodeOp::TryEnd,
            BytecodeOp::Return,
            BytecodeOp::Pop,
            BytecodeOp::Jump,
            BytecodeOp::JumpIfFalse,
            BytecodeOp::Unsupported,
            BytecodeOp::LoadConstConst,
            BytecodeOp::PopReg,
            BytecodeOp::CallOne,
        ]
    }

    pub fn mnemonic(self) -> &'static str {
        match self {
            BytecodeOp::Marker => "MARKER",
            BytecodeOp::Label => "LABEL",
            BytecodeOp::Declare => "DECLARE",
            BytecodeOp::LoadConst => "LOAD_CONST",
            BytecodeOp::LoadName => "LOAD_NAME",
            BytecodeOp::StoreName => "STORE_NAME",
            BytecodeOp::StoreMember => "STORE_MEMBER",
            BytecodeOp::Move => "MOVE",
            BytecodeOp::Binary => "BINARY",
            BytecodeOp::Unary => "UNARY",
            BytecodeOp::Member => "MEMBER",
            BytecodeOp::Array => "ARRAY",
            BytecodeOp::Object => "OBJECT",
            BytecodeOp::Call => "CALL",
            BytecodeOp::New => "NEW",
            BytecodeOp::Template => "TEMPLATE",
            BytecodeOp::FunctionStart => "FUNCTION_START",
            BytecodeOp::FunctionEnd => "FUNCTION_END",
            BytecodeOp::FunctionExprStart => "FUNCTION_EXPR_START",
            BytecodeOp::FunctionExprEnd => "FUNCTION_EXPR_END",
            BytecodeOp::Class => "CLASS",
            BytecodeOp::Import => "IMPORT",
            BytecodeOp::Export => "EXPORT",
            BytecodeOp::Throw => "THROW",
            BytecodeOp::TryStart => "TRY_START",
            BytecodeOp::CatchStart => "CATCH_START",
            BytecodeOp::FinallyStart => "FINALLY_START",
            BytecodeOp::TryEnd => "TRY_END",
            BytecodeOp::Return => "RETURN",
            BytecodeOp::Pop => "POP",
            BytecodeOp::Jump => "JUMP",
            BytecodeOp::JumpIfFalse => "JUMP_IF_FALSE",
            BytecodeOp::Unsupported => "UNSUPPORTED",
            BytecodeOp::LoadConstConst => "LOAD_CONST_CONST",
            BytecodeOp::PopReg => "POP_REG",
            BytecodeOp::CallOne => "CALL_1",
        }
    }

    pub fn from_mnemonic(mnemonic: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|op| op.mnemonic() == mnemonic)
    }

    fn canonical(self) -> Self {
        match self {
            BytecodeOp::LoadConstConst => BytecodeOp::LoadConst,
            BytecodeOp::PopReg => BytecodeOp::Pop,
            BytecodeOp::CallOne => BytecodeOp::Call,
            op => op,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingConfig {
    pub magic: String,
    pub opcodes: BTreeMap<String, u8>,
    pub operand_tags: BTreeMap<String, u8>,
    pub constant_tags: BTreeMap<String, u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingNames {
    pub opcodes: Vec<String>,
    pub operand_tags: Vec<String>,
    pub constant_tags: Vec<String>,
}

impl EncodingNames {
    pub fn flatten(&self) -> Vec<String> {
        self.opcodes
            .iter()
            .chain(&self.operand_tags)
            .chain(&self.constant_tags)
            .cloned()
            .collect()
    }
}

impl Default for EncodingNames {
    fn default() -> Self {
        Self {
            opcodes: default_opcode_mnemonics(),
            operand_tags: default_operand_tag_keys(),
            constant_tags: default_constant_tag_keys(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObfuscationConfig {
    pub encoding: EncodingNames,
    pub extern_slots: Vec<u8>,
}

impl Default for ObfuscationConfig {
    fn default() -> Self {
        Self {
            encoding: EncodingNames::default(),
            extern_slots: Vec::new(),
        }
    }
}

impl ObfuscationConfig {
    pub fn from_encoding_names(encoding: EncodingNames) -> Result<Self, EncodingError> {
        let config = Self {
            encoding,
            extern_slots: Vec::new(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_encoding_and_extern_slots(
        encoding: EncodingNames,
        extern_slots: Vec<u8>,
    ) -> Result<Self, EncodingError> {
        let config = Self {
            encoding,
            extern_slots,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_encoding_config(encoding: &EncodingConfig) -> Result<Self, EncodingError> {
        Self::from_encoding_names(encoding.names())
    }

    pub fn encoding_config(&self) -> Result<EncodingConfig, EncodingError> {
        EncodingConfig::from_names(&self.encoding)
    }

    pub fn config_seed(&self) -> Result<String, EncodingError> {
        self.paired_seed(&[])
    }

    pub fn paired_seed(&self, bytes: &[u8]) -> Result<String, EncodingError> {
        Ok(ObfuscationSeed::from_config(self.clone(), bytes)?.to_string())
    }

    pub fn from_seed(seed: &str) -> Result<Self, EncodingError> {
        Ok(ObfuscationSeed::parse(seed)?.config)
    }

    pub fn from_seed_for_bytes(seed: &str, bytes: &[u8]) -> Result<Self, EncodingError> {
        Ok(ObfuscationSeed::parse_for_bytes(seed, bytes)?.config)
    }

    pub fn validate(&self) -> Result<(), EncodingError> {
        EncodingConfig::from_names(&self.encoding)?;
        validate_slot_permutation(&self.extern_slots, "extern slot")
    }

    fn seed_permutation(&self) -> Result<String, EncodingError> {
        self.validate()?;
        let mut sections = vec![
            names_to_seed_permutation(
                &self.encoding.opcodes,
                &default_opcode_mnemonics(),
                "opcode",
            )?,
            names_to_seed_permutation(
                &self.encoding.operand_tags,
                &default_operand_tag_keys(),
                "operand tag",
            )?,
            names_to_seed_permutation(
                &self.encoding.constant_tags,
                &default_constant_tag_keys(),
                "constant tag",
            )?,
        ];
        if !self.extern_slots.is_empty() {
            sections.push(indexes_to_seed_permutation(
                &self.extern_slots,
                "extern slot",
            )?);
        }
        Ok(sections.join("."))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObfuscationSeed {
    pub fingerprint: u64,
    pub config: ObfuscationConfig,
}

impl ObfuscationSeed {
    pub fn from_config(config: ObfuscationConfig, bytes: &[u8]) -> Result<Self, EncodingError> {
        let permutation = config.seed_permutation()?;
        Ok(Self {
            fingerprint: seed_fingerprint(&permutation, bytes),
            config,
        })
    }

    pub fn parse(seed: &str) -> Result<Self, EncodingError> {
        let parsed = parse_obfuscation_seed(seed)?;
        let config = obfuscation_config_from_seed_permutation(&parsed.permutation)?;
        Ok(Self {
            fingerprint: parsed.fingerprint,
            config,
        })
    }

    pub fn parse_for_bytes(seed: &str, bytes: &[u8]) -> Result<Self, EncodingError> {
        let parsed = parse_obfuscation_seed(seed)?;
        let actual = seed_fingerprint(&parsed.permutation, bytes);
        if actual != parsed.fingerprint {
            return Err(EncodingError::Seed(
                "seed does not match bytecode bytes".to_string(),
            ));
        }
        let config = obfuscation_config_from_seed_permutation(&parsed.permutation)?;
        Ok(Self {
            fingerprint: parsed.fingerprint,
            config,
        })
    }

    pub fn permutation(&self) -> Result<String, EncodingError> {
        self.config.seed_permutation()
    }
}

impl fmt::Display for ObfuscationSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.permutation() {
            Ok(permutation) => {
                write!(
                    f,
                    "{ENCODING_SEED_PREFIX}-{:016x}-{permutation}",
                    self.fingerprint
                )
            }
            Err(_) => Err(fmt::Error),
        }
    }
}

pub const DEFAULT_BYTECODE_MAGIC: &str = "JS";

const ENCODING_SEED_PREFIX: &str = "JSTKSEED2";

impl Default for EncodingConfig {
    fn default() -> Self {
        let opcodes = [
            (BytecodeOp::Marker, 0),
            (BytecodeOp::Label, 1),
            (BytecodeOp::Declare, 2),
            (BytecodeOp::LoadConst, 3),
            (BytecodeOp::LoadName, 4),
            (BytecodeOp::StoreName, 5),
            (BytecodeOp::StoreMember, 6),
            (BytecodeOp::Move, 7),
            (BytecodeOp::Binary, 8),
            (BytecodeOp::Unary, 9),
            (BytecodeOp::Member, 10),
            (BytecodeOp::Array, 11),
            (BytecodeOp::Object, 12),
            (BytecodeOp::Call, 13),
            (BytecodeOp::New, 14),
            (BytecodeOp::Template, 15),
            (BytecodeOp::FunctionStart, 16),
            (BytecodeOp::FunctionEnd, 17),
            (BytecodeOp::FunctionExprStart, 18),
            (BytecodeOp::FunctionExprEnd, 19),
            (BytecodeOp::Class, 20),
            (BytecodeOp::Import, 21),
            (BytecodeOp::Export, 22),
            (BytecodeOp::Throw, 23),
            (BytecodeOp::TryStart, 24),
            (BytecodeOp::CatchStart, 25),
            (BytecodeOp::FinallyStart, 26),
            (BytecodeOp::TryEnd, 27),
            (BytecodeOp::Return, 28),
            (BytecodeOp::Pop, 29),
            (BytecodeOp::Jump, 30),
            (BytecodeOp::JumpIfFalse, 31),
            (BytecodeOp::Unsupported, 32),
            (BytecodeOp::LoadConstConst, 33),
            (BytecodeOp::PopReg, 34),
            (BytecodeOp::CallOne, 35),
        ]
        .into_iter()
        .map(|(op, code)| (op.mnemonic().to_string(), code))
        .collect();

        let operand_tags = [
            ("register", 0),
            ("constant", 1),
            ("name", 2),
            ("extern", 3),
            ("label", 4),
            ("count", 5),
            ("none", 6),
            ("function", 7),
        ]
        .into_iter()
        .map(|(name, tag)| (name.to_string(), tag))
        .collect();

        let constant_tags = [
            ("number", 0),
            ("string", 1),
            ("bool", 2),
            ("null", 3),
            ("undefined", 4),
        ]
        .into_iter()
        .map(|(name, tag)| (name.to_string(), tag))
        .collect();

        Self {
            magic: DEFAULT_BYTECODE_MAGIC.to_string(),
            opcodes,
            operand_tags,
            constant_tags,
        }
    }
}

impl EncodingConfig {
    pub fn from_names(names: &EncodingNames) -> Result<Self, EncodingError> {
        let mut config = Self::default();
        config.opcodes =
            names_to_encoding_map(&names.opcodes, &default_opcode_mnemonics(), "opcode")?;
        config.operand_tags = names_to_encoding_map(
            &names.operand_tags,
            &default_operand_tag_keys(),
            "operand tag",
        )?;
        config.constant_tags = names_to_encoding_map(
            &names.constant_tags,
            &default_constant_tag_keys(),
            "constant tag",
        )?;
        config.validate()?;
        Ok(config)
    }

    pub fn names(&self) -> EncodingNames {
        EncodingNames {
            opcodes: names_by_code(&self.opcodes),
            operand_tags: names_by_code(&self.operand_tags),
            constant_tags: names_by_code(&self.constant_tags),
        }
    }

    pub fn config_seed(&self) -> Result<String, EncodingError> {
        self.to_seed(&[])
    }

    pub fn paired_seed(&self, bytes: &[u8]) -> Result<String, EncodingError> {
        self.to_seed(bytes)
    }

    pub fn from_yaml(source: &str) -> Result<Self, EncodingError> {
        let mut config = Self::default();
        let mut section: Option<YamlSection> = None;
        let base_indent = source
            .lines()
            .filter(|line| !strip_yaml_comment(line).trim().is_empty())
            .map(|line| line.len() - line.trim_start().len())
            .min()
            .unwrap_or(0);

        for (line_index, raw_line) in source.lines().enumerate() {
            let line_number = line_index + 1;
            let dedented = raw_line.get(base_indent..).unwrap_or(raw_line);
            let line = strip_yaml_comment(dedented).trim_end();
            if line.trim().is_empty() {
                continue;
            }

            let indent = line.len() - line.trim_start().len();
            let trimmed = line.trim();
            if indent == 0 {
                let Some((key, value)) = trimmed.split_once(':') else {
                    return Err(EncodingError::Yaml(format!(
                        "line {line_number}: expected key/value or section"
                    )));
                };
                let key = key.trim();
                let value = value.trim();
                match key {
                    "magic" => {
                        if value.is_empty() {
                            return Err(EncodingError::Yaml(format!(
                                "line {line_number}: magic requires a value"
                            )));
                        }
                        config.magic = unquote_yaml(value).to_string();
                        section = None;
                    }
                    "opcodes" if value.is_empty() => section = Some(YamlSection::Opcodes),
                    "operand_tags" if value.is_empty() => {
                        section = Some(YamlSection::OperandTags);
                    }
                    "constant_tags" if value.is_empty() => {
                        section = Some(YamlSection::ConstantTags);
                    }
                    _ => {
                        return Err(EncodingError::Yaml(format!(
                            "line {line_number}: unknown encoding key {key:?}"
                        )));
                    }
                }
                continue;
            }

            let Some(section) = section else {
                return Err(EncodingError::Yaml(format!(
                    "line {line_number}: nested value without a section"
                )));
            };
            let Some((key, value)) = trimmed.split_once(':') else {
                return Err(EncodingError::Yaml(format!(
                    "line {line_number}: expected map entry"
                )));
            };
            let code = parse_u8_yaml(value.trim(), line_number)?;
            match section {
                YamlSection::Opcodes => {
                    config
                        .opcodes
                        .insert(normalize_opcode_key(key.trim()), code);
                }
                YamlSection::OperandTags => {
                    config
                        .operand_tags
                        .insert(normalize_tag_key(key.trim()), code);
                }
                YamlSection::ConstantTags => {
                    config
                        .constant_tags
                        .insert(normalize_tag_key(key.trim()), code);
                }
            }
        }

        config.validate()?;
        Ok(config)
    }

    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "magic: {:?}", self.magic);
        let _ = writeln!(out, "opcodes:");
        for (key, value) in &self.opcodes {
            let _ = writeln!(out, "  {key}: {value}");
        }
        let _ = writeln!(out, "operand_tags:");
        for (key, value) in &self.operand_tags {
            let _ = writeln!(out, "  {key}: {value}");
        }
        let _ = writeln!(out, "constant_tags:");
        for (key, value) in &self.constant_tags {
            let _ = writeln!(out, "  {key}: {value}");
        }
        out
    }

    pub fn to_seed(&self, bytes: &[u8]) -> Result<String, EncodingError> {
        self.validate()?;
        ObfuscationConfig::from_encoding_config(self)?.paired_seed(bytes)
    }

    pub fn from_seed(seed: &str) -> Result<Self, EncodingError> {
        ObfuscationConfig::from_seed(seed)?.encoding_config()
    }

    pub fn from_seed_for_bytes(seed: &str, bytes: &[u8]) -> Result<Self, EncodingError> {
        ObfuscationConfig::from_seed_for_bytes(seed, bytes)?.encoding_config()
    }

    pub fn validate(&self) -> Result<(), EncodingError> {
        if self.magic.is_empty() {
            return Err(EncodingError::MissingKey("magic".to_string()));
        }
        validate_unique_codes(&self.opcodes, "opcode")?;
        validate_unique_codes(&self.operand_tags, "operand tag")?;
        validate_unique_codes(&self.constant_tags, "constant tag")?;
        for op in BytecodeOp::all() {
            self.opcode(*op)?;
        }
        for key in [
            "register", "constant", "name", "extern", "label", "count", "none", "function",
        ] {
            self.operand_tag(key)?;
        }
        for key in ["number", "string", "bool", "null", "undefined"] {
            self.constant_tag(key)?;
        }
        Ok(())
    }

    fn opcode(&self, op: BytecodeOp) -> Result<u8, EncodingError> {
        let key = op.mnemonic();
        self.opcodes
            .get(key)
            .copied()
            .ok_or_else(|| EncodingError::MissingKey(format!("opcodes.{key}")))
    }

    fn opcode_from_code(&self, code: u8) -> Result<BytecodeOp, EncodingError> {
        let Some((mnemonic, _)) = self.opcodes.iter().find(|(_, value)| **value == code) else {
            return Err(EncodingError::UnknownCode(format!("opcode {code}")));
        };
        BytecodeOp::from_mnemonic(mnemonic)
            .ok_or_else(|| EncodingError::UnknownCode(format!("opcode mnemonic {mnemonic}")))
    }

    fn operand_tag(&self, key: &str) -> Result<u8, EncodingError> {
        self.operand_tags
            .get(key)
            .copied()
            .ok_or_else(|| EncodingError::MissingKey(format!("operand_tags.{key}")))
    }

    fn operand_kind_from_tag(&self, tag: u8) -> Result<&str, EncodingError> {
        self.operand_tags
            .iter()
            .find(|(_, value)| **value == tag)
            .map(|(key, _)| key.as_str())
            .ok_or_else(|| EncodingError::UnknownCode(format!("operand tag {tag}")))
    }

    fn constant_tag(&self, key: &str) -> Result<u8, EncodingError> {
        self.constant_tags
            .get(key)
            .copied()
            .ok_or_else(|| EncodingError::MissingKey(format!("constant_tags.{key}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingError {
    MissingKey(String),
    UnknownCode(String),
    UnexpectedOperand(String),
    UnexpectedEof,
    InvalidMagic { expected: String },
    Yaml(String),
    Seed(String),
}

impl fmt::Display for EncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodingError::MissingKey(key) => write!(f, "missing encoding key {key}"),
            EncodingError::UnknownCode(code) => write!(f, "unknown encoding code {code}"),
            EncodingError::UnexpectedOperand(message) => {
                write!(f, "unexpected bytecode operand: {message}")
            }
            EncodingError::UnexpectedEof => write!(f, "unexpected end of bytecode"),
            EncodingError::InvalidMagic { expected } => {
                write!(f, "invalid bytecode magic, expected {expected:?}")
            }
            EncodingError::Yaml(message) => write!(f, "invalid encoding yaml: {message}"),
            EncodingError::Seed(message) => write!(f, "invalid encoding seed: {message}"),
        }
    }
}

impl Error for EncodingError {}

#[derive(Debug, Clone, Copy)]
enum YamlSection {
    Opcodes,
    OperandTags,
    ConstantTags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytecodeOperand {
    Register(u32),
    Constant(u32),
    Name(u32),
    External(u32),
    Function(u32),
    Label(u32),
    Operator(u32),
    DeclKind(u32),
    Count(u32),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandKind {
    Register,
    Constant,
    Name,
    NameRef,
    Function,
    Label,
    Operator,
    DeclKind,
    Count,
    Value,
    OptionalRegister,
    OptionalName,
    OptionalValue,
}

impl BytecodeOperand {
    fn tag(&self, encoding: &EncodingConfig) -> Result<u8, EncodingError> {
        match self {
            BytecodeOperand::Register(_) => encoding.operand_tag("register"),
            BytecodeOperand::Constant(_) => encoding.operand_tag("constant"),
            BytecodeOperand::Name(_) => encoding.operand_tag("name"),
            BytecodeOperand::External(_) => encoding.operand_tag("extern"),
            BytecodeOperand::Function(_) => encoding.operand_tag("function"),
            BytecodeOperand::Label(_) => encoding.operand_tag("label"),
            BytecodeOperand::Operator(_) => encoding.operand_tag("count"),
            BytecodeOperand::Count(_) => encoding.operand_tag("count"),
            BytecodeOperand::DeclKind(_) => encoding.operand_tag("count"),
            BytecodeOperand::None => encoding.operand_tag("none"),
        }
    }

    fn payload(&self) -> u32 {
        match self {
            BytecodeOperand::Register(value)
            | BytecodeOperand::Constant(value)
            | BytecodeOperand::Name(value)
            | BytecodeOperand::External(value)
            | BytecodeOperand::Function(value)
            | BytecodeOperand::Label(value)
            | BytecodeOperand::Operator(value)
            | BytecodeOperand::DeclKind(value)
            | BytecodeOperand::Count(value) => *value,
            BytecodeOperand::None => 0,
        }
    }

    fn from_tag_payload(
        tag: u8,
        payload: u32,
        encoding: &EncodingConfig,
    ) -> Result<Self, EncodingError> {
        match encoding.operand_kind_from_tag(tag)? {
            "register" => Ok(BytecodeOperand::Register(payload)),
            "constant" => Ok(BytecodeOperand::Constant(payload)),
            "name" => Ok(BytecodeOperand::Name(payload)),
            "extern" => Ok(BytecodeOperand::External(payload)),
            "function" => Ok(BytecodeOperand::Function(payload)),
            "label" => Ok(BytecodeOperand::Label(payload)),
            "count" => Ok(BytecodeOperand::Count(payload)),
            "none" => Ok(BytecodeOperand::None),
            key => Err(EncodingError::UnknownCode(format!("operand kind {key}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeInstruction {
    pub op: BytecodeOp,
    pub operands: Vec<BytecodeOperand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeFunction {
    pub name: Option<u32>,
    pub params: Vec<u32>,
    pub has_return: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct BytecodeModule {
    pub extern_slots: Vec<String>,
    pub names: Vec<String>,
    pub functions: Vec<BytecodeFunction>,
    pub constants: Vec<BytecodeConstant>,
    pub instructions: Vec<BytecodeInstruction>,
}

impl BytecodeModule {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if !self.extern_slots.is_empty() {
            let _ = writeln!(out, ".externs");
            for (index, name) in self.extern_slots.iter().enumerate() {
                let _ = writeln!(out, "  e{index} = {name}");
            }
        }
        if !self.names.is_empty() {
            let _ = writeln!(out, ".names");
            for (index, name) in self.names.iter().enumerate() {
                let _ = writeln!(out, "  n{index} = {name:?}");
            }
        }
        if !self.functions.is_empty() {
            let _ = writeln!(out, ".fun");
            for (index, function) in self.functions.iter().enumerate() {
                let name = function
                    .name
                    .map(|name| self.format_name_index(name))
                    .unwrap_or_else(|| "<anonymous>".to_string());
                let params = function
                    .params
                    .iter()
                    .map(|param| self.format_name_index(*param))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(
                    out,
                    "  f{index} = name:{name}, argc:{}, returns:{}, params:[{params}]",
                    function.params.len(),
                    function.has_return
                );
            }
        }
        let _ = writeln!(out, ".constants");
        for (index, constant) in self.constants.iter().enumerate() {
            let _ = writeln!(out, "  c{index} = {constant}");
        }
        let _ = writeln!(out, ".code");
        for (index, instruction) in self.instructions.iter().enumerate() {
            let operands = instruction
                .operands
                .iter()
                .map(|operand| self.format_operand(operand))
                .collect::<Vec<_>>()
                .join(", ");
            if operands.is_empty() {
                let _ = writeln!(out, "{index:04} {}", instruction.op.mnemonic());
            } else {
                let _ = writeln!(out, "{index:04} {} {operands}", instruction.op.mnemonic());
            }
        }
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_bytes_with_encoding(&EncodingConfig::default())
            .expect("default bytecode encoding must be valid")
    }

    pub fn to_bytes_with_encoding(
        &self,
        encoding: &EncodingConfig,
    ) -> Result<Vec<u8>, EncodingError> {
        encoding.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(encoding.magic.as_bytes());
        write_u32(&mut bytes, self.extern_slots.len() as u32);
        for slot in &self.extern_slots {
            write_string(&mut bytes, slot);
        }
        write_u32(&mut bytes, self.names.len() as u32);
        for name in &self.names {
            write_name_string(&mut bytes, name, &self.extern_slots);
        }
        write_u32(&mut bytes, self.functions.len() as u32);
        for function in &self.functions {
            write_optional_u32(&mut bytes, function.name);
            write_u32(&mut bytes, function.params.len() as u32);
            bytes.push(u8::from(function.has_return));
            for param in &function.params {
                write_u32(&mut bytes, *param);
            }
        }
        write_u32(&mut bytes, self.constants.len() as u32);
        for constant in &self.constants {
            match constant {
                BytecodeConstant::Number(value) => {
                    bytes.push(encoding.constant_tag("number")?);
                    write_number(&mut bytes, *value);
                }
                BytecodeConstant::String(value) => {
                    bytes.push(encoding.constant_tag("string")?);
                    write_constant_string(&mut bytes, value);
                }
                BytecodeConstant::Bool(value) => {
                    bytes.push(encoding.constant_tag("bool")?);
                    bytes.push(u8::from(*value));
                }
                BytecodeConstant::Null => bytes.push(encoding.constant_tag("null")?),
                BytecodeConstant::Undefined => bytes.push(encoding.constant_tag("undefined")?),
            }
        }
        write_u32(&mut bytes, self.instructions.len() as u32);
        for instruction in &self.instructions {
            write_instruction(&mut bytes, instruction, encoding)?;
        }
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EncodingError> {
        Self::from_bytes_with_encoding(bytes, &EncodingConfig::default())
    }

    pub fn from_bytes_with_encoding(
        bytes: &[u8],
        encoding: &EncodingConfig,
    ) -> Result<Self, EncodingError> {
        encoding.validate()?;
        let mut cursor = ByteReader::new(bytes);
        cursor.expect_magic(encoding)?;

        let extern_count = cursor.read_bounded_count("extern slots")?;
        let mut extern_slots = Vec::with_capacity(extern_count);
        for _ in 0..extern_count {
            extern_slots.push(cursor.read_string()?);
        }

        let name_count = cursor.read_bounded_count("names")?;
        let mut names = Vec::with_capacity(name_count);
        for _ in 0..name_count {
            names.push(cursor.read_name_string(&extern_slots)?);
        }

        let function_count = cursor.read_bounded_count("functions")?;
        let mut functions = Vec::with_capacity(function_count);
        for _ in 0..function_count {
            let name = cursor.read_optional_u32()?;
            let param_count = cursor.read_bounded_count("function params")?;
            let has_return = cursor.read_u8()? != 0;
            let mut params = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                params.push(cursor.read_u32()?);
            }
            functions.push(BytecodeFunction {
                name,
                params,
                has_return,
            });
        }

        let constant_count = cursor.read_bounded_count("constants")?;
        let mut constants = Vec::with_capacity(constant_count);
        for _ in 0..constant_count {
            constants.push(cursor.read_constant(encoding)?);
        }

        let instruction_count = cursor.read_bounded_count("instructions")?;
        let mut instructions = Vec::with_capacity(instruction_count);
        for _ in 0..instruction_count {
            let op = encoding.opcode_from_code(cursor.read_u8()?)?;
            let operands = read_instruction_operands(&mut cursor, op, encoding)?;
            instructions.push(BytecodeInstruction {
                op: op.canonical(),
                operands,
            });
        }
        cursor.expect_end()?;

        Ok(Self {
            extern_slots,
            names,
            functions,
            constants,
            instructions,
        })
    }

    pub fn from_bytes_with_seed(bytes: &[u8], seed: &str) -> Result<Self, EncodingError> {
        let encoding = EncodingConfig::from_seed_for_bytes(seed, bytes)?;
        Self::from_bytes_with_encoding(bytes, &encoding)
    }

    fn format_operand(&self, operand: &BytecodeOperand) -> String {
        match operand {
            BytecodeOperand::Register(value) => format!("r{value}"),
            BytecodeOperand::Constant(index) => {
                let value = self
                    .constants
                    .get(*index as usize)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "<bad-const>".to_string());
                format!("c{index}({value})")
            }
            BytecodeOperand::Name(index) => {
                let name = self
                    .names
                    .get(*index as usize)
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "<bad-name>".to_string());
                format!("name#{index}({name})")
            }
            BytecodeOperand::External(index) => {
                let name = self
                    .extern_slots
                    .get(*index as usize)
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "<bad-extern>".to_string());
                format!("extern#{index}({name})")
            }
            BytecodeOperand::Function(index) => {
                format!("fun#{index}")
            }
            BytecodeOperand::Label(index) => {
                format!("label#{index}")
            }
            BytecodeOperand::Operator(value) => operator_name(*value)
                .map(|op| format!("op#{value}({op:?})"))
                .unwrap_or_else(|| format!("op#{value}(<bad-op>)")),
            BytecodeOperand::DeclKind(value) => decl_kind_name(*value)
                .map(str::to_string)
                .unwrap_or_else(|| format!("decl#{value}")),
            BytecodeOperand::Count(value) => format!("#{value}"),
            BytecodeOperand::None => "none".to_string(),
        }
    }

    fn format_name_index(&self, index: u32) -> String {
        let name = self
            .names
            .get(index as usize)
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "<bad-name>".to_string());
        format!("name#{index}({name})")
    }
}

fn write_instruction_operands(
    bytes: &mut Vec<u8>,
    instruction: &BytecodeInstruction,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    match instruction.op {
        BytecodeOp::Array => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(bytes, instruction, 2, count, OperandKind::Value, encoding)?;
            ensure_operand_len(instruction, 2 + count)
        }
        BytecodeOp::Object => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            ensure_operand_len(instruction, 2 + count * 2)?;
            for index in 0..count {
                write_operand(
                    bytes,
                    operand_at(instruction, 2 + index * 2)?,
                    OperandKind::Constant,
                    encoding,
                )?;
                write_operand(
                    bytes,
                    operand_at(instruction, 3 + index * 2)?,
                    OperandKind::Value,
                    encoding,
                )?;
            }
            Ok(())
        }
        BytecodeOp::Call | BytecodeOp::New => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Value,
                encoding,
            )?;
            let count = count_at(instruction, 2)?;
            write_operand(
                bytes,
                operand_at(instruction, 2)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(bytes, instruction, 3, count, OperandKind::Value, encoding)?;
            ensure_operand_len(instruction, 3 + count)
        }
        BytecodeOp::CallOne => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Value,
                encoding,
            )?;
            ensure_operand_len(instruction, 4)?;
            if count_at(instruction, 2)? != 1 {
                return Err(EncodingError::UnexpectedOperand(
                    "CALL_1 expected exactly one argument".to_string(),
                ));
            }
            write_operand(
                bytes,
                operand_at(instruction, 3)?,
                OperandKind::Value,
                encoding,
            )
        }
        BytecodeOp::Template => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let quasi_count = count_at(instruction, 1)?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(
                bytes,
                instruction,
                2,
                quasi_count,
                OperandKind::Constant,
                encoding,
            )?;
            let expr_count_index = 2 + quasi_count;
            let expr_count = count_at(instruction, expr_count_index)?;
            write_operand(
                bytes,
                operand_at(instruction, expr_count_index)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(
                bytes,
                instruction,
                expr_count_index + 1,
                expr_count,
                OperandKind::Value,
                encoding,
            )?;
            ensure_operand_len(instruction, expr_count_index + 1 + expr_count)
        }
        BytecodeOp::FunctionStart => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Function,
                encoding,
            )?;
            ensure_operand_len(instruction, 1)
        }
        BytecodeOp::FunctionExprStart => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Function,
                encoding,
            )?;
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::Class => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::OptionalRegister,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::OptionalName,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 2)?,
                OperandKind::OptionalValue,
                encoding,
            )?;
            let count = count_at(instruction, 3)?;
            write_operand(
                bytes,
                operand_at(instruction, 3)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(
                bytes,
                instruction,
                4,
                count,
                OperandKind::Constant,
                encoding,
            )?;
            ensure_operand_len(instruction, 4 + count)
        }
        BytecodeOp::Import => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Constant,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(
                bytes,
                instruction,
                2,
                count,
                OperandKind::Constant,
                encoding,
            )?;
            ensure_operand_len(instruction, 2 + count)
        }
        BytecodeOp::Export => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Constant,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            write_repeated_operands(bytes, instruction, 2, count, OperandKind::Name, encoding)?;
            ensure_operand_len(instruction, 2 + count)
        }
        op => {
            let schema = fixed_operand_schema(op);
            ensure_operand_len(instruction, schema.len())?;
            for (operand, kind) in instruction.operands.iter().zip(schema.iter().copied()) {
                write_operand(bytes, operand, kind, encoding)?;
            }
            Ok(())
        }
    }
}

fn write_instruction(
    bytes: &mut Vec<u8>,
    instruction: &BytecodeInstruction,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    let wire_op = specialized_wire_op(instruction);
    bytes.push(encoding.opcode(wire_op)?);
    write_instruction_operands(
        bytes,
        &BytecodeInstruction {
            op: wire_op,
            operands: instruction.operands.clone(),
        },
        encoding,
    )
}

fn specialized_wire_op(instruction: &BytecodeInstruction) -> BytecodeOp {
    match instruction.op {
        BytecodeOp::LoadConst => {
            if matches!(
                instruction.operands.as_slice(),
                [BytecodeOperand::Register(_), BytecodeOperand::Constant(_)]
            ) {
                BytecodeOp::LoadConstConst
            } else {
                BytecodeOp::LoadConst
            }
        }
        BytecodeOp::Pop => {
            if matches!(
                instruction.operands.as_slice(),
                [BytecodeOperand::Register(_)]
            ) {
                BytecodeOp::PopReg
            } else {
                BytecodeOp::Pop
            }
        }
        BytecodeOp::Call => {
            if matches!(
                instruction.operands.as_slice(),
                [
                    BytecodeOperand::Register(_),
                    _,
                    BytecodeOperand::Count(1),
                    _
                ]
            ) {
                BytecodeOp::CallOne
            } else {
                BytecodeOp::Call
            }
        }
        op => op,
    }
}

fn read_instruction_operands(
    cursor: &mut ByteReader<'_>,
    op: BytecodeOp,
    encoding: &EncodingConfig,
) -> Result<Vec<BytecodeOperand>, EncodingError> {
    let mut operands = Vec::new();
    match op {
        BytecodeOp::Array => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            let count = read_operand(cursor, OperandKind::Count, encoding)?;
            let count_value = bounded_dynamic_count(cursor, count.payload(), "array item")?;
            operands.push(count);
            read_repeated_operands(
                cursor,
                &mut operands,
                count_value,
                OperandKind::Value,
                encoding,
            )?;
        }
        BytecodeOp::Object => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            let count = read_operand(cursor, OperandKind::Count, encoding)?;
            let count_value = bounded_dynamic_count(cursor, count.payload(), "object property")?;
            operands.push(count);
            for _ in 0..count_value {
                operands.push(read_operand(cursor, OperandKind::Constant, encoding)?);
                operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
            }
        }
        BytecodeOp::Call | BytecodeOp::New => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
            let count = read_operand(cursor, OperandKind::Count, encoding)?;
            let count_value = bounded_dynamic_count(cursor, count.payload(), "call argument")?;
            operands.push(count);
            read_repeated_operands(
                cursor,
                &mut operands,
                count_value,
                OperandKind::Value,
                encoding,
            )?;
        }
        BytecodeOp::CallOne => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
            operands.push(BytecodeOperand::Count(1));
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
        }
        BytecodeOp::Template => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            let quasi_count = read_operand(cursor, OperandKind::Count, encoding)?;
            let quasi_count_value =
                bounded_dynamic_count(cursor, quasi_count.payload(), "template quasi")?;
            operands.push(quasi_count);
            read_repeated_operands(
                cursor,
                &mut operands,
                quasi_count_value,
                OperandKind::Constant,
                encoding,
            )?;
            let expr_count = read_operand(cursor, OperandKind::Count, encoding)?;
            let expr_count_value =
                bounded_dynamic_count(cursor, expr_count.payload(), "template expression")?;
            operands.push(expr_count);
            read_repeated_operands(
                cursor,
                &mut operands,
                expr_count_value,
                OperandKind::Value,
                encoding,
            )?;
        }
        BytecodeOp::FunctionStart => {
            operands.push(read_operand(cursor, OperandKind::Function, encoding)?);
        }
        BytecodeOp::FunctionExprStart => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            operands.push(read_operand(cursor, OperandKind::Function, encoding)?);
        }
        BytecodeOp::Class => {
            operands.push(read_operand(
                cursor,
                OperandKind::OptionalRegister,
                encoding,
            )?);
            operands.push(read_operand(cursor, OperandKind::OptionalName, encoding)?);
            operands.push(read_operand(cursor, OperandKind::OptionalValue, encoding)?);
            let count = read_operand(cursor, OperandKind::Count, encoding)?;
            let count_value = bounded_dynamic_count(cursor, count.payload(), "class member")?;
            operands.push(count);
            read_repeated_operands(
                cursor,
                &mut operands,
                count_value,
                OperandKind::Constant,
                encoding,
            )?;
        }
        BytecodeOp::Import => {
            operands.push(read_operand(cursor, OperandKind::Constant, encoding)?);
            let count = read_operand(cursor, OperandKind::Count, encoding)?;
            let count_value = bounded_dynamic_count(cursor, count.payload(), "import specifier")?;
            operands.push(count);
            read_repeated_operands(
                cursor,
                &mut operands,
                count_value,
                OperandKind::Constant,
                encoding,
            )?;
        }
        BytecodeOp::Export => {
            operands.push(read_operand(cursor, OperandKind::Constant, encoding)?);
            let count = read_operand(cursor, OperandKind::Count, encoding)?;
            let count_value = bounded_dynamic_count(cursor, count.payload(), "export name")?;
            operands.push(count);
            read_repeated_operands(
                cursor,
                &mut operands,
                count_value,
                OperandKind::Name,
                encoding,
            )?;
        }
        op => {
            for kind in fixed_operand_schema(op).iter().copied() {
                operands.push(read_operand(cursor, kind, encoding)?);
            }
        }
    }
    Ok(operands)
}

fn bounded_dynamic_count(
    cursor: &ByteReader<'_>,
    count: u32,
    kind: &str,
) -> Result<usize, EncodingError> {
    let count = count as usize;
    let remaining = cursor.remaining();
    if count > remaining {
        return Err(EncodingError::UnknownCode(format!(
            "{kind} count {count} exceeds remaining bytecode bytes {remaining}"
        )));
    }
    Ok(count)
}

fn fixed_operand_schema(op: BytecodeOp) -> &'static [OperandKind] {
    use OperandKind::*;
    match op {
        BytecodeOp::Marker => &[Constant],
        BytecodeOp::Label => &[Label],
        BytecodeOp::Declare => &[DeclKind, Name],
        BytecodeOp::LoadConst => &[Register, Value],
        BytecodeOp::LoadConstConst => &[Register, Constant],
        BytecodeOp::LoadName => &[Register, NameRef],
        BytecodeOp::StoreName => &[NameRef, Value],
        BytecodeOp::StoreMember => &[Value, Constant, Value],
        BytecodeOp::Move => &[Register, Value],
        BytecodeOp::Binary => &[Register, Operator, Value, Value],
        BytecodeOp::Unary => &[Register, Operator, Value],
        BytecodeOp::Member => &[Register, Value, Constant],
        BytecodeOp::FunctionStart => &[Function],
        BytecodeOp::FunctionExprStart => &[Register, Function],
        BytecodeOp::FunctionEnd
        | BytecodeOp::FunctionExprEnd
        | BytecodeOp::TryStart
        | BytecodeOp::FinallyStart
        | BytecodeOp::TryEnd => &[],
        BytecodeOp::CatchStart => &[OptionalName],
        BytecodeOp::Throw => &[Value],
        BytecodeOp::Return => &[OptionalValue],
        BytecodeOp::Pop => &[Value],
        BytecodeOp::PopReg => &[Register],
        BytecodeOp::Jump => &[Label],
        BytecodeOp::JumpIfFalse => &[Value, Label],
        BytecodeOp::Unsupported => &[Constant],
        BytecodeOp::Array
        | BytecodeOp::Object
        | BytecodeOp::Call
        | BytecodeOp::New
        | BytecodeOp::Template
        | BytecodeOp::Class
        | BytecodeOp::Import
        | BytecodeOp::Export
        | BytecodeOp::CallOne => &[],
    }
}

fn write_repeated_operands(
    bytes: &mut Vec<u8>,
    instruction: &BytecodeInstruction,
    start: usize,
    count: usize,
    kind: OperandKind,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    ensure_operand_min_len(instruction, start + count)?;
    for index in 0..count {
        write_operand(
            bytes,
            operand_at(instruction, start + index)?,
            kind,
            encoding,
        )?;
    }
    Ok(())
}

fn read_repeated_operands(
    cursor: &mut ByteReader<'_>,
    operands: &mut Vec<BytecodeOperand>,
    count: usize,
    kind: OperandKind,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    for _ in 0..count {
        operands.push(read_operand(cursor, kind, encoding)?);
    }
    Ok(())
}

fn write_operand(
    bytes: &mut Vec<u8>,
    operand: &BytecodeOperand,
    kind: OperandKind,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    match kind {
        OperandKind::Value
        | OperandKind::OptionalValue
        | OperandKind::OptionalRegister
        | OperandKind::OptionalName => {
            bytes.push(operand.tag(encoding)?);
            write_u32(bytes, operand.payload());
            Ok(())
        }
        _ => {
            ensure_operand_kind(operand, kind)?;
            let payload = if kind == OperandKind::NameRef {
                encode_name_ref_operand(operand)?
            } else {
                operand.payload()
            };
            write_u32(bytes, payload);
            Ok(())
        }
    }
}

fn read_operand(
    cursor: &mut ByteReader<'_>,
    kind: OperandKind,
    encoding: &EncodingConfig,
) -> Result<BytecodeOperand, EncodingError> {
    match kind {
        OperandKind::Register => Ok(BytecodeOperand::Register(cursor.read_u32()?)),
        OperandKind::Constant => Ok(BytecodeOperand::Constant(cursor.read_u32()?)),
        OperandKind::Name => Ok(BytecodeOperand::Name(cursor.read_u32()?)),
        OperandKind::NameRef => Ok(decode_name_ref_operand(cursor.read_u32()?)),
        OperandKind::Function => Ok(BytecodeOperand::Function(cursor.read_u32()?)),
        OperandKind::Label => Ok(BytecodeOperand::Label(cursor.read_u32()?)),
        OperandKind::Operator => Ok(BytecodeOperand::Operator(cursor.read_u32()?)),
        OperandKind::DeclKind => Ok(BytecodeOperand::DeclKind(cursor.read_u32()?)),
        OperandKind::Count => Ok(BytecodeOperand::Count(cursor.read_u32()?)),
        OperandKind::Value
        | OperandKind::OptionalRegister
        | OperandKind::OptionalName
        | OperandKind::OptionalValue => {
            let tag = cursor.read_u8()?;
            let payload = cursor.read_u32()?;
            BytecodeOperand::from_tag_payload(tag, payload, encoding)
        }
    }
}

fn ensure_operand_kind(operand: &BytecodeOperand, kind: OperandKind) -> Result<(), EncodingError> {
    let valid = matches!(
        (kind, operand),
        (OperandKind::Register, BytecodeOperand::Register(_))
            | (OperandKind::Constant, BytecodeOperand::Constant(_))
            | (OperandKind::Name, BytecodeOperand::Name(_))
            | (OperandKind::NameRef, BytecodeOperand::Name(_))
            | (OperandKind::NameRef, BytecodeOperand::External(_))
            | (OperandKind::Function, BytecodeOperand::Function(_))
            | (OperandKind::Label, BytecodeOperand::Label(_))
            | (OperandKind::Operator, BytecodeOperand::Operator(_))
            | (OperandKind::DeclKind, BytecodeOperand::DeclKind(_))
            | (OperandKind::Count, BytecodeOperand::Count(_))
    );
    if valid {
        Ok(())
    } else {
        Err(EncodingError::UnexpectedOperand(format!(
            "operand {operand:?} does not match schema {kind:?}"
        )))
    }
}

fn encode_name_ref_operand(operand: &BytecodeOperand) -> Result<u32, EncodingError> {
    let (payload, tag_bit) = match operand {
        BytecodeOperand::Name(value) => (*value, 0),
        BytecodeOperand::External(value) => (*value, 1),
        operand => {
            return Err(EncodingError::UnexpectedOperand(format!(
                "name ref expected name or extern, got {operand:?}"
            )));
        }
    };
    payload
        .checked_mul(2)
        .and_then(|value| value.checked_add(tag_bit))
        .ok_or_else(|| EncodingError::UnexpectedOperand("name ref index overflow".to_string()))
}

fn decode_name_ref_operand(payload: u32) -> BytecodeOperand {
    let index = payload >> 1;
    if payload & 1 == 0 {
        BytecodeOperand::Name(index)
    } else {
        BytecodeOperand::External(index)
    }
}

fn operand_at(
    instruction: &BytecodeInstruction,
    index: usize,
) -> Result<&BytecodeOperand, EncodingError> {
    instruction.operands.get(index).ok_or_else(|| {
        EncodingError::UnexpectedOperand(format!(
            "{} missing operand {index}",
            instruction.op.mnemonic()
        ))
    })
}

fn count_at(instruction: &BytecodeInstruction, index: usize) -> Result<usize, EncodingError> {
    match operand_at(instruction, index)? {
        BytecodeOperand::Count(value) => Ok(*value as usize),
        operand => Err(EncodingError::UnexpectedOperand(format!(
            "{} operand {index} expected count, got {operand:?}",
            instruction.op.mnemonic()
        ))),
    }
}

fn ensure_operand_len(
    instruction: &BytecodeInstruction,
    expected: usize,
) -> Result<(), EncodingError> {
    if instruction.operands.len() == expected {
        Ok(())
    } else {
        Err(EncodingError::UnexpectedOperand(format!(
            "{} expected {expected} operands, got {}",
            instruction.op.mnemonic(),
            instruction.operands.len()
        )))
    }
}

fn ensure_operand_min_len(
    instruction: &BytecodeInstruction,
    expected_min: usize,
) -> Result<(), EncodingError> {
    if instruction.operands.len() >= expected_min {
        Ok(())
    } else {
        Err(EncodingError::UnexpectedOperand(format!(
            "{} expected at least {expected_min} operands, got {}",
            instruction.op.mnemonic(),
            instruction.operands.len()
        )))
    }
}

#[derive(Default)]
struct BytecodeBuilder {
    extern_slots: Vec<String>,
    extern_slot_ids: BTreeMap<String, u32>,
    names: Vec<String>,
    name_ids: BTreeMap<String, u32>,
    label_ids: BTreeMap<String, u32>,
    scopes: Vec<NameScope>,
    local_name_id: usize,
    constants: Vec<BytecodeConstant>,
    constant_ids: BTreeMap<String, u32>,
    functions: Vec<BytecodeFunction>,
    instructions: Vec<BytecodeInstruction>,
}

#[derive(Debug, Default, Clone)]
struct NameScope {
    names: BTreeMap<String, String>,
}

impl BytecodeBuilder {
    fn compile_module(mut self, module: &IrModule) -> BytecodeModule {
        self.extern_slots = module.extern_slots.clone();
        self.extern_slot_ids = self
            .extern_slots
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index as u32))
            .collect();
        self.compile_instructions(&module.instructions);
        BytecodeModule {
            extern_slots: self.extern_slots.clone(),
            names: self.names,
            functions: self.functions,
            constants: self.constants,
            instructions: self.instructions,
        }
    }

    fn compile_instructions(&mut self, instructions: &[IrInstruction]) {
        for instruction in instructions {
            self.compile_instruction(instruction);
        }
    }

    fn compile_instruction(&mut self, instruction: &IrInstruction) {
        match instruction {
            IrInstruction::Marker(message) => {
                let operand = self.string_constant_operand(message);
                self.emit(BytecodeOp::Marker, vec![operand]);
            }
            IrInstruction::Label(label) => {
                let operand = self.label_operand(label);
                self.emit(BytecodeOp::Label, vec![operand]);
            }
            IrInstruction::Declare { kind, name } => {
                let kind = BytecodeOperand::DeclKind(decl_kind_id(kind));
                let name = self.name_operand(name);
                self.emit(BytecodeOp::Declare, vec![kind, name]);
            }
            IrInstruction::LoadConst { dst, value } => {
                let dst = self.register_operand(dst);
                let value = self.value_operand(value);
                self.emit(BytecodeOp::LoadConst, vec![dst, value]);
            }
            IrInstruction::LoadName { dst, name } => {
                let dst = self.register_operand(dst);
                let name = self.name_ref_operand(name);
                self.emit(BytecodeOp::LoadName, vec![dst, name]);
            }
            IrInstruction::StoreName { name, src } => {
                let name = self.name_ref_operand(name);
                let src = self.value_operand(src);
                self.emit(BytecodeOp::StoreName, vec![name, src]);
            }
            IrInstruction::StoreMember {
                object,
                property,
                src,
            } => {
                let object = self.value_operand(object);
                let property = self.string_constant_operand(property);
                let src = self.value_operand(src);
                self.emit(BytecodeOp::StoreMember, vec![object, property, src]);
            }
            IrInstruction::Move { dst, src } => {
                let dst = self.register_operand(dst);
                let src = self.value_operand(src);
                self.emit(BytecodeOp::Move, vec![dst, src]);
            }
            IrInstruction::Binary {
                dst,
                op,
                left,
                right,
            } => {
                let dst = self.register_operand(dst);
                let op = self.operator_operand(op);
                let left = self.value_operand(left);
                let right = self.value_operand(right);
                self.emit(BytecodeOp::Binary, vec![dst, op, left, right]);
            }
            IrInstruction::Unary { dst, op, arg } => {
                let dst = self.register_operand(dst);
                let op = self.operator_operand(op);
                let arg = self.value_operand(arg);
                self.emit(BytecodeOp::Unary, vec![dst, op, arg]);
            }
            IrInstruction::Member {
                dst,
                object,
                property,
            } => {
                let dst = self.register_operand(dst);
                let object = self.value_operand(object);
                let property = self.string_constant_operand(property);
                self.emit(BytecodeOp::Member, vec![dst, object, property]);
            }
            IrInstruction::Array { dst, items } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    BytecodeOperand::Count(items.len() as u32),
                ];
                operands.extend(items.iter().map(|item| self.value_operand(item)));
                self.emit(BytecodeOp::Array, operands);
            }
            IrInstruction::Object { dst, props } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    BytecodeOperand::Count(props.len() as u32),
                ];
                for (key, value) in props {
                    operands.push(self.string_constant_operand(key));
                    operands.push(self.value_operand(value));
                }
                self.emit(BytecodeOp::Object, operands);
            }
            IrInstruction::Call { dst, callee, args } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    self.value_operand(callee),
                    BytecodeOperand::Count(args.len() as u32),
                ];
                operands.extend(args.iter().map(|arg| self.value_operand(arg)));
                self.emit(BytecodeOp::Call, operands);
            }
            IrInstruction::New { dst, callee, args } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    self.value_operand(callee),
                    BytecodeOperand::Count(args.len() as u32),
                ];
                operands.extend(args.iter().map(|arg| self.value_operand(arg)));
                self.emit(BytecodeOp::New, operands);
            }
            IrInstruction::Template { dst, quasis, exprs } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    BytecodeOperand::Count(quasis.len() as u32),
                ];
                operands.extend(quasis.iter().map(|part| self.string_constant_operand(part)));
                operands.push(BytecodeOperand::Count(exprs.len() as u32));
                operands.extend(exprs.iter().map(|expr| self.value_operand(expr)));
                self.emit(BytecodeOp::Template, operands);
            }
            IrInstruction::Function { name, params, body } => {
                let scope = self.function_scope(params, body, Some(name));
                let function = self.function_entry(Some(name), params, body, &scope);
                self.emit(
                    BytecodeOp::FunctionStart,
                    vec![BytecodeOperand::Function(function)],
                );
                self.scopes.push(scope);
                self.compile_instructions(body);
                self.scopes.pop();
                self.emit(BytecodeOp::FunctionEnd, Vec::new());
            }
            IrInstruction::FunctionExpr {
                dst,
                name,
                params,
                body,
            } => {
                let scope = self.function_scope(params, body, name.as_deref());
                let function = self.function_entry(name.as_deref(), params, body, &scope);
                self.emit(
                    BytecodeOp::FunctionExprStart,
                    vec![
                        self.register_operand(dst),
                        BytecodeOperand::Function(function),
                    ],
                );
                self.scopes.push(scope);
                self.compile_instructions(body);
                self.scopes.pop();
                self.emit(BytecodeOp::FunctionExprEnd, Vec::new());
            }
            IrInstruction::Class {
                dst,
                name,
                super_class,
                members,
            } => {
                let mut operands = vec![
                    dst.as_ref()
                        .map(|dst| self.register_operand(dst))
                        .unwrap_or(BytecodeOperand::None),
                    name.as_ref()
                        .map(|name| self.name_operand(name))
                        .unwrap_or(BytecodeOperand::None),
                    super_class
                        .as_ref()
                        .map(|value| self.value_operand(value))
                        .unwrap_or(BytecodeOperand::None),
                    BytecodeOperand::Count(members.len() as u32),
                ];
                operands.extend(
                    members
                        .iter()
                        .map(|member| self.string_constant_operand(member)),
                );
                self.emit(BytecodeOp::Class, operands);
            }
            IrInstruction::Import { source, specifiers } => {
                let mut operands = vec![
                    self.string_constant_operand(source),
                    BytecodeOperand::Count(specifiers.len() as u32),
                ];
                operands.extend(
                    specifiers
                        .iter()
                        .map(|specifier| self.string_constant_operand(specifier)),
                );
                self.emit(BytecodeOp::Import, operands);
            }
            IrInstruction::Export { kind, names } => {
                let mut operands = vec![
                    self.string_constant_operand(kind),
                    BytecodeOperand::Count(names.len() as u32),
                ];
                operands.extend(names.iter().map(|name| self.name_operand(name)));
                self.emit(BytecodeOp::Export, operands);
            }
            IrInstruction::Throw(value) => {
                let value = self.value_operand(value);
                self.emit(BytecodeOp::Throw, vec![value]);
            }
            IrInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                self.emit(BytecodeOp::TryStart, Vec::new());
                self.compile_instructions(body);
                if !catch_body.is_empty() {
                    let catch_param = catch_param
                        .as_ref()
                        .map(|param| self.name_operand(param))
                        .unwrap_or(BytecodeOperand::None);
                    self.emit(BytecodeOp::CatchStart, vec![catch_param]);
                    self.compile_instructions(catch_body);
                }
                if !finally_body.is_empty() {
                    self.emit(BytecodeOp::FinallyStart, Vec::new());
                    self.compile_instructions(finally_body);
                }
                self.emit(BytecodeOp::TryEnd, Vec::new());
            }
            IrInstruction::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|value| self.value_operand(value))
                    .unwrap_or(BytecodeOperand::None);
                self.emit(BytecodeOp::Return, vec![value]);
            }
            IrInstruction::Pop(value) => {
                let value = self.value_operand(value);
                self.emit(BytecodeOp::Pop, vec![value]);
            }
            IrInstruction::Jump(label) => {
                let label = self.label_operand(label);
                self.emit(BytecodeOp::Jump, vec![label]);
            }
            IrInstruction::JumpIfFalse { test, label } => {
                let test = self.value_operand(test);
                let label = self.label_operand(label);
                self.emit(BytecodeOp::JumpIfFalse, vec![test, label]);
            }
            IrInstruction::Unsupported(message) => {
                let message = self.string_constant_operand(message);
                self.emit(BytecodeOp::Unsupported, vec![message]);
            }
        }
    }

    fn emit(&mut self, op: BytecodeOp, operands: Vec<BytecodeOperand>) {
        self.instructions.push(BytecodeInstruction { op, operands });
    }

    fn value_operand(&mut self, value: &IrValue) -> BytecodeOperand {
        match value {
            IrValue::Register(value) => self.register_operand(value),
            IrValue::Name(value) => self.name_ref_operand(value),
            IrValue::Number(value) => {
                BytecodeOperand::Constant(self.constant(BytecodeConstant::Number(*value)))
            }
            IrValue::String(value) => self.string_constant_operand(value),
            IrValue::Bool(value) => {
                BytecodeOperand::Constant(self.constant(BytecodeConstant::Bool(*value)))
            }
            IrValue::Null => BytecodeOperand::Constant(self.constant(BytecodeConstant::Null)),
            IrValue::Undefined => {
                BytecodeOperand::Constant(self.constant(BytecodeConstant::Undefined))
            }
        }
    }

    fn register_operand(&self, register: &str) -> BytecodeOperand {
        BytecodeOperand::Register(register_id(register))
    }

    fn name_operand(&mut self, name: &str) -> BytecodeOperand {
        let name = self.scoped_name(name);
        BytecodeOperand::Name(self.name(&name))
    }

    fn name_ref_operand(&mut self, name: &str) -> BytecodeOperand {
        let name = self.scoped_name(name);
        if let Some(slot) = self.extern_slot_ids.get(&name) {
            BytecodeOperand::External(*slot)
        } else {
            BytecodeOperand::Name(self.name(&name))
        }
    }

    fn label_operand(&mut self, label: &str) -> BytecodeOperand {
        BytecodeOperand::Label(self.label(label))
    }

    fn function_entry(
        &mut self,
        name: Option<&str>,
        params: &[String],
        body: &[IrInstruction],
        scope: &NameScope,
    ) -> u32 {
        let name = name.map(|name| {
            let scoped = self.scoped_name(name);
            self.name(&scoped)
        });
        let params = params
            .iter()
            .map(|param| {
                let name = scope.names.get(param).map(String::as_str).unwrap_or(param);
                self.name(name)
            })
            .collect();
        let id = self.functions.len() as u32;
        self.functions.push(BytecodeFunction {
            name,
            params,
            has_return: instructions_have_return_value(body),
        });
        id
    }

    fn operator_operand(&self, operator: &str) -> BytecodeOperand {
        BytecodeOperand::Operator(operator_id(operator))
    }

    fn string_constant_operand(&mut self, value: &str) -> BytecodeOperand {
        BytecodeOperand::Constant(self.constant(BytecodeConstant::String(value.to_string())))
    }

    fn constant(&mut self, constant: BytecodeConstant) -> u32 {
        let key = constant_key(&constant);
        if let Some(id) = self.constant_ids.get(&key) {
            return *id;
        }
        let id = self.constants.len() as u32;
        self.constants.push(constant);
        self.constant_ids.insert(key, id);
        id
    }

    fn name(&mut self, name: &str) -> u32 {
        if let Some(id) = self.name_ids.get(name) {
            return *id;
        }
        let id = self.names.len() as u32;
        self.names.push(name.to_string());
        self.name_ids.insert(name.to_string(), id);
        id
    }

    fn scoped_name(&self, name: &str) -> String {
        for scope in self.scopes.iter().rev() {
            if let Some(name) = scope.names.get(name) {
                return name.clone();
            }
        }
        name.to_string()
    }

    fn function_scope(
        &mut self,
        params: &[String],
        body: &[IrInstruction],
        function_name: Option<&str>,
    ) -> NameScope {
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        let mut used_encoded = BTreeSet::new();
        let mut scope = NameScope::default();
        if let Some(function_name) = function_name {
            if let Some(encoded) = self.scoped_name_if_local(function_name) {
                seen.insert(function_name.to_string());
                used_encoded.insert(encoded.clone());
                scope.names.insert(function_name.to_string(), encoded);
            }
        }
        for param in params {
            if seen.insert(param.clone()) {
                names.push(param.clone());
            }
        }
        collect_local_scope_names(body, &mut names, &mut seen);

        for name in names {
            let encoded = loop {
                let candidate = compact_local_name(self.local_name_id);
                self.local_name_id += 1;
                if used_encoded.insert(candidate.clone()) {
                    break candidate;
                }
            };
            scope.names.insert(name, encoded);
        }
        scope
    }

    fn scoped_name_if_local(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some(name) = scope.names.get(name) {
                return Some(name.clone());
            }
        }
        None
    }

    fn label(&mut self, label: &str) -> u32 {
        if let Some(id) = self.label_ids.get(label) {
            return *id;
        }
        let id = self.label_ids.len() as u32;
        self.label_ids.insert(label.to_string(), id);
        id
    }
}

fn register_id(register: &str) -> u32 {
    register
        .strip_prefix('t')
        .unwrap_or(register)
        .parse::<u32>()
        .unwrap_or(0)
}

fn collect_local_scope_names(
    instructions: &[IrInstruction],
    names: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) {
    for instruction in instructions {
        match instruction {
            IrInstruction::Declare { name, .. } | IrInstruction::Function { name, .. } => {
                if seen.insert(name.clone()) {
                    names.push(name.clone());
                }
            }
            IrInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_local_scope_names(body, names, seen);
                if let Some(catch_param) = catch_param {
                    if seen.insert(catch_param.clone()) {
                        names.push(catch_param.clone());
                    }
                }
                collect_local_scope_names(catch_body, names, seen);
                collect_local_scope_names(finally_body, names, seen);
            }
            IrInstruction::FunctionExpr { .. } => {}
            _ => {}
        }
    }
}

fn instructions_have_return_value(instructions: &[IrInstruction]) -> bool {
    instructions.iter().any(instruction_has_return_value)
}

fn instruction_has_return_value(instruction: &IrInstruction) -> bool {
    match instruction {
        IrInstruction::Return(Some(_)) => true,
        IrInstruction::Try {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            instructions_have_return_value(body)
                || instructions_have_return_value(catch_body)
                || instructions_have_return_value(finally_body)
        }
        IrInstruction::Function { .. } | IrInstruction::FunctionExpr { .. } => false,
        _ => false,
    }
}

fn compact_local_name(index: usize) -> String {
    const COMPACT_LOCAL_NAME_ALPHABET: &[char] = &[
        '~', '`', '!', '@', '#', '%', '^', '&', '*', '-', '+', '=', ':', ';', '.', ',', '?', '/',
        '|', '<', '>', '[', ']', '{', '}', '(', ')',
    ];

    if let Some(name) = COMPACT_LOCAL_NAME_ALPHABET.get(index) {
        return name.to_string();
    }

    format!(
        "~{}",
        encode_local_name_index(index - COMPACT_LOCAL_NAME_ALPHABET.len())
    )
}

fn encode_local_name_index(mut index: usize) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut encoded = Vec::new();
    loop {
        encoded.push(DIGITS[index % DIGITS.len()] as char);
        index /= DIGITS.len();
        if index == 0 {
            break;
        }
    }
    encoded.iter().rev().collect()
}

fn decl_kind_id(kind: &str) -> u32 {
    match kind {
        "var" => 0,
        "let" => 1,
        "const" => 2,
        _ => 3,
    }
}

fn decl_kind_name(kind: u32) -> Option<&'static str> {
    match kind {
        0 => Some("var"),
        1 => Some("let"),
        2 => Some("const"),
        3 => Some("decl"),
        _ => None,
    }
}

fn operator_id(operator: &str) -> u32 {
    OPERATOR_NAMES
        .iter()
        .position(|candidate| *candidate == operator)
        .unwrap_or(OPERATOR_NAMES.len()) as u32
}

fn operator_name(operator: u32) -> Option<&'static str> {
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

fn constant_key(constant: &BytecodeConstant) -> String {
    match constant {
        BytecodeConstant::Number(value) => format!("n:{value:?}"),
        BytecodeConstant::String(value) => format!("s:{value}"),
        BytecodeConstant::Bool(value) => format!("b:{value}"),
        BytecodeConstant::Null => "null".to_string(),
        BytecodeConstant::Undefined => "undefined".to_string(),
    }
}

fn strip_yaml_comment(line: &str) -> &str {
    line.split_once('#').map(|(value, _)| value).unwrap_or(line)
}

fn unquote_yaml(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}

fn parse_u8_yaml(value: &str, line_number: usize) -> Result<u8, EncodingError> {
    unquote_yaml(value)
        .parse::<u8>()
        .map_err(|err| EncodingError::Yaml(format!("line {line_number}: expected u8: {err}")))
}

fn normalize_opcode_key(key: &str) -> String {
    key.trim()
        .replace('-', "_")
        .chars()
        .flat_map(char::to_uppercase)
        .collect()
}

fn normalize_tag_key(key: &str) -> String {
    key.trim().replace('-', "_").to_ascii_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedObfuscationSeed {
    fingerprint: u64,
    permutation: String,
}

fn parse_obfuscation_seed(seed: &str) -> Result<ParsedObfuscationSeed, EncodingError> {
    let mut parts = seed.trim().split('-');
    let prefix = parts.next().unwrap_or_default();
    let fingerprint = parts.next().unwrap_or_default();
    let permutation = parts.next().unwrap_or_default();
    if parts.next().is_some() || prefix != ENCODING_SEED_PREFIX {
        return Err(EncodingError::Seed(format!(
            "expected {ENCODING_SEED_PREFIX}-<hash>-<perm>"
        )));
    }
    let fingerprint = u64::from_str_radix(fingerprint, 16)
        .map_err(|err| EncodingError::Seed(format!("invalid fingerprint: {err}")))?;
    validate_seed_permutation(permutation)?;
    Ok(ParsedObfuscationSeed {
        fingerprint,
        permutation: permutation.to_ascii_uppercase(),
    })
}

fn obfuscation_config_from_seed_permutation(
    permutation: &str,
) -> Result<ObfuscationConfig, EncodingError> {
    let mut parts = permutation.split('.');
    let opcode_perm = parts.next().unwrap_or_default();
    let operand_perm = parts.next().unwrap_or_default();
    let constant_perm = parts.next().unwrap_or_default();
    let extern_perm = parts.next();
    if parts.next().is_some() {
        return Err(EncodingError::Seed(
            "expected opcodes.operand_tags.constant_tags[.extern_slots] permutation".to_string(),
        ));
    }

    let config = ObfuscationConfig {
        encoding: EncodingNames {
            opcodes: seed_permutation_to_names(opcode_perm, &default_opcode_mnemonics(), "opcode")?,
            operand_tags: seed_permutation_to_names(
                operand_perm,
                &default_operand_tag_keys(),
                "operand tag",
            )?,
            constant_tags: seed_permutation_to_names(
                constant_perm,
                &default_constant_tag_keys(),
                "constant tag",
            )?,
        },
        extern_slots: extern_perm
            .map(|permutation| seed_permutation_to_indexes(permutation, "extern slot"))
            .transpose()?
            .unwrap_or_default(),
    };
    config.validate()?;
    Ok(config)
}

fn default_opcode_mnemonics() -> Vec<String> {
    BytecodeOp::all()
        .iter()
        .map(|op| op.mnemonic().to_string())
        .collect()
}

fn default_operand_tag_keys() -> Vec<String> {
    [
        "register", "constant", "name", "extern", "label", "count", "none", "function",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_constant_tag_keys() -> Vec<String> {
    ["number", "string", "bool", "null", "undefined"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn validate_unique_codes(values: &BTreeMap<String, u8>, kind: &str) -> Result<(), EncodingError> {
    let mut seen = BTreeSet::new();
    for (key, code) in values {
        if !seen.insert(*code) {
            return Err(EncodingError::Seed(format!(
                "duplicate {kind} code {code} at {key}"
            )));
        }
    }
    Ok(())
}

fn names_to_encoding_map(
    names: &[String],
    allowed: &[String],
    kind: &str,
) -> Result<BTreeMap<String, u8>, EncodingError> {
    if names.len() != allowed.len() {
        return Err(EncodingError::Seed(format!(
            "{kind} count mismatch: expected {}, got {}",
            allowed.len(),
            names.len()
        )));
    }

    let mut seen = vec![false; allowed.len()];
    let mut values = BTreeMap::new();
    for (code, name) in names.iter().enumerate() {
        let Some(index) = allowed.iter().position(|candidate| candidate == name) else {
            return Err(EncodingError::Seed(format!("unknown {kind} {name}")));
        };
        if seen[index] {
            return Err(EncodingError::Seed(format!("duplicate {kind} {name}")));
        }
        seen[index] = true;
        values.insert(name.clone(), code as u8);
    }
    Ok(values)
}

fn names_by_code(map: &BTreeMap<String, u8>) -> Vec<String> {
    let mut rows = map
        .iter()
        .map(|(name, code)| (*code, name.clone()))
        .collect::<Vec<_>>();
    rows.sort_by_key(|(code, _)| *code);
    rows.into_iter().map(|(_, name)| name).collect()
}

fn names_to_seed_permutation(
    names: &[String],
    keys: &[String],
    kind: &str,
) -> Result<String, EncodingError> {
    if names.len() != keys.len() {
        return Err(EncodingError::Seed(format!(
            "{kind} count mismatch: expected {}, got {}",
            keys.len(),
            names.len()
        )));
    }

    let mut seen = vec![false; keys.len()];
    let mut permutation = String::with_capacity(keys.len());
    for name in names {
        let normalized = if kind == "opcode" {
            normalize_opcode_key(name)
        } else {
            normalize_tag_key(name)
        };
        let Some(index) = keys.iter().position(|candidate| *candidate == normalized) else {
            return Err(EncodingError::Seed(format!("unknown {kind} {name}")));
        };
        if seen[index] {
            return Err(EncodingError::Seed(format!("duplicate {kind} {name}")));
        }
        seen[index] = true;
        permutation.push(encode_base36_digit(index as u8)?);
    }
    Ok(permutation)
}

fn indexes_to_seed_permutation(indexes: &[u8], kind: &str) -> Result<String, EncodingError> {
    validate_slot_permutation(indexes, kind)?;
    indexes
        .iter()
        .map(|index| encode_base36_digit(*index))
        .collect()
}

fn seed_permutation_to_names(
    permutation: &str,
    keys: &[String],
    kind: &str,
) -> Result<Vec<String>, EncodingError> {
    if permutation.len() != keys.len() {
        return Err(EncodingError::Seed(format!(
            "expected {kind} permutation length {}, got {}",
            keys.len(),
            permutation.len()
        )));
    }
    let mut seen = vec![false; keys.len()];
    let mut values = Vec::with_capacity(keys.len());
    for byte in permutation.bytes() {
        let index = decode_base36_digit(byte)? as usize;
        let Some(key) = keys.get(index) else {
            return Err(EncodingError::Seed(format!(
                "{kind} index {index} is outside seed range"
            )));
        };
        if seen[index] {
            return Err(EncodingError::Seed(format!(
                "duplicate {kind} {key} in seed"
            )));
        }
        seen[index] = true;
        values.push(key.clone());
    }
    if let Some((index, _)) = seen.iter().enumerate().find(|(_, value)| !**value) {
        return Err(EncodingError::Seed(format!(
            "missing {kind} {} in seed",
            keys[index]
        )));
    }
    Ok(values)
}

fn seed_permutation_to_indexes(permutation: &str, kind: &str) -> Result<Vec<u8>, EncodingError> {
    let mut indexes = Vec::with_capacity(permutation.len());
    for byte in permutation.bytes() {
        indexes.push(decode_base36_digit(byte)?);
    }
    validate_slot_permutation(&indexes, kind)?;
    Ok(indexes)
}

fn validate_slot_permutation(indexes: &[u8], kind: &str) -> Result<(), EncodingError> {
    if indexes.len() > 36 {
        return Err(EncodingError::Seed(format!(
            "{kind} permutation supports at most 36 entries"
        )));
    }
    let mut seen = vec![false; indexes.len()];
    for index in indexes {
        let index = *index as usize;
        if index >= indexes.len() {
            return Err(EncodingError::Seed(format!(
                "{kind} index {index} is outside seed range"
            )));
        }
        if seen[index] {
            return Err(EncodingError::Seed(format!(
                "duplicate {kind} index {index} in seed"
            )));
        }
        seen[index] = true;
    }
    Ok(())
}

fn validate_seed_permutation(permutation: &str) -> Result<(), EncodingError> {
    let mut parts = permutation.split('.');
    let opcode_perm = parts.next().unwrap_or_default();
    let operand_perm = parts.next().unwrap_or_default();
    let constant_perm = parts.next().unwrap_or_default();
    let extern_perm = parts.next();
    if parts.next().is_some() {
        return Err(EncodingError::Seed(
            "expected opcodes.operand_tags.constant_tags[.extern_slots] permutation".to_string(),
        ));
    }
    if opcode_perm.len() != BytecodeOp::all().len()
        || operand_perm.len() != default_operand_tag_keys().len()
        || constant_perm.len() != default_constant_tag_keys().len()
    {
        return Err(EncodingError::Seed(
            "seed permutation has invalid section length".to_string(),
        ));
    }
    if let Some(extern_perm) = extern_perm {
        seed_permutation_to_indexes(extern_perm, "extern slot")?;
    }
    Ok(())
}

fn encode_base36_digit(value: u8) -> Result<char, EncodingError> {
    match value {
        0..=9 => Ok(char::from(b'0' + value)),
        10..=35 => Ok(char::from(b'A' + value - 10)),
        _ => Err(EncodingError::Seed(format!(
            "seed index {value} is outside base36 range"
        ))),
    }
}

fn decode_base36_digit(value: u8) -> Result<u8, EncodingError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'z' => Ok(value - b'a' + 10),
        b'A'..=b'Z' => Ok(value - b'A' + 10),
        _ => Err(EncodingError::Seed(format!(
            "invalid base36 digit {:?}",
            char::from(value)
        ))),
    }
}

fn seed_fingerprint(permutation: &str, bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in permutation
        .as_bytes()
        .iter()
        .copied()
        .chain([0xff])
        .chain(bytes.iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn expect_magic(&mut self, encoding: &EncodingConfig) -> Result<(), EncodingError> {
        let magic = encoding.magic.as_bytes();
        let actual = self.read_slice(magic.len())?;
        if actual == magic {
            Ok(())
        } else {
            Err(EncodingError::InvalidMagic {
                expected: encoding.magic.clone(),
            })
        }
    }

    fn read_constant(
        &mut self,
        encoding: &EncodingConfig,
    ) -> Result<BytecodeConstant, EncodingError> {
        let tag = self.read_u8()?;
        if tag == encoding.constant_tag("number")? {
            return Ok(BytecodeConstant::Number(self.read_number()?));
        }
        if tag == encoding.constant_tag("string")? {
            return Ok(BytecodeConstant::String(self.read_constant_string()?));
        }
        if tag == encoding.constant_tag("bool")? {
            return Ok(BytecodeConstant::Bool(self.read_u8()? != 0));
        }
        if tag == encoding.constant_tag("null")? {
            return Ok(BytecodeConstant::Null);
        }
        if tag == encoding.constant_tag("undefined")? {
            return Ok(BytecodeConstant::Undefined);
        }
        Err(EncodingError::UnknownCode(format!("constant tag {tag}")))
    }

    fn read_u8(&mut self) -> Result<u8, EncodingError> {
        let bytes = self.read_slice(1)?;
        Ok(bytes[0])
    }

    fn read_u32(&mut self) -> Result<u32, EncodingError> {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.read_u8()?;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 32 {
                return Err(EncodingError::UnknownCode(
                    "varuint u32 overflow".to_string(),
                ));
            }
        }
    }

    fn read_optional_u32(&mut self) -> Result<Option<u32>, EncodingError> {
        match self.read_u32()? {
            0 => Ok(None),
            value => value
                .checked_sub(1)
                .map(Some)
                .ok_or_else(|| EncodingError::UnknownCode("optional u32 marker".to_string())),
        }
    }

    fn read_bounded_count(&mut self, kind: &str) -> Result<usize, EncodingError> {
        let count = self.read_u32()? as usize;
        let remaining = self.remaining();
        if count > remaining {
            return Err(EncodingError::UnknownCode(format!(
                "{kind} count {count} exceeds remaining bytecode bytes {remaining}"
            )));
        }
        Ok(count)
    }

    fn read_f64(&mut self) -> Result<f64, EncodingError> {
        let bytes = self.read_slice(8)?;
        Ok(f64::from_le_bytes(
            bytes.try_into().map_err(|_| EncodingError::UnexpectedEof)?,
        ))
    }

    fn read_number(&mut self) -> Result<f64, EncodingError> {
        match self.read_u8()? {
            0 => Ok(f64::from(decode_zigzag_u32(self.read_u32()?))),
            1 => self.read_f64(),
            kind => Err(EncodingError::UnknownCode(format!("number kind {kind}"))),
        }
    }

    fn read_string(&mut self) -> Result<String, EncodingError> {
        let len = self.read_u32()? as usize;
        let bytes = self.read_slice(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|err| EncodingError::UnknownCode(format!("utf8 string: {err}")))
    }

    fn read_constant_string(&mut self) -> Result<String, EncodingError> {
        match self.read_u32()? {
            0 => {
                let index = self.read_u32()? as usize;
                constant_string_atom(index)
                    .map(str::to_string)
                    .ok_or_else(|| EncodingError::UnknownCode(format!("string atom {index}")))
            }
            len_plus_one => {
                let len = len_plus_one
                    .checked_sub(1)
                    .ok_or_else(|| EncodingError::UnknownCode("string length marker".to_string()))?
                    as usize;
                let bytes = self.read_slice(len)?;
                String::from_utf8(bytes.to_vec())
                    .map_err(|err| EncodingError::UnknownCode(format!("utf8 string: {err}")))
            }
        }
    }

    fn read_name_string(&mut self, extern_slots: &[String]) -> Result<String, EncodingError> {
        let marker = self.read_u32()? as usize;
        if marker < extern_slots.len() {
            return Ok(extern_slots[marker].clone());
        }
        let atom_marker = extern_slots.len();
        if marker == atom_marker {
            let index = self.read_u32()? as usize;
            return constant_string_atom(index)
                .map(str::to_string)
                .ok_or_else(|| EncodingError::UnknownCode(format!("name string atom {index}")));
        }

        let len = marker
            .checked_sub(atom_marker + 1)
            .ok_or_else(|| EncodingError::UnknownCode("name string length marker".to_string()))?;
        let bytes = self.read_slice(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|err| EncodingError::UnknownCode(format!("utf8 name string: {err}")))
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], EncodingError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(EncodingError::UnexpectedEof)?;
        let Some(bytes) = self.bytes.get(self.offset..end) else {
            return Err(EncodingError::UnexpectedEof);
        };
        self.offset = end;
        Ok(bytes)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn expect_end(&self) -> Result<(), EncodingError> {
        let remaining = self.remaining();
        if remaining == 0 {
            Ok(())
        } else {
            Err(EncodingError::UnknownCode(format!(
                "trailing bytecode bytes {remaining}"
            )))
        }
    }
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    let mut value = value;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_optional_u32(bytes: &mut Vec<u8>, value: Option<u32>) {
    write_u32(bytes, value.map(|value| value + 1).unwrap_or(0));
}

fn write_number(bytes: &mut Vec<u8>, value: f64) {
    if value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX)
        && (value != 0.0 || !value.is_sign_negative())
    {
        bytes.push(0);
        write_u32(bytes, encode_zigzag_i32(value as i32));
    } else {
        bytes.push(1);
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn encode_zigzag_i32(value: i32) -> u32 {
    ((value << 1) ^ (value >> 31)) as u32
}

fn decode_zigzag_u32(value: u32) -> i32 {
    ((value >> 1) as i32) ^ (-((value & 1) as i32))
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
    write_u32(bytes, value.len() as u32);
    bytes.extend_from_slice(value.as_bytes());
}

fn write_constant_string(bytes: &mut Vec<u8>, value: &str) {
    if let Some(index) = constant_string_atom_index(value) {
        write_u32(bytes, 0);
        write_u32(bytes, index as u32);
    } else {
        write_u32(bytes, value.len() as u32 + 1);
        bytes.extend_from_slice(value.as_bytes());
    }
}

fn write_name_string(bytes: &mut Vec<u8>, value: &str, extern_slots: &[String]) {
    if let Some(index) = extern_slot_index(extern_slots, value) {
        write_u32(bytes, index as u32);
    } else if let Some(index) = constant_string_atom_index(value) {
        write_u32(bytes, extern_slots.len() as u32);
        write_u32(bytes, index as u32);
    } else {
        write_u32(bytes, extern_slots.len() as u32 + value.len() as u32 + 1);
        bytes.extend_from_slice(value.as_bytes());
    }
}

fn extern_slot_index(extern_slots: &[String], value: &str) -> Option<usize> {
    extern_slots.iter().position(|slot| slot == value)
}

fn constant_string_atom_index(value: &str) -> Option<usize> {
    CONSTANT_STRING_ATOMS
        .iter()
        .position(|candidate| *candidate == value)
}

fn constant_string_atom(index: usize) -> Option<&'static str> {
    CONSTANT_STRING_ATOMS.get(index).copied()
}

const CONSTANT_STRING_ATOMS: &[&str] = &[
    "",
    "const",
    "let",
    "var",
    "function",
    "return",
    "default",
    "named",
    "all",
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
    "!",
    "~",
    "typeof",
    "void",
    "delete",
    "in",
    "instanceof",
    "console",
    "log",
    "info",
    "warn",
    "error",
    "debug",
    "window",
    "document",
    "fetch",
    "then",
    "catch",
    "length",
    "prototype",
    "constructor",
    "toString",
    "valueOf",
];

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_BYTECODE_MAGIC, EncodingConfig, EncodingNames, IrInstruction, IrModule, IrValue,
        ObfuscationConfig, ObfuscationSeed,
    };

    #[test]
    fn renders_ir_text_from_core() {
        let module = IrModule {
            extern_slots: Vec::new(),
            instructions: vec![
                IrInstruction::Declare {
                    kind: "const".to_string(),
                    name: "a".to_string(),
                },
                IrInstruction::LoadConst {
                    dst: "t0".to_string(),
                    value: IrValue::Number(1.0),
                },
                IrInstruction::Return(Some(IrValue::Register("t0".to_string()))),
            ],
        };

        let text = module.to_text();
        assert!(text.contains("declare const a"));
        assert!(text.contains("%t0 = const 1"));
        assert!(text.contains("return %t0"));
    }

    #[test]
    fn lowers_ir_to_bytecode_in_core() {
        let module = IrModule {
            extern_slots: Vec::new(),
            instructions: vec![IrInstruction::LoadConst {
                dst: "t0".to_string(),
                value: IrValue::Number(1.0),
            }],
        };

        let bytecode = module.to_bytecode();
        assert!(bytecode.to_text().contains("LOAD_CONST"));
        assert!(
            bytecode
                .to_bytes()
                .starts_with(DEFAULT_BYTECODE_MAGIC.as_bytes())
        );
    }

    #[test]
    fn compact_bytecode_roundtrips_dynamic_operand_layouts() {
        let module = IrModule {
            extern_slots: vec!["console".to_string()],
            instructions: vec![
                IrInstruction::Array {
                    dst: "arr".to_string(),
                    items: vec![IrValue::Number(1.0), IrValue::String("x".to_string())],
                },
                IrInstruction::Object {
                    dst: "obj".to_string(),
                    props: vec![
                        ("a".to_string(), IrValue::Register("arr".to_string())),
                        ("b".to_string(), IrValue::Bool(true)),
                    ],
                },
                IrInstruction::Call {
                    dst: "call".to_string(),
                    callee: IrValue::Name("fn".to_string()),
                    args: vec![
                        IrValue::Register("arr".to_string()),
                        IrValue::Register("obj".to_string()),
                    ],
                },
                IrInstruction::New {
                    dst: "instance".to_string(),
                    callee: IrValue::Name("Ctor".to_string()),
                    args: vec![IrValue::Register("call".to_string())],
                },
                IrInstruction::Template {
                    dst: "template".to_string(),
                    quasis: vec!["hello ".to_string(), "".to_string()],
                    exprs: vec![IrValue::Register("instance".to_string())],
                },
                IrInstruction::Function {
                    name: "named".to_string(),
                    params: vec!["value".to_string()],
                    body: vec![IrInstruction::Return(Some(IrValue::Name(
                        "value".to_string(),
                    )))],
                },
                IrInstruction::FunctionExpr {
                    dst: "expr".to_string(),
                    name: None,
                    params: vec!["left".to_string(), "right".to_string()],
                    body: vec![IrInstruction::Return(None)],
                },
                IrInstruction::Class {
                    dst: Some("klass".to_string()),
                    name: Some("Klass".to_string()),
                    super_class: Some(IrValue::Name("Base".to_string())),
                    members: vec!["method".to_string(), "field".to_string()],
                },
                IrInstruction::Import {
                    source: "./mod.js".to_string(),
                    specifiers: vec!["a".to_string(), "b".to_string()],
                },
                IrInstruction::Export {
                    kind: "named".to_string(),
                    names: vec!["a".to_string(), "b".to_string()],
                },
            ],
        };

        let bytecode = module.to_bytecode();
        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();

        assert_eq!(restored, bytecode);
        assert!(bytes.starts_with(DEFAULT_BYTECODE_MAGIC.as_bytes()));
    }

    #[test]
    fn functions_are_declared_in_fun_section_and_indexed_from_code() {
        let module = IrModule {
            extern_slots: Vec::new(),
            instructions: vec![IrInstruction::Function {
                name: "add".to_string(),
                params: vec!["left".to_string(), "right".to_string()],
                body: vec![IrInstruction::Return(Some(IrValue::Name(
                    "left".to_string(),
                )))],
            }],
        };

        let bytecode = module.to_bytecode();
        let text = bytecode.to_text();
        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();

        assert_eq!(bytecode.functions.len(), 1);
        assert_eq!(bytecode.functions[0].params.len(), 2);
        assert!(bytecode.functions[0].has_return);
        assert!(text.contains(".fun"), "{text}");
        assert!(text.contains("argc:2"), "{text}");
        assert!(text.contains("returns:true"), "{text}");
        assert!(text.contains("FUNCTION_START fun#0"), "{text}");
        assert!(!text.contains("FUNCTION_START fun#0("), "{text}");
        assert!(!text.contains("FUNCTION_START name#"), "{text}");
        assert_eq!(restored, bytecode);
    }

    #[test]
    fn specialized_opcodes_decode_to_canonical_instructions() {
        let bytecode = super::BytecodeModule {
            extern_slots: Vec::new(),
            names: Vec::new(),
            functions: Vec::new(),
            constants: vec![super::BytecodeConstant::Number(1.0)],
            instructions: vec![
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Pop,
                    operands: vec![super::BytecodeOperand::Register(0)],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Return,
                    operands: vec![super::BytecodeOperand::Register(0)],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Call,
                    operands: vec![
                        super::BytecodeOperand::Register(1),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Count(1),
                        super::BytecodeOperand::Register(0),
                    ],
                },
            ],
        };
        let bytes = bytecode.to_bytes();

        assert!(bytes.contains(&(super::BytecodeOp::LoadConstConst as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::PopReg as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::CallOne as u8)));
        assert_eq!(super::BytecodeModule::from_bytes(&bytes).unwrap(), bytecode);
    }

    #[test]
    fn string_constants_use_compact_atoms() {
        let bytecode = super::BytecodeModule {
            extern_slots: Vec::new(),
            names: Vec::new(),
            functions: Vec::new(),
            constants: vec![
                super::BytecodeConstant::String("const".to_string()),
                super::BytecodeConstant::String("+".to_string()),
                super::BytecodeConstant::String("custom-name".to_string()),
            ],
            instructions: Vec::new(),
        };
        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();

        assert_eq!(restored, bytecode);
        assert!(bytes.len() < DEFAULT_BYTECODE_MAGIC.len() + 1 + 1 + 8 + 3 + 11 + 1);
    }

    #[test]
    fn load_name_can_reference_extern_slots_without_names_entry() {
        let bytecode = super::BytecodeModule {
            extern_slots: vec!["console".to_string()],
            names: Vec::new(),
            functions: Vec::new(),
            constants: vec![
                super::BytecodeConstant::String("log".to_string()),
                super::BytecodeConstant::String("ass".to_string()),
            ],
            instructions: vec![
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadName,
                    operands: vec![
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::External(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Member,
                    operands: vec![
                        super::BytecodeOperand::Register(1),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(2),
                        super::BytecodeOperand::Constant(1),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Call,
                    operands: vec![
                        super::BytecodeOperand::Register(3),
                        super::BytecodeOperand::Register(1),
                        super::BytecodeOperand::Count(1),
                        super::BytecodeOperand::Register(2),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Pop,
                    operands: vec![super::BytecodeOperand::Register(3)],
                },
            ],
        };

        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();
        let text = bytecode.to_text();

        assert_eq!(restored, bytecode);
        assert_eq!(count_subslice(&bytes, b"console"), 1);
        assert!(!text.contains(".names"));
        assert!(text.contains("LOAD_NAME r0, extern#0(\"console\")"));
    }

    #[test]
    fn encodes_bytecode_with_yaml_config() {
        let module = IrModule {
            extern_slots: Vec::new(),
            instructions: vec![IrInstruction::LoadConst {
                dst: "t0".to_string(),
                value: IrValue::Number(1.0),
            }],
        };
        let bytecode = module.to_bytecode();
        let encoding = EncodingConfig::from_yaml(
            r#"
            magic: "CUSTOM01"
            opcodes:
              LOAD_CONST: 99
            operand_tags:
              register: 11
              constant: 12
            constant_tags:
              number: 13
            "#,
        )
        .unwrap();

        let bytes = bytecode.to_bytes_with_encoding(&encoding).unwrap();
        let restored = super::BytecodeModule::from_bytes_with_encoding(&bytes, &encoding).unwrap();
        assert!(bytes.starts_with(b"CUSTOM01"));
        assert_eq!(restored, bytecode);
        assert!(super::BytecodeModule::from_bytes(&bytes).is_err());
    }

    #[test]
    fn encoding_seed_restores_config_and_rejects_mismatched_bytes() {
        let module = IrModule {
            extern_slots: Vec::new(),
            instructions: vec![IrInstruction::LoadConst {
                dst: "t0".to_string(),
                value: IrValue::Number(1.0),
            }],
        };
        let bytecode = module.to_bytecode();
        let encoding = EncodingConfig::from_yaml(
            r#"
            opcodes:
              LOAD_CONST: 8
              BINARY: 3
            operand_tags:
              register: 2
              constant: 0
              name: 1
            constant_tags:
              number: 2
              string: 0
              bool: 1
            "#,
        )
        .unwrap();

        let bytes = bytecode.to_bytes_with_encoding(&encoding).unwrap();
        let seed = encoding.to_seed(&bytes).unwrap();
        let restored = EncodingConfig::from_seed_for_bytes(&seed, &bytes).unwrap();
        assert_eq!(restored.opcodes.get("LOAD_CONST"), Some(&8));
        assert_eq!(restored.opcodes.get("BINARY"), Some(&3));
        assert_eq!(restored.operand_tags.get("register"), Some(&2));
        assert_eq!(restored.operand_tags.get("constant"), Some(&0));
        assert_eq!(restored.constant_tags.get("number"), Some(&2));
        assert_eq!(restored.constant_tags.get("string"), Some(&0));
        assert_eq!(
            bytecode,
            super::BytecodeModule::from_bytes_with_seed(&bytes, &seed).unwrap()
        );

        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(EncodingConfig::from_seed_for_bytes(&seed, &tampered).is_err());
    }

    #[test]
    fn encoding_names_roundtrip_through_seed() {
        let mut names = EncodingNames::default();
        names.opcodes.swap(3, 8);
        names.operand_tags.swap(0, 2);
        names.constant_tags.swap(0, 2);

        let encoding = EncodingConfig::from_names(&names).unwrap();
        let seed = encoding.config_seed().unwrap();
        let restored = EncodingConfig::from_seed(&seed).unwrap();

        assert_eq!(restored.names(), names);
        assert_eq!(restored.opcodes.get("LOAD_CONST"), Some(&8));
        assert_eq!(restored.operand_tags.get("register"), Some(&2));
        assert_eq!(restored.constant_tags.get("number"), Some(&2));
    }

    #[test]
    fn encoding_seed_accepts_ui_extern_slot_permutation() {
        let mut names = EncodingNames::default();
        names.opcodes.swap(3, 8);
        let encoding = EncodingConfig::from_names(&names).unwrap();
        let seed = format!("{}.210", encoding.config_seed().unwrap());
        let restored = EncodingConfig::from_seed(&seed).unwrap();
        let obfuscation = ObfuscationConfig::from_seed(&seed).unwrap();

        assert_eq!(restored.names(), names);
        assert_eq!(obfuscation.encoding, names);
        assert_eq!(obfuscation.extern_slots, vec![2, 1, 0]);
        assert!(
            EncodingConfig::from_seed(&format!("{}.211", encoding.config_seed().unwrap())).is_err()
        );
    }

    #[test]
    fn obfuscation_config_roundtrips_seed_and_fingerprint() {
        let mut names = EncodingNames::default();
        names.opcodes.swap(3, 8);
        names.operand_tags.swap(0, 2);

        let config =
            ObfuscationConfig::from_encoding_and_extern_slots(names.clone(), vec![1, 0]).unwrap();
        let seed = config.paired_seed(b"abc").unwrap();
        let parsed = ObfuscationSeed::parse_for_bytes(&seed, b"abc").unwrap();

        assert_eq!(parsed.config.encoding, names);
        assert_eq!(parsed.config.extern_slots, vec![1, 0]);
        assert!(ObfuscationSeed::parse_for_bytes(&seed, b"abd").is_err());
    }

    #[test]
    fn encoding_rejects_duplicate_codes() {
        let mut encoding = EncodingConfig::default();
        encoding.opcodes.insert("LOAD_CONST".to_string(), 8);

        assert!(encoding.validate().is_err());
    }

    fn count_subslice(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }
}
