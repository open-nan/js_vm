#!/usr/bin/env node

const { spawnSync } = require('node:child_process');
const path = require('node:path');
const log = require('./support/logger.js');
const { ROOT, loadVm, relative, runVmSource } = require('./support/vm_chain.js');
const {
  CORPUS_ROOT,
  readCorpusFiles,
  readCorpusCase,
  hostRunnable,
  observableHostSource,
} = require('./support/corpus.js');
const { requireJsvuV8Path, v8EvalArgs } = require('./support/reference_engine.js');

const DEFAULT_RANDOM_SEEDS = Number.parseInt(process.env.JS_VM_RANDOM_SEEDS || '8', 10);
const BASE_SEED = Number.parseInt(process.env.JS_VM_RANDOM_BASE_SEED || '1337', 10);

const rawArgs = process.argv.slice(2);
const command = normalizeCommand(rawArgs[0] && !rawArgs[0].startsWith('-') ? rawArgs.shift() : 'all');

main(command, rawArgs).catch((err) => {
  log.error(err?.stack || err);
  process.exit(err?.exitCode || 1);
});

async function main(nextCommand, args) {
  if (nextCommand === 'help') {
    printHelp();
    return;
  }

  if (nextCommand === 'all' || nextCommand === 'precommit') {
    await runAll(nextCommand);
    return;
  }
  if (nextCommand === 'unit') {
    await runUnit();
    return;
  }
  if (nextCommand === 'corpus') {
    await runCorpusSuite();
    return;
  }
  if (nextCommand === 'diff') {
    await runDifferentialSuite(parseDifferentialArgs(args));
    return;
  }
  if (nextCommand === 'fuzz') {
    runFuzz(args);
    return;
  }
  if (nextCommand === 'fuzz-diff') {
    runFuzz(['--differential', ...args]);
    return;
  }
  if (nextCommand === 'fuzz-smoke') {
    runFuzz(['--r=1', '--seeds=1', '--case-log=failures', ...args]);
    return;
  }
  if (nextCommand === 'update-js-fuzzer') {
    runCommand('Update js_fuzzer', process.execPath, ['tests/update-js-fuzzer.js', ...args]);
    return;
  }

  throw new Error(`unknown test command: ${nextCommand}`);
}

async function runAll(mode) {
  log.step(mode === 'precommit' ? 'Running pre-commit checks' : 'Running full test chain');
  await runStep('Rust unit tests', () => runCommand('Rust unit tests', 'cargo', ['test']));
  await runStep('Build wasm packages', () => runCommand('Build wasm packages', process.execPath, ['scripts/build-wasm.js']));
  await runStep('JS VM corpus', () => runCorpusSuite());
  await runStep('Differential tests', () => runDifferentialSuite(parseDifferentialArgs([])));
  await runStep('Fuzz smoke', () => runFuzz(['--r=1', '--seeds=1', '--case-log=failures']));
  log.finish(mode === 'precommit' ? 'Pre-commit checks passed' : 'Full test chain passed');
}

async function runUnit() {
  log.step('Running unit test chain');
  await runStep('Rust unit tests', () => runCommand('Rust unit tests', 'cargo', ['test']));
  await runStep('Build wasm packages', () => runCommand('Build wasm packages', process.execPath, ['scripts/build-wasm.js']));
  await runStep('JS VM corpus', () => runCorpusSuite());
  log.finish('Unit test chain passed');
}

async function runStep(label, run) {
  const started = Date.now();
  log.jest('RUN', label, '');
  await run();
  log.jest('PASS', label, `time=${log.formatDuration(Date.now() - started)}`);
}

function runCommand(label, command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: process.env,
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw Object.assign(
      new Error(`${label} failed: ${command} ${args.join(' ')} exited with code ${result.status}`),
      { exitCode: result.status || 1 },
    );
  }
}

