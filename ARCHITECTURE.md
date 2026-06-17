## 分层架构图

```Graph
┌──────────────────────────────────────────────────────────────┐
│              Presentation Layer (Web UI / CLI)               │ 
├──────────────────────────────┬───────────────────────────────┤
│        Compiler Layer        │        Runtime Layer          │
│   Source → IR → Bytecode     │ Decode → Execute → HostBridge │
│ Encode / Obfuscate / Package │    Value Model / Host APIs    │
├──────────────────────────────┴───────────────────────────────┤
│             Core Layer (IR / Bytecode / Encoding)            │
└──────────────────────────────────────────────────────────────┘
```

## 层级定义

Core Layer:
定义 IR、Bytecode、Module、EncodingConfig、Seed、Bytes 等核心数据结构与通用编解码能力。
主要优化方向包括：优化 IR 结构, 降低 IR 大小, 生成速度；优化字节码编译，降低字节码体积。

Compiler Layer:
负责 SWC 解析、AST Lowering、IR 生成、字节码构建、编码、混淆和产物打包。
主要优化方向包括：优化 IR 生成速度，提高编译效率；支持多线程编译，提高编译速度。

Runtime Layer:
负责字节码解码、执行器、Value Model、HostBridge、console/fetch 等宿主环境交互。
主要优化方向包括：优化解析器，提高解析效率；优化执行器，提高执行效率；优化主机桥接，提高主机交互效率。

Presentation Layer:
提供 Web UI、CLI、可视化面板、导出下载、调试日志等非核心交互能力。
主要优化方向包括：优化 Web UI，提高用户体验；优化命令行工具，提高使用效率。(优先级低)

## 公开的接口模块

### Core Layer

Core Layer 是底层数据结构和编解码接口，主要由 `js_token_core` 暴露。

#### IR 接口

- `IrModule`
  - `extern_slots: Vec<String>`
  - `instructions: Vec<IrInstruction>`
  - `to_text() -> String`
  - `to_bytecode() -> BytecodeModule`
- `IrInstruction`
  - JS 语法降低后的中间表示指令集合。
- `IrValue`
  - IR 中的值表示，包括 register、name、number、string、bool、null、undefined。

#### Bytecode 接口

- `BytecodeModule`
  - `extern_slots: Vec<String>`
  - `constants: Vec<BytecodeConstant>`
  - `instructions: Vec<BytecodeInstruction>`
  - `to_text() -> String`
  - `to_bytes() -> Vec<u8>`
  - `to_bytes_with_encoding(encoding: &EncodingConfig) -> Result<Vec<u8>, EncodingError>`
  - `from_bytes(bytes: &[u8]) -> Result<BytecodeModule, EncodingError>`
  - `from_bytes_with_encoding(bytes: &[u8], encoding: &EncodingConfig) -> Result<BytecodeModule, EncodingError>`
  - `from_bytes_with_seed(bytes: &[u8], seed: &str) -> Result<BytecodeModule, EncodingError>`
- `BytecodeInstruction`
  - `op: BytecodeOp`
  - `operands: Vec<BytecodeOperand>`
- `BytecodeOp`
  - `all() -> &'static [BytecodeOp]`
  - `mnemonic() -> &'static str`
  - `from_mnemonic(mnemonic: &str) -> Option<BytecodeOp>`

#### Encoding / Seed 接口

- `EncodingConfig`
  - `default() -> EncodingConfig`
  - `from_names(names: &EncodingNames) -> Result<EncodingConfig, EncodingError>`
  - `names() -> EncodingNames`
  - `config_seed() -> Result<String, EncodingError>`
  - `paired_seed(bytes: &[u8]) -> Result<String, EncodingError>`
  - `to_seed(bytes: &[u8]) -> Result<String, EncodingError>`
  - `from_seed(seed: &str) -> Result<EncodingConfig, EncodingError>`
  - `from_seed_for_bytes(seed: &str, bytes: &[u8]) -> Result<EncodingConfig, EncodingError>`
  - YAML 解析仍作为内部兼容能力存在，但不再作为公开传输格式。
- `EncodingNames`
  - `opcodes: Vec<String>`
  - `operand_tags: Vec<String>`
  - `constant_tags: Vec<String>`
  - `flatten() -> Vec<String>`
  - 表示编码表的名称排列，是 UI 表格、Seed 和 `EncodingConfig` 之间的核心抽象。
- `EncodingError`
  - 编码表、Seed、bytes 解码相关错误。

### Compiler Layer

Compiler Layer 主要由 `js_token_bin` 暴露 Rust/CLI 门面，并由 `js_vm_compiler` wasm 包暴露给 Web Workbench。编译器属于 Presentation Layer 的构建能力，不进入生成后的运行包。

#### 推荐 Rust 编译接口

- `CompileOptions`
  - 聚合编译配置：`externals`、`encoding_seed`、`extern_slots`。
