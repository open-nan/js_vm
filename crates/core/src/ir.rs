//! JavaScript 降低后的中间表示（IR）结构。
//!
//! 这一层只描述 IR 的数据形状，不包含 AST lowering、字节码生成或运行时执行逻辑。
//! IR 采用“表 + 索引”的组织方式：模块保存常量、函数、类和 extern 表，函数内部再保存
//! local、scope、basic block、exception handler 等表。指令之间通过 `RegisterId` 传递临时值，
//! 控制流通过 `BlockId` 和 `IrTerminator` 串接。

use std::fmt;

/// 模块常量表下标。
///
/// 指向 `IrModule::constants` 中的一项，通常用于字符串、数字、BigInt 和正则字面量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConstId(pub usize);

/// 外部名字槽下标。
///
/// 指向 `IrModule::extern_slots`。未在当前模块声明、需要由宿主或全局环境解析的名字会放在这里。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternId(pub usize);

/// 函数表下标。
///
/// 指向 `IrModule::functions`。函数声明、函数表达式、箭头函数、类方法都会引用这个 ID。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionId(pub usize);

/// 类表下标。
///
/// 指向 `IrModule::classes`。类表达式或类声明通过 `CreateClass` 把它实例化为运行时值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClassId(pub usize);

/// 函数局部绑定下标。
///
/// 指向当前 `IrFunction::locals`，表示 `var`、`let`、`const`、参数、catch 参数或内部临时绑定。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub usize);

/// 函数内寄存器下标。
///
/// 寄存器只表示当前函数里的临时 SSA-like 值，不直接等同于 JavaScript 词法绑定。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegisterId(pub usize);

/// 函数内词法作用域下标。
///
/// 指向 `IrFunction::scopes`。local 通过 `IrLocal::scope` 归属到某个作用域。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub usize);

/// 函数内基本块下标。
///
/// 指向 `IrFunction::blocks`。所有跳转、分支、switch 和异常处理边都用它连接。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub usize);

/// 函数内异常处理器下标。
///
/// 指向 `IrFunction::exception_handlers`，用于 `try/catch/finally` 的结构化记录。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExceptionHandlerId(pub usize);

/// 兼容旧草稿命名：常量表达式下标。
pub type ConstExprIdx = ConstId;
/// 兼容旧草稿命名：字面绑定下标。
pub type LiteralIdx = LocalId;
/// 兼容旧草稿命名：函数表达式下标。
pub type FunctionIdx = FunctionId;
/// 兼容旧草稿命名：跳转目标下标。
pub type JumpIdx = BlockId;
/// IR 根结构别名。
pub type Ir = IrModule;

/// 源码区间。
///
/// `start` 和 `end` 使用字节偏移，半开区间 `[start, end)`。字段为 `u32` 是为了让 IR 更紧凑。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceSpan {
    /// 起始字节偏移。
    pub start: u32,
    /// 结束字节偏移。
    pub end: u32,
}

/// 一个 JavaScript 编译单元的 IR。
///
/// `IrModule` 是 IR 的根节点，类似 AST 文档里的顶层 `AST_Toplevel`，但它不直接嵌套语句树，
/// 而是通过函数表、类表和入口函数组织可执行代码。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IrModule {
    /// 编译单元类型：普通 script 或 ES module。
    pub kind: IrModuleKind,
    /// 可选源码名，用于调试、错误信息或 sourcemap 关联。
    pub source_name: Option<String>,
    /// 外部名字槽表。
    pub extern_slots: Vec<String>,
    /// 常量池。
    pub constants: Vec<IrConst>,
    /// 函数表。入口函数也在这里。
    pub functions: Vec<IrFunction>,
    /// 类表。类的成员函数通过 `FunctionId` 回指函数表。
    pub classes: Vec<IrClass>,
    /// 静态 import 声明。
    pub imports: Vec<IrImportDecl>,
    /// export 声明。
    pub exports: Vec<IrExportDecl>,
    /// 模块入口函数。
    pub entry: FunctionId,
}

/// JavaScript 编译单元类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrModuleKind {
    /// 普通脚本，顶层 `this` 和全局绑定遵循 script 语义。
    #[default]
    Script,
    /// ES module，默认 strict，包含静态 import/export 语义。
    Module,
}

/// 常量池元素。
///
/// 小型立即值如 `null`、`undefined`、`bool` 不放入常量池，直接由 `IrValue` 表达。
#[derive(Debug, Clone, PartialEq)]
pub enum IrConst {
    /// 字符串字面量。
    String(String),
    /// 可安全表示为整数的 number。
    Int(i64),
    /// 浮点 number，包含非整数、NaN、Infinity 等需要保留 IEEE 语义的值。
    Float(f64),
    /// BigInt 字面量文本，不带末尾 `n`。
    BigInt(String),
    /// 正则字面量。
    Regex {
        /// 正则 pattern。
        pattern: String,
        /// 正则 flags。
        flags: String,
    },
}

/// 函数级 IR。
///
/// JavaScript 顶层代码也表示成一个入口函数。每个函数内部由作用域表、local 表、寄存器数量、
/// 异常处理表和基本块表组成。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IrFunction {
    /// 函数名；匿名函数为 `None`。
    pub name: Option<String>,
    /// 函数种类。
    pub kind: IrFunctionKind,
    /// async/generator/strict 等函数标记。
    pub flags: IrFunctionFlags,
    /// 位置参数列表。
    pub params: Vec<IrParam>,
    /// rest 参数对应的 local。
    pub rest_param: Option<LocalId>,
    /// 当前函数的全部词法绑定。
    pub locals: Vec<IrLocal>,
    /// 当前函数的作用域树。
    pub scopes: Vec<IrScope>,
    /// 从外层函数捕获的绑定。
    pub captures: Vec<IrCapture>,
    /// 当前函数分配的寄存器数量。
    pub register_count: usize,
    /// try/catch/finally 处理器表。
    pub exception_handlers: Vec<IrExceptionHandler>,
    /// 基本块表。
    pub blocks: Vec<IrBlock>,
    /// 函数入口基本块。
    pub entry: BlockId,
}

/// 函数语义分类。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrFunctionKind {
    /// 普通 `function`。
    #[default]
    Normal,
    /// 箭头函数。没有自己的 `this`、`arguments`、`super` 和 `new.target`。
    Arrow,
    /// 对象或类方法。
    Method,
    /// 类构造器。
    Constructor,
    /// getter 方法。
    Getter,
    /// setter 方法。
    Setter,
}

/// 函数级布尔标记。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IrFunctionFlags {
    /// 是否为 async 函数。
    pub is_async: bool,
    /// 是否为 generator 函数。
    pub is_generator: bool,
    /// 是否处于 strict mode。
    pub is_strict: bool,
}

