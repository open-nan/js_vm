# JS VM

一个实验性的 JavaScript 虚拟机项目，包含 JS 到 IR、IR 到 bytecode、bytecode 执行器，以及基于 wasm 的 Web 测试页面。

## 可视化概览 (代码与逻辑图)

```mermaid
flowchart TD
    %% =========================
    %% User Input
    %% =========================
    A["用户输入 Seed 字符串"]

    %% =========================
    %% Core Seed Pipeline
    %% =========================
    subgraph Core["核心模块 · crates/core/src/lib.rs"]
        B["parse_encoding_seed()<br/>解析 Seed"]
        C["ParsedEncodingSeed<br/>结构化 Seed"]
        D["seed_fingerprint()<br/>校验指纹"]
        E["FNV-1a Hash"]
        F["encoding_from_seed_permutation()<br/>重建编码映射"]
        G["EncodingConfig"]
        H["BytecodeModule::from_bytes_with_seed()<br/>按 Seed 解码字节码"]
        I["BytecodeModule"]
    end

    %% =========================
    %% Compiler APIs
    %% =========================
    subgraph Compiler["编译器 API · crates/bin/src/compiler.rs"]
        J["Compiler::to_bytes_with_seed()<br/>编译并编码字节码"]
        K["execute_bytecode_bytes_with_seed()<br/>执行 Seed 字节码"]
        L["js_encoding_yaml_from_seed()<br/>Seed 转 YAML 配置"]
    end

    %% =========================
    %% Executor / Host Bridge
    %% =========================
    subgraph Executor["执行器模块 · crates/bin/src/executor.rs"]
        M["HostBridge::call()<br/>调用宿主函数"]
        N["HostBridge::get()<br/>获取宿主属性"]
        O["host_console()<br/>console.log/info/warn/error/debug"]
    end

    %% =========================
    %% Web UI
    %% =========================
    subgraph UI["Web UI · index.html"]
        P["Opcode 配置面板"]
        Q["Console 日志面板"]
        R["字节码变更高亮"]
    end

    %% =========================
    %% Main Flow
    %% =========================
    A --> B
    B --> C
    C --> D
    D --> E
    C --> F
    F --> G
    G --> H
    H --> I

    %% =========================
    %% API Integration
    %% =========================
    G --> J
    H --> K
    L --> P

    %% =========================
    %% Runtime Flow
    %% =========================
    K --> M
    M --> O
    N --> O
    O --> Q

    %% =========================
    %% UI Feedback
    %% =========================
    J --> R
    I --> R

    %% =========================
    %% Styles
    %% =========================
    style A fill:#e3f2fd,color:#0d47a1
    style Core fill:#f8f9fa,stroke:#90a4ae
    style Compiler fill:#f3e5f5,stroke:#8e24aa
    style Executor fill:#fff3e0,stroke:#fb8c00
    style UI fill:#e8f5e9,stroke:#43a047

    style G fill:#c8e6c9,color:#1b5e20
    style H fill:#c8e6c9,color:#1b5e20
    style M fill:#ffe0b2,color:#e65100
    style Q fill:#dcedc8,color:#33691e
```

```mermaid
sequenceDiagram
    participant User as User
    participant JS as JavaScript Source
    participant Compiler as Compiler
    participant VM as Virtual Machine
    participant Host as Host Bridge
    participant Console as console

    User->>JS: 提供源代码
    JS->>Compiler: parse(source)
    Compiler->>Compiler: 生成 IR
    Compiler->>Compiler: 编译为 Bytecode
    Compiler-->>VM: 加载 Bytecode

    User->>VM: execute()
    VM->>VM: 解释执行 Opcode

    alt 调用宿主函数
        VM->>Host: invoke("console.log", args)
        Host->>Console: log(args)
        Console-->>Host: 输出完成
        Host-->>VM: 返回结果
    end

    VM-->>User: 执行结束
```

## Web Workbench

仓库根目录的 `index.html` 是浏览器测试台，依赖 `pkg/compiler` 中的 wasm 产物。

本地预览：

```bash
python3 -m http.server 4188 --bind 127.0.0.1
```

然后打开：

```text
http://127.0.0.1:4188/index.html
```

## Rust

```bash
cargo test
cargo check -p js_token_bin --target wasm32-unknown-unknown
```

## Build wasm

```bash
sh scripts/build-wasm.sh
```

Release 构建开启了 `opt-level = "z"`、LTO、单 codegen unit、`panic = "abort"` 和 symbol strip。
脚本会让 `wasm-pack` 先生成 web 目标，再用 `wasm-opt -Oz` 做二次体积优化。

## GitHub Pages

静态页面发布在 `gh-pages` 分支：

```text
https://open-nan.github.io/js_vm/
```
