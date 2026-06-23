# JS Fuzzer

本项目现在使用 `tests/Fuzz.js` 生成 JS 用例，并直接送入 JS VM 完整链路：

```text
JS source -> IR -> Bytecode -> bytes -> seed -> Runtime
```

默认模式不写临时 JS 文件，只在发现内部 VM 失败时把用例保存到 `artifacts/js_fuzzer/failures`。

## 运行

```bash
npm run fuzz
```

快速 smoke：

```bash
npm run fuzz:quick
```

自定义数量和 seed：

```bash
node tests/Fuzz.js --r=40 --seeds=4 --base-seed=20260622 --max-generated-bytes=8192
```

## 失败归档

默认只保存内部 VM 问题，例如：

- wasm trap
- memory access out of bounds
- invalid bytecode / seed mismatch
- bad operand / bad index
- Rust panic 或 unreachable

如果需要把编译错误或 JS 运行期错误也收集起来：

```bash
node tests/Fuzz.js \
  --r=40 \
  --save-compile-errors \
  --save-runtime-errors
```

确认是 JS VM bug 后：

1. 从 `artifacts/js_fuzzer/failures` 取失败 JS 和旁边的 `.json` 元数据。
2. 最小化为可读复现。
3. 修复 VM。
4. 将最小复现加入 `tests/corpus/regressions/*.test.js`，补 `// @expect`。
5. 运行 `npm run verify`。

## V8 js_fuzzer 源码

`tests/.vendor/js_fuzzer` 保存 V8 `tools/clusterfuzz/js_fuzzer` 的源码镜像。`tests/Fuzz.js` 会优先把它作为库接入：

- 使用 `ScriptMutator` 变异 `tests/corpus` 中的端到端样例。
- 默认复用 `tests/db` 作为 mutation DB；缺失或覆盖清单不匹配时会从 `tests/corpus` 全量重建。
- 如果 vendor 缺失或 DB 不可用，会退回内置模板生成器。
- 上游 js_fuzzer 可能生成当前 VM 尚不支持的运行时语义，因此默认只把 VM internal failure 持久化为失败样例。

更新镜像：

```bash
npm run update:js-fuzzer
```

当前项目级入口仍是 `tests/Fuzz.js`，避免把上游工具的 corpus/db、打包脚本和临时输出目录耦合进 VM 验证链路。

Fuzzer 会默认：

- 从 `tests/corpus` 读取端到端测试并派生 deterministic seed。
- 混入 `0 / 1 / -1 / i32 / safe integer` 等边界值种子。
- 跳过超过 `--max-generated-bytes` 的生成用例。
- 记录 VM 可观测输出 hash 和 bytecode opcode 覆盖率反馈。

常用调试参数：

```bash
node tests/Fuzz.js --r=4 --rebuild-js-fuzzer-db
node tests/Fuzz.js --r=4 --no-js-fuzzer
node tests/Fuzz.js --r=4 --save-runtime-errors
```

更新入口会使用和 `pull_v8_tool.sh` 相同的 sparse checkout 思路，只同步 V8 的 `tools/clusterfuzz/js_fuzzer` 子目录。