/// 位置参数描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrParam {
    /// 参数绑定对应的 local。
    pub local: LocalId,
    /// 默认值表达式降低后的寄存器；没有默认值时为 `None`。
    pub default: Option<RegisterId>,
}

/// 函数内词法绑定。
#[derive(Debug, Clone, PartialEq)]
pub struct IrLocal {
    /// 源码层名字。内部临时绑定可以没有名字。
    pub name: Option<String>,
    /// 绑定类型。
    pub kind: IrBindingKind,
    /// 所属作用域。
    pub scope: ScopeId,
    /// 是否可重新赋值。
    pub mutable: bool,
    /// 是否被内层函数捕获。
    pub captured: bool,
}

/// JavaScript 绑定类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrBindingKind {
    /// `var` 绑定，提升到函数或全局作用域。
    Var,
    /// `let` 绑定。
    Let,
    /// `const` 绑定。
    Const,
    /// 函数参数。
    Param,
    /// 函数声明绑定。
    Function,
    /// 类声明绑定。
    Class,
    /// catch 参数绑定。
    Catch,
    /// import 导入绑定。
    Import,
    /// 编译器内部绑定。
    #[default]
    Internal,
}

/// 词法作用域节点。
#[derive(Debug, Clone, PartialEq)]
pub struct IrScope {
    /// 父作用域；根作用域没有父节点。
    pub parent: Option<ScopeId>,
    /// 作用域类型。
    pub kind: IrScopeKind,
    /// 在这个作用域内声明的 local。
    pub bindings: Vec<LocalId>,
}

/// 作用域类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrScopeKind {
    /// script 全局作用域。
    Global,
    /// ES module 顶层作用域。
    Module,
    /// 函数作用域。
    Function,
    /// 块级作用域。
    #[default]
    Block,
    /// 循环头或循环体作用域。
    Loop,
    /// catch 子句作用域。
    Catch,
    /// class body 作用域。
    Class,
    /// `with` 动态对象作用域。
    With,
}

/// 闭包捕获项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrCapture {
    /// 被捕获的外层 local。
    pub source: LocalId,
    /// 捕获方式。
    pub mode: IrCaptureMode,
}

/// 闭包捕获方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrCaptureMode {
    /// 通过可变 cell 捕获，支持闭包内外共享写入。
    #[default]
    ByCell,
    /// 按值捕获，适合不会再变化的绑定。
    ByValue,
}

/// 基本块。
///
/// 一个基本块包含若干普通指令和一个终结符。普通指令顺序执行，终结符负责跳转、返回或抛出。
#[derive(Debug, Clone, PartialEq)]
pub struct IrBlock {
    /// 这个基本块默认所属的作用域。
    pub scope: Option<ScopeId>,
    /// 顺序执行的普通指令。
    pub instructions: Vec<IrInstruction>,
    /// 基本块终结符。
    pub terminator: IrTerminator,
}

impl Default for IrBlock {
    fn default() -> Self {
        Self {
            scope: None,
            instructions: Vec::new(),
            terminator: IrTerminator::Unreachable,
        }
    }
}

/// 一条带源码区间的 IR 指令。
#[derive(Debug, Clone, PartialEq)]
pub struct IrInstruction {
    /// 指令主体。
    pub kind: IrInstructionKind,
    /// 可选源码位置。
    pub span: Option<SourceSpan>,
}

impl IrInstruction {
    /// 创建一条没有源码位置的指令。
    pub fn new(kind: IrInstructionKind) -> Self {
        Self { kind, span: None }
    }

    /// 创建一条带源码位置的指令。
    pub fn with_span(kind: IrInstructionKind, span: SourceSpan) -> Self {
        Self {
            kind,
            span: Some(span),
        }
    }
}

/// 顺序执行的 IR 指令集合。
///
/// 这里不包含 `return`、`throw`、`jump` 等控制流终结动作；这些动作统一放在 `IrTerminator`。
#[derive(Debug, Clone, PartialEq)]
pub enum IrInstructionKind {
    /// 空操作，用于占位或调试阶段保留位置。
    Nop,
    /// 调试标记，不参与语义。
    Debug(String),
    /// 结构化区域内的局部标签。
    ///
    /// 函数级控制流应优先使用 `IrBlock` + `IrTerminator`；该指令用于 try/catch/finally
    /// 内部的短路表达式等需要保持连续 bytecode 区间的局部跳转。
    Label(String),
    /// 结构化区域内的局部无条件跳转。
    Jump(String),
    /// 结构化区域内的局部条件跳转，条件为 false 时跳转。
    JumpIfFalse {
        /// 条件值。
        test: IrValue,
        /// 目标标签。
        label: String,
    },
    /// 声明一个 local，并可携带初始化值。
    Declare(IrDeclaration),
    /// 把一个 IR 值移动到寄存器。
    Move {
        /// 目标寄存器。
        dst: RegisterId,
        /// 源值。
        src: IrValue,
    },
    /// 从可读取位置加载到寄存器。
    Load {
        /// 目标寄存器。
        dst: RegisterId,
        /// 读取位置。
        src: IrPlace,
    },
    /// 向可写位置存储值。
    Store {
        /// 写入位置。
        dst: IrPlace,
        /// 赋值操作类型。
        op: IrAssignOp,
        /// 右侧值。
        src: IrValue,
    },
    /// 前置或后置自增自减。
    Update {
        /// 表达式结果寄存器；语句上下文可为 `None`。
        dst: Option<RegisterId>,
        /// 被更新的位置。
        place: IrPlace,
        /// 更新形式。
        op: IrUpdateOp,
    },
    /// 一元运算。
    Unary {
        /// 目标寄存器。
        dst: RegisterId,
        /// 一元运算符。
        op: IrUnaryOp,
        /// 操作数。
        arg: IrValue,
    },
    /// 二元运算。
    Binary {
        /// 目标寄存器。
        dst: RegisterId,
        /// 二元运算符。
        op: IrBinaryOp,
        /// 左操作数。
        left: IrValue,
        /// 右操作数。
        right: IrValue,
    },
    /// `delete` 运算。
    ///
    /// `delete` 对引用位置有特殊语义，因此不作为普通 `IrUnaryOp`。
    Delete {
        /// 结果寄存器，保存 boolean 结果。
        dst: RegisterId,
        /// 删除目标位置。
        target: IrPlace,
    },
    /// 在当前顺序流中抛出异常。
    ///
    /// 基本块末尾的无条件抛出仍应使用 `IrTerminator::Throw`；这个形态用于 `try` body
    /// 这类需要和 catch/finally 标记共存在同一个结构化区域里的抛出点。
    Throw(IrValue),
    /// 在当前顺序流中返回。
    ///
    /// 普通基本块末尾返回仍应使用 `IrTerminator::Return`；这个形态用于 try/catch/finally
    /// 这类结构化区域内部需要保留顺序 opcode 的返回点。
    Return(Option<IrValue>),
    /// 创建数组。
    CreateArray {
        /// 目标寄存器。
        dst: RegisterId,
        /// 数组元素。
        elements: Vec<IrArrayElement>,
    },
    /// 创建对象字面量。
    CreateObject {
        /// 目标寄存器。
        dst: RegisterId,
        /// 对象属性列表。
        properties: Vec<IrObjectProperty>,
    },
    /// 创建函数对象。
    CreateFunction {
        /// 目标寄存器。
        dst: RegisterId,
        /// 函数表下标。
        function: FunctionId,
        /// 创建闭包时注入的捕获值。
        captures: Vec<IrValue>,
    },
    /// 声明一个命名函数。
    ///
    /// 用于 `function f() {}` 这种需要在当前作用域绑定函数名，并允许函数体自引用的声明形态。
    FunctionDeclaration {
        /// 函数表下标。
        function: FunctionId,
    },
    /// 创建类对象。
    CreateClass {
        /// 目标寄存器。
        dst: RegisterId,
        /// 类表下标。
        class: ClassId,
    },
    /// 函数调用。
    Call(IrCall),
    /// `new` 构造调用。
    Construct(IrConstruct),
    /// 模板字符串或 tagged template 数据。
    Template(IrTemplate),
    /// `await` 表达式。
    Await {
        /// 目标寄存器。
        dst: RegisterId,
        /// 被等待的值。
        value: IrValue,
    },
    /// `yield` 或 `yield*` 表达式。
    Yield {
        /// 恢复后接收值的寄存器；纯语句上下文可为 `None`。
        dst: Option<RegisterId>,
        /// yield 出去的值；裸 `yield` 为 `None`。
        value: Option<IrValue>,
        /// 是否为 `yield*`。
        delegate: bool,
    },
    /// 进入普通词法作用域。
    EnterScope(ScopeId),
    /// 进入 `with` 作用域。
    EnterWith {
        /// 动态作用域 ID。
        scope: ScopeId,
        /// `with` 对象。
        object: IrValue,
    },
    /// 离开词法作用域。
    LeaveScope(ScopeId),
    /// 进入 try 保护区。
    EnterTry(ExceptionHandlerId),
    /// 进入 catch 子句。
    EnterCatch {
        /// catch 参数 local。
        param: Option<LocalId>,
    },
    /// 进入 finally 子句。
    EnterFinally,
    /// 离开 try 保护区。
    LeaveTry(ExceptionHandlerId),
    /// 暂不支持的语法或语义节点。
    Unsupported(String),
}

