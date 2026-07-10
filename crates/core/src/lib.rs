use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::{self, Write},
};

pub mod ir;
pub use ir::*;

#[derive(Debug, Clone, PartialEq)]
enum LowerValue {
    Register(String),
    Name(String),
    LocalSlot(u32),
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Undefined,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum LowerBinding {
    Name(String),
    LocalSlot(u32),
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
enum LowerInstruction {
    Marker(String),
    Label(String),
    Declare {
        kind: String,
        name: LowerBinding,
    },
    LoadConst {
        dst: String,
        value: LowerValue,
    },
    LoadName {
        dst: String,
        name: LowerBinding,
    },
    StoreName {
        name: LowerBinding,
        src: LowerValue,
    },
    StoreMember {
        object: LowerValue,
        property: LowerValue,
        src: LowerValue,
    },
    Move {
        dst: String,
        src: LowerValue,
    },
    Binary {
        dst: String,
        op: String,
        left: LowerValue,
        right: LowerValue,
    },
    Unary {
        dst: String,
        op: String,
        arg: LowerValue,
    },
    Member {
        dst: String,
        object: LowerValue,
        property: LowerValue,
    },
    Array {
        dst: String,
        items: Vec<LowerValue>,
    },
    Object {
        dst: String,
        props: Vec<(String, LowerValue)>,
    },
    Call {
        dst: String,
        callee: LowerValue,
        args: Vec<LowerValue>,
    },
    New {
        dst: String,
        callee: LowerValue,
        args: Vec<LowerValue>,
    },
    Template {
        dst: String,
        quasis: Vec<String>,
        exprs: Vec<LowerValue>,
    },
    Function {
        name: String,
        params: Vec<LowerBinding>,
        body: Vec<LowerInstruction>,
    },
    FunctionExpr {
        dst: String,
        name: Option<String>,
        params: Vec<LowerBinding>,
        body: Vec<LowerInstruction>,
    },
    Class {
        dst: Option<String>,
        name: Option<String>,
        super_class: Option<LowerValue>,
        members: Vec<String>,
    },
    Import {
        source: String,
        specifiers: Vec<String>,
    },
    Export {
        kind: String,
        names: Vec<String>,
    },
    Throw(LowerValue),
    Try {
        body: Vec<LowerInstruction>,
        catch_param: Option<LowerBinding>,
        catch_body: Vec<LowerInstruction>,
        finally_body: Vec<LowerInstruction>,
    },
    TryStart,
    CatchStart(Option<LowerBinding>),
    FinallyStart,
    TryEnd,
    Scope {
        kind: String,
        body: Vec<LowerInstruction>,
    },
    EnterScope(String),
    LeaveScope,
    Return(Option<LowerValue>),
    Pop(LowerValue),
    Jump(String),
    JumpIfFalse {
        test: LowerValue,
        label: String,
    },
    Unsupported(String),
}

impl IrModule {
    pub fn to_text(&self) -> String {
        self.to_string()
    }

    pub fn to_bytecode(&self) -> BytecodeModule {
        BytecodeBuilder::default().compile_module(self)
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
    EnterScope = 36,
    LeaveScope = 37,
    LoadLocal = 38,
    StoreLocal = 39,
    LoadUndefined = 40,
    LoadNull = 41,
    LoadTrue = 42,
    LoadFalse = 43,
    LoadIntSmall = 44,
    MemberConst = 45,
    StoreMemberConst = 46,
    CallZero = 47,
    CallTwo = 48,
    ReturnReg = 49,
    ReturnConst = 50,
    JumpIfFalseReg = 51,
    BinaryRegReg = 52,
    BinaryRegConst = 53,
    LoadLocalSmall = 54,
    StoreLocalSmall = 55,
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
            BytecodeOp::EnterScope,
            BytecodeOp::LeaveScope,
            BytecodeOp::LoadLocal,
            BytecodeOp::StoreLocal,
            BytecodeOp::LoadUndefined,
            BytecodeOp::LoadNull,
            BytecodeOp::LoadTrue,
            BytecodeOp::LoadFalse,
            BytecodeOp::LoadIntSmall,
            BytecodeOp::MemberConst,
            BytecodeOp::StoreMemberConst,
            BytecodeOp::CallZero,
            BytecodeOp::CallTwo,
            BytecodeOp::ReturnReg,
            BytecodeOp::ReturnConst,
            BytecodeOp::JumpIfFalseReg,
            BytecodeOp::BinaryRegReg,
            BytecodeOp::BinaryRegConst,
            BytecodeOp::LoadLocalSmall,
            BytecodeOp::StoreLocalSmall,
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
            BytecodeOp::EnterScope => "ENTER_SCOPE",
            BytecodeOp::LeaveScope => "LEAVE_SCOPE",
            BytecodeOp::LoadLocal => "LOAD_LOCAL",
            BytecodeOp::StoreLocal => "STORE_LOCAL",
            BytecodeOp::LoadUndefined => "LOAD_UNDEFINED",
            BytecodeOp::LoadNull => "LOAD_NULL",
            BytecodeOp::LoadTrue => "LOAD_TRUE",
            BytecodeOp::LoadFalse => "LOAD_FALSE",
            BytecodeOp::LoadIntSmall => "LOAD_INT_SMALL",
            BytecodeOp::MemberConst => "MEMBER_CONST",
            BytecodeOp::StoreMemberConst => "STORE_MEMBER_CONST",
            BytecodeOp::CallZero => "CALL_0",
            BytecodeOp::CallTwo => "CALL_2",
            BytecodeOp::ReturnReg => "RETURN_REG",
            BytecodeOp::ReturnConst => "RETURN_CONST",
            BytecodeOp::JumpIfFalseReg => "JUMP_IF_FALSE_REG",
            BytecodeOp::BinaryRegReg => "BINARY_REG_REG",
            BytecodeOp::BinaryRegConst => "BINARY_REG_CONST",
            BytecodeOp::LoadLocalSmall => "LOAD_LOCAL_SMALL",
            BytecodeOp::StoreLocalSmall => "STORE_LOCAL_SMALL",
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
            BytecodeOp::LoadLocal => BytecodeOp::LoadName,
            BytecodeOp::StoreLocal => BytecodeOp::StoreName,
            BytecodeOp::LoadUndefined
            | BytecodeOp::LoadNull
            | BytecodeOp::LoadTrue
            | BytecodeOp::LoadFalse
            | BytecodeOp::LoadIntSmall => BytecodeOp::LoadConst,
            BytecodeOp::MemberConst => BytecodeOp::Member,
            BytecodeOp::StoreMemberConst => BytecodeOp::StoreMember,
            BytecodeOp::CallZero | BytecodeOp::CallTwo => BytecodeOp::Call,
            BytecodeOp::ReturnReg | BytecodeOp::ReturnConst => BytecodeOp::Return,
            BytecodeOp::JumpIfFalseReg => BytecodeOp::JumpIfFalse,
            BytecodeOp::BinaryRegReg | BytecodeOp::BinaryRegConst => BytecodeOp::Binary,
            BytecodeOp::LoadLocalSmall => BytecodeOp::LoadName,
            BytecodeOp::StoreLocalSmall => BytecodeOp::StoreName,
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
        let opcodes = BytecodeOp::all()
            .iter()
            .enumerate()
            .map(|(code, op)| (op.mnemonic().to_string(), code as u8))
            .collect();

        let operand_tags = [
            ("register", 0),
            ("constant", 1),
            ("name", 2),
            ("local", 3),
            ("extern", 4),
            ("label", 5),
            ("count", 6),
            ("none", 7),
            ("function", 8),
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
        validate_operand_tags_do_not_use_compact_range(&self.operand_tags)?;
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
    LocalSlot(u32),
    External(u32),
    Function(u32),
    Label(u32),
    Operator(u32),
    DeclKind(u32),
    ScopeKind(u32),
    Count(u32),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandKind {
    Register,
    Constant,
    Name,
    NameRef,
    LocalSlot,
    Function,
    Label,
    Operator,
    DeclKind,
    ScopeKind,
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
            BytecodeOperand::LocalSlot(_) => encoding.operand_tag("local"),
            BytecodeOperand::External(_) => encoding.operand_tag("extern"),
            BytecodeOperand::Function(_) => encoding.operand_tag("function"),
            BytecodeOperand::Label(_) => encoding.operand_tag("label"),
            BytecodeOperand::Operator(_) => encoding.operand_tag("count"),
            BytecodeOperand::Count(_) => encoding.operand_tag("count"),
            BytecodeOperand::DeclKind(_) => encoding.operand_tag("count"),
            BytecodeOperand::ScopeKind(_) => encoding.operand_tag("count"),
            BytecodeOperand::None => encoding.operand_tag("none"),
        }
    }

    fn payload(&self) -> u32 {
        match self {
            BytecodeOperand::Register(value)
            | BytecodeOperand::Constant(value)
            | BytecodeOperand::Name(value)
            | BytecodeOperand::LocalSlot(value)
            | BytecodeOperand::External(value)
            | BytecodeOperand::Function(value)
            | BytecodeOperand::Label(value)
            | BytecodeOperand::Operator(value)
            | BytecodeOperand::DeclKind(value)
            | BytecodeOperand::ScopeKind(value)
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
            "local" => Ok(BytecodeOperand::LocalSlot(payload)),
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
    pub params: Vec<BytecodeOperand>,
    pub body_start: u32,
    pub body_end: u32,
    pub flags: u32,
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BytecodeBytesProfile {
    pub total_bytes: usize,
    pub instruction_count: usize,
    pub sections: Vec<BytecodeBytesProfileEntry>,
    pub opcodes: Vec<BytecodeBytesProfileEntry>,
    pub operands: Vec<BytecodeBytesProfileEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BytecodeBytesProfileEntry {
    pub name: String,
    pub bytes: usize,
    pub count: usize,
}

impl BytecodeBytesProfile {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "BYTES PROFILE");
        let _ = writeln!(out, "total: {}", format_byte_size(self.total_bytes));
        let _ = writeln!(out, "instructions: {}", self.instruction_count);
        write_profile_entries(&mut out, "sections", &self.sections, self.total_bytes);
        write_profile_entries(&mut out, "opcodes", &self.opcodes, self.total_bytes);
        write_profile_entries(&mut out, "operands", &self.operands, self.total_bytes);
        out
    }
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
                    .map(|param| self.format_operand(param))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(
                    out,
                    "  f{index} = name:{name}, body:{}..{}, argc:{}, flags:{}, returns:{}, params:[{params}]",
                    function.body_start,
                    function.body_end,
                    function.params.len(),
                    function.flags,
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
        write_u32(&mut bytes, self.names.len() as u32);
        for name in &self.names {
            write_name_string(&mut bytes, name, &[]);
        }
        write_u32(&mut bytes, self.functions.len() as u32);
        for function in &self.functions {
            write_optional_u32(&mut bytes, function.name);
            write_u32(&mut bytes, function.body_start);
            write_u32(&mut bytes, function.body_end);
            write_u32(&mut bytes, function.flags);
            write_u32(&mut bytes, function.params.len() as u32);
            bytes.push(u8::from(function.has_return));
            for param in &function.params {
                write_tagged_operand(&mut bytes, param, encoding)?;
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
            write_instruction(&mut bytes, instruction, &self.constants, encoding)?;
        }
        Ok(bytes)
    }

    pub fn bytes_profile_text(&self) -> String {
        self.bytes_profile_text_with_encoding(&EncodingConfig::default())
            .expect("default bytecode encoding must be valid")
    }

    pub fn bytes_profile_text_with_encoding(
        &self,
        encoding: &EncodingConfig,
    ) -> Result<String, EncodingError> {
        Ok(self.bytes_profile_with_encoding(encoding)?.to_text())
    }

    pub fn bytes_profile_with_encoding(
        &self,
        encoding: &EncodingConfig,
    ) -> Result<BytecodeBytesProfile, EncodingError> {
        encoding.validate()?;
        let mut bytes = Vec::new();
        let mut profile = MutableBytesProfile::default();

        let start = bytes.len();
        bytes.extend_from_slice(encoding.magic.as_bytes());
        profile.add_section("magic", bytes.len() - start);

        let start = bytes.len();
        write_u32(&mut bytes, self.extern_slots.len() as u32);
        profile.add_section("externs", bytes.len() - start);

        let start = bytes.len();
        write_u32(&mut bytes, self.names.len() as u32);
        for name in &self.names {
            write_name_string(&mut bytes, name, &[]);
        }
        profile.add_section("names", bytes.len() - start);

        let start = bytes.len();
        write_u32(&mut bytes, self.functions.len() as u32);
        for function in &self.functions {
            write_optional_u32(&mut bytes, function.name);
            write_u32(&mut bytes, function.body_start);
            write_u32(&mut bytes, function.body_end);
            write_u32(&mut bytes, function.flags);
            write_u32(&mut bytes, function.params.len() as u32);
            bytes.push(u8::from(function.has_return));
            for param in &function.params {
                let param_start = bytes.len();
                write_tagged_operand(&mut bytes, param, encoding)?;
                profile.add_operand(
                    &format!("fun.param.{}", operand_profile_name(param)),
                    bytes.len() - param_start,
                );
            }
        }
        profile.add_section("functions", bytes.len() - start);

        let start = bytes.len();
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
        profile.add_section("constants", bytes.len() - start);

        let start = bytes.len();
        write_u32(&mut bytes, self.instructions.len() as u32);
        for instruction in &self.instructions {
            let wire_op = specialized_wire_op(instruction, &self.constants);
            let instruction_start = bytes.len();
            write_instruction(&mut bytes, instruction, &self.constants, encoding)?;
            profile.add_opcode(wire_op.mnemonic(), bytes.len() - instruction_start);
            profile_instruction_operands(
                &mut profile,
                instruction,
                wire_op,
                &self.constants,
                encoding,
            )?;
        }
        profile.add_section("code", bytes.len() - start);

        Ok(profile.finish(bytes.len(), self.instructions.len()))
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
        if extern_count > 65_535 {
            return Err(EncodingError::UnknownCode(format!(
                "extern slots count {extern_count} exceeds limit 65535"
            )));
        }
        let extern_slots = (0..extern_count)
            .map(|index| format!("e{index}"))
            .collect::<Vec<_>>();

        let name_count = cursor.read_bounded_count("names")?;
        let mut names = Vec::with_capacity(name_count);
        for _ in 0..name_count {
            names.push(cursor.read_name_string(&[])?);
        }

        let function_count = cursor.read_bounded_count("functions")?;
        let mut functions = Vec::with_capacity(function_count);
        for _ in 0..function_count {
            let name = cursor.read_optional_u32()?;
            let body_start = cursor.read_u32()?;
            let body_end = cursor.read_u32()?;
            let flags = cursor.read_u32()?;
            let param_count = cursor.read_bounded_count("function params")?;
            let has_return = cursor.read_u8()? != 0;
            let mut params = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                params.push(read_tagged_operand(&mut cursor, encoding)?);
            }
            functions.push(BytecodeFunction {
                name,
                params,
                body_start,
                body_end,
                flags,
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
            let operands = read_instruction_operands(&mut cursor, op, &mut constants, encoding)?;
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
            BytecodeOperand::LocalSlot(index) => format!("local#{index}"),
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
            BytecodeOperand::ScopeKind(value) => scope_kind_name(*value)
                .map(str::to_string)
                .unwrap_or_else(|| format!("scope#{value}")),
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
    constants: &[BytecodeConstant],
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    match instruction.op {
        BytecodeOp::LoadUndefined
        | BytecodeOp::LoadNull
        | BytecodeOp::LoadTrue
        | BytecodeOp::LoadFalse => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let expected = match instruction.op {
                BytecodeOp::LoadUndefined => BytecodeConstant::Undefined,
                BytecodeOp::LoadNull => BytecodeConstant::Null,
                BytecodeOp::LoadTrue => BytecodeConstant::Bool(true),
                BytecodeOp::LoadFalse => BytecodeConstant::Bool(false),
                _ => unreachable!(),
            };
            ensure_constant_operand(instruction, 1, constants, &expected)?;
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::LoadIntSmall => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let value = constant_i32_operand(instruction, 1, constants)?;
            if !is_small_i32(f64::from(value)) {
                return Err(EncodingError::UnexpectedOperand(format!(
                    "LOAD_INT_SMALL expected -64..63, got {value}"
                )));
            }
            write_u32(bytes, encode_zigzag_i32(value));
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::LoadLocalSmall => {
            let dst = register_payload(operand_at(instruction, 0)?)?;
            let slot = local_payload(operand_at(instruction, 1)?)?;
            if dst >= 16 || slot >= 16 {
                return Err(EncodingError::UnexpectedOperand(format!(
                    "LOAD_LOCAL_SMALL expected dst/local < 16, got r{dst}, local#{slot}"
                )));
            }
            bytes.push(((dst as u8) << 4) | slot as u8);
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::StoreLocalSmall => {
            let slot = local_payload(operand_at(instruction, 0)?)?;
            let src = register_payload(operand_at(instruction, 1)?)?;
            if slot >= 16 || src >= 16 {
                return Err(EncodingError::UnexpectedOperand(format!(
                    "STORE_LOCAL_SMALL expected local/src < 16, got local#{slot}, r{src}"
                )));
            }
            bytes.push(((slot as u8) << 4) | src as u8);
            ensure_operand_len(instruction, 2)
        }
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
        BytecodeOp::MemberConst => {
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
            write_operand(
                bytes,
                operand_at(instruction, 2)?,
                OperandKind::Constant,
                encoding,
            )?;
            ensure_operand_len(instruction, 3)
        }
        BytecodeOp::StoreMemberConst => {
            write_operand(
                bytes,
                operand_at(instruction, 0)?,
                OperandKind::Value,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 1)?,
                OperandKind::Constant,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 2)?,
                OperandKind::Value,
                encoding,
            )?;
            ensure_operand_len(instruction, 3)
        }
        BytecodeOp::CallZero => {
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
            if count_at(instruction, 2)? != 0 {
                return Err(EncodingError::UnexpectedOperand(
                    "CALL_0 expected zero arguments".to_string(),
                ));
            }
            ensure_operand_len(instruction, 3)
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
        BytecodeOp::CallTwo => {
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
            ensure_operand_len(instruction, 5)?;
            if count_at(instruction, 2)? != 2 {
                return Err(EncodingError::UnexpectedOperand(
                    "CALL_2 expected exactly two arguments".to_string(),
                ));
            }
            write_operand(
                bytes,
                operand_at(instruction, 3)?,
                OperandKind::Value,
                encoding,
            )?;
            write_operand(
                bytes,
                operand_at(instruction, 4)?,
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
    constants: &[BytecodeConstant],
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    let wire_op = specialized_wire_op(instruction, constants);
    bytes.push(encoding.opcode(wire_op)?);
    write_instruction_operands(
        bytes,
        &BytecodeInstruction {
            op: wire_op,
            operands: instruction.operands.clone(),
        },
        constants,
        encoding,
    )
}

fn specialized_wire_op(
    instruction: &BytecodeInstruction,
    constants: &[BytecodeConstant],
) -> BytecodeOp {
    match instruction.op {
        BytecodeOp::LoadName => {
            if matches!(instruction.operands.as_slice(), [BytecodeOperand::Register(dst), BytecodeOperand::LocalSlot(slot)] if *dst < 16 && *slot < 16)
            {
                BytecodeOp::LoadLocalSmall
            } else if matches!(
                instruction.operands.as_slice(),
                [BytecodeOperand::Register(_), BytecodeOperand::LocalSlot(_)]
            ) {
                BytecodeOp::LoadLocal
            } else {
                BytecodeOp::LoadName
            }
        }
        BytecodeOp::StoreName => {
            if matches!(instruction.operands.as_slice(), [BytecodeOperand::LocalSlot(slot), BytecodeOperand::Register(src)] if *slot < 16 && *src < 16)
            {
                BytecodeOp::StoreLocalSmall
            } else if matches!(
                instruction.operands.as_slice(),
                [BytecodeOperand::LocalSlot(_), BytecodeOperand::Register(_)]
            ) {
                BytecodeOp::StoreLocal
            } else {
                BytecodeOp::StoreName
            }
        }
        BytecodeOp::LoadConst => match instruction.operands.as_slice() {
            [
                BytecodeOperand::Register(_),
                BytecodeOperand::Constant(index),
            ] => match constants.get(*index as usize) {
                Some(BytecodeConstant::Undefined) => BytecodeOp::LoadUndefined,
                Some(BytecodeConstant::Null) => BytecodeOp::LoadNull,
                Some(BytecodeConstant::Bool(true)) => BytecodeOp::LoadTrue,
                Some(BytecodeConstant::Bool(false)) => BytecodeOp::LoadFalse,
                Some(BytecodeConstant::Number(value)) if is_small_i32(*value) => {
                    BytecodeOp::LoadIntSmall
                }
                _ => BytecodeOp::LoadConstConst,
            },
            _ => BytecodeOp::LoadConst,
        },
        BytecodeOp::Member => {
            if matches!(
                instruction.operands.as_slice(),
                [
                    BytecodeOperand::Register(_),
                    _,
                    BytecodeOperand::Constant(_)
                ]
            ) {
                BytecodeOp::MemberConst
            } else {
                BytecodeOp::Member
            }
        }
        BytecodeOp::StoreMember => {
            if matches!(
                instruction.operands.as_slice(),
                [_, BytecodeOperand::Constant(_), _]
            ) {
                BytecodeOp::StoreMemberConst
            } else {
                BytecodeOp::StoreMember
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
        BytecodeOp::Call => match instruction.operands.as_slice() {
            [BytecodeOperand::Register(_), _, BytecodeOperand::Count(0)] => BytecodeOp::CallZero,
            [
                BytecodeOperand::Register(_),
                _,
                BytecodeOperand::Count(1),
                _,
            ] => BytecodeOp::CallOne,
            [
                BytecodeOperand::Register(_),
                _,
                BytecodeOperand::Count(2),
                _,
                _,
            ] => BytecodeOp::CallTwo,
            _ => BytecodeOp::Call,
        },
        BytecodeOp::Return => match instruction.operands.as_slice() {
            [BytecodeOperand::Register(_)] => BytecodeOp::ReturnReg,
            [BytecodeOperand::Constant(_)] => BytecodeOp::ReturnConst,
            _ => BytecodeOp::Return,
        },
        BytecodeOp::JumpIfFalse => {
            if matches!(
                instruction.operands.as_slice(),
                [BytecodeOperand::Register(_), BytecodeOperand::Count(_)]
            ) {
                BytecodeOp::JumpIfFalseReg
            } else {
                BytecodeOp::JumpIfFalse
            }
        }
        BytecodeOp::Binary => match instruction.operands.as_slice() {
            [
                BytecodeOperand::Register(_),
                BytecodeOperand::Operator(_),
                BytecodeOperand::Register(_),
                BytecodeOperand::Register(_),
            ] => BytecodeOp::BinaryRegReg,
            [
                BytecodeOperand::Register(_),
                BytecodeOperand::Operator(_),
                BytecodeOperand::Register(_),
                BytecodeOperand::Constant(_),
            ] => BytecodeOp::BinaryRegConst,
            _ => BytecodeOp::Binary,
        },
        op => op,
    }
}

#[derive(Default)]
struct MutableBytesProfile {
    sections: Vec<BytecodeBytesProfileEntry>,
    opcodes: BTreeMap<String, BytecodeBytesProfileEntry>,
    operands: BTreeMap<String, BytecodeBytesProfileEntry>,
}

impl MutableBytesProfile {
    fn add_section(&mut self, name: &str, bytes: usize) {
        self.sections.push(BytecodeBytesProfileEntry {
            name: name.to_string(),
            bytes,
            count: 1,
        });
    }

    fn add_opcode(&mut self, name: &str, bytes: usize) {
        add_profile_entry(&mut self.opcodes, name, bytes);
    }

    fn add_operand(&mut self, name: &str, bytes: usize) {
        add_profile_entry(&mut self.operands, name, bytes);
    }

    fn finish(self, total_bytes: usize, instruction_count: usize) -> BytecodeBytesProfile {
        BytecodeBytesProfile {
            total_bytes,
            instruction_count,
            sections: self.sections,
            opcodes: sorted_profile_entries(self.opcodes),
            operands: sorted_profile_entries(self.operands),
        }
    }
}

fn add_profile_entry(
    entries: &mut BTreeMap<String, BytecodeBytesProfileEntry>,
    name: &str,
    bytes: usize,
) {
    let entry = entries
        .entry(name.to_string())
        .or_insert_with(|| BytecodeBytesProfileEntry {
            name: name.to_string(),
            bytes: 0,
            count: 0,
        });
    entry.bytes += bytes;
    entry.count += 1;
}

fn sorted_profile_entries(
    entries: BTreeMap<String, BytecodeBytesProfileEntry>,
) -> Vec<BytecodeBytesProfileEntry> {
    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.name.cmp(&right.name))
    });
    entries
}

fn write_profile_entries(
    out: &mut String,
    title: &str,
    entries: &[BytecodeBytesProfileEntry],
    total_bytes: usize,
) {
    let _ = writeln!(out);
    let _ = writeln!(out, "{title}:");
    if entries.is_empty() {
        let _ = writeln!(out, "  (empty)");
        return;
    }
    for entry in entries {
        let percent = if total_bytes == 0 {
            0.0
        } else {
            entry.bytes as f64 * 100.0 / total_bytes as f64
        };
        let avg = if entry.count == 0 {
            0.0
        } else {
            entry.bytes as f64 / entry.count as f64
        };
        let _ = writeln!(
            out,
            "  {:<28} {:>9} {:>6.2}% count:{:<6} avg:{:.2}",
            entry.name,
            format_byte_size(entry.bytes),
            percent,
            entry.count,
            avg
        );
    }
}

fn format_byte_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MiB", bytes as f64 / 1024.0 / 1024.0)
    }
}

fn profile_instruction_operands(
    profile: &mut MutableBytesProfile,
    instruction: &BytecodeInstruction,
    wire_op: BytecodeOp,
    constants: &[BytecodeConstant],
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    match wire_op {
        BytecodeOp::LoadUndefined
        | BytecodeOp::LoadNull
        | BytecodeOp::LoadTrue
        | BytecodeOp::LoadFalse => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let expected = match wire_op {
                BytecodeOp::LoadUndefined => BytecodeConstant::Undefined,
                BytecodeOp::LoadNull => BytecodeConstant::Null,
                BytecodeOp::LoadTrue => BytecodeConstant::Bool(true),
                BytecodeOp::LoadFalse => BytecodeConstant::Bool(false),
                _ => unreachable!(),
            };
            ensure_constant_operand(instruction, 1, constants, &expected)?;
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::LoadIntSmall => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let value = constant_i32_operand(instruction, 1, constants)?;
            if !is_small_i32(f64::from(value)) {
                return Err(EncodingError::UnexpectedOperand(format!(
                    "LOAD_INT_SMALL expected -64..63, got {value}"
                )));
            }
            profile.add_operand("small_int", encoded_u32_len(encode_zigzag_i32(value)));
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::LoadLocalSmall => {
            let dst = register_payload(operand_at(instruction, 0)?)?;
            let slot = local_payload(operand_at(instruction, 1)?)?;
            if dst >= 16 || slot >= 16 {
                return Err(EncodingError::UnexpectedOperand(format!(
                    "LOAD_LOCAL_SMALL expected dst/local < 16, got r{dst}, local#{slot}"
                )));
            }
            profile.add_operand("local_pair", 1);
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::StoreLocalSmall => {
            let slot = local_payload(operand_at(instruction, 0)?)?;
            let src = register_payload(operand_at(instruction, 1)?)?;
            if slot >= 16 || src >= 16 {
                return Err(EncodingError::UnexpectedOperand(format!(
                    "STORE_LOCAL_SMALL expected local/src < 16, got local#{slot}, r{src}"
                )));
            }
            profile.add_operand("local_pair", 1);
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::Array => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(profile, instruction, 2, count, OperandKind::Value, encoding)
        }
        BytecodeOp::Object => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            ensure_operand_len(instruction, 2 + count * 2)?;
            for index in 0..count {
                profile_operand(
                    profile,
                    operand_at(instruction, 2 + index * 2)?,
                    OperandKind::Constant,
                    encoding,
                )?;
                profile_operand(
                    profile,
                    operand_at(instruction, 3 + index * 2)?,
                    OperandKind::Value,
                    encoding,
                )?;
            }
            Ok(())
        }
        BytecodeOp::Call | BytecodeOp::New => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Value,
                encoding,
            )?;
            let count = count_at(instruction, 2)?;
            profile_operand(
                profile,
                operand_at(instruction, 2)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(profile, instruction, 3, count, OperandKind::Value, encoding)
        }
        BytecodeOp::MemberConst => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Value,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 2)?,
                OperandKind::Constant,
                encoding,
            )?;
            ensure_operand_len(instruction, 3)
        }
        BytecodeOp::StoreMemberConst => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Value,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Constant,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 2)?,
                OperandKind::Value,
                encoding,
            )?;
            ensure_operand_len(instruction, 3)
        }
        BytecodeOp::CallZero => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Value,
                encoding,
            )?;
            if count_at(instruction, 2)? != 0 {
                return Err(EncodingError::UnexpectedOperand(
                    "CALL_0 expected zero arguments".to_string(),
                ));
            }
            ensure_operand_len(instruction, 3)
        }
        BytecodeOp::CallOne => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            profile_operand(
                profile,
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
            profile_operand(
                profile,
                operand_at(instruction, 3)?,
                OperandKind::Value,
                encoding,
            )
        }
        BytecodeOp::CallTwo => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Value,
                encoding,
            )?;
            ensure_operand_len(instruction, 5)?;
            if count_at(instruction, 2)? != 2 {
                return Err(EncodingError::UnexpectedOperand(
                    "CALL_2 expected exactly two arguments".to_string(),
                ));
            }
            profile_operand(
                profile,
                operand_at(instruction, 3)?,
                OperandKind::Value,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 4)?,
                OperandKind::Value,
                encoding,
            )
        }
        BytecodeOp::Template => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            let quasi_count = count_at(instruction, 1)?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(
                profile,
                instruction,
                2,
                quasi_count,
                OperandKind::Constant,
                encoding,
            )?;
            let expr_count_index = 2 + quasi_count;
            let expr_count = count_at(instruction, expr_count_index)?;
            profile_operand(
                profile,
                operand_at(instruction, expr_count_index)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(
                profile,
                instruction,
                expr_count_index + 1,
                expr_count,
                OperandKind::Value,
                encoding,
            )
        }
        BytecodeOp::FunctionStart => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Function,
                encoding,
            )?;
            ensure_operand_len(instruction, 1)
        }
        BytecodeOp::FunctionExprStart => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Register,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Function,
                encoding,
            )?;
            ensure_operand_len(instruction, 2)
        }
        BytecodeOp::Class => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::OptionalRegister,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::OptionalName,
                encoding,
            )?;
            profile_operand(
                profile,
                operand_at(instruction, 2)?,
                OperandKind::OptionalValue,
                encoding,
            )?;
            let count = count_at(instruction, 3)?;
            profile_operand(
                profile,
                operand_at(instruction, 3)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(
                profile,
                instruction,
                4,
                count,
                OperandKind::Constant,
                encoding,
            )
        }
        BytecodeOp::Import => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Constant,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(
                profile,
                instruction,
                2,
                count,
                OperandKind::Constant,
                encoding,
            )
        }
        BytecodeOp::Export => {
            profile_operand(
                profile,
                operand_at(instruction, 0)?,
                OperandKind::Constant,
                encoding,
            )?;
            let count = count_at(instruction, 1)?;
            profile_operand(
                profile,
                operand_at(instruction, 1)?,
                OperandKind::Count,
                encoding,
            )?;
            profile_repeated_operands(profile, instruction, 2, count, OperandKind::Name, encoding)
        }
        op => {
            let schema = fixed_operand_schema(op);
            ensure_operand_len(instruction, schema.len())?;
            for (operand, kind) in instruction.operands.iter().zip(schema.iter().copied()) {
                profile_operand(profile, operand, kind, encoding)?;
            }
            Ok(())
        }
    }
}

