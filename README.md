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
- [ ] 为 Seeb 加盐，在构建时为compiler，executor 添加鸿，添加一个参数用来将盐加到编译器和执行器中，他们编译器会根据Seeb的盐进行编译，执行器会根据Seeb的盐进行执行，只有Seeb的盐才能执行
- [ ] 优化js编辑、hex、 Bytecode大文本渲染，借鉴虚拟列表渲染机制
- [ ] 优化UI 去除offset行，滚动只在字节码区域
- [ ] 修复 gh-pages 中缺少 pkg 包