/// 声明指令载荷。
#[derive(Debug, Clone, PartialEq)]
pub struct IrDeclaration {
    /// 被声明的 local。
    pub local: LocalId,
    /// 绑定类型。
    pub kind: IrBindingKind,
    /// 源码层名字。
    pub name: Option<String>,
    /// 可选初始化值。
    pub init: Option<IrValue>,
}

/// 可作为表达式输入的值。
#[derive(Debug, Clone, PartialEq)]
pub enum IrValue {
    /// JavaScript `undefined`。
    Undefined,
    /// JavaScript `null`。
    Null,
    /// boolean 立即值。
    Bool(bool),
    /// 常量池引用。
    Const(ConstId),
    /// local 当前值。
    Local(LocalId),
    /// 寄存器临时值。
    Register(RegisterId),
    /// 函数表引用。
    Function(FunctionId),
    /// 类表引用。
    Class(ClassId),
    /// 外部名字槽引用。
    External(ExternId),
    /// 当前 `this`。
    This,
    /// 当前 `super`。
    Super,
    /// 当前 `new.target`。
    NewTarget,
    /// ES module 的 `import.meta`。
    ImportMeta,
}

impl Default for IrValue {
    fn default() -> Self {
        Self::Undefined
    }
}

/// 可读写的位置。
///
/// `IrValue` 表示值，`IrPlace` 表示引用位置。赋值、更新、delete 等需要引用语义的指令使用它。
#[derive(Debug, Clone, PartialEq)]
pub enum IrPlace {
    /// local 绑定位置。
    Local(LocalId),
    /// extern/global 位置。
    External(ExternId),
    /// 对象成员位置。
    Member(IrMember),
    /// `super.foo` 或 `super[foo]` 位置。
    SuperMember(IrPropertyKey),
}

/// 对象成员引用。
#[derive(Debug, Clone, PartialEq)]
pub struct IrMember {
    /// 成员所属对象。
    pub object: IrValue,
    /// 属性键。
    pub property: IrPropertyKey,
    /// 是否来自 optional chaining。
    pub optional: bool,
}

/// JavaScript 属性键。
#[derive(Debug, Clone, PartialEq)]
pub enum IrPropertyKey {
    /// 静态字符串属性，如 `.foo` 或 `{ foo: 1 }`。
    Static(String),
    /// 数字属性键，如数组索引或数字字面属性。
    Number(f64),
    /// 运行期计算属性，如 `[expr]`。
    Computed(IrValue),
    /// 私有字段或方法名，如 `#x`。
    Private(String),
}

/// 赋值运算符。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrAssignOp {
    /// `=`。
    #[default]
    Assign,
    /// `+=`。
    Add,
    /// `-=`。
    Sub,
    /// `*=`。
    Mul,
    /// `/=`。
    Div,
    /// `%=`。
    Mod,
    /// `**=`。
    Pow,
    /// `<<=`。
    Shl,
    /// `>>=`。
    Shr,
    /// `>>>=`。
    UShr,
    /// `|=`。
    BitOr,
    /// `^=`。
    BitXor,
    /// `&=`。
    BitAnd,
    /// `||=`。
    LogicalOr,
    /// `&&=`。
    LogicalAnd,
    /// `??=`。
    Nullish,
}

/// 自增自减运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrUpdateOp {
    /// `++x`。
    PreIncrement,
    /// `--x`。
    PreDecrement,
    /// `x++`。
    PostIncrement,
    /// `x--`。
    PostDecrement,
}

/// 一元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrUnaryOp {
    /// 一元 `+`。
    Plus,
    /// 一元 `-`。
    Minus,
    /// 逻辑非 `!`。
    Not,
    /// 按位非 `~`。
    BitNot,
    /// `typeof`。
    TypeOf,
    /// `void`。
    Void,
    /// `delete`。
    Delete,
}

/// 二元或短路运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrBinaryOp {
    /// `+`。
    Add,
    /// `-`。
    Sub,
    /// `*`。
    Mul,
    /// `/`。
    Div,
    /// `%`。
    Mod,
    /// `**`。
    Pow,
    /// `==`。
    Eq,
    /// `===`。
    StrictEq,
    /// `!=`。
    NotEq,
    /// `!==`。
    StrictNotEq,
    /// `<`。
    Lt,
    /// `<=`。
    Le,
    /// `>`。
    Gt,
    /// `>=`。
    Ge,
    /// `&&`。
    LogicalAnd,
    /// `||`。
    LogicalOr,
    /// `??`。
    Nullish,
    /// `&`。
    BitAnd,
    /// `|`。
    BitOr,
    /// `^`。
    BitXor,
    /// `<<`。
    Shl,
    /// `>>`。
    Shr,
    /// `>>>`。
    UShr,
    /// `in`。
    In,
    /// `instanceof`。
    InstanceOf,
}

