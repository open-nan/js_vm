# JS VM Architecture

本文档描述当前库的执行架构、公开 API、使用方式、IR/Bytecode 抽象、执行器语义和混淆器设计。

## 模块边界

当前 workspace 分为两个 Rust crate 和一个 Web Workbench：

- `crates/core`: VM 的核心抽象层。定义 IR、Bytecode、编码表、Seed、序列化和反序列化。
- `crates/bin`: 编译器、执行器、CLI 和 wasm facade。负责 JS -> IR、IR -> Bytecode、Bytecode 执行。
- `index.html`: wasm Web 测试台。提供 JS 编辑器、IR/Bytecode/Bytes 视图、日志面板和 Obfuscation 配置。

整体数据流：

```text
JavaScript source
  -> SWC parser
  -> LoweringContext
  -> IrModule
  -> BytecodeModule
  -> bytes + seed
  -> BytecodeModule::from_bytes_with_seed
  -> Executor
  -> HostBridge
```

## 执行架构

### 编译阶段

编译器入口在 `crates/bin/src/compiler.rs`。

1. 使用 SWC 解析 JavaScript 源码。
2. `LoweringContext` 遍历 SWC AST，将语句和表达式降低到 `IrInstruction`。
3. 未在当前作用域声明、且不是隐式全局的标识符会进入 `extern_slots`。
4. `IrModule::to_bytecode()` 调用 core 中的 `BytecodeBuilder`，生成 `BytecodeModule`。
5. `BytecodeModule::to_bytes_with_encoding()` 使用编码表输出二进制字节流。
6. `EncodingConfig::to_seed(bytes)` 生成可恢复编码表的 Seed，并绑定当前 bytes 指纹。

### 执行阶段

执行器入口在 `crates/bin/src/executor.rs`。

1. `BytecodeModule::from_bytes_with_seed(bytes, seed)` 先验证 seed 与 bytes 是否配对。
2. 验证通过后，从 seed 恢复 `EncodingConfig`，再按该编码表解码 bytecode。
3. `Executor::run()` 初始化寄存器、全局对象、标签表和外部引用。
4. VM 按 `BytecodeInstruction` 顺序解释执行。
5. `LoadName` 会从 globals 读取值；外部对象被注入为 `Value::ExternalRef(path)`。
6. 成员访问会通过 `HostBridge::get(path, property)` 延展外部路径。
7. 函数调用遇到 `ExternalRef` 时，交给 `HostBridge::call(path, args)`。

当前 HostBridge 注册了：

- `console.log`
- `console.info`
- `console.warn`
- `console.error`
- `console.debug`
- `fetch`
- `window.fetch`
- `globalThis.fetch`

未注册的外部函数会返回运行时错误：

```text
external function <path> is not registered
```

## IR 结构

IR 定义在 `crates/core/src/lib.rs`。

### IrValue

`IrValue` 表示 IR 层的值：

- `Register(String)`: 临时寄存器，例如 `%t0`
- `Name(String)`: 标识符名称
- `Number(f64)`
- `String(String)`
- `Bool(bool)`
- `Null`
- `Undefined`

### IrInstruction

`IrInstruction` 是编译器中间表示，覆盖当前 VM 支持的 JS 操作：

- 声明与名称：`Declare`、`LoadName`、`StoreName`
- 常量与移动：`LoadConst`、`Move`
- 运算：`Binary`、`Unary`
- 对象和数组：`Member`、`StoreMember`、`Array`、`Object`
- 调用和构造：`Call`、`New`
- 函数：`FunctionStart`、`FunctionEnd`、`FunctionExprStart`、`FunctionExprEnd`
- 类和模块：`Class`、`Import`、`Export`
- 异常：`Throw`、`TryStart`、`CatchStart`、`FinallyStart`、`TryEnd`
- 控制流：`Label`、`Jump`、`JumpIfFalse`、`Return`、`Pop`
- 兼容占位：`Marker`、`Unsupported`

`IrModule` 包含：

```rust
pub struct IrModule {
    pub extern_slots: Vec<String>,
    pub instructions: Vec<IrInstruction>,
}
```

`extern_slots` 是编译器自动解析出的外部依赖表。例如：

