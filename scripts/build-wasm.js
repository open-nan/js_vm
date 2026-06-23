#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const log = require('../tests/support/logger.js');

const ROOT = path.resolve(__dirname, '..');

main();

function main() {
  process.chdir(ROOT);

  log.step('Building wasm packages');
  log.info('Cleaning pkg/compiler and pkg/executor');
  fs.rmSync(path.join(ROOT, 'pkg/compiler'), { recursive: true, force: true });
  fs.rmSync(path.join(ROOT, 'pkg/executor'), { recursive: true, force: true });

  run('compiler wasm', 'wasm-pack', [
    'build',
    'crates/compiler',
    '--target',
    'web',
    '--out-dir',
    '../../pkg/compiler',
    '--release',
  ]);
  run('executor wasm', 'wasm-pack', [
    'build',
    'crates/runtime',
    '--target',
    'web',
    '--out-dir',
    '../../pkg/executor',
    '--release',
  ]);

  const wasmOpt = findWasmOpt();
  if (wasmOpt) {
    log.info(`Using wasm-opt: ${wasmOpt}`);
    optimizeWasm(wasmOpt, 'pkg/compiler/js_vm_compiler_bg.wasm');
    optimizeWasm(wasmOpt, 'pkg/executor/js_vm_runtime_bg.wasm');
  } else {
    log.warn('wasm-opt not found; wasm output was built but not post-optimized');
  }

  patchWasmBindgenJs('pkg/compiler/js_vm_compiler.js');
  patchWasmBindgenJs('pkg/executor/js_vm_runtime.js');

  printSizes([
    'pkg/compiler/js_vm_compiler_bg.wasm',
    'pkg/compiler/js_vm_compiler.js',
    'pkg/executor/js_vm_runtime_bg.wasm',
    'pkg/executor/js_vm_runtime.js',
  ]);
  log.finish('Wasm build completed');
}

function patchWasmBindgenJs(file) {
  const fullPath = path.join(ROOT, file);
  const source = fs.readFileSync(fullPath, 'utf8');
  const patched = source.replace(
    /(\n\s*)let (deferred\d+_0);\n(\s*)let (deferred\d+_1);/g,
    (_match, firstIndent, firstName, secondIndent, secondName) =>
      `${firstIndent}let ${firstName} = 0;\n${secondIndent}let ${secondName} = 0;`,
  );
  if (patched !== source) {
    fs.writeFileSync(fullPath, patched);
    log.info(`Patched wasm-bindgen deferred frees in ${file}`);
  }
}

function optimizeWasm(wasmOpt, file) {
  run(`optimize ${file}`, wasmOpt, [
    file,
    '-Oz',
    '--enable-bulk-memory',
    '--enable-nontrapping-float-to-int',
    '-o',
    file,
  ]);
}

function findWasmOpt() {
  if (process.env.WASM_OPT && isExecutable(process.env.WASM_OPT)) return process.env.WASM_OPT;
  const fromPath = commandPath('wasm-opt');
  if (fromPath) return fromPath;
  const cached = path.join(
    process.env.HOME || '',
    'Library/Caches/.wasm-pack/wasm-opt-50385c9e73ccee70/bin/wasm-opt',
  );
  return isExecutable(cached) ? cached : '';
}

function commandPath(command) {
  const result = spawnSync('which', [command], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  return result.status === 0 ? result.stdout.trim() : '';
}

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function printSizes(files) {
  log.step('Wasm package sizes');
  let total = 0;
  for (const file of files) {
    const fullPath = path.join(ROOT, file);
    const size = fs.statSync(fullPath).size;
    const md5 = log.md5File(fullPath);
    total += size;
    log.info(`${String(size).padStart(8)} md5=${md5} ${file}`);
  }
  log.info(`${String(total).padStart(8)} total`);
}

function run(label, command, args) {
  const started = Date.now();
  log.step(label);
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: process.env,
    stdio: 'inherit',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with code ${result.status}`);
  }
  log.ok(`${label} completed in ${log.formatDuration(Date.now() - started)}`);
}