/// 数组字面量元素。
#[derive(Debug, Clone, PartialEq)]
pub enum IrArrayElement {
    /// 普通元素值。
    Value(IrValue),
    /// spread 元素，如 `[...x]`。
    Spread(IrValue),
    /// 稀疏数组空洞，如 `[,,]`。
    Hole,
}

/// 对象字面量属性。
#[derive(Debug, Clone, PartialEq)]
pub enum IrObjectProperty {
    /// 数据属性。
    Data {
        /// 属性键。
        key: IrPropertyKey,
        /// 属性值。
        value: IrValue,
    },
    /// 方法属性。
    Method {
        /// 属性键。
        key: IrPropertyKey,
        /// 方法函数。
        function: FunctionId,
    },
    /// getter 属性。
    Getter {
        /// 属性键。
        key: IrPropertyKey,
        /// getter 函数。
        function: FunctionId,
    },
    /// setter 属性。
    Setter {
        /// 属性键。
        key: IrPropertyKey,
        /// setter 函数。
        function: FunctionId,
    },
    /// 对象 spread，如 `{ ...x }`。
    Spread(IrValue),
}

/// 函数调用指令载荷。
#[derive(Debug, Clone, PartialEq)]
pub struct IrCall {
    /// 调用结果寄存器。表达式结果被丢弃时可以为 `None`。
    pub dst: Option<RegisterId>,
    /// 调用类型。
    pub kind: IrCallKind,
    /// 被调用值。
    pub callee: IrValue,
    /// 显式 this 值；普通函数直接调用时可为 `None`。
    pub this_arg: Option<IrValue>,
    /// 实参列表。
    pub args: Vec<IrArgument>,
}

/// 调用类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IrCallKind {
    /// 普通调用。
    #[default]
    Normal,
    /// optional chaining 调用。
    Optional,
    /// 直接 `eval` 调用。
    Eval,
    /// `super(...)` 调用。
    Super,
    /// 动态 `import(...)`。
    Import,
}

/// 构造调用指令载荷。
#[derive(Debug, Clone, PartialEq)]
pub struct IrConstruct {
    /// 构造结果寄存器。
    pub dst: RegisterId,
    /// 构造器值。
    pub callee: IrValue,
    /// 构造实参。
    pub args: Vec<IrArgument>,
}

/// 调用实参。
#[derive(Debug, Clone, PartialEq)]
pub enum IrArgument {
    /// 普通实参。
    Value(IrValue),
    /// spread 实参。
    Spread(IrValue),
}

/// 模板字符串数据。
#[derive(Debug, Clone, PartialEq)]
pub struct IrTemplate {
    /// 结果寄存器。
    pub dst: RegisterId,
    /// cooked 字符串段；非法 escape 对应 `None`。
    pub cooked: Vec<Option<String>>,
    /// raw 字符串段。
    pub raw: Vec<String>,
    /// 插值表达式值。
    pub expressions: Vec<IrValue>,
}

/// 基本块终结符。
#[derive(Debug, Clone, PartialEq)]
pub enum IrTerminator {
    /// 无条件跳转。
    Jump(BlockId),
    /// 条件分支。
    Branch {
        /// 条件值。
        test: IrValue,
        /// truthy 分支。
        truthy: BlockId,
        /// falsy 分支。
        falsy: BlockId,
    },
    /// switch 分发。
    Switch {
        /// 被匹配值。
        discriminant: IrValue,
        /// case 列表。
        cases: Vec<IrSwitchCase>,
        /// default 目标；没有显式 default 时也可指向 switch 后续块。
        default: BlockId,
    },
    /// 函数返回。
    Return(Option<IrValue>),
    /// 抛出异常。
    Throw(IrValue),
    /// 在 catch/finally 处理中重新抛出当前异常。
    Rethrow,
    /// 不可达终结符。
    Unreachable,
}

/// switch case 目标。
#[derive(Debug, Clone, PartialEq)]
pub struct IrSwitchCase {
    /// case 测试值；`None` 表示 default。
    pub test: Option<IrValue>,
    /// case 入口基本块。
    pub target: BlockId,
}

/// try/catch/finally 结构描述。
#[derive(Debug, Clone, PartialEq)]
pub struct IrExceptionHandler {
    /// 受该 handler 保护的基本块集合。
    pub protected_blocks: Vec<BlockId>,
    /// catch 参数 local。
    pub catch_param: Option<LocalId>,
    /// catch 入口块。
    pub catch_block: Option<BlockId>,
    /// finally 入口块。
    pub finally_block: Option<BlockId>,
    /// 异常结构正常结束后的汇合块。
    pub exit_block: Option<BlockId>,
}

/// 类定义数据。
#[derive(Debug, Clone, PartialEq)]
pub struct IrClass {
    /// 类名；匿名类为 `None`。
    pub name: Option<String>,
    /// 父类表达式值。
    pub super_class: Option<IrValue>,
    /// 构造器函数。
    pub constructor: Option<FunctionId>,
    /// 类成员列表。
    pub members: Vec<IrClassMember>,
    /// static block 对应的函数列表。
    pub static_blocks: Vec<FunctionId>,
}

/// 类成员。
#[derive(Debug, Clone, PartialEq)]
pub struct IrClassMember {
    /// 成员名。
    pub key: IrPropertyKey,
    /// 成员类型。
    pub kind: IrClassMemberKind,
    /// 是否为 static 成员。
    pub is_static: bool,
}

/// 类成员类型。
#[derive(Debug, Clone, PartialEq)]
pub enum IrClassMemberKind {
    /// class field。
    Field {
        /// 字段初始化值。
        value: Option<IrValue>,
    },
    /// 方法。
    Method {
        /// 方法函数。
        function: FunctionId,
    },
    /// getter。
    Getter {
        /// getter 函数。
        function: FunctionId,
    },
    /// setter。
    Setter {
        /// setter 函数。
        function: FunctionId,
    },
    /// auto accessor。
    AutoAccessor {
        /// getter 函数。
        getter: Option<FunctionId>,
        /// setter 函数。
        setter: Option<FunctionId>,
        /// 初始值。
        value: Option<IrValue>,
    },
}

/// 静态 import 声明。
#[derive(Debug, Clone, PartialEq)]
pub struct IrImportDecl {
    /// 模块来源字符串。
    pub source: String,
    /// import specifier 列表。
    pub specifiers: Vec<IrImportSpecifier>,
}

