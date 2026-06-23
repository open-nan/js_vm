#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { ROOT, loadVm, relative, runVmSource } = require('./support/vm_chain.js');
const log = require('./support/logger.js');
const { CORPUS_ROOT, readCorpusFiles, readCorpusCase } = require('./support/corpus.js');

const DEFAULT_RANDOM_SEEDS = Number.parseInt(process.env.JS_VM_RANDOM_SEEDS || '8', 10);
const BASE_SEED = Number.parseInt(process.env.JS_VM_RANDOM_BASE_SEED || '1337', 10);

async function runCase(vm, file) {
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
    file,
    md5,
    bytes: Buffer.byteLength(rawSource),
    iterations: result.iterations,
    durationMs: Date.now() - started,
  };
}

async function main() {
  const files = readCorpusFiles();
  if (!files.length) {
    throw new Error(`no test files found under ${path.relative(ROOT, CORPUS_ROOT)}`);
  }

  log.step(`Loading JS VM wasm packages`);
  const vm = await loadVm();
  log.ok(`Loaded JS VM wasm packages`);
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
      result = await runCase(vm, file);
    } catch (err) {
      log.jest('FAIL', name, `${progress} md5=${fileMd5}`);
      throw err;
    }
    runs += result.iterations;
    log.jest(
      'PASS',
      name,
      `${progress} md5=${result.md5} bytes=${result.bytes} seeds=${result.iterations} time=${log.formatDuration(
        result.durationMs,
      )}`,
    );
  }
  log.summary([
    ['Test Suites', `${files.length} passed, ${files.length} total`],
    ['Tests', `${runs} passed, ${runs} total`],
    ['Time', log.formatDuration(Date.now() - suiteStarted)],
  ]);
  log.finish(`${files.length} test files, ${runs} compile/encode/run checks passed`);
}

main().catch((err) => {
  log.error(err?.stack || err);
  process.exit(1);
});
