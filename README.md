# JS VM

一个实验性的 JavaScript 虚拟机项目，包含 JS 到 IR、IR 到 bytecode、bytecode 执行器，以及基于 wasm 的 Web 测试页面。

## Documentation

- [Architecture](docs/architecture.md): 当前执行架构、API、编译器、执行器、混淆器和 IR/Bytecode 结构说明。

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

## Chain Tests

链路测试集位于 `tests`，按 `regressions`、`syntax`、`runtime`、`obfuscation`、`fixtures` 分类组织。测试文件使用 `*.test.js` / `*.test.ts` / `*.test.jsx` / `*.test.tsx` / `*.test.vue` 命名。

```bash
sh scripts/verify.sh
```

该脚本会依次运行 Rust 测试、构建 wasm、执行 `node tests/index.js`，覆盖编译、编码、随机 seed 和执行器完整链路。

## CI

`.github/workflows/ci.yml` 会在 PR、`dev/codex`、`main` 和 merge queue 上运行完整验证：

```text
cargo test -> sh scripts/build-wasm.sh -> node tests/index.js
```

为了保证合并到 `main` 前测试必须全通过，需要在 GitHub 仓库设置中把 `CI / Verify Full Test Chain` 配置为 `main` 分支的 required status check。

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


# TODO
- [ ] 优化 constants 锻，减少内存占用
- [ ] 优化 bytecode 执行器，减少指令执行时间
- [ ] 优化 UI，增加更多功能