/// import specifier。
#[derive(Debug, Clone, PartialEq)]
pub enum IrImportSpecifier {
    /// `import x from "m"`。
    Default {
        /// 本地绑定。
        local: LocalId,
    },
    /// `import * as ns from "m"`。
    Namespace {
        /// 本地绑定。
        local: LocalId,
    },
    /// `import { a as b } from "m"`。
    Named {
        /// 导入名。
        imported: String,
        /// 本地绑定。
        local: LocalId,
    },
}

/// export 声明。
#[derive(Debug, Clone, PartialEq)]
pub enum IrExportDecl {
    /// 导出本地绑定。
    Local {
        /// 本地绑定。
        local: LocalId,
        /// 导出名。
        exported: String,
    },
    /// default export。
    Default {
        /// 默认导出的值。
        value: IrValue,
    },
    /// 从另一个模块转导命名导出。
    ReExport {
        /// 来源模块。
        source: String,
        /// 被导入名。
        imported: String,
        /// 导出名。
        exported: String,
    },
    /// `export * from "m"` 或 `export * as ns from "m"`。
    ExportAll {
        /// 来源模块。
        source: String,
        /// 命名空间导出名；裸 `export *` 为 `None`。
        exported: Option<String>,
    },
}

macro_rules! display_id {
    ($ty:ty, $prefix:literal) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
    };
}

display_id!(ConstId, "const#");
display_id!(ExternId, "extern#");
display_id!(FunctionId, "fn#");
display_id!(ClassId, "class#");
display_id!(LocalId, "local#");
display_id!(RegisterId, "r");
display_id!(ScopeId, "scope#");
display_id!(BlockId, "block#");
display_id!(ExceptionHandlerId, "handler#");

impl fmt::Display for SourceSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl fmt::Display for IrModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, ".module kind={} entry={}", self.kind, self.entry)?;
        if let Some(source_name) = &self.source_name {
            writeln!(f, "  .source {source_name:?}")?;
        }

        if !self.extern_slots.is_empty() {
            writeln!(f, "  .externs")?;
            for (index, name) in self.extern_slots.iter().enumerate() {
                writeln!(f, "    {} = {name}", ExternId(index))?;
            }
        }

        if !self.constants.is_empty() {
            writeln!(f, "  .consts")?;
            for (index, constant) in self.constants.iter().enumerate() {
                writeln!(f, "    {} = {constant}", ConstId(index))?;
            }
        }

        if !self.imports.is_empty() {
            writeln!(f, "  .imports")?;
            for import in &self.imports {
                writeln!(f, "    {import}")?;
            }
        }

        if !self.exports.is_empty() {
            writeln!(f, "  .exports")?;
            for export in &self.exports {
                writeln!(f, "    {export}")?;
            }
        }

        if !self.classes.is_empty() {
            writeln!(f, "  .classes")?;
            for (index, class) in self.classes.iter().enumerate() {
                class.fmt_with_id(f, ClassId(index), 2)?;
            }
        }

        if !self.functions.is_empty() {
            writeln!(f, "  .functions")?;
            for (index, function) in self.functions.iter().enumerate() {
                function.fmt_with_id(f, FunctionId(index), 2)?;
            }
        }

        Ok(())
    }
}

impl fmt::Display for IrModuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrModuleKind::Script => f.write_str("script"),
            IrModuleKind::Module => f.write_str("module"),
        }
    }
}

impl fmt::Display for IrConst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrConst::String(value) => write!(f, "{value:?}"),
            IrConst::Int(value) => write!(f, "{value}"),
            IrConst::Float(value) => write!(f, "{value}"),
            IrConst::BigInt(value) => write!(f, "{value}n"),
            IrConst::Regex { pattern, flags } => write!(f, "/{pattern}/{flags}"),
        }
    }
}

impl IrFunction {
    fn fmt_with_id(
        &self,
        f: &mut fmt::Formatter<'_>,
        id: FunctionId,
        indent: usize,
    ) -> fmt::Result {
        write_indent(f, indent)?;
        writeln!(
            f,
            ".function {id} name={} kind={} flags={} entry={} registers={}",
            display_optional_name(self.name.as_deref()),
            self.kind,
            self.flags,
            self.entry,
            self.register_count
        )?;

        if !self.params.is_empty() || self.rest_param.is_some() {
            write_indent(f, indent + 1)?;
            f.write_str(".params ")?;
            write_display_list(f, &self.params)?;
            if let Some(rest_param) = self.rest_param {
                if !self.params.is_empty() {
                    f.write_str(", ")?;
                }
                write!(f, "...{rest_param}")?;
            }
            f.write_str("\n")?;
        }

        if !self.locals.is_empty() {
            write_indent(f, indent + 1)?;
            writeln!(f, ".locals")?;
            for (index, local) in self.locals.iter().enumerate() {
                write_indent(f, indent + 2)?;
                writeln!(f, "{} = {local}", LocalId(index))?;
            }
        }

        if !self.scopes.is_empty() {
            write_indent(f, indent + 1)?;
            writeln!(f, ".scopes")?;
            for (index, scope) in self.scopes.iter().enumerate() {
                write_indent(f, indent + 2)?;
                writeln!(f, "{} = {scope}", ScopeId(index))?;
            }
        }

        if !self.captures.is_empty() {
            write_indent(f, indent + 1)?;
            f.write_str(".captures ")?;
            write_display_list(f, &self.captures)?;
            f.write_str("\n")?;
        }

        if !self.exception_handlers.is_empty() {
            write_indent(f, indent + 1)?;
            writeln!(f, ".handlers")?;
            for (index, handler) in self.exception_handlers.iter().enumerate() {
                write_indent(f, indent + 2)?;
                writeln!(f, "{} = {handler}", ExceptionHandlerId(index))?;
            }
        }

        write_indent(f, indent + 1)?;
        writeln!(f, ".blocks")?;
        for (index, block) in self.blocks.iter().enumerate() {
            block.fmt_with_id(f, BlockId(index), indent + 2)?;
        }

        Ok(())
    }
}

impl fmt::Display for IrFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_id(f, FunctionId(0), 0)
    }
}

impl fmt::Display for IrFunctionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrFunctionKind::Normal => f.write_str("normal"),
            IrFunctionKind::Arrow => f.write_str("arrow"),
            IrFunctionKind::Method => f.write_str("method"),
            IrFunctionKind::Constructor => f.write_str("constructor"),
            IrFunctionKind::Getter => f.write_str("getter"),
            IrFunctionKind::Setter => f.write_str("setter"),
        }
    }
}

impl fmt::Display for IrFunctionFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut wrote = false;
        f.write_str("[")?;
        if self.is_async {
            f.write_str("async")?;
            wrote = true;
        }
        if self.is_generator {
            if wrote {
                f.write_str(",")?;
            }
            f.write_str("generator")?;
            wrote = true;
        }
        if self.is_strict {
            if wrote {
                f.write_str(",")?;
            }
            f.write_str("strict")?;
            wrote = true;
        }
        if !wrote {
            f.write_str("none")?;
        }
        f.write_str("]")
    }
}