fn profile_repeated_operands(
    profile: &mut MutableBytesProfile,
    instruction: &BytecodeInstruction,
    start: usize,
    count: usize,
    kind: OperandKind,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    ensure_operand_min_len(instruction, start + count)?;
    for index in 0..count {
        profile_operand(
            profile,
            operand_at(instruction, start + index)?,
            kind,
            encoding,
        )?;
    }
    Ok(())
}

fn profile_operand(
    profile: &mut MutableBytesProfile,
    operand: &BytecodeOperand,
    kind: OperandKind,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    let bytes = encoded_operand_len(operand, kind)?;
    let name = match kind {
        OperandKind::Value => format!("value.{}", operand_profile_name(operand)),
        OperandKind::OptionalValue => format!("optional_value.{}", operand_profile_name(operand)),
        OperandKind::OptionalRegister => {
            format!("optional_register.{}", operand_profile_name(operand))
        }
        OperandKind::OptionalName => format!("optional_name.{}", operand_profile_name(operand)),
        OperandKind::NameRef => format!("name_ref.{}", operand_profile_name(operand)),
        _ => operand_kind_profile_name(kind).to_string(),
    };
    if matches!(
        kind,
        OperandKind::Value
            | OperandKind::OptionalValue
            | OperandKind::OptionalRegister
            | OperandKind::OptionalName
    ) {
        let _ = operand.tag(encoding)?;
    } else {
        ensure_operand_kind(operand, kind)?;
    }
    profile.add_operand(&name, bytes);
    Ok(())
}

