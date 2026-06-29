#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.resolve(__dirname, '..');
const DIST = path.join(ROOT, 'dist');

const FILES = [
  ['index.html', 'index.html'],
  ['pkg/compiler/js_vm_compiler.js', 'pkg/compiler/js_vm_compiler.js'],
  ['pkg/compiler/js_vm_compiler_bg.wasm', 'pkg/compiler/js_vm_compiler_bg.wasm'],
  ['pkg/compiler/js_vm_compiler.d.ts', 'pkg/compiler/js_vm_compiler.d.ts'],
  ['pkg/compiler/js_vm_compiler_bg.wasm.d.ts', 'pkg/compiler/js_vm_compiler_bg.wasm.d.ts'],
  ['pkg/compiler/package.json', 'pkg/compiler/package.json'],
  ['pkg/executor/js_vm_runtime.js', 'pkg/executor/js_vm_runtime.js'],
  ['pkg/executor/js_vm_runtime_bg.wasm', 'pkg/executor/js_vm_runtime_bg.wasm'],
  ['pkg/executor/js_vm_runtime.d.ts', 'pkg/executor/js_vm_runtime.d.ts'],
  ['pkg/executor/js_vm_runtime_bg.wasm.d.ts', 'pkg/executor/js_vm_runtime_bg.wasm.d.ts'],
  ['pkg/executor/package.json', 'pkg/executor/package.json'],
];

main();

function main() {
  if (process.argv.includes('--verify')) {
    verifyPayload();
    return;
  }

  fs.rmSync(DIST, { recursive: true, force: true });
  for (const [from, to] of FILES) {
    const source = path.join(ROOT, from);
    const target = path.join(DIST, to);
    if (!fs.existsSync(source)) {
      throw new Error(`missing site payload file: ${from}`);
    }
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.copyFileSync(source, target);
  }
  fs.writeFileSync(path.join(DIST, '.nojekyll'), '');

  for (const file of walk(DIST)) {
    console.log(path.relative(ROOT, file));
  }
}

function verifyPayload() {
  for (const [, to] of FILES) {
    const target = path.join(DIST, to);
    if (!fs.existsSync(target)) {
      throw new Error(`missing dist payload file: ${path.relative(ROOT, target)}`);
    }
  }
  for (const file of walk(DIST)) {
    console.log(path.relative(ROOT, file));
  }
}

function walk(dir) {
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .flatMap((entry) => {
      const fullPath = path.join(dir, entry.name);
      return entry.isDirectory() ? walk(fullPath) : [fullPath];
    })
    .sort();
}