impl fmt::Display for IrParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.local)?;
        if let Some(default) = self.default {
            write!(f, "={default}")?;
        }
        Ok(())
    }
}

impl fmt::Display for IrLocal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "name={} kind={} scope={} mutable={} captured={}",
            display_optional_name(self.name.as_deref()),
            self.kind,
            self.scope,
            self.mutable,
            self.captured
        )
    }
}

impl fmt::Display for IrBindingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrBindingKind::Var => f.write_str("var"),
            IrBindingKind::Let => f.write_str("let"),
            IrBindingKind::Const => f.write_str("const"),
            IrBindingKind::Param => f.write_str("param"),
            IrBindingKind::Function => f.write_str("function"),
            IrBindingKind::Class => f.write_str("class"),
            IrBindingKind::Catch => f.write_str("catch"),
            IrBindingKind::Import => f.write_str("import"),
            IrBindingKind::Internal => f.write_str("internal"),
        }
    }
}

impl fmt::Display for IrScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kind={} parent=", self.kind)?;
        match self.parent {
            Some(parent) => write!(f, "{parent}")?,
            None => f.write_str("none")?,
        }
        f.write_str(" bindings=[")?;
        write_display_list(f, &self.bindings)?;
        f.write_str("]")
    }
}

impl fmt::Display for IrScopeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrScopeKind::Global => f.write_str("global"),
            IrScopeKind::Module => f.write_str("module"),
            IrScopeKind::Function => f.write_str("function"),
            IrScopeKind::Block => f.write_str("block"),
            IrScopeKind::Loop => f.write_str("loop"),
            IrScopeKind::Catch => f.write_str("catch"),
            IrScopeKind::Class => f.write_str("class"),
            IrScopeKind::With => f.write_str("with"),
        }
    }
}

impl fmt::Display for IrCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.mode, self.source)
    }
}

impl fmt::Display for IrCaptureMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrCaptureMode::ByCell => f.write_str("cell"),
            IrCaptureMode::ByValue => f.write_str("value"),
        }
    }
}

impl IrBlock {
    fn fmt_with_id(&self, f: &mut fmt::Formatter<'_>, id: BlockId, indent: usize) -> fmt::Result {
        write_indent(f, indent)?;
        write!(f, "{id}")?;
        if let Some(scope) = self.scope {
            write!(f, " scope={scope}")?;
        }
        writeln!(f, ":")?;

        for instruction in &self.instructions {
            write_indent(f, indent + 1)?;
            writeln!(f, "{instruction}")?;
        }

        write_indent(f, indent + 1)?;
        writeln!(f, "{}", self.terminator)
    }
}

impl fmt::Display for IrBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_id(f, BlockId(0), 0)
    }
}

impl fmt::Display for IrInstruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(span) = self.span {
            write!(f, " @{span}")?;
        }
        Ok(())
    }
}

impl fmt::Display for IrInstructionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrInstructionKind::Nop => f.write_str("nop"),
            IrInstructionKind::Debug(message) => write!(f, "debug {message:?}"),
            IrInstructionKind::Label(label) => write!(f, "label {label}"),
            IrInstructionKind::Jump(label) => write!(f, "jump {label}"),
            IrInstructionKind::JumpIfFalse { test, label } => {
                write!(f, "jump_if_false {test}, {label}")
            }
            IrInstructionKind::Declare(declaration) => write!(f, "declare {declaration}"),
            IrInstructionKind::Move { dst, src } => write!(f, "{dst} = move {src}"),
            IrInstructionKind::Load { dst, src } => write!(f, "{dst} = load {src}"),
            IrInstructionKind::Store { dst, op, src } => write!(f, "store {dst} {op} {src}"),
            IrInstructionKind::Update { dst, place, op } => match dst {
                Some(dst) => write!(f, "{dst} = update {op} {place}"),
                None => write!(f, "update {op} {place}"),
            },
            IrInstructionKind::Unary { dst, op, arg } => write!(f, "{dst} = unary {op} {arg}"),
            IrInstructionKind::Binary {
                dst,
                op,
                left,
                right,
            } => write!(f, "{dst} = binary {op} {left}, {right}"),
            IrInstructionKind::Delete { dst, target } => write!(f, "{dst} = delete {target}"),
            IrInstructionKind::Throw(value) => write!(f, "throw {value}"),
            IrInstructionKind::Return(value) => {
                f.write_str("return")?;
                if let Some(value) = value {
                    write!(f, " {value}")?;
                }
                Ok(())
            }
            IrInstructionKind::CreateArray { dst, elements } => {
                write!(f, "{dst} = array [")?;
                write_display_list(f, elements)?;
                f.write_str("]")
            }
            IrInstructionKind::CreateObject { dst, properties } => {
                write!(f, "{dst} = object {{")?;
                write_display_list(f, properties)?;
                f.write_str("}")
            }
            IrInstructionKind::CreateFunction {
                dst,
                function,
                captures,
            } => {
                write!(f, "{dst} = function {function} captures=[")?;
                write_display_list(f, captures)?;
                f.write_str("]")
            }
            IrInstructionKind::FunctionDeclaration { function } => {
                write!(f, "function_declaration {function}")
            }
            IrInstructionKind::CreateClass { dst, class } => write!(f, "{dst} = class {class}"),
            IrInstructionKind::Call(call) => write!(f, "{call}"),
            IrInstructionKind::Construct(construct) => write!(f, "{construct}"),
            IrInstructionKind::Template(template) => write!(f, "{template}"),
            IrInstructionKind::Await { dst, value } => write!(f, "{dst} = await {value}"),
            IrInstructionKind::Yield {
                dst,
                value,
                delegate,
            } => {
                if let Some(dst) = dst {
                    write!(f, "{dst} = ")?;
                }
                if *delegate {
                    f.write_str("yield*")?;
                } else {
                    f.write_str("yield")?;
                }
                if let Some(value) = value {
                    write!(f, " {value}")?;
                }
                Ok(())
            }
            IrInstructionKind::EnterScope(scope) => write!(f, "enter_scope {scope}"),
            IrInstructionKind::EnterWith { scope, object } => {
                write!(f, "enter_with {scope}, {object}")
            }
            IrInstructionKind::LeaveScope(scope) => write!(f, "leave_scope {scope}"),
            IrInstructionKind::EnterTry(handler) => write!(f, "enter_try {handler}"),
            IrInstructionKind::EnterCatch { param } => {
                f.write_str("enter_catch ")?;
                write_optional_display(f, *param)
            }
            IrInstructionKind::EnterFinally => f.write_str("enter_finally"),
            IrInstructionKind::LeaveTry(handler) => write!(f, "leave_try {handler}"),
            IrInstructionKind::Unsupported(message) => write!(f, "unsupported {message:?}"),
        }
    }
}