fn encoded_operand_len(
    operand: &BytecodeOperand,
    kind: OperandKind,
) -> Result<usize, EncodingError> {
    match kind {
        OperandKind::Value
        | OperandKind::OptionalValue
        | OperandKind::OptionalRegister
        | OperandKind::OptionalName => Ok(compact_value_operand_byte(operand)
            .map(|_| 1)
            .unwrap_or_else(|| 1 + encoded_u32_len(operand.payload()))),
        OperandKind::NameRef => Ok(encoded_u32_len(encode_name_ref_operand(operand)?)),
        _ => Ok(encoded_u32_len(operand.payload())),
    }
}

fn encoded_u32_len(mut value: u32) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

fn operand_profile_name(operand: &BytecodeOperand) -> &'static str {
    match operand {
        BytecodeOperand::Register(_) => "register",
        BytecodeOperand::Constant(_) => "constant",
        BytecodeOperand::Name(_) => "name",
        BytecodeOperand::LocalSlot(_) => "local",
        BytecodeOperand::External(_) => "extern",
        BytecodeOperand::Function(_) => "function",
        BytecodeOperand::Label(_) => "label",
        BytecodeOperand::Operator(_) => "operator",
        BytecodeOperand::DeclKind(_) => "decl_kind",
        BytecodeOperand::ScopeKind(_) => "scope_kind",
        BytecodeOperand::Count(_) => "count",
        BytecodeOperand::None => "none",
    }
}

fn operand_kind_profile_name(kind: OperandKind) -> &'static str {
    match kind {
        OperandKind::Register => "register",
        OperandKind::Constant => "constant",
        OperandKind::Name => "name",
        OperandKind::NameRef => "name_ref",
        OperandKind::LocalSlot => "local",
        OperandKind::Function => "function",
        OperandKind::Label => "label",
        OperandKind::Operator => "operator",
        OperandKind::DeclKind => "decl_kind",
        OperandKind::ScopeKind => "scope_kind",
        OperandKind::Count => "count",
        OperandKind::Value => "value",
        OperandKind::OptionalRegister => "optional_register",
        OperandKind::OptionalName => "optional_name",
        OperandKind::OptionalValue => "optional_value",
    }
}

fn read_instruction_operands(
    cursor: &mut ByteReader<'_>,
    op: BytecodeOp,
    constants: &mut Vec<BytecodeConstant>,
    encoding: &EncodingConfig,
) -> Result<Vec<BytecodeOperand>, EncodingError> {
    let mut operands = Vec::new();
    match op {
        BytecodeOp::LoadUndefined
        | BytecodeOp::LoadNull
        | BytecodeOp::LoadTrue
        | BytecodeOp::LoadFalse => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            let constant = match op {
                BytecodeOp::LoadUndefined => BytecodeConstant::Undefined,
                BytecodeOp::LoadNull => BytecodeConstant::Null,
                BytecodeOp::LoadTrue => BytecodeConstant::Bool(true),
                BytecodeOp::LoadFalse => BytecodeConstant::Bool(false),
                _ => unreachable!(),
            };
            operands.push(BytecodeOperand::Constant(constant_index_or_insert(
                constants, constant,
            )));
        }
        BytecodeOp::LoadIntSmall => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            let value = f64::from(decode_zigzag_u32(cursor.read_u32()?));
            operands.push(BytecodeOperand::Constant(constant_index_or_insert(
                constants,
                BytecodeConstant::Number(value),
            )));
        }
        BytecodeOp::LoadLocalSmall => {
            let byte = cursor.read_u8()?;
            operands.push(BytecodeOperand::Register(u32::from(byte >> 4)));
            operands.push(BytecodeOperand::LocalSlot(u32::from(byte & 0x0f)));
        }
        BytecodeOp::StoreLocalSmall => {
            let byte = cursor.read_u8()?;
            operands.push(BytecodeOperand::LocalSlot(u32::from(byte >> 4)));
            operands.push(BytecodeOperand::Register(u32::from(byte & 0x0f)));
        }
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
        BytecodeOp::CallZero => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
            operands.push(BytecodeOperand::Count(0));
        }
        BytecodeOp::CallOne => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
            operands.push(BytecodeOperand::Count(1));
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
        }
        BytecodeOp::CallTwo => {
            operands.push(read_operand(cursor, OperandKind::Register, encoding)?);
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
            operands.push(BytecodeOperand::Count(2));
            operands.push(read_operand(cursor, OperandKind::Value, encoding)?);
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
        BytecodeOp::Declare => &[DeclKind, NameRef],
        BytecodeOp::LoadConst => &[Register, Value],
        BytecodeOp::LoadConstConst => &[Register, Constant],
        BytecodeOp::LoadUndefined
        | BytecodeOp::LoadNull
        | BytecodeOp::LoadTrue
        | BytecodeOp::LoadFalse => &[Register],
        BytecodeOp::LoadIntSmall => &[Register, Count],
        BytecodeOp::LoadName => &[Register, NameRef],
        BytecodeOp::LoadLocal => &[Register, LocalSlot],
        BytecodeOp::LoadLocalSmall => &[Count],
        BytecodeOp::StoreName => &[NameRef, Value],
        BytecodeOp::StoreLocal => &[LocalSlot, Register],
        BytecodeOp::StoreLocalSmall => &[Count],
        BytecodeOp::StoreMember => &[Value, Value, Value],
        BytecodeOp::StoreMemberConst => &[Value, Constant, Value],
        BytecodeOp::Move => &[Register, Value],
        BytecodeOp::Binary => &[Register, Operator, Value, Value],
        BytecodeOp::BinaryRegReg => &[Register, Operator, Register, Register],
        BytecodeOp::BinaryRegConst => &[Register, Operator, Register, Constant],
        BytecodeOp::Unary => &[Register, Operator, Value],
        BytecodeOp::Member => &[Register, Value, Value],
        BytecodeOp::MemberConst => &[Register, Value, Constant],
        BytecodeOp::FunctionStart => &[Function],
        BytecodeOp::FunctionExprStart => &[Register, Function],
        BytecodeOp::FunctionEnd
        | BytecodeOp::FunctionExprEnd
        | BytecodeOp::TryStart
        | BytecodeOp::FinallyStart
        | BytecodeOp::TryEnd
        | BytecodeOp::LeaveScope => &[],
        BytecodeOp::EnterScope => &[ScopeKind],
        BytecodeOp::CatchStart => &[OptionalName],
        BytecodeOp::Throw => &[Value],
        BytecodeOp::Return => &[OptionalValue],
        BytecodeOp::ReturnReg => &[Register],
        BytecodeOp::ReturnConst => &[Constant],
        BytecodeOp::Pop => &[Value],
        BytecodeOp::PopReg => &[Register],
        BytecodeOp::Jump => &[Count],
        BytecodeOp::JumpIfFalse => &[Value, Count],
        BytecodeOp::JumpIfFalseReg => &[Register, Count],
        BytecodeOp::Unsupported => &[Constant],
        BytecodeOp::Array
        | BytecodeOp::Object
        | BytecodeOp::Call
        | BytecodeOp::New
        | BytecodeOp::Template
        | BytecodeOp::Class
        | BytecodeOp::Import
        | BytecodeOp::Export
        | BytecodeOp::CallOne
        | BytecodeOp::CallZero
        | BytecodeOp::CallTwo => &[],
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
            if let Some(byte) = compact_value_operand_byte(operand) {
                bytes.push(byte);
                return Ok(());
            }
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

fn write_tagged_operand(
    bytes: &mut Vec<u8>,
    operand: &BytecodeOperand,
    encoding: &EncodingConfig,
) -> Result<(), EncodingError> {
    bytes.push(operand.tag(encoding)?);
    write_u32(bytes, operand.payload());
    Ok(())
}

fn read_tagged_operand(
    cursor: &mut ByteReader<'_>,
    encoding: &EncodingConfig,
) -> Result<BytecodeOperand, EncodingError> {
    let tag = cursor.read_u8()?;
    let payload = cursor.read_u32()?;
    BytecodeOperand::from_tag_payload(tag, payload, encoding)
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
        OperandKind::LocalSlot => Ok(BytecodeOperand::LocalSlot(cursor.read_u32()?)),
        OperandKind::Function => Ok(BytecodeOperand::Function(cursor.read_u32()?)),
        OperandKind::Label => Ok(BytecodeOperand::Label(cursor.read_u32()?)),
        OperandKind::Operator => Ok(BytecodeOperand::Operator(cursor.read_u32()?)),
        OperandKind::DeclKind => Ok(BytecodeOperand::DeclKind(cursor.read_u32()?)),
        OperandKind::ScopeKind => Ok(BytecodeOperand::ScopeKind(cursor.read_u32()?)),
        OperandKind::Count => Ok(BytecodeOperand::Count(cursor.read_u32()?)),
        OperandKind::Value
        | OperandKind::OptionalRegister
        | OperandKind::OptionalName
        | OperandKind::OptionalValue => {
            let tag = cursor.read_u8()?;
            if let Some(operand) = decode_compact_value_operand(tag) {
                return Ok(operand);
            }
            let payload = cursor.read_u32()?;
            BytecodeOperand::from_tag_payload(tag, payload, encoding)
        }
    }
}

fn compact_value_operand_byte(operand: &BytecodeOperand) -> Option<u8> {
    match operand {
        BytecodeOperand::Register(value) if *value < 64 => {
            Some(COMPACT_REGISTER_BASE | *value as u8)
        }
        BytecodeOperand::Constant(value) if *value < 32 => Some(0xc0 | *value as u8),
        BytecodeOperand::LocalSlot(value) if *value < 16 => Some(0xe0 | *value as u8),
        BytecodeOperand::External(value) if *value < 8 => Some(0xf0 | *value as u8),
        BytecodeOperand::Function(value) if *value < 4 => Some(0xf8 | *value as u8),
        BytecodeOperand::Name(value) if *value < 3 => Some(0xfc | *value as u8),
        BytecodeOperand::None => Some(0xff),
        _ => None,
    }
}

const COMPACT_VALUE_OPERAND_BASE: u8 = 0x80;
const COMPACT_REGISTER_BASE: u8 = 0x80;

fn decode_compact_value_operand(byte: u8) -> Option<BytecodeOperand> {
    match byte {
        0x80..=0xbf => Some(BytecodeOperand::Register(u32::from(byte & 0x3f))),
        0xc0..=0xdf => Some(BytecodeOperand::Constant(u32::from(byte & 0x1f))),
        0xe0..=0xef => Some(BytecodeOperand::LocalSlot(u32::from(byte & 0x0f))),
        0xf0..=0xf7 => Some(BytecodeOperand::External(u32::from(byte & 0x07))),
        0xf8..=0xfb => Some(BytecodeOperand::Function(u32::from(byte & 0x03))),
        0xfc..=0xfe => Some(BytecodeOperand::Name(u32::from(byte - 0xfc))),
        0xff => Some(BytecodeOperand::None),
        _ => None,
    }
}

