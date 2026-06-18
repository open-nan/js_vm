# JS VM Chain Tests

`tests` 是端到端链路测试集，按语义分类直接组织：

- `regressions/`：修 bug 后沉淀的最小复现。
- `syntax/`：JS/TS 语法覆盖。
- `runtime/`：对象、函数、异常、模块等运行时语义。
- `obfuscation/`：seed / opcode / extern 混淆链路。
- `fixtures/`：复杂综合样例。

每个测试文件都会经过：

1. JS/TS source -> IR
2. IR -> Bytecode
3. Bytecode -> bytes
4. 默认编码 seed 执行
5. 多组确定性随机 seed 重新编码并执行

## 文件命名

测试文件使用以下后缀：

- `*.test.js`
- `*.test.ts`
- `*.test.jsx`
- `*.test.tsx`
- `*.test.vue`

Vue 测试会抽取第一个 `<script>` 块。

## 元信息

每个测试文件必须声明期望返回值：

```js
// @expect 21
// @seeds 12
```

- `@expect`：执行器最终返回值的字符串形式。
- `@seeds`：随机 seed 轮数；实际会额外跑 1 次默认 seed。

## 异常归档流程

当遇到新的异常：

1. 先判断是否是 JS VM 的 bug，还是当前尚未支持的 JS 语义。
2. 如果是 bug，修复实现。
3. 将最小复现代码保存到 `tests/regressions/*.test.js`。
4. 添加或调整 `@expect`。
5. 运行 `sh scripts/verify.sh`。

以后改动库时，必须让 `cargo test` 和 `node tests/index.js` 全部通过。

## 推送流程

本地提交后运行：

```bash
sh scripts/verify-and-push-dev-codex.sh
```

脚本会先运行完整验证，全部通过且工作区干净后，将当前 `HEAD` 推送到 `origin/dev/codex`。