impl fmt::Display for IrDeclaration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} name={}",
            self.kind,
            self.local,
            display_optional_name(self.name.as_deref())
        )?;
        if let Some(init) = &self.init {
            write!(f, " init={init}")?;
        }
        Ok(())
    }
}

impl fmt::Display for IrValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrValue::Undefined => f.write_str("undefined"),
            IrValue::Null => f.write_str("null"),
            IrValue::Bool(value) => write!(f, "{value}"),
            IrValue::Const(value) => write!(f, "{value}"),
            IrValue::Local(value) => write!(f, "{value}"),
            IrValue::Register(value) => write!(f, "{value}"),
            IrValue::Function(value) => write!(f, "{value}"),
            IrValue::Class(value) => write!(f, "{value}"),
            IrValue::External(value) => write!(f, "{value}"),
            IrValue::This => f.write_str("this"),
            IrValue::Super => f.write_str("super"),
            IrValue::NewTarget => f.write_str("new.target"),
            IrValue::ImportMeta => f.write_str("import.meta"),
        }
    }
}

impl fmt::Display for IrPlace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrPlace::Local(local) => write!(f, "{local}"),
            IrPlace::External(external) => write!(f, "{external}"),
            IrPlace::Member(member) => write!(f, "{member}"),
            IrPlace::SuperMember(property) => write!(f, "super.{property}"),
        }
    }
}

impl fmt::Display for IrMember {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.optional {
            write!(f, "{}?.{}", self.object, self.property)
        } else {
            write!(f, "{}.{}", self.object, self.property)
        }
    }
}

impl fmt::Display for IrPropertyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrPropertyKey::Static(value) => write!(f, "{value}"),
            IrPropertyKey::Number(value) => write!(f, "{value}"),
            IrPropertyKey::Computed(value) => write!(f, "[{value}]"),
            IrPropertyKey::Private(value) => write!(f, "#{value}"),
        }
    }
}

impl fmt::Display for IrAssignOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IrAssignOp::Assign => "=",
            IrAssignOp::Add => "+=",
            IrAssignOp::Sub => "-=",
            IrAssignOp::Mul => "*=",
            IrAssignOp::Div => "/=",
            IrAssignOp::Mod => "%=",
            IrAssignOp::Pow => "**=",
            IrAssignOp::Shl => "<<=",
            IrAssignOp::Shr => ">>=",
            IrAssignOp::UShr => ">>>=",
            IrAssignOp::BitOr => "|=",
            IrAssignOp::BitXor => "^=",
            IrAssignOp::BitAnd => "&=",
            IrAssignOp::LogicalOr => "||=",
            IrAssignOp::LogicalAnd => "&&=",
            IrAssignOp::Nullish => "??=",
        })
    }
}

impl fmt::Display for IrUpdateOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IrUpdateOp::PreIncrement => "pre++",
            IrUpdateOp::PreDecrement => "pre--",
            IrUpdateOp::PostIncrement => "post++",
            IrUpdateOp::PostDecrement => "post--",
        })
    }
}

impl fmt::Display for IrUnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IrUnaryOp::Plus => "+",
            IrUnaryOp::Minus => "-",
            IrUnaryOp::Not => "!",
            IrUnaryOp::BitNot => "~",
            IrUnaryOp::TypeOf => "typeof",
            IrUnaryOp::Void => "void",
            IrUnaryOp::Delete => "delete",
        })
    }
}

impl fmt::Display for IrBinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IrBinaryOp::Add => "+",
            IrBinaryOp::Sub => "-",
            IrBinaryOp::Mul => "*",
            IrBinaryOp::Div => "/",
            IrBinaryOp::Mod => "%",
            IrBinaryOp::Pow => "**",
            IrBinaryOp::Eq => "==",
            IrBinaryOp::StrictEq => "===",
            IrBinaryOp::NotEq => "!=",
            IrBinaryOp::StrictNotEq => "!==",
            IrBinaryOp::Lt => "<",
            IrBinaryOp::Le => "<=",
            IrBinaryOp::Gt => ">",
            IrBinaryOp::Ge => ">=",
            IrBinaryOp::LogicalAnd => "&&",
            IrBinaryOp::LogicalOr => "||",
            IrBinaryOp::Nullish => "??",
            IrBinaryOp::BitAnd => "&",
            IrBinaryOp::BitOr => "|",
            IrBinaryOp::BitXor => "^",
            IrBinaryOp::Shl => "<<",
            IrBinaryOp::Shr => ">>",
            IrBinaryOp::UShr => ">>>",
            IrBinaryOp::In => "in",
            IrBinaryOp::InstanceOf => "instanceof",
        })
    }
}

impl fmt::Display for IrArrayElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrArrayElement::Value(value) => write!(f, "{value}"),
            IrArrayElement::Spread(value) => write!(f, "...{value}"),
            IrArrayElement::Hole => f.write_str("<hole>"),
        }
    }
}

impl fmt::Display for IrObjectProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrObjectProperty::Data { key, value } => write!(f, "{key}: {value}"),
            IrObjectProperty::Method { key, function } => write!(f, "{key}: method {function}"),
            IrObjectProperty::Getter { key, function } => write!(f, "get {key}: {function}"),
            IrObjectProperty::Setter { key, function } => write!(f, "set {key}: {function}"),
            IrObjectProperty::Spread(value) => write!(f, "...{value}"),
        }
    }
}

impl fmt::Display for IrCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(dst) = self.dst {
            write!(f, "{dst} = ")?;
        }
        write!(f, "call {} {}", self.kind, self.callee)?;
        if let Some(this_arg) = &self.this_arg {
            write!(f, " this={this_arg}")?;
        }
        f.write_str("(")?;
        write_display_list(f, &self.args)?;
        f.write_str(")")
    }
}

impl fmt::Display for IrCallKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrCallKind::Normal => f.write_str("normal"),
            IrCallKind::Optional => f.write_str("optional"),
            IrCallKind::Eval => f.write_str("eval"),
            IrCallKind::Super => f.write_str("super"),
            IrCallKind::Import => f.write_str("import"),
        }
    }
}

impl fmt::Display for IrConstruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = new {}(", self.dst, self.callee)?;
        write_display_list(f, &self.args)?;
        f.write_str(")")
    }
}

impl fmt::Display for IrArgument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrArgument::Value(value) => write!(f, "{value}"),
            IrArgument::Spread(value) => write!(f, "...{value}"),
        }
    }
}

impl fmt::Display for IrTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = template cooked=[", self.dst)?;
        write_optional_string_list(f, &self.cooked)?;
        f.write_str("] raw=[")?;
        write_debug_string_list(f, &self.raw)?;
        f.write_str("] exprs=[")?;
        write_display_list(f, &self.expressions)?;
        f.write_str("]")
    }
}