fn ensure_operand_kind(operand: &BytecodeOperand, kind: OperandKind) -> Result<(), EncodingError> {
    let valid = matches!(
        (kind, operand),
        (OperandKind::Register, BytecodeOperand::Register(_))
            | (OperandKind::Constant, BytecodeOperand::Constant(_))
            | (OperandKind::Name, BytecodeOperand::Name(_))
            | (OperandKind::NameRef, BytecodeOperand::Name(_))
            | (OperandKind::NameRef, BytecodeOperand::LocalSlot(_))
            | (OperandKind::NameRef, BytecodeOperand::External(_))
            | (OperandKind::LocalSlot, BytecodeOperand::LocalSlot(_))
            | (OperandKind::Function, BytecodeOperand::Function(_))
            | (OperandKind::Label, BytecodeOperand::Label(_))
            | (OperandKind::Operator, BytecodeOperand::Operator(_))
            | (OperandKind::DeclKind, BytecodeOperand::DeclKind(_))
            | (OperandKind::ScopeKind, BytecodeOperand::ScopeKind(_))
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
    let (payload, tag) = match operand {
        BytecodeOperand::Name(value) => (*value, 0),
        BytecodeOperand::LocalSlot(value) => (*value, 1),
        BytecodeOperand::External(value) => (*value, 2),
        operand => {
            return Err(EncodingError::UnexpectedOperand(format!(
                "name ref expected name, local, or extern, got {operand:?}"
            )));
        }
    };
    payload
        .checked_mul(4)
        .and_then(|value| value.checked_add(tag))
        .ok_or_else(|| EncodingError::UnexpectedOperand("name ref index overflow".to_string()))
}

fn decode_name_ref_operand(payload: u32) -> BytecodeOperand {
    let index = payload >> 2;
    match payload & 3 {
        0 => BytecodeOperand::Name(index),
        1 => BytecodeOperand::LocalSlot(index),
        2 => BytecodeOperand::External(index),
        _ => BytecodeOperand::Name(index),
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

fn register_payload(operand: &BytecodeOperand) -> Result<u32, EncodingError> {
    match operand {
        BytecodeOperand::Register(value) => Ok(*value),
        operand => Err(EncodingError::UnexpectedOperand(format!(
            "expected register operand, got {operand:?}"
        ))),
    }
}

fn local_payload(operand: &BytecodeOperand) -> Result<u32, EncodingError> {
    match operand {
        BytecodeOperand::LocalSlot(value) => Ok(*value),
        operand => Err(EncodingError::UnexpectedOperand(format!(
            "expected local operand, got {operand:?}"
        ))),
    }
}

fn constant_i32_operand(
    instruction: &BytecodeInstruction,
    index: usize,
    constants: &[BytecodeConstant],
) -> Result<i32, EncodingError> {
    let constant_index = match operand_at(instruction, index)? {
        BytecodeOperand::Constant(value) => *value as usize,
        operand => {
            return Err(EncodingError::UnexpectedOperand(format!(
                "{} operand {index} expected constant, got {operand:?}",
                instruction.op.mnemonic()
            )));
        }
    };
    let Some(BytecodeConstant::Number(value)) = constants.get(constant_index) else {
        return Err(EncodingError::UnexpectedOperand(format!(
            "{} operand {index} expected number constant#{constant_index}",
            instruction.op.mnemonic()
        )));
    };
    if value.fract() != 0.0 || *value < i32::MIN as f64 || *value > i32::MAX as f64 {
        return Err(EncodingError::UnexpectedOperand(format!(
            "{} operand {index} expected i32 number constant, got {value}",
            instruction.op.mnemonic()
        )));
    }
    Ok(*value as i32)
}

fn ensure_constant_operand(
    instruction: &BytecodeInstruction,
    index: usize,
    constants: &[BytecodeConstant],
    expected: &BytecodeConstant,
) -> Result<(), EncodingError> {
    let constant_index = match operand_at(instruction, index)? {
        BytecodeOperand::Constant(value) => *value as usize,
        operand => {
            return Err(EncodingError::UnexpectedOperand(format!(
                "{} operand {index} expected constant, got {operand:?}",
                instruction.op.mnemonic()
            )));
        }
    };
    let actual = constants.get(constant_index).ok_or_else(|| {
        EncodingError::UnexpectedOperand(format!(
            "{} operand {index} references missing constant#{constant_index}",
            instruction.op.mnemonic()
        ))
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(EncodingError::UnexpectedOperand(format!(
            "{} operand {index} expected {expected:?}, got {actual:?}",
            instruction.op.mnemonic()
        )))
    }
}

fn constant_index_or_insert(
    constants: &mut Vec<BytecodeConstant>,
    constant: BytecodeConstant,
) -> u32 {
    if let Some(index) = constants
        .iter()
        .position(|candidate| candidate == &constant)
    {
        index as u32
    } else {
        constants.push(constant);
        (constants.len() - 1) as u32
    }
}

fn is_small_i32(value: f64) -> bool {
    value.fract() == 0.0
        && value >= -64.0
        && value <= 63.0
        && (value != 0.0 || !value.is_sign_negative())
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

fn lower_module_to_bytecode_instructions(module: &IrModule) -> Vec<LowerInstruction> {
    let mut instructions = Vec::new();

    for import in &module.imports {
        instructions.push(LowerInstruction::Import {
            source: import.source.clone(),
            specifiers: import
                .specifiers
                .iter()
                .map(|specifier| import_specifier_name(module, specifier))
                .collect(),
        });
    }

    if let Some(entry) = module.functions.get(module.entry.0) {
        instructions.extend(lower_function_body(module, module.entry, entry));
    }

    for export in &module.exports {
        let (kind, names) = export_decl_names(module, export);
        instructions.push(LowerInstruction::Export { kind, names });
    }

    instructions
}

fn lower_function_body(
    module: &IrModule,
    function_id: FunctionId,
    function: &IrFunction,
) -> Vec<LowerInstruction> {
    let mut out = Vec::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if index != function.entry.0 || function.blocks.len() > 1 {
            out.push(LowerInstruction::Label(block_label(
                function_id,
                BlockId(index),
            )));
        }
        for instruction in &block.instructions {
            lower_ir_instruction(module, function, instruction, &mut out);
        }
        lower_ir_terminator(module, function_id, function, &block.terminator, &mut out);
    }
    out
}

fn lower_ir_instruction(
    module: &IrModule,
    function: &IrFunction,
    instruction: &IrInstruction,
    out: &mut Vec<LowerInstruction>,
) {
    match &instruction.kind {
        IrInstructionKind::Nop => {}
        IrInstructionKind::Debug(message) => out.push(LowerInstruction::Marker(message.clone())),
        IrInstructionKind::Label(label) => out.push(LowerInstruction::Label(label.clone())),
        IrInstructionKind::Jump(label) => out.push(LowerInstruction::Jump(label.clone())),
        IrInstructionKind::JumpIfFalse { test, label } => {
            out.push(LowerInstruction::JumpIfFalse {
                test: lower_ir_value(module, function, test),
                label: label.clone(),
            });
        }
        IrInstructionKind::Declare(declaration) => {
            out.push(LowerInstruction::Declare {
                kind: declaration.kind.to_string(),
                name: lower_local_binding(function, declaration.local),
            });
            if let Some(init) = &declaration.init {
                out.push(LowerInstruction::StoreName {
                    name: lower_local_binding(function, declaration.local),
                    src: lower_ir_value(module, function, init),
                });
            }
        }
        IrInstructionKind::Move { dst, src } => out.push(LowerInstruction::Move {
            dst: register_name(*dst),
            src: lower_ir_value(module, function, src),
        }),
        IrInstructionKind::Load { dst, src } => lower_ir_load(module, function, *dst, src, out),
        IrInstructionKind::Store { dst, op, src } => {
            if *op != IrAssignOp::Assign {
                out.push(LowerInstruction::Unsupported(format!(
                    "compound assignment {op}"
                )));
            }
            lower_ir_store(module, function, dst, src, out);
        }
        IrInstructionKind::Update { dst, place, op } => {
            out.push(LowerInstruction::Unsupported(format!(
                "structured update {op} {place}"
            )));
            if let Some(dst) = dst {
                out.push(LowerInstruction::Move {
                    dst: register_name(*dst),
                    src: LowerValue::Undefined,
                });
            }
        }
        IrInstructionKind::Unary { dst, op, arg } => out.push(LowerInstruction::Unary {
            dst: register_name(*dst),
            op: op.to_string(),
            arg: lower_ir_value(module, function, arg),
        }),
        IrInstructionKind::Binary {
            dst,
            op,
            left,
            right,
        } => out.push(LowerInstruction::Binary {
            dst: register_name(*dst),
            op: op.to_string(),
            left: lower_ir_value(module, function, left),
            right: lower_ir_value(module, function, right),
        }),
        IrInstructionKind::Delete { dst, target } => {
            out.push(LowerInstruction::Unary {
                dst: register_name(*dst),
                op: "delete".to_string(),
                arg: lower_place_as_value(module, function, target),
            });
        }
        IrInstructionKind::Throw(value) => {
            out.push(LowerInstruction::Throw(lower_ir_value(
                module, function, value,
            )));
        }
        IrInstructionKind::Return(value) => {
            out.push(LowerInstruction::Return(
                value
                    .as_ref()
                    .map(|value| lower_ir_value(module, function, value)),
            ));
        }
        IrInstructionKind::CreateArray { dst, elements } => {
            out.push(LowerInstruction::Array {
                dst: register_name(*dst),
                items: elements
                    .iter()
                    .map(|element| match element {
                        IrArrayElement::Value(value) | IrArrayElement::Spread(value) => {
                            lower_ir_value(module, function, value)
                        }
                        IrArrayElement::Hole => LowerValue::Undefined,
                    })
                    .collect(),
            });
        }
        IrInstructionKind::CreateObject { dst, properties } => {
            let mut props = Vec::new();
            for property in properties {
                match property {
                    IrObjectProperty::Data { key, value } => {
                        props.push((
                            property_key_name(module, function, key),
                            lower_ir_value(module, function, value),
                        ));
                    }
                    IrObjectProperty::Method {
                        key,
                        function: method_function,
                    }
                    | IrObjectProperty::Getter {
                        key,
                        function: method_function,
                    }
                    | IrObjectProperty::Setter {
                        key,
                        function: method_function,
                    } => {
                        props.push((
                            property_key_name(module, function, key),
                            LowerValue::Name(function_name(module, *method_function)),
                        ));
                    }
                    IrObjectProperty::Spread(value) => {
                        props.push(("...".to_string(), lower_ir_value(module, function, value)));
                    }
                }
            }
            out.push(LowerInstruction::Object {
                dst: register_name(*dst),
                props,
            });
        }
        IrInstructionKind::CreateFunction { dst, function, .. } => {
            if let Some(ir_function) = module.functions.get(function.0) {
                out.push(LowerInstruction::FunctionExpr {
                    dst: register_name(*dst),
                    name: ir_function.name.clone(),
                    params: function_param_bindings(ir_function),
                    body: lower_function_body(module, *function, ir_function),
                });
            } else {
                out.push(LowerInstruction::Unsupported(format!(
                    "missing function {function}"
                )));
            }
        }
        IrInstructionKind::FunctionDeclaration { function } => {
            if let Some(ir_function) = module.functions.get(function.0) {
                out.push(LowerInstruction::Function {
                    name: ir_function
                        .name
                        .clone()
                        .unwrap_or_else(|| function.to_string()),
                    params: function_param_bindings(ir_function),
                    body: lower_function_body(module, *function, ir_function),
                });
            } else {
                out.push(LowerInstruction::Unsupported(format!(
                    "missing function {function}"
                )));
            }
        }
        IrInstructionKind::CreateClass { dst, class } => {
            if let Some(ir_class) = module.classes.get(class.0) {
                out.push(LowerInstruction::Class {
                    dst: Some(register_name(*dst)),
                    name: ir_class.name.clone(),
                    super_class: ir_class
                        .super_class
                        .as_ref()
                        .map(|value| lower_ir_value(module, function, value)),
                    members: ir_class
                        .members
                        .iter()
                        .map(|member| property_key_name(module, function, &member.key))
                        .collect(),
                });
            } else {
                out.push(LowerInstruction::Unsupported(format!(
                    "missing class {class}"
                )));
            }
        }
        IrInstructionKind::Call(call) => out.push(LowerInstruction::Call {
            dst: call
                .dst
                .map(register_name)
                .unwrap_or_else(|| "_".to_string()),
            callee: lower_ir_value(module, function, &call.callee),
            args: call
                .args
                .iter()
                .map(|arg| lower_ir_argument(module, function, arg))
                .collect(),
        }),
        IrInstructionKind::Construct(construct) => out.push(LowerInstruction::New {
            dst: register_name(construct.dst),
            callee: lower_ir_value(module, function, &construct.callee),
            args: construct
                .args
                .iter()
                .map(|arg| lower_ir_argument(module, function, arg))
                .collect(),
        }),
        IrInstructionKind::Template(template) => out.push(LowerInstruction::Template {
            dst: register_name(template.dst),
            quasis: template
                .cooked
                .iter()
                .map(|value| value.clone().unwrap_or_default())
                .collect(),
            exprs: template
                .expressions
                .iter()
                .map(|value| lower_ir_value(module, function, value))
                .collect(),
        }),
        IrInstructionKind::Await { dst, value } => {
            out.push(LowerInstruction::Marker(format!(
                "%{} = await {}",
                register_name(*dst),
                lower_ir_value_text(module, function, value)
            )));
            out.push(LowerInstruction::Move {
                dst: register_name(*dst),
                src: lower_ir_value(module, function, value),
            });
        }
        IrInstructionKind::Yield {
            dst,
            value,
            delegate,
        } => {
            out.push(LowerInstruction::Marker(format!(
                "yield{} {}",
                if *delegate { "*" } else { "" },
                value
                    .as_ref()
                    .map(|value| lower_ir_value_text(module, function, value))
                    .unwrap_or_default()
            )));
            if let Some(dst) = dst {
                out.push(LowerInstruction::Move {
                    dst: register_name(*dst),
                    src: LowerValue::Undefined,
                });
            }
        }
        IrInstructionKind::EnterScope(scope) => {
            let kind = function
                .scopes
                .get(scope.0)
                .map(|scope| scope.kind.to_string())
                .unwrap_or_else(|| "block".to_string());
            out.push(LowerInstruction::EnterScope(kind));
        }
        IrInstructionKind::EnterWith { scope, object } => {
            out.push(LowerInstruction::Marker(format!(
                "enter_with {scope}, {}",
                lower_ir_value_text(module, function, object)
            )));
        }
        IrInstructionKind::LeaveScope(scope) => {
            let _ = scope;
            out.push(LowerInstruction::LeaveScope);
        }
        IrInstructionKind::EnterTry(handler) => {
            let _ = handler;
            out.push(LowerInstruction::TryStart);
        }
        IrInstructionKind::EnterCatch { param } => {
            out.push(LowerInstruction::CatchStart(
                param.map(|param| lower_local_binding(function, param)),
            ));
        }
        IrInstructionKind::EnterFinally => {
            out.push(LowerInstruction::FinallyStart);
        }
        IrInstructionKind::LeaveTry(handler) => {
            let _ = handler;
            out.push(LowerInstruction::TryEnd);
        }
        IrInstructionKind::Unsupported(message) => {
            out.push(LowerInstruction::Unsupported(message.clone()));
        }
    }
}

fn lower_ir_terminator(
    module: &IrModule,
    function_id: FunctionId,
    function: &IrFunction,
    terminator: &IrTerminator,
    out: &mut Vec<LowerInstruction>,
) {
    match terminator {
        IrTerminator::Jump(target) => {
            out.push(LowerInstruction::Jump(block_label(function_id, *target)))
        }
        IrTerminator::Branch {
            test,
            truthy,
            falsy,
        } => {
            out.push(LowerInstruction::JumpIfFalse {
                test: lower_ir_value(module, function, test),
                label: block_label(function_id, *falsy),
            });
            out.push(LowerInstruction::Jump(block_label(function_id, *truthy)));
        }
        IrTerminator::Switch { default, .. } => {
            out.push(LowerInstruction::Unsupported(
                "switch terminator".to_string(),
            ));
            out.push(LowerInstruction::Jump(block_label(function_id, *default)));
        }
        IrTerminator::Return(value) => out.push(LowerInstruction::Return(
            value
                .as_ref()
                .map(|value| lower_ir_value(module, function, value)),
        )),
        IrTerminator::Throw(value) => {
            out.push(LowerInstruction::Throw(lower_ir_value(
                module, function, value,
            )));
        }
        IrTerminator::Rethrow => out.push(LowerInstruction::Unsupported("rethrow".to_string())),
        IrTerminator::Unreachable => {}
    }
}

fn lower_ir_load(
    module: &IrModule,
    function: &IrFunction,
    dst: RegisterId,
    src: &IrPlace,
    out: &mut Vec<LowerInstruction>,
) {
    match src {
        IrPlace::Local(local) => out.push(LowerInstruction::LoadName {
            dst: register_name(dst),
            name: lower_local_binding(function, *local),
        }),
        IrPlace::External(external) => out.push(LowerInstruction::LoadName {
            dst: register_name(dst),
            name: LowerBinding::Name(extern_name(module, *external)),
        }),
        IrPlace::Member(member) => out.push(LowerInstruction::Member {
            dst: register_name(dst),
            object: lower_ir_value(module, function, &member.object),
            property: property_key_value(module, function, &member.property),
        }),
        IrPlace::SuperMember(property) => out.push(LowerInstruction::Member {
            dst: register_name(dst),
            object: LowerValue::Name("super".to_string()),
            property: property_key_value(module, function, property),
        }),
    }
}

fn lower_ir_store(
    module: &IrModule,
    function: &IrFunction,
    dst: &IrPlace,
    src: &IrValue,
    out: &mut Vec<LowerInstruction>,
) {
    match dst {
        IrPlace::Local(local) => out.push(LowerInstruction::StoreName {
            name: lower_local_binding(function, *local),
            src: lower_ir_value(module, function, src),
        }),
        IrPlace::External(external) => out.push(LowerInstruction::StoreName {
            name: LowerBinding::Name(extern_name(module, *external)),
            src: lower_ir_value(module, function, src),
        }),
        IrPlace::Member(member) => out.push(LowerInstruction::StoreMember {
            object: lower_ir_value(module, function, &member.object),
            property: property_key_value(module, function, &member.property),
            src: lower_ir_value(module, function, src),
        }),
        IrPlace::SuperMember(property) => out.push(LowerInstruction::StoreMember {
            object: LowerValue::Name("super".to_string()),
            property: property_key_value(module, function, property),
            src: lower_ir_value(module, function, src),
        }),
    }
}

fn lower_place_as_value(module: &IrModule, function: &IrFunction, place: &IrPlace) -> LowerValue {
    match place {
        IrPlace::Local(local) => lower_local_value(function, *local),
        IrPlace::External(external) => LowerValue::Name(extern_name(module, *external)),
        IrPlace::Member(member) => property_key_value(module, function, &member.property),
        IrPlace::SuperMember(property) => property_key_value(module, function, property),
    }
}

fn lower_ir_value(module: &IrModule, function: &IrFunction, value: &IrValue) -> LowerValue {
    match value {
        IrValue::Undefined => LowerValue::Undefined,
        IrValue::Null => LowerValue::Null,
        IrValue::Bool(value) => LowerValue::Bool(*value),
        IrValue::Const(constant) => lower_ir_const(module, *constant),
        IrValue::Local(local) => lower_local_value(function, *local),
        IrValue::Register(register) => LowerValue::Register(register_name(*register)),
        IrValue::Function(function) => LowerValue::Name(function_name(module, *function)),
        IrValue::Class(class) => LowerValue::Name(
            module
                .classes
                .get(class.0)
                .and_then(|class| class.name.clone())
                .unwrap_or_else(|| class.to_string()),
        ),
        IrValue::External(external) => LowerValue::Name(extern_name(module, *external)),
        IrValue::This => LowerValue::Name("this".to_string()),
        IrValue::Super => LowerValue::Name("super".to_string()),
        IrValue::NewTarget => LowerValue::Name("new.target".to_string()),
        IrValue::ImportMeta => LowerValue::Name("import.meta".to_string()),
    }
}

fn lower_ir_value_text(module: &IrModule, function: &IrFunction, value: &IrValue) -> String {
    match lower_ir_value(module, function, value) {
        LowerValue::Register(value) => format!("%{value}"),
        LowerValue::Name(value) => value,
        LowerValue::LocalSlot(slot) => format!("local#{slot}"),
        LowerValue::Number(value) => value.to_string(),
        LowerValue::String(value) => format!("{value:?}"),
        LowerValue::Bool(value) => value.to_string(),
        LowerValue::Null => "null".to_string(),
        LowerValue::Undefined => "undefined".to_string(),
    }
}

fn lower_ir_const(module: &IrModule, id: ConstId) -> LowerValue {
    match module.constants.get(id.0) {
        Some(IrConst::String(value)) => LowerValue::String(value.clone()),
        Some(IrConst::Int(value)) => LowerValue::Number(*value as f64),
        Some(IrConst::Float(value)) => LowerValue::Number(*value),
        Some(IrConst::BigInt(value)) => LowerValue::String(format!("{value}n")),
        Some(IrConst::Regex { pattern, flags }) => {
            LowerValue::String(format!("/{pattern}/{flags}"))
        }
        None => LowerValue::Undefined,
    }
}

fn lower_ir_argument(
    module: &IrModule,
    function: &IrFunction,
    argument: &IrArgument,
) -> LowerValue {
    match argument {
        IrArgument::Value(value) | IrArgument::Spread(value) => {
            lower_ir_value(module, function, value)
        }
    }
}

fn property_key_value(module: &IrModule, function: &IrFunction, key: &IrPropertyKey) -> LowerValue {
    match key {
        IrPropertyKey::Static(value) | IrPropertyKey::Private(value) => {
            LowerValue::String(value.clone())
        }
        IrPropertyKey::Number(value) => LowerValue::Number(*value),
        IrPropertyKey::Computed(value) => lower_ir_value(module, function, value),
    }
}

fn property_key_name(module: &IrModule, function: &IrFunction, key: &IrPropertyKey) -> String {
    match key {
        IrPropertyKey::Static(value) | IrPropertyKey::Private(value) => value.clone(),
        IrPropertyKey::Number(value) => value.to_string(),
        IrPropertyKey::Computed(value) => lower_ir_value_text(module, function, value),
    }
}

fn function_param_bindings(function: &IrFunction) -> Vec<LowerBinding> {
    function
        .params
        .iter()
        .map(|param| lower_local_binding(function, param.local))
        .collect()
}

fn function_name(module: &IrModule, id: FunctionId) -> String {
    module
        .functions
        .get(id.0)
        .and_then(|function| function.name.clone())
        .unwrap_or_else(|| id.to_string())
}

fn local_name(function: &IrFunction, id: LocalId) -> Option<String> {
    function
        .locals
        .get(id.0)
        .and_then(|local| local.name.clone())
}

fn lower_local_binding(function: &IrFunction, id: LocalId) -> LowerBinding {
    match function.locals.get(id.0) {
        Some(local) if local.name.as_deref() == Some("arguments") => {
            LowerBinding::Name("arguments".to_string())
        }
        Some(local) if local.kind == IrBindingKind::Function => {
            LowerBinding::Name(local.name.clone().unwrap_or_else(|| id.to_string()))
        }
        Some(local) if local.captured => {
            LowerBinding::Name(local.name.clone().unwrap_or_else(|| id.to_string()))
        }
        _ => LowerBinding::LocalSlot(id.0 as u32),
    }
}

fn lower_local_value(function: &IrFunction, id: LocalId) -> LowerValue {
    match lower_local_binding(function, id) {
        LowerBinding::Name(name) => LowerValue::Name(name),
        LowerBinding::LocalSlot(slot) => LowerValue::LocalSlot(slot),
    }
}

fn extern_name(module: &IrModule, id: ExternId) -> String {
    module
        .extern_slots
        .get(id.0)
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

fn block_label(function: FunctionId, id: BlockId) -> String {
    format!("f{}_b{}", function.0, id.0)
}

fn register_name(id: RegisterId) -> String {
    format!("t{}", id.0)
}

fn import_specifier_name(module: &IrModule, specifier: &IrImportSpecifier) -> String {
    match specifier {
        IrImportSpecifier::Default { local } => local_name_for_module_import(module, *local),
        IrImportSpecifier::Namespace { local } => {
            format!("* as {}", local_name_for_module_import(module, *local))
        }
        IrImportSpecifier::Named { imported, local } => {
            let local = local_name_for_module_import(module, *local);
            if imported == &local {
                imported.clone()
            } else {
                format!("{imported} as {local}")
            }
        }
    }
}

fn local_name_for_module_import(module: &IrModule, local: LocalId) -> String {
    module
        .functions
        .get(module.entry.0)
        .and_then(|function| local_name(function, local))
        .unwrap_or_else(|| local.to_string())
}

fn export_decl_names(module: &IrModule, export: &IrExportDecl) -> (String, Vec<String>) {
    match export {
        IrExportDecl::Local { local, exported } => (
            "local".to_string(),
            vec![format!(
                "{} as {exported}",
                local_name_for_module_import(module, *local)
            )],
        ),
        IrExportDecl::Default { value } => (
            "default".to_string(),
            vec![
                module
                    .functions
                    .get(module.entry.0)
                    .map(|function| lower_ir_value_text(module, function, value))
                    .unwrap_or_else(|| "default".to_string()),
            ],
        ),
        IrExportDecl::ReExport {
            source,
            imported,
            exported,
        } => (
            format!("re-export from {source:?}"),
            vec![format!("{imported} as {exported}")],
        ),
        IrExportDecl::ExportAll { source, exported } => (
            format!("all from {source:?}"),
            exported.iter().cloned().collect(),
        ),
    }
}

#[derive(Default)]
struct BytecodeBuilder {
    extern_slots: Vec<String>,
    extern_slot_ids: BTreeMap<String, u32>,
    names: Vec<String>,
    name_ids: BTreeMap<String, u32>,
    label_ids: BTreeMap<String, u32>,
    referenced_labels: BTreeSet<String>,
    scopes: Vec<NameScope>,
    constants: Vec<BytecodeConstant>,
    constant_ids: BTreeMap<String, u32>,
    functions: Vec<BytecodeFunction>,
    instructions: Vec<BytecodeInstruction>,
}

#[derive(Debug, Default, Clone)]
struct NameScope {
    names: BTreeMap<String, ScopedName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScopedName {
    Name(String),
    LocalSlot(u32),
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
        let instructions = lower_module_to_bytecode_instructions(module);
        self.referenced_labels = referenced_labels(&instructions);
        self.compile_instructions(&instructions);
        self.optimize_control_flow();
        self.resolve_labels_to_jump_targets();
        self.renumber_registers();
        BytecodeModule {
            extern_slots: self.extern_slots.clone(),
            names: self.names,
            functions: self.functions,
            constants: self.constants,
            instructions: self.instructions,
        }
    }

    fn compile_instructions(&mut self, instructions: &[LowerInstruction]) {
        for instruction in instructions {
            self.compile_instruction(instruction);
        }
    }

    fn optimize_control_flow(&mut self) {
        loop {
            let label_positions = self.label_positions();
            let redundant_jumps = self
                .instructions
                .iter()
                .enumerate()
                .filter_map(|(index, instruction)| {
                    if instruction.op != BytecodeOp::Jump {
                        return None;
                    }
                    let label = match instruction.operands.first() {
                        Some(BytecodeOperand::Label(label)) => *label,
                        _ => return None,
                    };
                    (label_positions.get(&label).copied() == Some(index + 1)).then_some(index)
                })
                .collect::<BTreeSet<_>>();

            if redundant_jumps.is_empty() {
                break;
            }

            self.remove_instruction_indexes(&redundant_jumps);
            self.remove_unreferenced_label_instructions();
        }
    }

    fn renumber_registers(&mut self) {
        let end = self.instructions.len();
        renumber_registers_in_range(&mut self.instructions, &self.functions, 0, end);
    }

    fn resolve_labels_to_jump_targets(&mut self) {
        let label_positions = self.label_positions();
        if label_positions.is_empty() {
            return;
        }
        let label_indexes = self
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                (instruction.op == BytecodeOp::Label).then_some(index)
            })
            .collect::<BTreeSet<_>>();

        let mut targets = BTreeMap::new();
        for (label, index) in &label_positions {
            let raw_target = index + 1;
            let removed_before_target = label_indexes
                .iter()
                .filter(|removed| **removed < raw_target)
                .count();
            targets.insert(*label, (raw_target - removed_before_target) as u32);
        }

        for instruction in &mut self.instructions {
            match instruction.op {
                BytecodeOp::Jump => {
                    if let Some(BytecodeOperand::Label(label)) = instruction.operands.first() {
                        if let Some(target) = targets.get(label) {
                            instruction.operands[0] = BytecodeOperand::Count(*target);
                        }
                    }
                }
                BytecodeOp::JumpIfFalse => {
                    if let Some(BytecodeOperand::Label(label)) = instruction.operands.get(1) {
                        if let Some(target) = targets.get(label) {
                            instruction.operands[1] = BytecodeOperand::Count(*target);
                        }
                    }
                }
                _ => {}
            }
        }

        self.remove_instruction_indexes(&label_indexes);
    }

    fn label_positions(&self) -> BTreeMap<u32, usize> {
        self.instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                if instruction.op != BytecodeOp::Label {
                    return None;
                }
                match instruction.operands.first() {
                    Some(BytecodeOperand::Label(label)) => Some((*label, index)),
                    _ => None,
                }
            })
            .collect()
    }

    fn remove_unreferenced_label_instructions(&mut self) {
        let referenced = self
            .instructions
            .iter()
            .filter_map(|instruction| match instruction.op {
                BytecodeOp::Jump | BytecodeOp::JumpIfFalse => match instruction.operands.last() {
                    Some(BytecodeOperand::Label(label)) => Some(*label),
                    _ => None,
                },
                _ => None,
            })
            .collect::<BTreeSet<_>>();

        let removed = self
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                if instruction.op != BytecodeOp::Label {
                    return None;
                }
                match instruction.operands.first() {
                    Some(BytecodeOperand::Label(label)) if referenced.contains(label) => None,
                    _ => Some(index),
                }
            })
            .collect::<BTreeSet<_>>();
        self.remove_instruction_indexes(&removed);
    }

    fn remove_instruction_indexes(&mut self, removed: &BTreeSet<usize>) {
        if removed.is_empty() {
            return;
        }
        self.instructions = self
            .instructions
            .iter()
            .enumerate()
            .filter(|(index, _)| !removed.contains(index))
            .map(|(_, instruction)| instruction.clone())
            .collect();
        for function in &mut self.functions {
            function.body_start = remap_instruction_boundary(function.body_start as usize, removed);
            function.body_end = remap_instruction_boundary(function.body_end as usize, removed);
        }
    }

    fn compile_instruction(&mut self, instruction: &LowerInstruction) {
        match instruction {
            LowerInstruction::Marker(message) => {
                let operand = self.string_constant_operand(message);
                self.emit(BytecodeOp::Marker, vec![operand]);
            }
            LowerInstruction::Label(label) => {
                if !self.referenced_labels.contains(label) {
                    return;
                }
                let operand = self.label_operand(label);
                self.emit(BytecodeOp::Label, vec![operand]);
            }
            LowerInstruction::Declare { kind, name } => {
                let kind = BytecodeOperand::DeclKind(decl_kind_id(kind));
                let name = self.binding_operand(name);
                self.emit(BytecodeOp::Declare, vec![kind, name]);
            }
            LowerInstruction::LoadConst { dst, value } => {
                let dst = self.register_operand(dst);
                let value = self.value_operand(value);
                self.emit(BytecodeOp::LoadConst, vec![dst, value]);
            }
            LowerInstruction::LoadName { dst, name } => {
                let dst = self.register_operand(dst);
                let name = self.binding_ref_operand(name);
                self.emit(BytecodeOp::LoadName, vec![dst, name]);
            }
            LowerInstruction::StoreName { name, src } => {
                let name = self.binding_operand(name);
                let src = self.value_operand(src);
                self.emit(BytecodeOp::StoreName, vec![name, src]);
            }
            LowerInstruction::StoreMember {
                object,
                property,
                src,
            } => {
                let object = self.value_operand(object);
                let property = self.value_operand(property);
                let src = self.value_operand(src);
                self.emit(BytecodeOp::StoreMember, vec![object, property, src]);
            }
            LowerInstruction::Move { dst, src } => {
                let dst = self.register_operand(dst);
                let src = self.value_operand(src);
                self.emit(BytecodeOp::Move, vec![dst, src]);
            }
            LowerInstruction::Binary {
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
            LowerInstruction::Unary { dst, op, arg } => {
                let dst = self.register_operand(dst);
                let op = self.operator_operand(op);
                let arg = self.value_operand(arg);
                self.emit(BytecodeOp::Unary, vec![dst, op, arg]);
            }
            LowerInstruction::Member {
                dst,
                object,
                property,
            } => {
                let dst = self.register_operand(dst);
                let object = self.value_operand(object);
                let property = self.value_operand(property);
                self.emit(BytecodeOp::Member, vec![dst, object, property]);
            }
            LowerInstruction::Array { dst, items } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    BytecodeOperand::Count(items.len() as u32),
                ];
                operands.extend(items.iter().map(|item| self.value_operand(item)));
                self.emit(BytecodeOp::Array, operands);
            }
            LowerInstruction::Object { dst, props } => {
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
            LowerInstruction::Call { dst, callee, args } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    self.value_operand(callee),
                    BytecodeOperand::Count(args.len() as u32),
                ];
                operands.extend(args.iter().map(|arg| self.value_operand(arg)));
                self.emit(BytecodeOp::Call, operands);
            }
            LowerInstruction::New { dst, callee, args } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    self.value_operand(callee),
                    BytecodeOperand::Count(args.len() as u32),
                ];
                operands.extend(args.iter().map(|arg| self.value_operand(arg)));
                self.emit(BytecodeOp::New, operands);
            }
            LowerInstruction::Template { dst, quasis, exprs } => {
                let mut operands = vec![
                    self.register_operand(dst),
                    BytecodeOperand::Count(quasis.len() as u32),
                ];
                operands.extend(quasis.iter().map(|part| self.string_constant_operand(part)));
                operands.push(BytecodeOperand::Count(exprs.len() as u32));
                operands.extend(exprs.iter().map(|expr| self.value_operand(expr)));
                self.emit(BytecodeOp::Template, operands);
            }
            LowerInstruction::Function { name, params, body } => {
                let scope = self.function_scope(params, body, Some(name));
                let function = self.function_entry(Some(name), params, body, Some(&scope));
                self.emit(
                    BytecodeOp::FunctionStart,
                    vec![BytecodeOperand::Function(function)],
                );
                self.scopes.push(scope);
                let body_start = self.instructions.len();
                self.compile_instructions(body);
                let body_end = self.instructions.len();
                self.scopes.pop();
                self.update_function_body(function, body_start, body_end);
            }
            LowerInstruction::FunctionExpr {
                dst,
                name,
                params,
                body,
            } => {
                let scope = self.function_scope(params, body, name.as_deref());
                let function = self.function_entry(name.as_deref(), params, body, Some(&scope));
                self.emit(
                    BytecodeOp::FunctionExprStart,
                    vec![
                        self.register_operand(dst),
                        BytecodeOperand::Function(function),
                    ],
                );
                self.scopes.push(scope);
                let body_start = self.instructions.len();
                self.compile_instructions(body);
                let body_end = self.instructions.len();
                self.scopes.pop();
                self.update_function_body(function, body_start, body_end);
            }
            LowerInstruction::Class {
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
            LowerInstruction::Import { source, specifiers } => {
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
            LowerInstruction::Export { kind, names } => {
                let mut operands = vec![
                    self.string_constant_operand(kind),
                    BytecodeOperand::Count(names.len() as u32),
                ];
                operands.extend(names.iter().map(|name| self.name_operand(name)));
                self.emit(BytecodeOp::Export, operands);
            }
            LowerInstruction::Throw(value) => {
                let value = self.value_operand(value);
                self.emit(BytecodeOp::Throw, vec![value]);
            }
            LowerInstruction::Try {
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
                        .map(|param| self.binding_operand(param))
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
            LowerInstruction::TryStart => {
                self.emit(BytecodeOp::TryStart, Vec::new());
            }
            LowerInstruction::CatchStart(catch_param) => {
                let catch_param = catch_param
                    .as_ref()
                    .map(|param| self.binding_operand(param))
                    .unwrap_or(BytecodeOperand::None);
                self.emit(BytecodeOp::CatchStart, vec![catch_param]);
            }
            LowerInstruction::FinallyStart => {
                self.emit(BytecodeOp::FinallyStart, Vec::new());
            }
            LowerInstruction::TryEnd => {
                self.emit(BytecodeOp::TryEnd, Vec::new());
            }
            LowerInstruction::Scope { kind, body } => {
                let scope = self.block_scope(body);
                self.emit(
                    BytecodeOp::EnterScope,
                    vec![BytecodeOperand::ScopeKind(scope_kind_id(kind))],
                );
                self.scopes.push(scope);
                self.compile_instructions(body);
                self.scopes.pop();
                self.emit(BytecodeOp::LeaveScope, Vec::new());
            }
            LowerInstruction::EnterScope(kind) => {
                self.emit(
                    BytecodeOp::EnterScope,
                    vec![BytecodeOperand::ScopeKind(scope_kind_id(kind))],
                );
            }
            LowerInstruction::LeaveScope => {
                self.emit(BytecodeOp::LeaveScope, Vec::new());
            }
            LowerInstruction::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|value| self.value_operand(value))
                    .unwrap_or(BytecodeOperand::None);
                self.emit(BytecodeOp::Return, vec![value]);
            }
            LowerInstruction::Pop(value) => {
                let value = self.value_operand(value);
                self.emit(BytecodeOp::Pop, vec![value]);
            }
            LowerInstruction::Jump(label) => {
                let label = self.label_operand(label);
                self.emit(BytecodeOp::Jump, vec![label]);
            }
            LowerInstruction::JumpIfFalse { test, label } => {
                let test = self.value_operand(test);
                let label = self.label_operand(label);
                self.emit(BytecodeOp::JumpIfFalse, vec![test, label]);
            }
            LowerInstruction::Unsupported(message) => {
                let message = self.string_constant_operand(message);
                self.emit(BytecodeOp::Unsupported, vec![message]);
            }
        }
    }

    fn emit(&mut self, op: BytecodeOp, operands: Vec<BytecodeOperand>) {
        self.instructions.push(BytecodeInstruction { op, operands });
    }

    fn value_operand(&mut self, value: &LowerValue) -> BytecodeOperand {
        match value {
            LowerValue::Register(value) => self.register_operand(value),
            LowerValue::Name(value) => self.name_ref_operand(value),
            LowerValue::LocalSlot(value) => BytecodeOperand::LocalSlot(*value),
            LowerValue::Number(value) => {
                BytecodeOperand::Constant(self.constant(BytecodeConstant::Number(*value)))
            }
            LowerValue::String(value) => self.string_constant_operand(value),
            LowerValue::Bool(value) => {
                BytecodeOperand::Constant(self.constant(BytecodeConstant::Bool(*value)))
            }
            LowerValue::Null => BytecodeOperand::Constant(self.constant(BytecodeConstant::Null)),
            LowerValue::Undefined => {
                BytecodeOperand::Constant(self.constant(BytecodeConstant::Undefined))
            }
        }
    }

    fn register_operand(&self, register: &str) -> BytecodeOperand {
        BytecodeOperand::Register(register_id(register))
    }

    fn name_operand(&mut self, name: &str) -> BytecodeOperand {
        if let Some(scoped) = self.scoped_binding(name) {
            return match scoped {
                ScopedName::Name(name) => BytecodeOperand::Name(self.name(&name)),
                ScopedName::LocalSlot(slot) => BytecodeOperand::LocalSlot(slot),
            };
        }
        let name = self.scoped_name(name);
        BytecodeOperand::Name(self.name(&name))
    }

    fn binding_operand(&mut self, binding: &LowerBinding) -> BytecodeOperand {
        match binding {
            LowerBinding::Name(name) => self.name_operand(name),
            LowerBinding::LocalSlot(slot) => BytecodeOperand::LocalSlot(*slot),
        }
    }

    fn binding_ref_operand(&mut self, binding: &LowerBinding) -> BytecodeOperand {
        match binding {
            LowerBinding::Name(name) => self.name_ref_operand(name),
            LowerBinding::LocalSlot(slot) => BytecodeOperand::LocalSlot(*slot),
        }
    }

    fn name_ref_operand(&mut self, name: &str) -> BytecodeOperand {
        if let Some(scoped) = self.scoped_binding(name) {
            return match scoped {
                ScopedName::Name(name) => BytecodeOperand::Name(self.name(&name)),
                ScopedName::LocalSlot(slot) => BytecodeOperand::LocalSlot(slot),
            };
        }
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
        params: &[LowerBinding],
        body: &[LowerInstruction],
        scope: Option<&NameScope>,
    ) -> u32 {
        let name = name.map(|name| {
            let scoped = self.scoped_name(name);
            self.name(&scoped)
        });
        let params = params
            .iter()
            .map(|param| self.binding_operand_in_scope(param, scope))
            .collect();
        let id = self.functions.len() as u32;
        let has_return = instructions_have_return_value(body);
        self.functions.push(BytecodeFunction {
            name,
            params,
            body_start: 0,
            body_end: 0,
            flags: function_flags(has_return),
            has_return,
        });
        id
    }

    fn binding_operand_in_scope(
        &mut self,
        binding: &LowerBinding,
        scope: Option<&NameScope>,
    ) -> BytecodeOperand {
        match binding {
            LowerBinding::Name(name) => {
                if let Some(scope) = scope
                    && let Some(scoped) = scope.names.get(name)
                {
                    return match scoped {
                        ScopedName::LocalSlot(slot) => BytecodeOperand::LocalSlot(*slot),
                        ScopedName::Name(name) => BytecodeOperand::Name(self.name(name)),
                    };
                }
                self.name_operand(name)
            }
            LowerBinding::LocalSlot(slot) => BytecodeOperand::LocalSlot(*slot),
        }
    }

    fn update_function_body(&mut self, function: u32, body_start: usize, body_end: usize) {
        if let Some(function) = self.functions.get_mut(function as usize) {
            function.body_start = body_start as u32;
            function.body_end = body_end as u32;
        }
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
            if let Some(ScopedName::Name(name)) = scope.names.get(name) {
                return name.clone();
            }
        }
        name.to_string()
    }

    fn scoped_binding(&self, name: &str) -> Option<ScopedName> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.names.get(name).cloned())
    }

    fn function_scope(
        &mut self,
        params: &[LowerBinding],
        body: &[LowerInstruction],
        function_name: Option<&str>,
    ) -> NameScope {
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        let mut scope = NameScope::default();
        if let Some(function_name) = function_name {
            if let Some(encoded) = self.scoped_name_if_local(function_name) {
                seen.insert(function_name.to_string());
                scope
                    .names
                    .insert(function_name.to_string(), ScopedName::Name(encoded));
            }
        }
        for param in params {
            if let LowerBinding::Name(param) = param {
                if seen.insert(param.clone()) {
                    names.push(param.clone());
                }
            }
        }
        collect_local_scope_names(body, &mut names, &mut seen);
        let captured = captured_by_nested_functions(body, &seen);
        let function_declarations = function_declaration_names(body);

        let mut local_slot = 0;
        for name in names {
            let captured = captured.contains(&name) || function_declarations.contains(&name);
            if captured {
                scope.names.insert(name.clone(), ScopedName::Name(name));
            } else {
                scope.names.insert(name, ScopedName::LocalSlot(local_slot));
                local_slot += 1;
            }
        }
        scope
    }

    fn block_scope(&mut self, body: &[LowerInstruction]) -> NameScope {
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        let mut scope = NameScope::default();
        collect_local_scope_names(body, &mut names, &mut seen);
        let captured = captured_by_nested_functions(body, &seen);
        let function_declarations = function_declaration_names(body);
        let mut local_slot = 0;
        for name in names {
            if captured.contains(&name) || function_declarations.contains(&name) {
                scope.names.insert(name.clone(), ScopedName::Name(name));
            } else {
                scope.names.insert(name, ScopedName::LocalSlot(local_slot));
                local_slot += 1;
            }
        }
        scope
    }

    fn scoped_name_if_local(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some(ScopedName::Name(name)) = scope.names.get(name) {
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

fn function_flags(has_return: bool) -> u32 {
    u32::from(has_return)
}

fn remap_instruction_boundary(boundary: usize, removed: &BTreeSet<usize>) -> u32 {
    let removed_before = removed
        .iter()
        .take_while(|index| **index < boundary)
        .count();
    boundary.saturating_sub(removed_before) as u32
}

#[derive(Debug, Clone, Copy)]
struct RegisterInterval {
    start: usize,
    end: usize,
}

fn renumber_registers_in_range(
    instructions: &mut [BytecodeInstruction],
    functions: &[BytecodeFunction],
    start: usize,
    end: usize,
) {
    let mut current_frame = Vec::new();
    let mut index = start;
    while index < end {
        match instructions[index].op {
            BytecodeOp::FunctionStart | BytecodeOp::FunctionExprStart => {
                let function_start_op = instructions[index].op;
                let function_end = function_body_end(instructions, functions, index)
                    .filter(|function_end| *function_end > index)
                    .unwrap_or(end);
                if function_start_op == BytecodeOp::FunctionExprStart {
                    current_frame.push(index);
                }
                if index + 1 < function_end {
                    renumber_registers_in_range(instructions, functions, index + 1, function_end);
                }
                index = function_end;
            }
            BytecodeOp::FunctionEnd | BytecodeOp::FunctionExprEnd => {
                index += 1;
            }
            _ => {
                current_frame.push(index);
                index += 1;
            }
        }
    }

    let mapping = if current_frame_has_control_flow(instructions, &current_frame) {
        dense_register_mapping(instructions, &current_frame)
    } else {
        allocate_register_mapping(instructions, &current_frame)
    };
    if mapping.is_empty() {
        return;
    }
    for index in current_frame {
        for operand in &mut instructions[index].operands {
            if let BytecodeOperand::Register(register) = operand {
                if let Some(mapped) = mapping.get(register) {
                    *register = *mapped;
                }
            }
        }
    }
}

fn function_body_end(
    instructions: &[BytecodeInstruction],
    functions: &[BytecodeFunction],
    start: usize,
) -> Option<usize> {
    let function_index = function_index_from_start_instruction(instructions.get(start)?)?;
    functions
        .get(function_index as usize)
        .map(|function| function.body_end as usize)
}

fn function_index_from_start_instruction(instruction: &BytecodeInstruction) -> Option<u32> {
    let operand_index = match instruction.op {
        BytecodeOp::FunctionStart => 0,
        BytecodeOp::FunctionExprStart => 1,
        _ => return None,
    };
    match instruction.operands.get(operand_index) {
        Some(BytecodeOperand::Function(index)) => Some(*index),
        _ => None,
    }
}

fn current_frame_has_control_flow(instructions: &[BytecodeInstruction], indexes: &[usize]) -> bool {
    indexes.iter().copied().any(|index| {
        matches!(
            instructions[index].op,
            BytecodeOp::Label
                | BytecodeOp::Jump
                | BytecodeOp::JumpIfFalse
                | BytecodeOp::TryStart
                | BytecodeOp::CatchStart
                | BytecodeOp::FinallyStart
                | BytecodeOp::TryEnd
        )
    })
}

fn dense_register_mapping(
    instructions: &[BytecodeInstruction],
    indexes: &[usize],
) -> BTreeMap<u32, u32> {
    let mut mapping = BTreeMap::new();
    for index in indexes {
        for operand in &instructions[*index].operands {
            if let BytecodeOperand::Register(register) = operand {
                let next = mapping.len() as u32;
                mapping.entry(*register).or_insert(next);
            }
        }
    }
    mapping
}

fn allocate_register_mapping(
    instructions: &[BytecodeInstruction],
    indexes: &[usize],
) -> BTreeMap<u32, u32> {
    let mut intervals = BTreeMap::<u32, RegisterInterval>::new();
    for (position, index) in indexes.iter().copied().enumerate() {
        for operand in &instructions[index].operands {
            if let BytecodeOperand::Register(register) = operand {
                intervals
                    .entry(*register)
                    .and_modify(|interval| interval.end = position)
                    .or_insert(RegisterInterval {
                        start: position,
                        end: position,
                    });
            }
        }
    }

    let mut ordered = intervals
        .into_iter()
        .map(|(register, interval)| (register, interval.start, interval.end))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(register, start, end)| (*start, *end, *register));

    let mut active = Vec::<(u32, usize, u32)>::new();
    let mut mapping = BTreeMap::new();
    for (register, start, end) in ordered {
        active.retain(|(_, active_end, _)| *active_end >= start);
        let physical = first_free_register(&active);
        active.push((register, end, physical));
        mapping.insert(register, physical);
    }
    mapping
}

fn first_free_register(active: &[(u32, usize, u32)]) -> u32 {
    let mut candidate = 0u32;
    loop {
        if active.iter().all(|(_, _, physical)| *physical != candidate) {
            return candidate;
        }
        candidate += 1;
    }
}

fn collect_local_scope_names(
    instructions: &[LowerInstruction],
    names: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) {
    for instruction in instructions {
        match instruction {
            LowerInstruction::Declare { name, .. } => {
                if let LowerBinding::Name(name) = name {
                    if seen.insert(name.clone()) {
                        names.push(name.clone());
                    }
                }
            }
            LowerInstruction::Function { name, .. } => {
                if seen.insert(name.clone()) {
                    names.push(name.clone());
                }
            }
            LowerInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_local_scope_names(body, names, seen);
                if let Some(catch_param) = catch_param {
                    if let LowerBinding::Name(catch_param) = catch_param {
                        if seen.insert(catch_param.clone()) {
                            names.push(catch_param.clone());
                        }
                    }
                }
                collect_local_scope_names(catch_body, names, seen);
                collect_local_scope_names(finally_body, names, seen);
            }
            LowerInstruction::Scope { .. } => {}
            LowerInstruction::FunctionExpr { .. } => {}
            _ => {}
        }
    }
}

