## 分层架构图

```Graph
┌──────────────────────────────────────────────────────────────┐
│             Presentation Layer (Web UI / Scripts)            │ 
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
负责字节码解码、执行器、Value Model、LexicalEnv、HostBridge、console/fetch 等宿主环境交互。
主要优化方向包括：优化解析器，提高解析效率；优化执行器，提高执行效率；优化主机桥接，提高主机交互效率。

Presentation Layer:
提供 Web UI、脚本、可视化面板、导出下载、调试日志等非核心交互能力。
主要优化方向包括：优化 Web UI，提高用户体验；优化脚本工具，提高使用效率。(优先级低)

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
  - `names: Vec<String>`
  - `functions: Vec<BytecodeFunction>`
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

Compiler Layer 目前由 `js_vm_compiler` crate 暴露 Rust 内部接口和 wasm 包。编译器属于 Presentation Layer 的构建能力，不进入生成后的运行包。

#### Rust 编译接口

- `compiler::Compiler::new(source: &str) -> Result<Compiler, JsValue>`
  - 解析 JS 并生成 IR。
- `compiler.extern_slots() -> Vec<String>`
  - 返回编译器从未声明全局引用中解析出的 extern slot。
- `compiler.to_text() -> String`
  - 输出 IR 文本。
- `compiler.to_bytecode_artifact(seed, extern_slots) -> Result<CompilerArtifact, String>`
  - 统一输出 Bytecode 文本和 bytes。`seed` 为空使用默认编码，`extern_slots` 为空使用自动解析的 slot 顺序。
- `CompilerArtifact.bytecode_text() -> String`
- `CompilerArtifact.bytes() -> Vec<u8>`

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

当前仓库没有独立 CLI crate。命令行能力由 `npm run verify`、`npm run build:wasm` 和 `npm run fuzz` 这些 Node 入口承担；测试和 fuzz 入口位于 `tests`，构建和部署脚本保留在 `scripts`。

### Runtime Layer

Runtime Layer 主要由 `js_vm_runtime` 暴露 Rust 执行器和 wasm 包。执行器只依赖 Core Layer，不依赖 SWC 或编译器。

#### Rust 执行接口

- `BytecodeModule::from_bytes_with_seed(bytes, seed) -> Result<BytecodeModule, EncodingError>`
  - 由 Core Layer 校验 seed 与 bytes 是否匹配，并按 seed 恢复编码表。
- `Executor::run(module: &BytecodeModule) -> Result<Value, ExecuteError>`
  - 执行已解码的 BytecodeModule。
- `Executor::run_with_host_bridge(module: &BytecodeModule, host_bridge: HostBridge) -> Result<Value, ExecuteError>`
  - 使用自定义 HostBridge 注册表执行 BytecodeModule。

#### Executor 接口

- `Executor::run(module: &BytecodeModule) -> Result<Value, ExecuteError>`
  - 执行 BytecodeModule。
- `Executor::run_with_external_names(module: &BytecodeModule, externals: &[String]) -> Result<Value, ExecuteError>`
  - 注入额外 externals 后执行 BytecodeModule。
- `Executor::run_with_host_bridge(module: &BytecodeModule, host_bridge: HostBridge) -> Result<Value, ExecuteError>`
  - 使用可注册的 HostBridge 能力表执行。
- `ExecuteError`
  - 执行期错误类型，包括 operand 错误、运行时错误、throw 等。
- `Value`
  - 运行时值模型。

#### Scope / Environment 接口

Runtime 已从平面 `globals` 推进到显式词法环境模型：

- `LexicalEnv`
  - 表示当前执行上下文的词法环境链。
  - 函数创建时捕获 `LexicalEnv`。
  - 函数调用时在捕获环境上追加 `Function` frame。
- `ScopeFrame`
  - 表示一个作用域帧。
  - 当前已有 `Global`、`Function`、`Catch` 三类 frame。
- `EnvironmentRecord`
  - 保存单个作用域帧内的 binding。

当前已覆盖全局作用域、函数作用域、闭包捕获、闭包内可变 binding、catch 参数作用域和 extern 注入。块级作用域还需要 Core/Bytecode 增加显式 `ENTER_SCOPE` / `LEAVE_SCOPE` 或等价指令后才能精确表达。

#### wasm Runtime 函数

- `js_execute_bytes_with_seed(bytes: &[u8], seed: &str) -> Result<String, String>`
  - 按 Seed 校验并执行 bytes。
  - 这是生成场景唯一需要的 wasm Runtime API；Seed 已携带 Encoding/Obfuscation 配置并绑定 bytes 指纹。

#### CLI 执行器

当前仓库没有独立 CLI 执行器。生成场景通过 wasm Runtime 或 Node fuzz/replay 脚本执行。

#### HostBridge 接口边界

HostBridge 已抽成可注册能力模型。外部值仍由 `Value::ExternalRef(path)` 表示；成员访问会扩展 path，函数调用时由 HostBridge 注册表解析。

- `HostBridge::empty() -> HostBridge`
  - 创建空能力表，未注册的外部调用会报错。
- `HostBridge::with_default_capabilities() -> HostBridge`
  - 创建默认能力表。
- `HostBridge::register_function(path, function)`
  - 注册宿主函数能力。
- `HostBridge::has_function(path) -> bool`
  - 查询能力是否已注册。`window.` 和 `globalThis.` 前缀会归一化。

默认注册能力：

- `console.log`
- `console.info`
- `console.warn`
- `console.error`
- `console.debug`
- `fetch`
- `window.fetch`
- `globalThis.fetch`

wasm Runtime 当前仍使用默认 HostBridge。JS 侧动态注册宿主函数需要后续新增 wasm 绑定层。

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
