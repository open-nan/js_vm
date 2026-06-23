# JS VM Chain Tests

`tests/corpus` 是端到端链路测试集，按语义分类直接组织：

- `corpus/regressions/`：修 bug 后沉淀的最小复现。
- `corpus/syntax/`：JS/TS 语法覆盖。
- `corpus/runtime/`：对象、函数、异常、模块等运行时语义。
- `corpus/obfuscation/`：seed / opcode / extern 混淆链路。
- `corpus/fixtures/`：复杂综合样例。

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
3. 将最小复现代码保存到 `tests/corpus/regressions/*.test.js`。
4. 添加或调整 `@expect`。
5. 运行 `npm run verify`。

以后改动库时，必须让 `cargo test`、`node tests/E2E.js` 和 `node tests/Differential.js` 全部通过。

## JS Fuzzer 语料

JS Fuzzer 入口放在 `tests/Fuzz.js`。它会从 `tests/corpus` 派生 seed，混入边界值种子，并把生成的 JS 直接送入 VM，只保存失败样例：

```bash
npm run fuzz
```

摘要会输出总耗时、opcode 覆盖率和测试用例重复率；重复率按生成源码的 md5 指纹统计。

限时运行会在指定时间内尽可能多地生成和执行 fuzz case，支持 `ms/s/m/h` 后缀，裸数字按秒处理：

```bash
npm run fuzz -- --time=30s
```

多线程运行会并行编译和执行不同 fuzz case，默认单线程，也可用 `JS_VM_FUZZ_THREADS` 设置：

```bash
npm run fuzz -- --threads=4 --time=2m
```

多线程模式只打印周期进度；异常结果进入异步日志队列后再输出和归档，减少主线程日志瓶颈。`--case-log` 只用于单线程调试输出。

需要完整日志和异常归档时：

```bash
npm run fuzz -- --log --error=5
```

`--log` 会输出 `tests/issues/<YY-MM-DD:HH:mm:ss>/log.txt`，`--error=0..5` 控制归档范围：`1` internal failures，`2` compile errors，`3` runtime errors，`4` expected JS runtime errors，`5` skipped。归档源码和元数据会按状态写入 `tests/issues/<YY-MM-DD:HH:mm:ss>/<status>/`，错误摘要写入 `tests/issues/<YY-MM-DD:HH:mm:ss>/errors.log`。

需要清空历史 issue 或设置错误熔断时：

```bash
npm run fuzz -- --clear-issues --error-limit=20
```

`--clear-issues` 会先清空 `--issue-dir` 下的历史归档；`--error-limit` / `--max-errors` 会在 compile/runtime/expected-runtime/internal failures 达到阈值后停止继续运行。多线程 worker 会在当前 case 完成后退出；如果长时间没有 worker 消息，`--worker-idle-check-ms` 会定期检查 worker 是否仍然存活并输出等待日志，不再因为时间预算结束而强制终止 worker。单个 case 运行超过 `--worker-stuck-ms` 时会被判定为卡死，源码会按 md5 归档，然后终止对应 worker，避免整个 fuzz 被一个非终止 case 拖住。

回放某次 issues 归档：

```bash
npm run fuzz -- --replay-issues=26-06-23:12:21:15 --replay-timeout-ms=30000
```

`--replay-issues` 可以传 `tests/issues` 下的时间目录名，也可以传完整目录路径。回放会逐个隔离执行归档源码，超时的 case 会被标记为 internal failure。

只有确认是 JS VM bug 并修复后，才将最小复现归档到 `tests/corpus/regressions/*.test.js`。

## Differential Test

差分测试入口：

```bash
npm run differential
```

它会对 host-runnable 的 corpus 文件注入可观测输出，比较 Node、可选 d8 和 VM。TypeScript/JSX 用例会跳过，未安装 d8 时只比较 Node 和 VM。

V8 上游 `js_fuzzer` 镜像放在 `tests/.vendor/js_fuzzer`，使用下面命令更新：

```bash
npm run update:js-fuzzer
```

`tests/Fuzz.js` 会优先调用 V8 `ScriptMutator`，默认使用 `tests/db` 作为 mutation DB。DB 会从 `tests/corpus` 下所有测试用例生成并写入覆盖清单；如果 vendor 或 DB 不可用，会退回内置模板生成器。

## 提交前检查

提交或推送前运行：

```bash
npm run verify
```

该脚本会运行完整验证。推送和分支管理不再由测试脚本负责。

本地 commit 前可以安装 Git hook：

```bash
npm run hooks:install
```

安装后 `git commit` 会先运行 `npm run verify` 和 `npm run fuzz:quick`，失败时阻止 commit。