fn function_declaration_names(instructions: &[LowerInstruction]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_function_declaration_names(instructions, &mut names);
    names
}

fn collect_function_declaration_names(
    instructions: &[LowerInstruction],
    names: &mut BTreeSet<String>,
) {
    for instruction in instructions {
        match instruction {
            LowerInstruction::Function { name, .. } => {
                names.insert(name.clone());
            }
            LowerInstruction::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                collect_function_declaration_names(body, names);
                collect_function_declaration_names(catch_body, names);
                collect_function_declaration_names(finally_body, names);
            }
            LowerInstruction::Scope { body, .. } => {
                collect_function_declaration_names(body, names);
            }
            LowerInstruction::FunctionExpr { .. } => {}
            _ => {}
        }
    }
}

fn captured_by_nested_functions(
    instructions: &[LowerInstruction],
    local_names: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut captured = BTreeSet::new();
    for instruction in instructions {
        match instruction {
            LowerInstruction::Function { name, params, body } => {
                let shadowed = function_shadowed_names(Some(name), params, body);
                collect_name_refs(body, local_names, &shadowed, &mut captured);
            }
            LowerInstruction::FunctionExpr {
                name, params, body, ..
            } => {
                let shadowed = function_shadowed_names(name.as_deref(), params, body);
                collect_name_refs(body, local_names, &shadowed, &mut captured);
            }
            LowerInstruction::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                captured.extend(captured_by_nested_functions(body, local_names));
                captured.extend(captured_by_nested_functions(catch_body, local_names));
                captured.extend(captured_by_nested_functions(finally_body, local_names));
            }
            LowerInstruction::Scope { body, .. } => {
                captured.extend(captured_by_nested_functions(body, local_names));
            }
            _ => {}
        }
    }
    captured
}