```js
console.log(1);
window.fetch("/api");
```

可能生成：

```text
.externs
  e0 = console
  e1 = window
```

## Bytecode 结构

Bytecode 定义在 `crates/core/src/lib.rs`。

### BytecodeModule

```rust
pub struct BytecodeModule {
    pub extern_slots: Vec<String>,
    pub constants: Vec<BytecodeConstant>,
    pub instructions: Vec<BytecodeInstruction>,
}
```

二进制布局：

```text
magic: 8 bytes, default "JSTKBC01"
extern_count: u32
extern_slots: repeated string
constant_count: u32
constants: repeated tagged constant
instruction_count: u32
instructions:
  opcode: u8
  operand_count: u32
  operands:
    tag: u8
    payload: u32
```

### BytecodeOp

当前指令集：

```text
MARKER, LABEL, DECLARE, LOAD_CONST, LOAD_NAME, STORE_NAME,
STORE_MEMBER, MOVE, BINARY, UNARY, MEMBER, ARRAY, OBJECT,
CALL, NEW, TEMPLATE, FUNCTION_START, FUNCTION_END,
FUNCTION_EXPR_START, FUNCTION_EXPR_END, CLASS, IMPORT, EXPORT,
THROW, TRY_START, CATCH_START, FINALLY_START, TRY_END,
RETURN, POP, JUMP, JUMP_IF_FALSE, UNSUPPORTED
```

### Operand tags

操作数通过 tag + payload 表示：

- `register`
- `constant`
- `name`
- `label`
- `count`
- `none`

### Constant tags

常量池支持：

- `number`
- `string`
- `bool`
- `null`
- `undefined`

## 混淆器

混淆器的核心是 `EncodingConfig`。它不改变 VM 的语义，而是改变 bytecode 的编码表：

- `opcodes`: 指令名称 -> opcode code
- `operand_tags`: operand kind -> tag code
- `constant_tags`: constant kind -> tag code

默认编码表示例见 `examples/encoding.yaml`。

```yaml
magic: "JSTKBC01"
opcodes:
  LOAD_CONST: 3
operand_tags:
  register: 0
constant_tags:
  number: 0
```

### Seed

Seed 用于紧凑保存混淆配置，并绑定 bytes：

```text
JSTKSEED2-<fingerprint>-<opcode_perm>.<operand_perm>.<constant_perm>
```

执行器只接受 `bytes + seed`。流程是：

1. 从 seed 解析 permutation。
2. 用 permutation 和 bytes 计算指纹。
3. 指纹不匹配则拒绝运行。
4. 指纹匹配则恢复 `EncodingConfig`。
5. 用恢复出的编码表解码 bytes。

### Extern slot 混淆

Web Workbench 中的 Obfuscation 面板还支持 extern slot 混淆。

表格格式：

```text
value       HostBridge slot
console     e1
window      e0
document    e2
```

左侧 `value` 固定，右侧 `HostBridge slot` 可交换。生成 bytes 时会按 slot 顺序重排 `BytecodeModule.extern_slots`，从而改变 bytecode 头部和 seed 指纹。

注意：当前 extern slot 混淆改变的是 bytecode 中的外部槽位顺序；JS 名称解析仍通过名称常量和 globals 完成。

## Rust API

### 编译 API

```rust
use js_token_bin::{
    compile_to_ir,
    compile_to_bytecode,
    compile_to_bytecode_text,
    compile_to_bytecode_bytes,
};

let source = "let x = 1 + 2; x;";
let ir = compile_to_ir(source)?;
let bytecode = compile_to_bytecode(source)?;
let text = compile_to_bytecode_text(source)?;
let bytes = compile_to_bytecode_bytes(source)?;
```

### 自定义编码

```rust
use js_token_bin::{
    compile_to_bytecode_bytes_with_encoding_yaml,
    encoding_seed_for_bytes,
    execute_bytecode_bytes_with_seed,
};

let yaml = std::fs::read_to_string("examples/encoding.yaml")?;
let bytes = compile_to_bytecode_bytes_with_encoding_yaml("1 + 2", &yaml)?;
let seed = encoding_seed_for_bytes(&yaml, &bytes)?;
let result = execute_bytecode_bytes_with_seed(&bytes, &seed)?;
```

