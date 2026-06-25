# JS VM

一个实验性的 JavaScript 虚拟机项目，包含 JS 到 IR、IR 到 bytecode、bytecode 执行器，以及基于 wasm 的 Web 测试页面。

## Documentation

- [Architecture](ARCHITECTURE.md): 当前执行架构、API、编译器、执行器、混淆器和 IR/Bytecode 结构说明。
- [JS Fuzzer](docs/js_fuzzer.md): 生成 JS 语料，并回放到 JS VM 完整链路。

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
cargo check -p js_token_core -p js_vm_compiler -p js_vm_runtime
```

## Chain Tests

链路测试集归档在 `tests/corpus`，按 `regressions`、`syntax`、`runtime`、`obfuscation`、`fixtures` 分类组织。测试文件使用 `*.test.js` / `*.test.ts` / `*.test.jsx` / `*.test.tsx` / `*.test.vue` 命名。

```bash
npm test
```

`tests/index.js` 是唯一测试入口。默认链路会依次运行 Rust 测试、构建 wasm、执行 corpus、执行 jsvu V8/VM 可观测输出对比，并追加一轮 fuzz smoke，覆盖编译、编码、随机 seed、执行器、异常对比和 opcode 覆盖率反馈。

按需运行：

```bash
npm run test:unit
npm run test:diff
npm run test:fuzz -- --time=30s
```

## JS Fuzzer

`tests/Fuzz.js` 用作 JS 语料生成器，生成的 JS 会回放到 JS VM 的编译、编码、seed 校验和执行链路。fuzzer 会从 `tests/corpus` 派生 seed，混入边界值种子，记录 VM 可观测输出 hash 和 opcode 覆盖率反馈；如果存在 `tests/.vendor/js_fuzzer`，会优先接入 V8 `ScriptMutator` 生成变异用例。mutation DB 默认由 `tests/corpus` 下所有测试用例生成，并写入覆盖清单。

运行摘要会统计总耗时、opcode 覆盖率和测试用例重复率；重复率按生成源码的 md5 指纹计算。

快速运行：

```bash
npm run test:fuzz -- --r=1 --seeds=1 --case-log=failures
```

完整运行：

```bash
npm run test:fuzz
```

限时运行会在指定时间内尽可能多地生成和执行 fuzz case：

```bash
npm run test:fuzz -- --time=30s
```

多线程运行会并行编译和执行不同 fuzz case，默认单线程：

```bash
npm run test:fuzz -- --threads=4 --time=2m
```

多线程模式只打印周期进度；异常结果进入异步日志队列后再输出和归档，避免 console I/O 限制并发吞吐。`--case-log` 只用于单线程调试输出。

只有内部 VM 失败会默认写入 `artifacts/js_fuzzer/failures`。调试时可以追加：

```bash
npm run test:fuzz -- --log --error=5
```

`--log` 会把完整终端输出写入 `tests/.issues/<YY-MM-DD:HH:mm:ss>/log.txt`；`--error=0..5` 用来控制异常归档范围：`1` 只归档 internal failures / VM runtime timeouts / VM-only failures，`2` 增加 compile errors，`3` 增加 runtime errors / differential mismatches，`4` 增加 expected JS runtime errors，`5` 再增加 both-engine timeout / skipped。归档结果按状态写入 `tests/.issues/<YY-MM-DD:HH:mm:ss>/<status>/`，摘要写入 `tests/.issues/<YY-MM-DD:HH:mm:ss>/errors.log`。

需要清理历史归档或让错误过多时提前停止，可以追加：

```bash
npm run test:fuzz -- --clear-issues --error-limit=20
```

`--clear-issues` 会在运行前清空 `--issue-dir` 指向的历史 issues；`--error-limit` / `--max-errors` 会在 compile/runtime/expected-runtime/internal failures 达到阈值后停止继续生成 case。多线程模式下会通过共享停止标记让 worker 在当前 case 结束后退出；如果长时间没有 worker 消息，`--worker-idle-check-ms` 会定期检查 worker 是否仍然存活并输出等待日志，不再因为时间预算结束而强制终止 worker。单个 case 运行超过 `--worker-stuck-ms` 时会被判定为卡死，源码会按 md5 归档，然后终止对应 worker，避免整个 fuzz 被一个非终止 case 拖住。

已经归档到 `tests/.issues/<YY-MM-DD:HH:mm:ss>` 的异常可以按时间目录回放：

```bash
npm run test:fuzz -- --replay-issues=26-06-23:12:21:15 --replay-timeout-ms=30000
```

回放会重新执行该次 issue 目录下的源码，并在摘要中输出每个 worker 完成的 case 数。

V8 上游 `js_fuzzer` 镜像放在 `tests/.vendor/js_fuzzer`，通过下面命令更新：

```bash
npm run update:js-fuzzer
```

该更新入口复刻 `pull_v8_tool.sh` 的 sparse checkout 流程，只拉取 `tools/clusterfuzz/js_fuzzer`。

## CI

`.github/workflows/ci.yml` 会在 PR、`dev/codex`、`main` 和 merge queue 上运行完整验证：

```text
npm test
```

为了保证合并到 `main` 前测试必须全通过，需要在 GitHub 仓库设置中把 `CI / Verify Full Test Chain` 配置为 `main` 分支的 required status check。

## Git Hooks

安装本地提交钩子：

```bash
npm run hooks:install
```

之后每次 `git commit` 前会自动运行：

```text
npm test
```

任意一步失败都会阻止 commit。

## Build wasm

```bash
npm run build:wasm
```

Release 构建开启了 `opt-level = "z"`、LTO、单 codegen unit、`panic = "abort"` 和 symbol strip。
脚本会让 `wasm-pack` 先生成 web 目标，再用 `wasm-opt -Oz` 做二次体积优化。

## GitHub Pages

静态页面发布在 `gh-pages` 分支：

```text
https://open-nan.github.io/js_vm/
```


# TODO
- [ ] 将分支列为
main
  ↓ 拉分支
dev/xxx
  ↓ push 后跑单元测试
PR → test
  ↓ test 分支跑模糊测试
PR → main
  ↓ main 合并后自动部署
- [ ] 接入 JS Fuzzer 在 test 分支提起 PR 的时候运行
- [ ] 优化 UI，增加更多功能