impl fmt::Display for IrTerminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrTerminator::Jump(target) => write!(f, "jump {target}"),
            IrTerminator::Branch {
                test,
                truthy,
                falsy,
            } => write!(f, "branch {test}, {truthy}, {falsy}"),
            IrTerminator::Switch {
                discriminant,
                cases,
                default,
            } => {
                write!(f, "switch {discriminant} [")?;
                write_display_list(f, cases)?;
                write!(f, "] default={default}")
            }
            IrTerminator::Return(Some(value)) => write!(f, "return {value}"),
            IrTerminator::Return(None) => f.write_str("return"),
            IrTerminator::Throw(value) => write!(f, "throw {value}"),
            IrTerminator::Rethrow => f.write_str("rethrow"),
            IrTerminator::Unreachable => f.write_str("unreachable"),
        }
    }
}

impl fmt::Display for IrSwitchCase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.test {
            Some(test) => write!(f, "case {test} => {}", self.target),
            None => write!(f, "default => {}", self.target),
        }
    }
}

impl fmt::Display for IrExceptionHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("protected=[")?;
        write_display_list(f, &self.protected_blocks)?;
        f.write_str("] catch_param=")?;
        write_optional_display(f, self.catch_param)?;
        f.write_str(" catch=")?;
        write_optional_display(f, self.catch_block)?;
        f.write_str(" finally=")?;
        write_optional_display(f, self.finally_block)?;
        f.write_str(" exit=")?;
        write_optional_display(f, self.exit_block)
    }
}

impl IrClass {
    fn fmt_with_id(&self, f: &mut fmt::Formatter<'_>, id: ClassId, indent: usize) -> fmt::Result {
        write_indent(f, indent)?;
        write!(
            f,
            ".class {id} name={}",
            display_optional_name(self.name.as_deref())
        )?;
        if let Some(super_class) = &self.super_class {
            write!(f, " extends={super_class}")?;
        }
        if let Some(constructor) = self.constructor {
            write!(f, " constructor={constructor}")?;
        }
        writeln!(f)?;

        for member in &self.members {
            write_indent(f, indent + 1)?;
            writeln!(f, "{member}")?;
        }
        if !self.static_blocks.is_empty() {
            write_indent(f, indent + 1)?;
            f.write_str("static_blocks=[")?;
            write_display_list(f, &self.static_blocks)?;
            f.write_str("]\n")?;
        }
        Ok(())
    }
}

impl fmt::Display for IrClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_id(f, ClassId(0), 0)
    }
}

impl fmt::Display for IrClassMember {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_static {
            f.write_str("static ")?;
        }
        write!(f, "{} {}", self.kind, self.key)
    }
}

impl fmt::Display for IrClassMemberKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrClassMemberKind::Field { value } => {
                f.write_str("field")?;
                if let Some(value) = value {
                    write!(f, "={value}")?;
                }
                Ok(())
            }
            IrClassMemberKind::Method { function } => write!(f, "method {function}"),
            IrClassMemberKind::Getter { function } => write!(f, "getter {function}"),
            IrClassMemberKind::Setter { function } => write!(f, "setter {function}"),
            IrClassMemberKind::AutoAccessor {
                getter,
                setter,
                value,
            } => {
                f.write_str("auto_accessor getter=")?;
                write_optional_display(f, *getter)?;
                f.write_str(" setter=")?;
                write_optional_display(f, *setter)?;
                if let Some(value) = value {
                    write!(f, " value={value}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for IrImportDecl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "from {:?} import [", self.source)?;
        write_display_list(f, &self.specifiers)?;
        f.write_str("]")
    }
}

impl fmt::Display for IrImportSpecifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrImportSpecifier::Default { local } => write!(f, "default as {local}"),
            IrImportSpecifier::Namespace { local } => write!(f, "* as {local}"),
            IrImportSpecifier::Named { imported, local } => write!(f, "{imported} as {local}"),
        }
    }
}

impl fmt::Display for IrExportDecl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrExportDecl::Local { local, exported } => write!(f, "export {local} as {exported}"),
            IrExportDecl::Default { value } => write!(f, "export default {value}"),
            IrExportDecl::ReExport {
                source,
                imported,
                exported,
            } => write!(f, "export {imported} as {exported} from {source:?}"),
            IrExportDecl::ExportAll { source, exported } => match exported {
                Some(exported) => write!(f, "export * as {exported} from {source:?}"),
                None => write!(f, "export * from {source:?}"),
            },
        }
    }
}

fn write_indent(f: &mut fmt::Formatter<'_>, indent: usize) -> fmt::Result {
    for _ in 0..indent {
        f.write_str("  ")?;
    }
    Ok(())
}

fn write_display_list<T: fmt::Display>(f: &mut fmt::Formatter<'_>, values: &[T]) -> fmt::Result {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{value}")?;
    }
    Ok(())
}

fn write_optional_display<T: fmt::Display>(
    f: &mut fmt::Formatter<'_>,
    value: Option<T>,
) -> fmt::Result {
    match value {
        Some(value) => write!(f, "{value}"),
        None => f.write_str("none"),
    }
}

fn write_debug_string_list(f: &mut fmt::Formatter<'_>, values: &[String]) -> fmt::Result {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{value:?}")?;
    }
    Ok(())
}

fn write_optional_string_list(
    f: &mut fmt::Formatter<'_>,
    values: &[Option<String>],
) -> fmt::Result {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        match value {
            Some(value) => write!(f, "{value:?}")?,
            None => f.write_str("none")?,
        }
    }
    Ok(())
}

fn display_optional_name(name: Option<&str>) -> DisplayOptionalName<'_> {
    DisplayOptionalName(name)
}

struct DisplayOptionalName<'a>(Option<&'a str>);

impl fmt::Display for DisplayOptionalName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(name) => write!(f, "{name}"),
            None => f.write_str("<anonymous>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_module_ir() {
        let module = IrModule {
            constants: vec![IrConst::Int(1)],
            functions: vec![IrFunction {
                name: Some("main".to_string()),
                register_count: 1,
                blocks: vec![IrBlock {
                    instructions: vec![IrInstruction::new(IrInstructionKind::Move {
                        dst: RegisterId(0),
                        src: IrValue::Const(ConstId(0)),
                    })],
                    terminator: IrTerminator::Return(Some(IrValue::Register(RegisterId(0)))),
                    ..IrBlock::default()
                }],
                ..IrFunction::default()
            }],
            ..IrModule::default()
        };

        let text = module.to_string();

        assert!(text.contains(".module kind=script entry=fn#0"), "{text}");
        assert!(text.contains("const#0 = 1"), "{text}");
        assert!(text.contains(".function fn#0 name=main"), "{text}");
        assert!(text.contains("r0 = move const#0"), "{text}");
        assert!(text.contains("return r0"), "{text}");
    }
}
