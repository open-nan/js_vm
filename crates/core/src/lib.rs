use std::{
    collections::BTreeMap,
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
        }
    }

    pub fn from_mnemonic(mnemonic: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|op| op.mnemonic() == mnemonic)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingConfig {
    pub magic: String,
    pub opcodes: BTreeMap<String, u8>,
    pub operand_tags: BTreeMap<String, u8>,
    pub constant_tags: BTreeMap<String, u8>,
}

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
        ]
        .into_iter()
        .map(|(op, code)| (op.mnemonic().to_string(), code))
        .collect();

        let operand_tags = [
            ("register", 0),
            ("constant", 1),
            ("name", 2),
            ("label", 3),
            ("count", 4),
            ("none", 5),
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
            magic: "JSTKBC01".to_string(),
            opcodes,
            operand_tags,
            constant_tags,
        }
    }
}

impl EncodingConfig {
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
        let perm = self.seed_permutation()?;
        let fingerprint = seed_fingerprint(&perm, bytes);
        Ok(format!("{ENCODING_SEED_PREFIX}-{fingerprint:016x}-{perm}"))
    }

    pub fn from_seed(seed: &str) -> Result<Self, EncodingError> {
        let parsed = parse_encoding_seed(seed)?;
        encoding_from_seed_permutation(&parsed.permutation)
    }

    pub fn from_seed_for_bytes(seed: &str, bytes: &[u8]) -> Result<Self, EncodingError> {
        let parsed = parse_encoding_seed(seed)?;
        let actual = seed_fingerprint(&parsed.permutation, bytes);
        if actual != parsed.fingerprint {
            return Err(EncodingError::Seed(
                "seed does not match bytecode bytes".to_string(),
            ));
        }
        encoding_from_seed_permutation(&parsed.permutation)
    }

    fn seed_permutation(&self) -> Result<String, EncodingError> {
        Ok(format!(
            "{}.{}.{}",
            map_to_seed_permutation(&self.opcodes, &default_opcode_mnemonics(), "opcode")?,
            map_to_seed_permutation(
                &self.operand_tags,
                &default_operand_tag_keys(),
                "operand tag"
            )?,
            map_to_seed_permutation(
                &self.constant_tags,
                &default_constant_tag_keys(),
                "constant tag"
            )?
        ))
    }

    fn validate(&self) -> Result<(), EncodingError> {
        if self.magic.is_empty() {
            return Err(EncodingError::MissingKey("magic".to_string()));
        }
        for op in BytecodeOp::all() {
            self.opcode(*op)?;
        }
        for key in ["register", "constant", "name", "label", "count", "none"] {
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
    Label(u32),
    Count(u32),
    None,
}

impl BytecodeOperand {
    fn tag(&self, encoding: &EncodingConfig) -> Result<u8, EncodingError> {
        match self {
            BytecodeOperand::Register(_) => encoding.operand_tag("register"),
            BytecodeOperand::Constant(_) => encoding.operand_tag("constant"),
            BytecodeOperand::Name(_) => encoding.operand_tag("name"),
            BytecodeOperand::Label(_) => encoding.operand_tag("label"),
            BytecodeOperand::Count(_) => encoding.operand_tag("count"),
            BytecodeOperand::None => encoding.operand_tag("none"),
        }
    }

    fn payload(&self) -> u32 {
        match self {
            BytecodeOperand::Register(value)
            | BytecodeOperand::Constant(value)
            | BytecodeOperand::Name(value)
            | BytecodeOperand::Label(value)
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

#[derive(Debug, Default, Clone, PartialEq)]
pub struct BytecodeModule {
    pub extern_slots: Vec<String>,
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
        write_u32(&mut bytes, self.constants.len() as u32);
        for constant in &self.constants {
            match constant {
                BytecodeConstant::Number(value) => {
                    bytes.push(encoding.constant_tag("number")?);
                    write_number(&mut bytes, *value);
                }
                BytecodeConstant::String(value) => {
                    bytes.push(encoding.constant_tag("string")?);
                    write_string(&mut bytes, value);
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
            bytes.push(encoding.opcode(instruction.op)?);
            write_u32(&mut bytes, instruction.operands.len() as u32);
            for operand in &instruction.operands {
                bytes.push(operand.tag(encoding)?);
                write_u32(&mut bytes, operand.payload());
            }
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

        let extern_count = cursor.read_u32()? as usize;
        let mut extern_slots = Vec::with_capacity(extern_count);
        for _ in 0..extern_count {
            extern_slots.push(cursor.read_string()?);
        }

        let constant_count = cursor.read_u32()? as usize;
        let mut constants = Vec::with_capacity(constant_count);
        for _ in 0..constant_count {
            constants.push(cursor.read_constant(encoding)?);
        }

        let instruction_count = cursor.read_u32()? as usize;
        let mut instructions = Vec::with_capacity(instruction_count);
        for _ in 0..instruction_count {
            let op = encoding.opcode_from_code(cursor.read_u8()?)?;
            let operand_count = cursor.read_u32()? as usize;
            let mut operands = Vec::with_capacity(operand_count);
            for _ in 0..operand_count {
                let tag = cursor.read_u8()?;
                let payload = cursor.read_u32()?;
                operands.push(BytecodeOperand::from_tag_payload(tag, payload, encoding)?);
            }
            instructions.push(BytecodeInstruction { op, operands });
        }

        Ok(Self {
            extern_slots,
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
                    .constants
                    .get(*index as usize)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "<bad-name>".to_string());
                format!("name#{index}({name})")
            }
            BytecodeOperand::Label(index) => {
                let label = self
                    .constants
                    .get(*index as usize)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "<bad-label>".to_string());
                format!("label#{index}({label})")
            }
            BytecodeOperand::Count(value) => format!("#{value}"),
            BytecodeOperand::None => "none".to_string(),
        }
    }
}

#[derive(Default)]
struct BytecodeBuilder {
    constants: Vec<BytecodeConstant>,
    constant_ids: BTreeMap<String, u32>,
    instructions: Vec<BytecodeInstruction>,
}

impl BytecodeBuilder {
    fn compile_module(mut self, module: &IrModule) -> BytecodeModule {
        self.compile_instructions(&module.instructions);
        BytecodeModule {
            extern_slots: module.extern_slots.clone(),
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
                let kind = self.string_constant_operand(kind);
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
                let name = self.name_operand(name);
                self.emit(BytecodeOp::LoadName, vec![dst, name]);
            }
            IrInstruction::StoreName { name, src } => {
                let name = self.name_operand(name);
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
                let op = self.string_constant_operand(op);
                let left = self.value_operand(left);
                let right = self.value_operand(right);
                self.emit(BytecodeOp::Binary, vec![dst, op, left, right]);
            }
            IrInstruction::Unary { dst, op, arg } => {
                let dst = self.register_operand(dst);
                let op = self.string_constant_operand(op);
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
                let mut operands = vec![
                    self.name_operand(name),
                    BytecodeOperand::Count(params.len() as u32),
                ];
                operands.extend(params.iter().map(|param| self.name_operand(param)));
                self.emit(BytecodeOp::FunctionStart, operands);
                self.compile_instructions(body);
                self.emit(BytecodeOp::FunctionEnd, Vec::new());
            }
            IrInstruction::FunctionExpr {
                dst,
                name,
                params,
                body,
            } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    name.as_ref()
                        .map(|name| self.name_operand(name))
                        .unwrap_or(BytecodeOperand::None),
                    BytecodeOperand::Count(params.len() as u32),
                ];
                operands.extend(params.iter().map(|param| self.name_operand(param)));
                self.emit(BytecodeOp::FunctionExprStart, operands);
                self.compile_instructions(body);
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
            IrValue::Name(value) => self.name_operand(value),
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
        BytecodeOperand::Name(self.constant(BytecodeConstant::String(name.to_string())))
    }

    fn label_operand(&mut self, label: &str) -> BytecodeOperand {
        BytecodeOperand::Label(self.constant(BytecodeConstant::String(label.to_string())))
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
}

fn register_id(register: &str) -> u32 {
    register
        .strip_prefix('t')
        .unwrap_or(register)
        .parse::<u32>()
        .unwrap_or(0)
}

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
struct ParsedEncodingSeed {
    fingerprint: u64,
    permutation: String,
}

fn parse_encoding_seed(seed: &str) -> Result<ParsedEncodingSeed, EncodingError> {
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
    Ok(ParsedEncodingSeed {
        fingerprint,
        permutation: permutation.to_ascii_uppercase(),
    })
}

fn encoding_from_seed_permutation(permutation: &str) -> Result<EncodingConfig, EncodingError> {
    let mut parts = permutation.split('.');
    let opcode_perm = parts.next().unwrap_or_default();
    let operand_perm = parts.next().unwrap_or_default();
    let constant_perm = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err(EncodingError::Seed(
            "expected opcodes.operand_tags.constant_tags permutation".to_string(),
        ));
    }

    let mut config = EncodingConfig::default();
    config.opcodes = seed_permutation_to_map(opcode_perm, &default_opcode_mnemonics(), "opcode")?;
    config.operand_tags =
        seed_permutation_to_map(operand_perm, &default_operand_tag_keys(), "operand tag")?;
    config.constant_tags =
        seed_permutation_to_map(constant_perm, &default_constant_tag_keys(), "constant tag")?;
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
    ["register", "constant", "name", "label", "count", "none"]
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

fn map_to_seed_permutation(
    values: &BTreeMap<String, u8>,
    keys: &[String],
    kind: &str,
) -> Result<String, EncodingError> {
    let mut by_code = vec![String::new(); keys.len()];
    for (key, code) in values {
        let code = *code as usize;
        if code >= by_code.len() {
            return Err(EncodingError::Seed(format!(
                "{kind} code {code} is outside seed range"
            )));
        }
        if !by_code[code].is_empty() {
            return Err(EncodingError::Seed(format!(
                "duplicate {kind} code {code} in seed source"
            )));
        }
        by_code[code] = if kind == "opcode" {
            normalize_opcode_key(key)
        } else {
            normalize_tag_key(key)
        };
    }

    let mut perm = String::with_capacity(keys.len());
    for key in by_code {
        let Some(index) = keys.iter().position(|candidate| *candidate == key) else {
            return Err(EncodingError::Seed(format!("unknown {kind} {key}")));
        };
        perm.push(encode_base36_digit(index as u8));
    }
    Ok(perm)
}

fn seed_permutation_to_map(
    permutation: &str,
    keys: &[String],
    kind: &str,
) -> Result<BTreeMap<String, u8>, EncodingError> {
    if permutation.len() != keys.len() {
        return Err(EncodingError::Seed(format!(
            "expected {kind} permutation length {}, got {}",
            keys.len(),
            permutation.len()
        )));
    }
    let mut seen = vec![false; keys.len()];
    let mut values = BTreeMap::new();
    for (code, byte) in permutation.bytes().enumerate() {
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
        values.insert(key.clone(), code as u8);
    }
    if let Some((index, _)) = seen.iter().enumerate().find(|(_, value)| !**value) {
        return Err(EncodingError::Seed(format!(
            "missing {kind} {} in seed",
            keys[index]
        )));
    }
    Ok(values)
}

fn validate_seed_permutation(permutation: &str) -> Result<(), EncodingError> {
    let mut parts = permutation.split('.');
    let opcode_perm = parts.next().unwrap_or_default();
    let operand_perm = parts.next().unwrap_or_default();
    let constant_perm = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err(EncodingError::Seed(
            "expected opcodes.operand_tags.constant_tags permutation".to_string(),
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
    Ok(())
}

fn encode_base36_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=35 => char::from(b'A' + value - 10),
        _ => '?',
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
            return Ok(BytecodeConstant::String(self.read_string()?));
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

#[cfg(test)]
mod tests {
    use super::{EncodingConfig, IrInstruction, IrModule, IrValue};

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
        assert!(bytecode.to_bytes().starts_with(b"JSTKBC01"));
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
}