fn collect_name_refs(
    instructions: &[LowerInstruction],
    local_names: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    captured: &mut BTreeSet<String>,
) {
    for instruction in instructions {
        match instruction {
            LowerInstruction::Declare { name, .. } => {
                collect_binding_ref(name, local_names, shadowed, captured)
            }
            LowerInstruction::LoadConst { value, .. } => {
                collect_value_ref(value, local_names, shadowed, captured)
            }
            LowerInstruction::LoadName { name, .. } => {
                collect_binding_ref(name, local_names, shadowed, captured)
            }
            LowerInstruction::StoreName { name, src } => {
                collect_binding_ref(name, local_names, shadowed, captured);
                collect_value_ref(src, local_names, shadowed, captured);
            }
            LowerInstruction::StoreMember {
                object,
                property,
                src,
            } => {
                collect_value_ref(object, local_names, shadowed, captured);
                collect_value_ref(property, local_names, shadowed, captured);
                collect_value_ref(src, local_names, shadowed, captured);
            }
            LowerInstruction::Move { src, .. }
            | LowerInstruction::Unary { arg: src, .. }
            | LowerInstruction::Throw(src)
            | LowerInstruction::Pop(src) => collect_value_ref(src, local_names, shadowed, captured),
            LowerInstruction::Binary { left, right, .. } => {
                collect_value_ref(left, local_names, shadowed, captured);
                collect_value_ref(right, local_names, shadowed, captured);
            }
            LowerInstruction::Member {
                object, property, ..
            } => {
                collect_value_ref(object, local_names, shadowed, captured);
                collect_value_ref(property, local_names, shadowed, captured);
            }
            LowerInstruction::Array { items, .. } => {
                for item in items {
                    collect_value_ref(item, local_names, shadowed, captured);
                }
            }
            LowerInstruction::Object { props, .. } => {
                for (_, value) in props {
                    collect_value_ref(value, local_names, shadowed, captured);
                }
            }
            LowerInstruction::Call { callee, args, .. }
            | LowerInstruction::New { callee, args, .. } => {
                collect_value_ref(callee, local_names, shadowed, captured);
                for arg in args {
                    collect_value_ref(arg, local_names, shadowed, captured);
                }
            }
            LowerInstruction::Template { exprs, .. } => {
                for expr in exprs {
                    collect_value_ref(expr, local_names, shadowed, captured);
                }
            }
            LowerInstruction::Function { name, params, body } => {
                let child_shadowed =
                    merge_shadowed(shadowed, &function_shadowed_names(Some(name), params, body));
                collect_name_refs(body, local_names, &child_shadowed, captured)
            }
            LowerInstruction::FunctionExpr {
                name, params, body, ..
            } => {
                let child_shadowed = merge_shadowed(
                    shadowed,
                    &function_shadowed_names(name.as_deref(), params, body),
                );
                collect_name_refs(body, local_names, &child_shadowed, captured)
            }
            LowerInstruction::Scope { body, .. } => {
                collect_name_refs(body, local_names, shadowed, captured)
            }
            LowerInstruction::Class { super_class, .. } => {
                if let Some(super_class) = super_class {
                    collect_value_ref(super_class, local_names, shadowed, captured);
                }
            }
            LowerInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_name_refs(body, local_names, shadowed, captured);
                if let Some(catch_param) = catch_param {
                    collect_binding_ref(catch_param, local_names, shadowed, captured);
                }
                collect_name_refs(catch_body, local_names, shadowed, captured);
                collect_name_refs(finally_body, local_names, shadowed, captured);
            }
            LowerInstruction::Return(value) => {
                if let Some(value) = value {
                    collect_value_ref(value, local_names, shadowed, captured);
                }
            }
            LowerInstruction::JumpIfFalse { test, .. } => {
                collect_value_ref(test, local_names, shadowed, captured)
            }
            _ => {}
        }
    }
}

