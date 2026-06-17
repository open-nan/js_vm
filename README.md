# JS VM

一个实验性的 JavaScript 虚拟机项目，包含 JS 到 IR、IR 到 Bytecode、Bytecode 执行器、可配置混淆编码，以及基于 wasm 的 Web Workbench。

根目录的 [ARCHITECTURE.md](ARCHITECTURE.md) 是 GitHub 可直接显示的可点击架构图。后续你可以直接改这份 Markdown，把新的模块、数据流、运行方式写进去，我会优先按根目录架构图理解你的意图。

## Quick Start

本地预览 Web Workbench：

```bash
python3 -m http.server 4188 --bind 127.0.0.1
```

打开：

```text
http://127.0.0.1:4188/index.html
```

构建 wasm：

```bash
sh scripts/build-wasm.sh
```

测试：

```bash
cargo fmt --all --check
cargo test
cargo check --target wasm32-unknown-unknown
```

## Architecture

当前 workspace 分为 Core、Runtime、Compiler 和两个 wasm facade：

- `crates/core`: VM 核心抽象层。定义 IR、Bytecode、编码表、Seed、序列化和反序列化。
- `crates/runtime`: 执行器运行时。只依赖 Core，负责 Bytecode 执行和 HostBridge。
- `crates/bin`: 编译器、CLI 和 Rust facade。负责 JS -> IR、IR -> Bytecode。
- `crates/compiler_wasm`: Presentation Layer 使用的编译器 wasm。
- `crates/executor_wasm`: 生成场景使用的轻量执行器 wasm。
- `index.html`: wasm Web Workbench。提供 JS 编辑器、IR/Bytecode/Hex 视图、日志面板和 Obfuscation 配置。

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

可点击分层架构图见 [ARCHITECTURE.md](ARCHITECTURE.md)。


## Compiler

编译器入口在 [crates/bin/src/compiler.rs](crates/bin/src/compiler.rs)。

编译阶段：

1. 使用 SWC 解析 JavaScript 源码。
2. `LoweringContext` 遍历 SWC AST，将语句和表达式降低到 `IrInstruction`。
3. 未在当前作用域声明、且不是隐式全局的标识符会进入 `extern_slots`。
4. `IrModule::to_bytecode()` 调用 core 中的 `BytecodeBuilder`，生成 `BytecodeModule`。
5. `BytecodeModule::to_bytes_with_encoding()` 使用编码表输出二进制字节流。
6. `EncodingConfig::to_seed(bytes)` 生成可恢复编码表的 Seed，并绑定当前 bytes 指纹。

Rust API：

```rust
use js_token_bin::{CompileOptions, compile_source, compile_source_with_options};

let source = "let x = 1 + 2; x;";
let output = compile_source(source)?;

let ir = output.ir;
let bytecode = output.bytecode;
let bytes = output.bytes;

let options = CompileOptions {
    externals: vec!["console".to_string()],
    encoding_seed: Some(config_seed),
    extern_slots: None,
};
let output = compile_source_with_options("console.log(1)", &options)?;
```

wasm API：

```js
import initCompiler, {
  Compiler,
  js_encoding_seed_for_seed_and_bytes,
  js_encoding_seed_from_rows,
} from "./pkg/compiler/js_vm_compiler.js";
import initExecutor, { js_execute_bytes_with_seed } from "./pkg/executor/js_vm_executor.js";

await Promise.all([
  initCompiler("./pkg/compiler/js_vm_compiler_bg.wasm"),
  initExecutor("./pkg/executor/js_vm_executor_bg.wasm"),
]);

const compiler = new Compiler("console.log(1)");
const irText = compiler.to_text();
const externs = compiler.extern_slots();
const configSeed = js_encoding_seed_from_rows(opcodes, operandTags, constantTags, new Uint8Array());
const artifact = compiler.to_bytecode_artifact(configSeed, externs);
const bytecodeText = artifact.bytecode_text();
const bytes = artifact.bytes();
const seed = js_encoding_seed_for_seed_and_bytes(configSeed, bytes);
const result = js_execute_bytes_with_seed(bytes, seed);
artifact.free();
```

## IR

IR 定义在 [crates/core/src/lib.rs](crates/core/src/lib.rs)。

`IrValue` 表示 IR 层的值：

- `Register(String)`: 临时寄存器，例如 `%t0`
- `Name(String)`: 标识符名称
- `Number(f64)`
- `String(String)`
- `Bool(bool)`
- `Null`
- `Undefined`

`IrInstruction` 覆盖当前 VM 支持的 JS 操作：

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

`IrModule`：

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

## Bytecode

Bytecode 定义在 [crates/core/src/lib.rs](crates/core/src/lib.rs)。

```rust
pub struct BytecodeModule {
    pub extern_slots: Vec<String>,
    pub constants: Vec<BytecodeConstant>,
    pub instructions: Vec<BytecodeInstruction>,
}
```

当前二进制布局：

```text
magic: 8 bytes, default "JSTKBC01"
extern_count: varuint u32
extern_slots: repeated compact string
constant_count: varuint u32
constants: repeated tagged compact constant
instruction_count: varuint u32
instructions:
  opcode: u8
  operand_count: varuint u32
  operands:
    tag: u8
    payload: varuint u32
```

紧凑编码：