async function runCorpusSuite() {
  const files = readCorpusFiles();
  if (!files.length) {
    throw new Error(`no test files found under ${path.relative(ROOT, CORPUS_ROOT)}`);
  }

  log.step('Loading JS VM wasm packages');
  const vm = await loadVm();
  log.ok('Loaded JS VM wasm packages');
  log.step(`Running ${files.length} JS VM corpus files`);
  const suiteStarted = Date.now();
  let runs = 0;

  for (const [index, file] of files.entries()) {
    const name = relative(file);
    const progress = log.progressText(index + 1, files.length);
    const fileMd5 = log.md5File(file);
    log.jest('RUN', name, `${progress} md5=${fileMd5}`);
    let result;
    try {
      result = runCorpusCase(vm, file);
    } catch (err) {
      log.jest('FAIL', name, `${progress} md5=${fileMd5}`);
      throw err;
    }
    runs += result.iterations;
    log.jest(
      'PASS',
      name,
      `${progress} md5=${result.md5} bytes=${result.bytes} seeds=${result.iterations} time=${log.formatDuration(result.durationMs)}`,
    );
  }

  log.summary([
    ['Test Suites', `${files.length} passed, ${files.length} total`],
    ['Tests', `${runs} passed, ${runs} total`],
    ['Time', log.formatDuration(Date.now() - suiteStarted)],
  ]);
  log.finish(`${files.length} test files, ${runs} compile/encode/run checks passed`);
}

function runCorpusCase(vm, file) {
  const started = Date.now();
  const { rawSource, source, meta } = readCorpusCase(file);
  const md5 = log.md5Text(rawSource);
  const result = runVmSource(vm, source, {
    seeds: meta.seeds ?? DEFAULT_RANDOM_SEEDS,
    baseSeed: BASE_SEED,
    expect: meta.expect,
    id: relative(file),
  });
  return {
    md5,
    bytes: Buffer.byteLength(rawSource),
    iterations: result.iterations,
    durationMs: Date.now() - started,
  };
}

async function runDifferentialSuite(args) {
  const files = readCorpusFiles().filter(hostRunnable);
  const selected = args.maxCases > 0 ? files.slice(0, args.maxCases) : files;
  const v8Path = requireJsvuV8Path(args.v8Path);
  const stats = {
    passed: 0,
    skipped: files.length - selected.length,
    failed: 0,
    v8Compared: 0,
  };

  log.step('Loading JS VM wasm packages');
  const vm = await loadVm();
  log.ok('Loaded JS VM wasm packages');
  log.step(`Running differential tests cases=${selected.length}, reference=v8(${v8Path})`);

  for (const [index, file] of selected.entries()) {
    const progress = log.progressText(index + 1, selected.length);
    const name = relative(file);
    const test = readCorpusCase(file);
    const skipReason = differentialSkipReason(test.source);
    if (skipReason) {
      stats.skipped += 1;
      log.jest('SKIP', name, `${progress} reason=${skipReason}`);
      continue;
    }

    const v8Source = observableHostSource(test.source, 'v8');
    if (!v8Source) {
      stats.skipped += 1;
      log.jest('SKIP', name, `${progress} reason=no observable final expression`);
      continue;
    }

    log.jest('RUN', name, `${progress}`);
    const vmObserved = runVmObserved(vm, test.source);
    const v8Observed = runHost('v8', v8Path, v8EvalArgs(v8Source, v8Path), args.timeoutMs);
    stats.v8Compared += 1;

    if (v8Observed.output !== vmObserved.output) {
      stats.failed += 1;
      log.jest('FAIL', name, `${progress} vm=${vmObserved.hash} v8=${v8Observed.hash}`);
      log.error([`VM output:\n${vmObserved.output}`, `V8 output:\n${v8Observed.output}`].join('\n\n'));
      continue;
    }

    stats.passed += 1;
    log.jest('PASS', name, `${progress} output=${vmObserved.hash} engine=v8`);
  }

  log.summary([
    ['Differential', `${stats.passed} passed, ${stats.failed} failed, ${stats.skipped} skipped`],
    ['Engines', `v8 ${stats.v8Compared} compared`],
  ]);
  if (stats.failed > 0) {
    throw Object.assign(new Error(`${stats.failed} differential case(s) failed`), { exitCode: 1 });
  }
}

function differentialSkipReason(source) {
  if (/\b(?:console|window|fetch|document|localStorage|sessionStorage)\b/.test(source)) {
    return 'host external';
  }
  if (/\b(?:import|export)\b/.test(source)) {
    return 'v8 shell module syntax';
  }
  return '';
}