fn function_shadowed_names(
    function_name: Option<&str>,
    params: &[LowerBinding],
    body: &[LowerInstruction],
) -> BTreeSet<String> {
    let mut shadowed = BTreeSet::new();
    if let Some(function_name) = function_name {
        shadowed.insert(function_name.to_string());
    }
    for param in params {
        if let LowerBinding::Name(param) = param {
            shadowed.insert(param.clone());
        }
    }
    collect_function_declared_names(body, &mut shadowed);
    shadowed
}

fn collect_function_declared_names(
    instructions: &[LowerInstruction],
    names: &mut BTreeSet<String>,
) {
    for instruction in instructions {
        match instruction {
            LowerInstruction::Declare { name, .. } => {
                if let LowerBinding::Name(name) = name {
                    names.insert(name.clone());
                }
            }
            LowerInstruction::Function { name, .. } => {
                names.insert(name.clone());
            }
            LowerInstruction::Try {
                body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                collect_function_declared_names(body, names);
                if let Some(LowerBinding::Name(catch_param)) = catch_param {
                    names.insert(catch_param.clone());
                }
                collect_function_declared_names(catch_body, names);
                collect_function_declared_names(finally_body, names);
            }
            LowerInstruction::Scope { .. } | LowerInstruction::FunctionExpr { .. } => {}
            _ => {}
        }
    }
}

fn merge_shadowed(left: &BTreeSet<String>, right: &BTreeSet<String>) -> BTreeSet<String> {
    left.union(right).cloned().collect()
}

fn collect_binding_ref(
    binding: &LowerBinding,
    local_names: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    captured: &mut BTreeSet<String>,
) {
    if let LowerBinding::Name(name) = binding {
        if local_names.contains(name) && !shadowed.contains(name) {
            captured.insert(name.clone());
        }
    }
}

fn collect_value_ref(
    value: &LowerValue,
    local_names: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    captured: &mut BTreeSet<String>,
) {
    if let LowerValue::Name(name) = value {
        if local_names.contains(name) && !shadowed.contains(name) {
            captured.insert(name.clone());
        }
    }
}

fn referenced_labels(instructions: &[LowerInstruction]) -> BTreeSet<String> {
    let mut labels = BTreeSet::new();
    collect_referenced_labels(instructions, &mut labels);
    labels
}

fn collect_referenced_labels(instructions: &[LowerInstruction], labels: &mut BTreeSet<String>) {
    for instruction in instructions {
        match instruction {
            LowerInstruction::Jump(label) => {
                labels.insert(label.clone());
            }
            LowerInstruction::JumpIfFalse { label, .. } => {
                labels.insert(label.clone());
            }
            LowerInstruction::Function { body, .. }
            | LowerInstruction::FunctionExpr { body, .. }
            | LowerInstruction::Scope { body, .. } => {
                collect_referenced_labels(body, labels);
            }
            LowerInstruction::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                collect_referenced_labels(body, labels);
                collect_referenced_labels(catch_body, labels);
                collect_referenced_labels(finally_body, labels);
            }
            _ => {}
        }
    }
}

fn instructions_have_return_value(instructions: &[LowerInstruction]) -> bool {
    instructions.iter().any(instruction_has_return_value)
}