- `u32` 使用 varuint 编码，小整数通常只占 1 字节。
- 字符串长度使用 varuint。
- `Number` 常量如果是安全的 `i32` 整数，使用 `kind + zigzag varuint`。
- 其他 number 回退为 `kind + f64`。

当前指令集：

```text
MARKER, LABEL, DECLARE, LOAD_CONST, LOAD_NAME, STORE_NAME,
STORE_MEMBER, MOVE, BINARY, UNARY, MEMBER, ARRAY, OBJECT,
CALL, NEW, TEMPLATE, FUNCTION_START, FUNCTION_END,
FUNCTION_EXPR_START, FUNCTION_EXPR_END, CLASS, IMPORT, EXPORT,
THROW, TRY_START, CATCH_START, FINALLY_START, TRY_END,
RETURN, POP, JUMP, JUMP_IF_FALSE, UNSUPPORTED
```

Operand tags：

- `register`
- `constant`
- `name`
- `label`
- `count`
- `none`

Constant tags：

- `number`
- `string`
- `bool`
- `null`
- `undefined`

## Executor

执行器入口在 [crates/bin/src/executor.rs](crates/bin/src/executor.rs)。

执行阶段：

1. `BytecodeModule::from_bytes_with_seed(bytes, seed)` 先验证 seed 与 bytes 是否配对。
2. 验证通过后，从 seed 恢复 `EncodingConfig`，再按该编码表解码 bytecode。
3. `Executor::run()` 初始化寄存器、全局对象、标签表和外部引用。
4. VM 按 `BytecodeInstruction` 顺序解释执行。
5. `LoadName` 会从 globals 读取值；外部对象被注入为 `Value::ExternalRef(path)`。
6. 成员访问会通过 `HostBridge::get(path, property)` 延展外部路径。
7. 函数调用遇到 `ExternalRef` 时，交给 `HostBridge::call(path, args)`。

当前 `Value` 模型包含：

- `Number`
- `String`
- `Bool`
- `Array`
- `Object`
- `Function`
- `BoundFunction`
- `ExternalRef`
- `Class`
- `Module`
- `Null`
- `Undefined`

当前 HostBridge 注册：

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

执行 API：

```rust
use js_token_bin::{execute_bytes, execute_source};

let result = execute_source("1 + 2")?;

let result = execute_bytes(&bytes, &seed)?;
```

## Obfuscation

混淆器的核心是 `EncodingConfig`。它不改变 VM 语义，而是改变 bytecode 的编码表：

- `opcodes`: 指令名称 -> opcode code
- `operand_tags`: operand kind -> tag code
- `constant_tags`: constant kind -> tag code

外部传输不再使用 YAML。UI 或宿主侧只传递 seed；Core Layer 使用 `EncodingNames` 表示 opcode、operand tag、constant tag 的名称排列，再和 `EncodingConfig` 相互转换。

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

Web Workbench 中的 Obfuscation 面板还支持 extern slot 混淆：

```text
value       HostBridge slot
console     e1
window      e0
document    e2
```

左侧 `value` 固定，右侧 `HostBridge slot` 可交换。生成 bytes 时会按 slot 顺序重排 `BytecodeModule.extern_slots`，从而改变 bytecode 头部和 seed 指纹。

## CLI

编译器：

```bash
cargo run -p js_token_bin --bin js-compiler -- --emit ir input.js
cargo run -p js_token_bin --bin js-compiler -- --emit bytecode input.js
cargo run -p js_token_bin --bin js-compiler -- --emit bytes input.js
```

执行器：

```bash
cargo run -p js_token_bin --bin js-executor -- input.bytecode
cargo run -p js_token_bin --bin js-executor -- --seed "$SEED" input.bytecode
```

## Web Workbench

页面能力：

- JavaScript 编辑器
- JS 语法高亮和补全
- IR、Bytecode、Hex 三视图
- JavaScript bytes 和 bytecode bytes 体积显示
- 编译耗时和执行耗时
- Console 风格日志面板
- Obfuscation 面板
- opcode、operand tag、constant tag 混淆
- extern slot 混淆
- seed 同步、恢复、校验
- Hex byte 与 ASCII 联动高亮
- Hex 方向键导航
- JS 高亮、行号、IR/Bytecode/Hex 大文本渲染优化
- 下载执行器运行包和 runner 脚本生成

## GitHub Pages

静态页面发布在 `gh-pages` 分支：

```text
https://open-nan.github.io/js_vm/
```

Pages workflow 会构建 wasm，并显式发布：

```text
dist/index.html
dist/pkg/compiler/js_vm_compiler.js
dist/pkg/compiler/js_vm_compiler_bg.wasm
dist/pkg/compiler/package.json
dist/pkg/executor/js_vm_executor.js
dist/pkg/executor/js_vm_executor_bg.wasm
dist/pkg/executor/package.json
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

## Roadmap

- [ ] 为 Seed 加盐：构建时为 compiler/executor 注入 salt，只有匹配 salt 的 seed 才能执行。
- [ ] 继续压缩 Bytecode：在保证 roundtrip fuzz 通过的前提下探索固定 arity、省略重复常量、短寄存器编码。
- [ ] 扩展 HostBridge：从硬编码 host 方法演进到可注册、可权限控制的宿主 API 表。
- [ ] 补齐更完整的 ECMAScript 运行时语义。
