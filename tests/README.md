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
5. 运行 `npm test`。

以后改动库时，必须让 `npm test` 全部通过。`tests/index.js` 是唯一测试入口，会编排 Rust unit、wasm build、corpus、differential 和 fuzz smoke。

## JS Fuzzer 语料

JS Fuzzer 入口放在 `tests/Fuzz.js`。它会从 `tests/corpus` 派生 seed，混入边界值种子，并把生成的 JS 直接送入 VM，只保存失败样例：

```bash
npm run test:fuzz
```

摘要会输出总耗时、opcode 覆盖率和测试用例重复率；重复率按生成源码的 md5 指纹统计。

限时运行会在指定时间内尽可能多地生成和执行 fuzz case，支持 `ms/s/m/h` 后缀，裸数字按秒处理：

```bash
npm run test:fuzz -- --time=30s
```

多线程运行会并行编译和执行不同 fuzz case，默认单线程，也可用 `JS_VM_FUZZ_THREADS` 设置：

```bash
npm run test:fuzz -- --threads=4 --time=2m
```

多线程模式只打印周期进度；异常结果进入异步日志队列后再输出和归档，减少主线程日志瓶颈。`--case-log` 只用于单线程调试输出。

需要完整日志和异常归档时：

```bash
npm run test:fuzz -- --log --error=5
```

默认只会在出现一级 VM/internal 异常时按需写入 `tests/.issues/<YY-MM-DD:HH:mm:ss>`；没有异常不会创建空归档目录。`--log` 会输出 `tests/.issues/<YY-MM-DD:HH:mm:ss>/log.txt`，`--error=0..5` 控制归档范围：`0` 关闭异常归档，`1` internal failures / VM runtime timeouts / VM-only failures，`2` compile errors，`3` runtime errors / differential mismatches，`4` expected JS runtime errors，`5` both-engine timeout / skipped。归档源码和元数据会按状态写入 `tests/.issues/<YY-MM-DD:HH:mm:ss>/<status>/`，错误摘要写入 `tests/.issues/<YY-MM-DD:HH:mm:ss>/errors.log`。

需要把 fuzz 用例接入参考引擎做差分时：

```bash
npm run test:fuzz -- --differential --threads=4 --time=30s --log --error=3
```

差分链路是：

1. `js_fuzzer` 在内存中生成 `fuzz-N.js` 内容。
2. JS VM 先执行同一段源码，生成结构化 `actual`：`stdout`、`stderr`、`exitCode`、`signal`、`timeout`。
3. jsvu 安装的 V8 再执行同一段源码；默认查找 `$JSVU_V8_PATH`、`$JSVU_V8`、`~/.jsvu/bin/v8`。
4. 生成结构化 `expected`：`stdout`、`stderr`、`exitCode`、`signal`、`timeout`。
5. 比较 `stdout/stderr/exitCode/signal/timeout`；`stderr` 默认用首个 JS 错误摘要比较，可用 `--differential-stderr=exact` 改为精确比较，或 `ignore` 忽略。

JS VM 每个 case 由隔离 runner 执行，超过 `--vm-timeout-ms` 会 kill runner 并标记为 VM 超时；随后继续运行 V8 做分级。如果 V8 也超过 `--host-timeout-ms`，归入 `timeouts/`，不参与 expected/actual 对比；如果 V8 正常结束，归入最高等级 `runtimeTimeouts/`。VM 报错但 V8 正常结束会归入最高等级 `vmFailures/`。如果 VM 已经返回但 V8 超时，按 `timeout` 字段差异归入 `differentialFailures/`，用于捕获 VM 提前结束非终止程序的情况。V8 shell 无法解析的生成源码、模块语法、以及 `fetch/document/localStorage` 这类非可移植 host API 会标记为 skipped。差分失败会归档到 `differentialFailures/`，同源文件旁会保存 `.expected.json` 和 `.actual.json`，便于按 issues 时间目录回放定位。

需要清空历史 issue 或设置错误熔断时：

```bash
npm run test:fuzz -- --clear-issues --error-limit=20
```

`--clear-issues` 会先清空 `--issue-dir` 下的历史归档；`--error-limit` / `--max-errors` 会在 compile/runtime/expected-runtime/internal failures 达到阈值后停止继续运行。多线程 worker 会在当前 case 完成后退出；如果长时间没有 worker 消息，`--worker-idle-check-ms` 会定期检查 worker 是否仍然存活并输出等待日志，不再因为时间预算结束而强制终止 worker。单个 case 运行超过 `--worker-stuck-ms` 时会被判定为卡死，源码会按 md5 归档，然后终止对应 worker，避免整个 fuzz 被一个非终止 case 拖住。

回放某次 issues 归档：

```bash
npm run test:fuzz -- --replay-issues=26-06-23:12:21:15 --replay-timeout-ms=30000
```

`--replay-issues` 可以传 `tests/.issues` 下的时间目录名，也可以传完整目录路径。回放会逐个隔离执行归档源码，超时的 case 会被标记为 internal failure。

只有确认是 JS VM bug 并修复后，才将最小复现归档到 `tests/corpus/regressions/*.test.js`。

## Differential Test

差分测试入口：

```bash
npx jsvu --engines=v8
npm run test:diff
```

它会对 host-runnable 的 corpus 文件注入可观测输出，比较 jsvu V8 和 VM。TypeScript/JSX 与模块语法用例会跳过；未安装 jsvu V8 时测试会失败并提示安装命令。

V8 上游 `js_fuzzer` 镜像放在 `tests/.vendor/js_fuzzer`，使用下面命令更新：

```bash
npm run update:js-fuzzer
```

`tests/Fuzz.js` 会优先调用 V8 `ScriptMutator`，默认使用 `tests/db` 作为 mutation DB。DB 会从 `tests/corpus` 下所有测试用例生成并写入覆盖清单；如果 vendor 或 DB 不可用，会退回内置模板生成器。

## 提交前检查

提交或推送前运行：

```bash
npm test
```

该脚本会运行完整验证。推送和分支管理不再由测试脚本负责。

本地 commit 前可以安装 Git hook：

```bash
npm run hooks:install
```

安装后 `git commit` 会先运行 `npm test`，失败时阻止 commit。