- `CompileOutput`
  - 聚合编译结果：`ir`、`bytecode`、`bytes`、`seed`。
  - `ir_text() -> String`
  - `bytecode_text() -> String`
- `compile_source(source: &str) -> Result<CompileOutput, String>`
  - 默认配置编译 JS，返回完整产物。
- `compile_source_with_options(source: &str, options: &CompileOptions) -> Result<CompileOutput, String>`
  - 使用 externs、编码表和 slot 顺序编译 JS。

#### 编码 / 混淆接口

- `encoding_seed_from_names(opcodes, operand_tags, constant_tags, bytes) -> Result<String, String>`
  - 从编码表名称排列和 bytes 生成 Seed。
- `encoding_seed_for_seed_and_bytes(seed: &str, bytes: &[u8]) -> Result<String, String>`
  - 从配置 Seed 和 bytes 生成配对运行 Seed。
- `encoding_names_from_seed(seed: &str) -> Result<Vec<String>, String>`
  - 从 Seed 恢复编码表名称排列。

#### wasm Compiler 类 (`pkg/compiler`)

- `new Compiler(source: &str)`
  - 创建编译器实例，并立即完成 JS -> IR。
- `compiler.extern_slots() -> Vec<String>`
  - 获取编译器自动解析出的 extern slot 表。
- `compiler.to_text() -> String`
  - 输出 IR 文本。
- `compiler.to_bytecode_artifact(seed, extern_slots) -> Result<CompilerArtifact, String>`
  - 统一构建 Bytecode 产物；`seed` 为空时使用默认编码，`extern_slots` 为空时使用自动解析出的 slot 表。
- `CompilerArtifact.bytecode_text() -> String`
  - 输出 Bytecode 文本。
- `CompilerArtifact.bytes() -> Vec<u8>`
  - 输出编码后的 bytecode bytes。

#### CLI 编译器

- `js-compiler --emit ir`
- `js-compiler --emit bytecode`
- `js-compiler --emit bytes`
- `js-compiler --extern name`

### Runtime Layer

Runtime Layer 主要由 `js_token_runtime` 暴露 Rust 执行器，并由 `js_vm_runtime` wasm 包暴露给生成场景。执行器只依赖 Core Layer，不依赖 SWC 或编译器。

#### 推荐 Rust 执行接口

- `BytecodeModule::from_bytes_with_seed(bytes, seed) -> Result<BytecodeModule, EncodingError>`
  - 由 Core Layer 校验 seed 与 bytes 是否匹配，并按 seed 恢复编码表。
- `Executor::run(module: &BytecodeModule) -> Result<Value, ExecuteError>`
  - 执行已解码的 BytecodeModule。
- `js_token_bin::execute_bytes(bytes, seed) -> Result<String, String>`
  - Rust facade，组合 Core 解码和 Runtime 执行。

#### Executor 接口

- `Executor::run(module: &BytecodeModule) -> Result<Value, ExecuteError>`
  - 执行 BytecodeModule。
- `Executor::run_with_external_names(module: &BytecodeModule, externals: &[String]) -> Result<Value, ExecuteError>`
  - 注入额外 externals 后执行 BytecodeModule。
- `ExecuteError`
  - 执行期错误类型，包括 operand 错误、运行时错误、throw 等。
- `Value`
  - 运行时值模型。

#### wasm Runtime 函数

- `js_execute_bytes_with_seed(bytes: &[u8], seed: &str) -> Result<String, String>`
  - 按 Seed 校验并执行 bytes。
  - 这是生成场景唯一需要的 wasm Runtime API；Seed 已携带 Encoding/Obfuscation 配置并绑定 bytes 指纹。

#### CLI 执行器

- `js-executor input.bytecode`
- `js-executor --seed seed input.bytecode`
- `js-executor --extern name input.bytecode`

#### HostBridge 接口边界

当前 HostBridge 不是公开 Rust trait，而是运行时内部边界。当前注册能力：

- `console.log`
- `console.info`
- `console.warn`
- `console.error`
- `console.debug`
- `fetch`
- `window.fetch`
- `globalThis.fetch`

### Presentation Layer

Presentation Layer 主要提供人机交互能力，不属于核心 VM 语义。

#### Web Workbench

- `index.html`
  - JS 编辑器。
  - IR / Bytecode / Hex 视图。
  - Obfuscation 表格。
  - extern slot 混淆。
  - Console 日志。
  - 运行包下载。
  - Seed 输入、同步和校验。

#### wasm packages

- `pkg/compiler/js_vm_compiler.js`
- `pkg/compiler/js_vm_compiler_bg.wasm`
- `pkg/compiler/package.json`
- `pkg/executor/js_vm_runtime.js`
- `pkg/executor/js_vm_runtime_bg.wasm`
- `pkg/executor/package.json`

#### GitHub Pages

- `.github/workflows/pages.yml`
  - 构建 wasm。
  - 复制 `index.html`。
  - 显式发布 `pkg/compiler` 编译器文件和 `pkg/executor` 运行时文件。
  - 推送到 `gh-pages`。
