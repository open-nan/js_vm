# Test262 Conformance Profile

这里放 JS VM 的 Test262 适配层，不改虚拟机本体。

## 目录约定

- `../test262-runner.js`：Test262 runner，解析 frontmatter、拼接 harness、执行 VM、输出报告。
- `../update-test262.js`：拉取或刷新官方 `tc39/test262`。
- `baseline.txt`：默认执行的保守路径集合。
- `unsupported.json`：当前 VM 暂不纳入基线的 flag、feature、path。
- `reports/`：生成的 `latest.json` 和失败时的 `latest-errors.md`，不入库。
- `fixtures/`：runner 自测用的 Test262-shaped 迷你语料，不代表标准套件。

官方 Test262 checkout 放在 `tests/.vendor/test262`，该目录已被根 `.gitignore` 排除。

## 常用命令

```bash
npm run update:test262
npm run test:test262
```

默认命令只跑 `baseline.txt` 中的路径，并跳过 `unsupported.json` 中标记的能力。runner 默认使用 `--harness=light`，避开当前 VM 尚未对齐的官方 harness 初始化差异；需要精确检查官方 harness 兼容性时使用 `--official-harness`。

报告固定写入：

```text
tests/test262/reports/latest.json
```

如果出现与 Test262 期望不一致的行为，runner 退出码为 1，并额外写入：

```text
tests/test262/reports/latest-errors.md
```

控制台默认只给摘要和错误报告位置；需要逐 case 日志时加 `--verbose`。

## 扩大测试范围

按路径扩大：

```bash
npm run test:test262 -- --path=language/expressions/addition --max-cases=50
```

跑整个 checkout 的采样：

```bash
npm run test:test262 -- --all --max-cases=1000
```

临时取消 unsupported 过滤：

```bash
npm run test:test262 -- --path=built-ins/Array --no-skip-unsupported
```

使用官方 Test262 harness：

```bash
npm run test:test262 -- --path=language/literals/boolean --official-harness
```

纳入新能力时，先用 `--path` 定向跑，修复 VM 后再把路径移入 `baseline.txt`，或从 `unsupported.json` 删除对应 feature/path。

## 更新方案

1. 每周或每次跟进 TC39 新语义前运行 `npm run update:test262`。
2. 查看 `tests/.vendor/test262/.upstream` 记录的 `revision` 和 `version`。
3. 先跑默认 `npm run test:test262`，只看 `latest-errors.md`。
4. 对新标准能力用 `--path` 做定向扩展，例如 `built-ins/Array`、`language/classes`。
5. 确认 VM 支持后，把路径加入 `baseline.txt`，并删掉 `unsupported.json` 中过期的 skip。
6. 把失败的最小复现沉淀到 `tests/corpus/regressions`，再跑 `npm test`。

`TEST262_REF` 或 `JS_VM_TEST262_REF` 可以 pin 到指定 tag/commit/branch：

```bash
TEST262_REF=main npm run update:test262
```
