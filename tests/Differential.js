#!/usr/bin/env node

const { spawnSync } = require('node:child_process');
const log = require('./support/logger.js');
const { loadVm, relative, runVmSource } = require('./support/vm_chain.js');
const {
  readCorpusFiles,
  readCorpusCase,
  hostRunnable,
  observableHostSource,
} = require('./support/corpus.js');

const args = parseArgs(process.argv.slice(2));

if (args.help) {
  printHelp();
  process.exit(0);
}

main().catch((err) => {
  log.error(err?.stack || err);
  process.exit(1);
});

async function main() {
  const files = readCorpusFiles().filter(hostRunnable);
  const selected = args.maxCases > 0 ? files.slice(0, args.maxCases) : files;
  const d8Path = commandPath('d8');
  const stats = {
    passed: 0,
    skipped: files.length - selected.length,
    failed: 0,
    d8Compared: 0,
    d8Skipped: 0,
  };

  log.step(`Loading JS VM wasm packages`);
  const vm = await loadVm();
  log.ok(`Loaded JS VM wasm packages`);
  log.step(
    `Running differential tests cases=${selected.length}, node=${process.version}, d8=${d8Path || 'skip'}`,
  );

  for (const [index, file] of selected.entries()) {
    const progress = log.progressText(index + 1, selected.length);
    const name = relative(file);
    const test = readCorpusCase(file);
    if (usesHostExternals(test.source)) {
      stats.skipped += 1;
      log.jest('SKIP', name, `${progress} reason=host external`);
      continue;
    }
    const nodeSource = observableHostSource(test.source, 'node');
    if (!nodeSource) {
      stats.skipped += 1;
      log.jest('SKIP', name, `${progress} reason=no observable final expression`);
      continue;
    }

    log.jest('RUN', name, `${progress}`);
    const vmObserved = runVmObserved(vm, test.source);
    const nodeObserved = runHost('node', process.execPath, ['--input-type=module', '--eval', nodeSource]);

    const comparisons = [['node', nodeObserved]];
    if (d8Path && !/\b(?:import|export)\b/.test(test.source)) {
      const d8Source = observableHostSource(test.source, 'd8');
      comparisons.push(['d8', runHost('d8', d8Path, ['-e', d8Source])]);
      stats.d8Compared += 1;
    } else {
      stats.d8Skipped += 1;
    }

    const mismatch = comparisons.find(([, observed]) => observed.output !== vmObserved.output);
    if (mismatch) {
      stats.failed += 1;
      log.jest(
        'FAIL',
        name,
        `${progress} vm=${vmObserved.hash} ${mismatch[0]}=${mismatch[1].hash}`,
      );
      log.error(
        [
          `VM output:\n${vmObserved.output}`,
          `${mismatch[0]} output:\n${mismatch[1].output}`,
        ].join('\n\n'),
      );
      continue;
    }

    stats.passed += 1;
    log.jest(
      'PASS',
      name,
      `${progress} output=${vmObserved.hash} engines=${comparisons.map(([engine]) => engine).join(',')}`,
    );
  }

  log.summary([
    ['Differential', `${stats.passed} passed, ${stats.failed} failed, ${stats.skipped} skipped`],
    ['Engines', `node compared, d8 ${stats.d8Compared} compared / ${stats.d8Skipped} skipped`],
  ]);
  if (stats.failed > 0) process.exit(1);
}

function usesHostExternals(source) {
  return /\b(?:console|window|fetch|document|localStorage|sessionStorage)\b/.test(source);
}

function runVmObserved(vm, source) {
  const result = runVmSource(vm, source, {
    seeds: 0,
    baseSeed: 1337,
    captureLogs: true,
  });
  const lines = [...result.logs, String(result.lastResult)];
  return normalizeOutput(lines.join('\n'));
}

function runHost(engine, command, commandArgs) {
  const result = spawnSync(command, commandArgs, {
    encoding: 'utf8',
    timeout: args.timeoutMs,
    maxBuffer: 1024 * 1024,
  });
  if (result.error) {
    return normalizeOutput(`${engine} error: ${result.error.message}`);
  }
  if (result.status !== 0) {
    return normalizeOutput(`${engine} exited ${result.status}\n${result.stderr.trim()}`);
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

function commandPath(command) {
  const result = spawnSync('which', [command], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  return result.status === 0 ? result.stdout.trim() : '';
}

function parseArgs(argv) {
  const parsed = {
    maxCases: numberEnv('JS_VM_DIFF_MAX_CASES', 0),
    timeoutMs: numberEnv('JS_VM_DIFF_TIMEOUT_MS', 5000),
    help: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') parsed.help = true;
    else if (arg === '--max-cases') parsed.maxCases = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--max-cases=')) parsed.maxCases = Number.parseInt(arg.slice(12), 10);
    else if (arg === '--timeout-ms') parsed.timeoutMs = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--timeout-ms=')) parsed.timeoutMs = Number.parseInt(arg.slice(13), 10);
    else throw new Error(`unknown option: ${arg}`);
  }

  return parsed;
}

function numberEnv(name, fallback) {
  return Number.parseInt(process.env[name] || String(fallback), 10);
}

function printHelp() {
  console.log(`Usage:
  node tests/Differential.js [options]

Options:
  --max-cases <n>   Compare at most n corpus files. Default: all host-runnable cases.
  --timeout-ms <n>  Per-engine timeout. Default: 5000

Compares observable output from Node, optional d8, and JS VM. TypeScript/JSX corpus files are skipped.`);
}