function runVmObserved(vm, source) {
  const result = runVmSource(vm, source, {
    seeds: 0,
    baseSeed: BASE_SEED,
    captureLogs: true,
  });
  const lines = [...result.logs, String(result.lastResult)];
  return normalizeOutput(lines.join('\n'));
}

function runHost(engine, command, commandArgs, timeoutMs) {
  const result = spawnSync(command, commandArgs, {
    encoding: 'utf8',
    timeout: timeoutMs,
    maxBuffer: 1024 * 1024,
  });
  if (result.error) {
    return normalizeOutput(`${engine} error: ${result.error.message}`);
  }
  if (result.status !== 0) {
    return normalizeOutput(`${engine} exited ${result.status}\n${String(result.stderr || '').trim()}`);
  }
  return normalizeOutput(result.stdout);
}

function normalizeOutput(value) {
  const output = String(value).replace(/\r\n/g, '\n').replace(/\n+$/g, '');
  return {
    output,
    hash: log.md5Text(output).slice(0, 12),
  };
}

function runFuzz(args) {
  runCommand('Fuzz tests', process.execPath, ['tests/Fuzz.js', ...args]);
}

function parseDifferentialArgs(argv) {
  const parsed = {
    maxCases: numberEnv('JS_VM_DIFF_MAX_CASES', 0),
    timeoutMs: numberEnv('JS_VM_DIFF_TIMEOUT_MS', 5000),
    v8Path: process.env.JSVU_V8_PATH || process.env.JSVU_V8 || '',
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--max-cases') parsed.maxCases = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--max-cases=')) parsed.maxCases = Number.parseInt(arg.slice(12), 10);
    else if (arg === '--timeout-ms') parsed.timeoutMs = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--timeout-ms=')) parsed.timeoutMs = Number.parseInt(arg.slice(13), 10);
    else if (arg === '--v8-path') parsed.v8Path = argv[++index];
    else if (arg.startsWith('--v8-path=')) parsed.v8Path = arg.slice(10);
    else throw new Error(`unknown diff option: ${arg}`);
  }

  return parsed;
}

function numberEnv(name, fallback) {
  return Number.parseInt(process.env[name] || String(fallback), 10);
}

function normalizeCommand(command) {
  const value = String(command || 'all').trim().toLowerCase();
  const aliases = new Map([
    ['all', 'all'],
    ['verify', 'all'],
    ['ci', 'all'],
    ['precommit', 'precommit'],
    ['unit', 'unit'],
    ['corpus', 'corpus'],
    ['e2e', 'corpus'],
    ['diff', 'diff'],
    ['differential', 'diff'],
    ['fuzz', 'fuzz'],
    ['fuzz:diff', 'fuzz-diff'],
    ['fuzz-diff', 'fuzz-diff'],
    ['fuzz:differential', 'fuzz-diff'],
    ['fuzz-smoke', 'fuzz-smoke'],
    ['quick', 'fuzz-smoke'],
    ['update-js-fuzzer', 'update-js-fuzzer'],
    ['update:fuzzer', 'update-js-fuzzer'],
    ['help', 'help'],
    ['--help', 'help'],
    ['-h', 'help'],
  ]);
  return aliases.get(value) || value;
}

function printHelp() {
  console.log(`Usage:
  node tests/index.js [command] [options]

Commands:
  all                 Rust tests -> wasm build -> corpus -> differential -> fuzz smoke. Default.
  unit                Rust tests -> wasm build -> corpus.
  corpus              Run tests/corpus through compile/encode/seed/runtime.
  diff                Compare corpus observable output with jsvu V8.
  fuzz                Run the in-memory JS fuzzer. Pass Fuzz.js options after the command.
  fuzz:diff           Run fuzz with --differential.
  fuzz-smoke          Run a short fuzz smoke: --r=1 --seeds=1 --case-log=failures.
  precommit           Same chain as all, intended for git hooks.
  update-js-fuzzer    Update tests/.vendor/js_fuzzer.

Examples:
  npm test
  npm run test:unit
  npm run test:diff -- --timeout-ms=1000
  npm run test:fuzz -- --threads=8 --time=30s --error=3
  npm run test:fuzz -- --differential --threads=8 --time=30s`);
}