fn instruction_has_return_value(instruction: &LowerInstruction) -> bool {
    match instruction {
        LowerInstruction::Return(Some(_)) => true,
        LowerInstruction::Try {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            instructions_have_return_value(body)
                || instructions_have_return_value(catch_body)
                || instructions_have_return_value(finally_body)
        }
        LowerInstruction::Scope { body, .. } => instructions_have_return_value(body),
        LowerInstruction::Function { .. } | LowerInstruction::FunctionExpr { .. } => false,
        _ => false,
    }
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

fn scope_kind_id(kind: &str) -> u32 {
    match kind {
        "block" => 0,
        "function" => 1,
        "catch" => 2,
        _ => 3,
    }
}

fn scope_kind_name(kind: u32) -> Option<&'static str> {
    match kind {
        0 => Some("block"),
        1 => Some("function"),
        2 => Some("catch"),
        3 => Some("scope"),
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
        permutation: permutation.to_string(),
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
        "register", "constant", "name", "local", "extern", "label", "count", "none", "function",
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

fn validate_operand_tags_do_not_use_compact_range(
    values: &BTreeMap<String, u8>,
) -> Result<(), EncodingError> {
    for (key, code) in values {
        if *code >= COMPACT_VALUE_OPERAND_BASE {
            return Err(EncodingError::Seed(format!(
                "operand tag {key} uses compact operand reserved code {code}"
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
    if indexes.len() > SEED_DIGITS.len() {
        return Err(EncodingError::Seed(format!(
            "{kind} permutation supports at most {} entries",
            SEED_DIGITS.len()
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
    SEED_DIGITS
        .get(value as usize)
        .map(|digit| char::from(*digit))
        .ok_or_else(|| EncodingError::Seed(format!("seed index {value} is outside seed range")))
}

fn decode_base36_digit(value: u8) -> Result<u8, EncodingError> {
    SEED_DIGITS
        .iter()
        .position(|digit| *digit == value)
        .map(|index| index as u8)
        .ok_or_else(|| EncodingError::Seed(format!("invalid seed digit {:?}", char::from(value))))
}

const SEED_DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_~";

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

    fn read_constant_string(&mut self) -> Result<String, EncodingError> {
        match self.read_u32()? {
            0 => {
                let index = self.read_u32()? as usize;
                constant_string_atom(index)
                    .map(str::to_string)
                    .ok_or_else(|| EncodingError::UnknownCode(format!("string atom {index}")))
            }
            1 => self.read_prefixed_string(),
            len_plus_two => {
                let len = len_plus_two
                    .checked_sub(2)
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
        if marker == atom_marker + 1 {
            return self.read_prefixed_string();
        }

        let len = marker
            .checked_sub(atom_marker + 2)
            .ok_or_else(|| EncodingError::UnknownCode("name string length marker".to_string()))?;
        let bytes = self.read_slice(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|err| EncodingError::UnknownCode(format!("utf8 name string: {err}")))
    }

    fn read_prefixed_string(&mut self) -> Result<String, EncodingError> {
        let prefix_index = self.read_u32()? as usize;
        let prefix = string_prefix_atom(prefix_index)
            .ok_or_else(|| EncodingError::UnknownCode(format!("string prefix {prefix_index}")))?;
        Ok(format!("{prefix}{}", self.read_constant_string()?))
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

fn write_constant_string(bytes: &mut Vec<u8>, value: &str) {
    if let Some(index) = constant_string_atom_index(value) {
        write_u32(bytes, 0);
        write_u32(bytes, index as u32);
    } else if let Some((prefix, suffix)) = string_prefix_atom_index(value) {
        write_u32(bytes, 1);
        write_u32(bytes, prefix as u32);
        write_constant_string(bytes, suffix);
    } else {
        write_u32(bytes, value.len() as u32 + 2);
        bytes.extend_from_slice(value.as_bytes());
    }
}

fn write_name_string(bytes: &mut Vec<u8>, value: &str, extern_slots: &[String]) {
    if let Some(index) = extern_slot_index(extern_slots, value) {
        write_u32(bytes, index as u32);
    } else if let Some(index) = constant_string_atom_index(value) {
        write_u32(bytes, extern_slots.len() as u32);
        write_u32(bytes, index as u32);
    } else if let Some((prefix, suffix)) = string_prefix_atom_index(value) {
        write_u32(bytes, extern_slots.len() as u32 + 1);
        write_u32(bytes, prefix as u32);
        write_constant_string(bytes, suffix);
    } else {
        write_u32(bytes, extern_slots.len() as u32 + value.len() as u32 + 2);
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

fn string_prefix_atom_index(value: &str) -> Option<(usize, &str)> {
    STRING_PREFIX_ATOMS
        .iter()
        .enumerate()
        .filter_map(|(index, prefix)| {
            value
                .strip_prefix(prefix)
                .filter(|suffix| !suffix.is_empty())
                .map(|suffix| (index, *prefix, suffix))
        })
        .max_by_key(|(_, prefix, _)| prefix.len())
        .map(|(index, _, suffix)| (index, suffix))
}

fn string_prefix_atom(index: usize) -> Option<&'static str> {
    STRING_PREFIX_ATOMS.get(index).copied()
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
    "module",
    "exports",
    "require",
    "define",
    "globalThis",
    "self",
    "this",
    "undefined",
    "__esModule",
    "fetch",
    "then",
    "catch",
    "length",
    "prototype",
    "constructor",
    "createElement",
    "getElementById",
    "querySelector",
    "addEventListener",
    "removeEventListener",
    "appendChild",
    "removeChild",
    "setAttribute",
    "getAttribute",
    "className",
    "fn",
    "extend",
    "jQuery",
    "jquery",
    "style",
    "body",
    "head",
    "nodeType",
    "ownerDocument",
    "parentNode",
    "firstChild",
    "lastChild",
    "nextSibling",
    "previousSibling",
    "childNodes",
    "innerHTML",
    "textContent",
    "readyState",
    "DOMContentLoaded",
    "defaultView",
    "location",
    "href",
    "push",
    "pop",
    "slice",
    "splice",
    "indexOf",
    "forEach",
    "map",
    "filter",
    "reduce",
    "call",
    "apply",
    "bind",
    "toString",
    "valueOf",
];

const STRING_PREFIX_ATOMS: &[&str] = &[
    "prototype.",
    "constructor.",
    "module.",
    "exports.",
    "window.",
    "document.",
    "globalThis.",
    "Object.",
    "Array.",
    "String.",
    "Function.",
    "RegExp.",
    "Math.",
    "console.",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn bytecode_from_lower(
        extern_slots: Vec<String>,
        instructions: Vec<LowerInstruction>,
    ) -> BytecodeModule {
        let mut builder = BytecodeBuilder {
            extern_slots,
            ..BytecodeBuilder::default()
        };
        builder.extern_slot_ids = builder
            .extern_slots
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index as u32))
            .collect();
        builder.referenced_labels = referenced_labels(&instructions);
        builder.compile_instructions(&instructions);
        builder.optimize_control_flow();
        builder.resolve_labels_to_jump_targets();
        builder.renumber_registers();
        BytecodeModule {
            extern_slots: builder.extern_slots,
            names: builder.names,
            functions: builder.functions,
            constants: builder.constants,
            instructions: builder.instructions,
        }
    }

    fn max_register(module: &BytecodeModule) -> Option<u32> {
        module
            .instructions
            .iter()
            .flat_map(|instruction| &instruction.operands)
            .filter_map(|operand| match operand {
                BytecodeOperand::Register(register) => Some(*register),
                _ => None,
            })
            .max()
    }

    fn simple_ir_module(
        constants: Vec<IrConst>,
        instructions: Vec<IrInstruction>,
        terminator: IrTerminator,
    ) -> IrModule {
        IrModule {
            constants,
            functions: vec![IrFunction {
                name: Some("entry".to_string()),
                locals: vec![IrLocal {
                    name: Some("a".to_string()),
                    kind: IrBindingKind::Const,
                    scope: ScopeId(0),
                    mutable: false,
                    captured: false,
                }],
                scopes: vec![IrScope {
                    parent: None,
                    kind: IrScopeKind::Global,
                    bindings: vec![LocalId(0)],
                }],
                register_count: 1,
                blocks: vec![IrBlock {
                    instructions,
                    terminator,
                    ..IrBlock::default()
                }],
                ..IrFunction::default()
            }],
            entry: FunctionId(0),
            ..IrModule::default()
        }
    }

    #[test]
    fn renders_ir_text_from_core() {
        let module = simple_ir_module(
            vec![IrConst::Int(1)],
            vec![
                IrInstruction::new(IrInstructionKind::Declare(IrDeclaration {
                    local: LocalId(0),
                    kind: IrBindingKind::Const,
                    name: Some("a".to_string()),
                    init: None,
                })),
                IrInstruction::new(IrInstructionKind::Move {
                    dst: RegisterId(0),
                    src: IrValue::Const(ConstId(0)),
                }),
            ],
            IrTerminator::Return(Some(IrValue::Register(RegisterId(0)))),
        );

        let text = module.to_text();
        assert!(text.contains("declare const local#0 name=a"), "{text}");
        assert!(text.contains("r0 = move const#0"), "{text}");
        assert!(text.contains("return r0"), "{text}");
    }

    #[test]
    fn lowers_ir_to_bytecode_in_core() {
        let module = simple_ir_module(
            vec![IrConst::Int(1)],
            vec![IrInstruction::new(IrInstructionKind::Move {
                dst: RegisterId(0),
                src: IrValue::Const(ConstId(0)),
            })],
            IrTerminator::Return(Some(IrValue::Register(RegisterId(0)))),
        );

        let bytecode = module.to_bytecode();
        assert!(bytecode.to_text().contains("MOVE"));
        assert!(
            bytecode
                .to_bytes()
                .starts_with(DEFAULT_BYTECODE_MAGIC.as_bytes())
        );
    }

    #[test]
    fn compact_bytecode_roundtrips_dynamic_operand_layouts() {
        let bytecode = bytecode_from_lower(
            vec!["console".to_string()],
            vec![
                LowerInstruction::Array {
                    dst: "arr".to_string(),
                    items: vec![LowerValue::Number(1.0), LowerValue::String("x".to_string())],
                },
                LowerInstruction::Object {
                    dst: "obj".to_string(),
                    props: vec![
                        ("a".to_string(), LowerValue::Register("arr".to_string())),
                        ("b".to_string(), LowerValue::Bool(true)),
                    ],
                },
                LowerInstruction::Call {
                    dst: "call".to_string(),
                    callee: LowerValue::Name("fn".to_string()),
                    args: vec![
                        LowerValue::Register("arr".to_string()),
                        LowerValue::Register("obj".to_string()),
                    ],
                },
                LowerInstruction::New {
                    dst: "instance".to_string(),
                    callee: LowerValue::Name("Ctor".to_string()),
                    args: vec![LowerValue::Register("call".to_string())],
                },
                LowerInstruction::Template {
                    dst: "template".to_string(),
                    quasis: vec!["hello ".to_string(), "".to_string()],
                    exprs: vec![LowerValue::Register("instance".to_string())],
                },
                LowerInstruction::Function {
                    name: "named".to_string(),
                    params: vec![LowerBinding::Name("value".to_string())],
                    body: vec![LowerInstruction::Return(Some(LowerValue::Name(
                        "value".to_string(),
                    )))],
                },
                LowerInstruction::FunctionExpr {
                    dst: "expr".to_string(),
                    name: None,
                    params: vec![
                        LowerBinding::Name("left".to_string()),
                        LowerBinding::Name("right".to_string()),
                    ],
                    body: vec![LowerInstruction::Return(None)],
                },
                LowerInstruction::Class {
                    dst: Some("klass".to_string()),
                    name: Some("Klass".to_string()),
                    super_class: Some(LowerValue::Name("Base".to_string())),
                    members: vec!["method".to_string(), "field".to_string()],
                },
                LowerInstruction::Import {
                    source: "./mod.js".to_string(),
                    specifiers: vec!["a".to_string(), "b".to_string()],
                },
                LowerInstruction::Export {
                    kind: "named".to_string(),
                    names: vec!["a".to_string(), "b".to_string()],
                },
            ],
        );
        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();
        let mut expected = bytecode.clone();
        expected.extern_slots = vec!["e0".to_string()];

        assert_eq!(restored, expected);
        assert!(bytes.starts_with(DEFAULT_BYTECODE_MAGIC.as_bytes()));
    }

    #[test]
    fn functions_are_declared_in_fun_section_and_indexed_from_code() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![LowerInstruction::Function {
                name: "add".to_string(),
                params: vec![
                    LowerBinding::Name("left".to_string()),
                    LowerBinding::Name("right".to_string()),
                ],
                body: vec![LowerInstruction::Return(Some(LowerValue::Name(
                    "left".to_string(),
                )))],
            }],
        );
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
    fn scope_ir_emits_explicit_scope_opcodes() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![LowerInstruction::Scope {
                kind: "block".to_string(),
                body: vec![
                    LowerInstruction::Declare {
                        kind: "let".to_string(),
                        name: LowerBinding::Name("value".to_string()),
                    },
                    LowerInstruction::StoreName {
                        name: LowerBinding::Name("value".to_string()),
                        src: LowerValue::Number(1.0),
                    },
                ],
            }],
        );
        let text = bytecode.to_text();
        let restored = super::BytecodeModule::from_bytes(&bytecode.to_bytes()).unwrap();

        assert_eq!(
            bytecode.instructions.first().unwrap().op,
            super::BytecodeOp::EnterScope
        );
        assert_eq!(
            bytecode.instructions.last().unwrap().op,
            super::BytecodeOp::LeaveScope
        );
        assert!(text.contains("ENTER_SCOPE block"), "{text}");
        assert_eq!(restored, bytecode);
    }

    #[test]
    fn specialized_opcodes_decode_to_canonical_instructions() {
        let bytecode = super::BytecodeModule {
            extern_slots: Vec::new(),
            names: Vec::new(),
            functions: Vec::new(),
            constants: vec![
                super::BytecodeConstant::Number(128.0),
                super::BytecodeConstant::Undefined,
                super::BytecodeConstant::Null,
                super::BytecodeConstant::Bool(true),
                super::BytecodeConstant::Bool(false),
                super::BytecodeConstant::Number(-7.0),
                super::BytecodeConstant::String("key".to_string()),
            ],
            instructions: vec![
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(3),
                        super::BytecodeOperand::Constant(1),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(4),
                        super::BytecodeOperand::Constant(2),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(5),
                        super::BytecodeOperand::Constant(3),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(6),
                        super::BytecodeOperand::Constant(4),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(7),
                        super::BytecodeOperand::Constant(5),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Pop,
                    operands: vec![super::BytecodeOperand::Register(0)],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadName,
                    operands: vec![
                        super::BytecodeOperand::Register(2),
                        super::BytecodeOperand::LocalSlot(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadName,
                    operands: vec![
                        super::BytecodeOperand::Register(20),
                        super::BytecodeOperand::LocalSlot(20),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::StoreName,
                    operands: vec![
                        super::BytecodeOperand::LocalSlot(0),
                        super::BytecodeOperand::Register(2),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::StoreName,
                    operands: vec![
                        super::BytecodeOperand::LocalSlot(20),
                        super::BytecodeOperand::Register(20),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Member,
                    operands: vec![
                        super::BytecodeOperand::Register(8),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(6),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::StoreMember,
                    operands: vec![
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(6),
                        super::BytecodeOperand::Register(8),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Binary,
                    operands: vec![
                        super::BytecodeOperand::Register(9),
                        super::BytecodeOperand::Operator(0),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Register(8),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Binary,
                    operands: vec![
                        super::BytecodeOperand::Register(10),
                        super::BytecodeOperand::Operator(0),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(5),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::JumpIfFalse,
                    operands: vec![
                        super::BytecodeOperand::Register(10),
                        super::BytecodeOperand::Count(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Return,
                    operands: vec![super::BytecodeOperand::Register(0)],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Return,
                    operands: vec![super::BytecodeOperand::Constant(0)],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Call,
                    operands: vec![
                        super::BytecodeOperand::Register(1),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Count(0),
                    ],
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
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Call,
                    operands: vec![
                        super::BytecodeOperand::Register(1),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Count(2),
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Register(2),
                    ],
                },
            ],
        };
        let bytes = bytecode.to_bytes();

        assert!(bytes.contains(&(super::BytecodeOp::LoadConstConst as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadUndefined as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadNull as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadTrue as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadFalse as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadIntSmall as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::PopReg as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::CallZero as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::CallOne as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::CallTwo as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::ReturnReg as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::ReturnConst as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::JumpIfFalseReg as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::BinaryRegReg as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::BinaryRegConst as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::MemberConst as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::StoreMemberConst as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadLocal as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::StoreLocal as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::LoadLocalSmall as u8)));
        assert!(bytes.contains(&(super::BytecodeOp::StoreLocalSmall as u8)));
        assert_eq!(super::BytecodeModule::from_bytes(&bytes).unwrap(), bytecode);
    }

    #[test]
    fn bytecode_builder_renumbers_registers_per_function() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![
                LowerInstruction::LoadConst {
                    dst: "t120".to_string(),
                    value: LowerValue::Number(1.0),
                },
                LowerInstruction::LoadConst {
                    dst: "t250".to_string(),
                    value: LowerValue::Number(2.0),
                },
                LowerInstruction::Binary {
                    dst: "t999".to_string(),
                    op: "+".to_string(),
                    left: LowerValue::Register("t120".to_string()),
                    right: LowerValue::Register("t250".to_string()),
                },
                LowerInstruction::Pop(LowerValue::Register("t999".to_string())),
                LowerInstruction::FunctionExpr {
                    dst: "t700".to_string(),
                    name: Some("inner".to_string()),
                    params: Vec::new(),
                    body: vec![
                        LowerInstruction::LoadConst {
                            dst: "t400".to_string(),
                            value: LowerValue::Number(3.0),
                        },
                        LowerInstruction::Pop(LowerValue::Register("t400".to_string())),
                        LowerInstruction::LoadConst {
                            dst: "t401".to_string(),
                            value: LowerValue::Number(4.0),
                        },
                        LowerInstruction::Return(Some(LowerValue::Register("t401".to_string()))),
                    ],
                },
                LowerInstruction::Pop(LowerValue::Register("t700".to_string())),
            ],
        );

        assert!(
            max_register(&bytecode).is_some_and(|register| register < 4),
            "{:#?}",
            bytecode.instructions
        );
        assert_eq!(
            super::BytecodeModule::from_bytes(&bytecode.to_bytes()).unwrap(),
            bytecode
        );
    }

    #[test]
    fn register_renumbering_does_not_reuse_across_control_flow() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![
                LowerInstruction::FunctionExpr {
                    dst: "t0".to_string(),
                    name: Some("outer".to_string()),
                    params: Vec::new(),
                    body: vec![LowerInstruction::Return(Some(LowerValue::Number(1.0)))],
                },
                LowerInstruction::Jump("late".to_string()),
                LowerInstruction::Label("join".to_string()),
                LowerInstruction::FunctionExpr {
                    dst: "t2".to_string(),
                    name: Some("factory".to_string()),
                    params: Vec::new(),
                    body: vec![LowerInstruction::Return(Some(LowerValue::Number(2.0)))],
                },
                LowerInstruction::Call {
                    dst: "t3".to_string(),
                    callee: LowerValue::Register("t0".to_string()),
                    args: vec![
                        LowerValue::Register("t1".to_string()),
                        LowerValue::Register("t2".to_string()),
                    ],
                },
                LowerInstruction::Pop(LowerValue::Register("t3".to_string())),
                LowerInstruction::Jump("done".to_string()),
                LowerInstruction::Label("late".to_string()),
                LowerInstruction::LoadConst {
                    dst: "t4".to_string(),
                    value: LowerValue::Number(4.0),
                },
                LowerInstruction::Move {
                    dst: "t1".to_string(),
                    src: LowerValue::Register("t4".to_string()),
                },
                LowerInstruction::Jump("join".to_string()),
                LowerInstruction::Label("done".to_string()),
            ],
        );

        let call_callee = bytecode
            .instructions
            .iter()
            .find(|instruction| instruction.op == BytecodeOp::Call)
            .and_then(|instruction| instruction.operands.get(1))
            .cloned();
        let late_load_dst = bytecode
            .instructions
            .iter()
            .rev()
            .find(|instruction| instruction.op == BytecodeOp::LoadConst)
            .and_then(|instruction| instruction.operands.first())
            .cloned();

        assert_ne!(call_callee, late_load_dst, "{:#?}", bytecode.instructions);
        assert!(
            max_register(&bytecode).is_some_and(|register| register < 64),
            "{:#?}",
            bytecode.instructions
        );
    }

    #[test]
    fn compact_value_operands_use_single_byte_for_common_operands() {
        let bytecode = super::BytecodeModule {
            extern_slots: vec!["console".to_string()],
            names: vec!["globalName".to_string()],
            functions: vec![super::BytecodeFunction {
                name: None,
                params: Vec::new(),
                body_start: 0,
                body_end: 0,
                flags: 0,
                has_return: false,
            }],
            constants: vec![
                super::BytecodeConstant::Undefined,
                super::BytecodeConstant::Null,
                super::BytecodeConstant::Bool(true),
                super::BytecodeConstant::Bool(false),
            ],
            instructions: vec![
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Move,
                    operands: vec![
                        super::BytecodeOperand::Register(1),
                        super::BytecodeOperand::Register(2),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Move,
                    operands: vec![
                        super::BytecodeOperand::Register(3),
                        super::BytecodeOperand::Constant(2),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Move,
                    operands: vec![
                        super::BytecodeOperand::Register(4),
                        super::BytecodeOperand::LocalSlot(1),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Move,
                    operands: vec![
                        super::BytecodeOperand::Register(5),
                        super::BytecodeOperand::External(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Move,
                    operands: vec![
                        super::BytecodeOperand::Register(6),
                        super::BytecodeOperand::Function(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Move,
                    operands: vec![
                        super::BytecodeOperand::Register(7),
                        super::BytecodeOperand::Name(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Return,
                    operands: vec![super::BytecodeOperand::None],
                },
            ],
        };
        let bytes = bytecode.to_bytes();

        assert!(bytes.contains(&0x82), "{bytes:?}");
        assert!(bytes.contains(&0xc2), "{bytes:?}");
        assert!(bytes.contains(&0xe1), "{bytes:?}");
        assert!(bytes.contains(&0xf0), "{bytes:?}");
        assert!(bytes.contains(&0xf8), "{bytes:?}");
        assert!(bytes.contains(&0xfc), "{bytes:?}");
        assert!(bytes.contains(&0xff), "{bytes:?}");
        let mut expected = bytecode.clone();
        expected.extern_slots = vec!["e0".to_string()];
        assert_eq!(super::BytecodeModule::from_bytes(&bytes).unwrap(), expected);
    }

    #[test]
    fn bytecode_bytes_profile_reports_sections_opcodes_and_operands() {
        let bytecode = super::BytecodeModule {
            extern_slots: Vec::new(),
            names: Vec::new(),
            functions: Vec::new(),
            constants: vec![super::BytecodeConstant::Number(128.0)],
            instructions: vec![
                super::BytecodeInstruction {
                    op: super::BytecodeOp::LoadConst,
                    operands: vec![
                        super::BytecodeOperand::Register(0),
                        super::BytecodeOperand::Constant(0),
                    ],
                },
                super::BytecodeInstruction {
                    op: super::BytecodeOp::Return,
                    operands: vec![super::BytecodeOperand::Register(0)],
                },
            ],
        };
        let text = bytecode.bytes_profile_text();

        assert!(text.contains("BYTES PROFILE"), "{text}");
        assert!(text.contains("sections:"), "{text}");
        assert!(text.contains("opcodes:"), "{text}");
        assert!(text.contains("operands:"), "{text}");
        assert!(text.contains("LOAD_CONST_CONST"), "{text}");
        assert!(text.contains("RETURN_REG"), "{text}");
        assert!(text.contains("constant"), "{text}");
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
                super::BytecodeConstant::String("exports".to_string()),
                super::BytecodeConstant::String("createElement".to_string()),
                super::BytecodeConstant::String("custom-name".to_string()),
            ],
            instructions: Vec::new(),
        };
        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();

        assert_eq!(restored, bytecode);
        assert_eq!(count_subslice(&bytes, b"exports"), 0);
        assert_eq!(count_subslice(&bytes, b"createElement"), 0);
        let raw_string_total = DEFAULT_BYTECODE_MAGIC.len()
            + 1
            + 1
            + 1
            + 1
            + ["const", "+", "exports", "createElement", "custom-name"]
                .iter()
                .map(|value| 1 + encoded_u32_len(value.len() as u32 + 2) + value.len())
                .sum::<usize>()
            + 1;
        assert!(bytes.len() < raw_string_total, "{bytes:?}");
    }

    #[test]
    fn string_constants_use_prefix_dictionary() {
        let bytecode = super::BytecodeModule {
            extern_slots: Vec::new(),
            names: vec!["module.exports".to_string()],
            functions: Vec::new(),
            constants: vec![
                super::BytecodeConstant::String("prototype.extend".to_string()),
                super::BytecodeConstant::String("document.createElement".to_string()),
                super::BytecodeConstant::String("module.exports".to_string()),
            ],
            instructions: Vec::new(),
        };
        let bytes = bytecode.to_bytes();
        let restored = super::BytecodeModule::from_bytes(&bytes).unwrap();

        assert_eq!(restored, bytecode);
        assert_eq!(count_subslice(&bytes, b"prototype."), 0);
        assert_eq!(count_subslice(&bytes, b"document."), 0);
        assert_eq!(count_subslice(&bytes, b"module."), 0);
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
        let mut expected = bytecode.clone();
        expected.extern_slots = vec!["e0".to_string()];

        assert_eq!(restored, expected);
        assert_eq!(count_subslice(&bytes, b"console"), 0);
        assert!(!text.contains(".names"));
        assert!(text.contains("LOAD_NAME r0, extern#0(\"console\")"));
    }

    #[test]
    fn local_slots_do_not_enter_names_table() {
        let function_body = vec![
            LowerInstruction::Declare {
                kind: "const".to_string(),
                name: LowerBinding::LocalSlot(0),
            },
            LowerInstruction::LoadName {
                dst: "0".to_string(),
                name: LowerBinding::LocalSlot(0),
            },
            LowerInstruction::Return(Some(LowerValue::Register("0".to_string()))),
        ];
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![
                LowerInstruction::Function {
                    name: "first".to_string(),
                    params: vec![LowerBinding::LocalSlot(1)],
                    body: function_body.clone(),
                },
                LowerInstruction::Function {
                    name: "second".to_string(),
                    params: vec![LowerBinding::LocalSlot(1)],
                    body: function_body,
                },
            ],
        );

        let text = bytecode.to_text();
        assert!(
            !bytecode.names.contains(&"input".to_string()),
            "{:?}",
            bytecode.names
        );
        assert!(
            !bytecode.names.contains(&"value".to_string()),
            "{:?}",
            bytecode.names
        );
        assert!(text.contains("params:[local#1]"), "{text}");
        assert!(text.contains("LOAD_NAME r0, local#0"), "{text}");
    }

    #[test]
    fn function_params_do_not_resolve_to_extern_slots() {
        let bytecode = bytecode_from_lower(
            vec!["window".to_string()],
            vec![LowerInstruction::FunctionExpr {
                dst: "0".to_string(),
                name: None,
                params: vec![LowerBinding::Name("window".to_string())],
                body: vec![
                    LowerInstruction::LoadName {
                        dst: "1".to_string(),
                        name: LowerBinding::Name("window".to_string()),
                    },
                    LowerInstruction::Return(Some(LowerValue::Register("1".to_string()))),
                ],
            }],
        );

        let text = bytecode.to_text();
        assert_eq!(
            bytecode.functions[0].params,
            vec![BytecodeOperand::LocalSlot(0)]
        );
        assert!(text.contains("params:[local#0]"), "{text}");
        assert!(!text.contains("params:[extern#0(\"window\")]"), "{text}");
        assert!(text.contains("LOAD_NAME r0, local#0"), "{text}");
    }

    #[test]
    fn unreferenced_labels_are_not_encoded() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![
                LowerInstruction::Label("unused".to_string()),
                LowerInstruction::Jump("used".to_string()),
                LowerInstruction::Return(Some(LowerValue::Undefined)),
                LowerInstruction::Label("used".to_string()),
                LowerInstruction::Return(Some(LowerValue::Undefined)),
            ],
        );

        let label_count = bytecode
            .instructions
            .iter()
            .filter(|instruction| instruction.op == BytecodeOp::Label)
            .count();

        assert_eq!(label_count, 0, "{:?}", bytecode.instructions);
    }

    #[test]
    fn jumps_to_next_label_are_not_encoded() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![
                LowerInstruction::Jump("next".to_string()),
                LowerInstruction::Label("next".to_string()),
                LowerInstruction::Return(Some(LowerValue::Undefined)),
            ],
        );

        assert!(
            bytecode
                .instructions
                .iter()
                .all(|instruction| instruction.op != BytecodeOp::Jump),
            "{:?}",
            bytecode.instructions
        );
        assert!(
            bytecode
                .instructions
                .iter()
                .all(|instruction| instruction.op != BytecodeOp::Label),
            "{:?}",
            bytecode.instructions
        );
    }

    #[test]
    fn encodes_bytecode_with_yaml_config() {
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![LowerInstruction::LoadConst {
                dst: "t0".to_string(),
                value: LowerValue::Number(1.0),
            }],
        );
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
        let bytecode = bytecode_from_lower(
            Vec::new(),
            vec![LowerInstruction::LoadConst {
                dst: "t0".to_string(),
                value: LowerValue::Number(1.0),
            }],
        );
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

    fn encoded_u32_len(mut value: u32) -> usize {
        let mut len = 1;
        while value >= 0x80 {
            value >>= 7;
            len += 1;
        }
        len
    }
}