### 执行 API

```rust
use js_token_bin::{
    execute_source,
    execute_source_with_externals,
    execute_bytecode_bytes,
    execute_bytecode_bytes_with_seed,
};

let result = execute_source("1 + 2")?;

let externals = vec!["console".to_string(), "window".to_string()];
let result = execute_source_with_externals("console.log(1)", &externals)?;
```

### Executor API

```rust
use js_token_bin::Executor;

let module = js_token_bin::compile_to_bytecode("1 + 2")?;
let value = Executor::run(&module)?;
```

## wasm API

wasm facade 由 `wasm-bindgen` 导出，入口在 `pkg/compiler/js_token_bin.js`。

```js
import init, {
  Compiler,
  js_encoding_seed_for_bytes,
  js_encoding_yaml_from_seed,
  js_execute_bytes_with_seed,
} from "./pkg/compiler/js_token_bin.js";

await init("./pkg/compiler/js_token_bin_bg.wasm");

const compiler = new Compiler("console.log(1)");
const irText = compiler.to_text();
const bytecodeText = compiler.to_bytecode_text();
const externs = compiler.extern_slots();
```

### 生成 bytes + seed

```js
const yaml = `
magic: "JSTKBC01"
opcodes:
  LOAD_CONST: 3
operand_tags:
  register: 0
constant_tags:
  number: 0
`;

const bytes = compiler.to_bytes_with_encoding(yaml);
const seed = js_encoding_seed_for_bytes(yaml, bytes);
const result = js_execute_bytes_with_seed(bytes, seed);
```

### 使用 extern slot 混淆

```js
const externs = compiler.extern_slots();
const obfuscatedExternSlots = ["window", "console", "document"];

const bytecodeText = compiler.to_bytecode_text_with_extern_slots(obfuscatedExternSlots);
const bytes = compiler.to_bytes_with_encoding_and_extern_slots(yaml, obfuscatedExternSlots);
const seed = js_encoding_seed_for_bytes(yaml, bytes);
```

`obfuscatedExternSlots.length` 必须等于 `compiler.extern_slots().length`，否则会返回：

```text
extern slot count mismatch
```

## CLI

### 编译器

```bash
cargo run -p js_token_bin --bin js-compiler -- --emit ir input.js
cargo run -p js_token_bin --bin js-compiler -- --emit bytecode input.js
cargo run -p js_token_bin --bin js-compiler -- --emit bytes --encoding examples/encoding.yaml input.js
```

### 执行器

```bash
cargo run -p js_token_bin --bin js-executor -- input.bytecode
cargo run -p js_token_bin --bin js-executor -- --seed "$SEED" input.bytecode
cargo run -p js_token_bin --bin js-executor -- --encoding examples/encoding.yaml input.bytecode
```

## Web Workbench

本地启动：

```bash
python3 -m http.server 4188 --bind 127.0.0.1
```

打开：

```text
http://127.0.0.1:4188/index.html
```

页面能力：

- JavaScript 编辑器
- JS 语法高亮和补全
- 多文件资源管理器和模块测试入口
- IR、Bytecode、Bytes 三视图
- 编译耗时和执行耗时
- Console 风格日志面板
- Obfuscation 面板
- opcode、operand tag、constant tag 混淆
- extern slot 混淆
- seed 复制、同步、恢复
- bytes 变化高亮

## 测试和构建

```bash
cargo fmt --all --check
cargo test -p js_token_core -p js_token_bin
sh scripts/build-wasm.sh
```

wasm release 配置在 workspace 根 `Cargo.toml`：

```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

## 当前语义覆盖

执行器当前已覆盖：

- 基础算术和全局变量
- 对象和数组
- 函数、闭包和 this 绑定方法
- 类的基本值表示
- 模块值表示
- try/catch/finally 和 throw
- null/undefined 操作错误
- HostBridge 外部调用
- console 和 fetch 日志模拟

这仍然是实验性 JS VM，不是完整 ECMAScript 引擎。实现策略是先让 IR/Bytecode/Executor 的架构稳定，再逐步补齐语义细节。
