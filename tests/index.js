#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

const ROOT = path.resolve(__dirname, '..');
const TEST_ROOT = __dirname;
const DEFAULT_RANDOM_SEEDS = Number.parseInt(process.env.JS_VM_RANDOM_SEEDS || '8', 10);
const BASE_SEED = Number.parseInt(process.env.JS_VM_RANDOM_BASE_SEED || '1337', 10);

const OPCODES = [
  'MARKER',
  'LABEL',
  'DECLARE',
  'LOAD_CONST',
  'LOAD_NAME',
  'STORE_NAME',
  'STORE_MEMBER',
  'MOVE',
  'BINARY',
  'UNARY',
  'MEMBER',
  'ARRAY',
  'OBJECT',
  'CALL',
  'NEW',
  'TEMPLATE',
  'FUNCTION_START',
  'FUNCTION_END',
  'FUNCTION_EXPR_START',
  'FUNCTION_EXPR_END',
  'CLASS',
  'IMPORT',
  'EXPORT',
  'THROW',
  'TRY_START',
  'CATCH_START',
  'FINALLY_START',
  'TRY_END',
  'RETURN',
  'POP',
  'JUMP',
  'JUMP_IF_FALSE',
  'UNSUPPORTED',
  'LOAD_CONST_CONST',
  'POP_REG',
  'CALL_1',
];

const OPERAND_TAGS = [
  'register',
  'constant',
  'name',
  'extern',
  'label',
  'count',
  'none',
  'function',
];

const CONSTANT_TAGS = ['number', 'string', 'bool', 'null', 'undefined'];
const TEST_FILE_RE = /\.test\.(?:js|ts|jsx|tsx|vue|vue\.tsx|vue\.jsx)$/;

function readCases(dir = TEST_ROOT) {
  if (!fs.existsSync(dir)) return [];
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  return entries
    .flatMap((entry) => {
      const fullPath = path.join(dir, entry.name);
      return entry.isDirectory() ? readCases(fullPath) : [fullPath];
    })
    .filter((file) => TEST_FILE_RE.test(file))
    .sort();
}

function parseMeta(source, file) {
  const meta = {};
  for (const line of source.split(/\r?\n/)) {
    const match = line.match(/^\s*(?:\/\/|<!--)\s*@([a-zA-Z_-]+)\s*(.*?)\s*(?:-->)?\s*$/);
    if (!match) continue;
    meta[match[1]] = match[2];
  }
  if (!Object.prototype.hasOwnProperty.call(meta, 'expect')) {
    throw new Error(`${relative(file)} is missing // @expect <value>`);
  }
  return {
    expect: meta.expect,
    seeds: meta.seeds ? Number.parseInt(meta.seeds, 10) : DEFAULT_RANDOM_SEEDS,
  };
}

function sourceForCompiler(source, file) {
  if (!/\.vue(?:\.[tj]sx?)?$/.test(file)) return source;
  const script = source.match(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/i);
  if (!script) {
    throw new Error(`${relative(file)} does not contain a <script> block`);
  }
  return script[1];
}

function makePrng(seed) {
  let state = seed >>> 0;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return state >>> 0;
  };
}

function shuffled(values, rand) {
  const next = values.slice();
  for (let index = next.length - 1; index > 0; index -= 1) {
    const swapIndex = rand() % (index + 1);
    [next[index], next[swapIndex]] = [next[swapIndex], next[index]];
  }
  return next;
}

function seedRows(index) {
  if (index === 0) {
    return {
      label: 'default',
      opcodes: OPCODES,
      operandTags: OPERAND_TAGS,
      constantTags: CONSTANT_TAGS,
    };
  }
  const rand = makePrng((BASE_SEED + index * 0x9e3779b9) >>> 0);
  return {
    label: `random-${index}`,
    opcodes: shuffled(OPCODES, rand),
    operandTags: shuffled(OPERAND_TAGS, rand),
    constantTags: shuffled(CONSTANT_TAGS, rand),
  };
}

function externSlotsForIteration(externs, iteration) {
  if (iteration === 0 || externs.length < 2) return externs;
  const rand = makePrng((BASE_SEED ^ (iteration * 0x85ebca6b)) >>> 0);
  return shuffled(externs, rand);
}

function relative(file) {
  return path.relative(ROOT, file);
}

async function loadVm() {
  const compilerPath = path.join(ROOT, 'pkg/compiler/js_vm_compiler.js');
  const compilerWasmPath = path.join(ROOT, 'pkg/compiler/js_vm_compiler_bg.wasm');
  const runtimePath = path.join(ROOT, 'pkg/executor/js_vm_runtime.js');
  const runtimeWasmPath = path.join(ROOT, 'pkg/executor/js_vm_runtime_bg.wasm');

  for (const file of [compilerPath, compilerWasmPath, runtimePath, runtimeWasmPath]) {
    if (!fs.existsSync(file)) {
      throw new Error(`${relative(file)} is missing; run sh scripts/build-wasm.sh first`);
    }
  }

  globalThis.__jsVmHostLog = globalThis.__jsVmHostLog || (() => {});

  const compilerPkg = await import(pathToFileURL(compilerPath).href);
  const runtimePkg = await import(pathToFileURL(runtimePath).href);
  await compilerPkg.default({ module_or_path: fs.readFileSync(compilerWasmPath) });
  await runtimePkg.default({ module_or_path: fs.readFileSync(runtimeWasmPath) });
  return {
    Compiler: compilerPkg.Compiler,
    js_encoding_seed_from_rows: compilerPkg.js_encoding_seed_from_rows,
    js_encoding_seed_for_seed_and_bytes: compilerPkg.js_encoding_seed_for_seed_and_bytes,
    js_execute_bytes_with_seed: runtimePkg.js_execute_bytes_with_seed,
  };
}

async function runCase(vm, file) {
  const rawSource = fs.readFileSync(file, 'utf8');
  const meta = parseMeta(rawSource, file);
  const source = sourceForCompiler(rawSource, file);
  const iterations = Math.max(1, meta.seeds + 1);
  const compiler = new vm.Compiler(source);
  const externs = Array.from(compiler.extern_slots());

  for (let iteration = 0; iteration < iterations; iteration += 1) {
    const rows = seedRows(iteration);
    const configSeed = vm.js_encoding_seed_from_rows(
      rows.opcodes,
      rows.operandTags,
      rows.constantTags,
      new Uint8Array(),
    );
    const externSlots = externSlotsForIteration(externs, iteration);
    const artifact = compiler.to_bytecode_artifact(configSeed, externSlots);
    const bytes = artifact.bytes();
    const seed = vm.js_encoding_seed_for_seed_and_bytes(configSeed, bytes);
    const result = vm.js_execute_bytes_with_seed(bytes, seed);
    artifact.free();

    if (result !== meta.expect) {
      throw new Error(
        `${relative(file)} failed on ${rows.label}: expected ${JSON.stringify(meta.expect)}, got ${JSON.stringify(result)}\nseed=${seed}`,
      );
    }
  }

  compiler.free();
  return { file, iterations };
}

async function main() {
  const files = readCases();
  if (!files.length) {
    throw new Error(`no test files found under ${relative(TEST_ROOT)}`);
  }

  const vm = await loadVm();
  let runs = 0;
  for (const file of files) {
    const result = await runCase(vm, file);
    runs += result.iterations;
    console.log(`ok ${relative(file)} (${result.iterations} seeds)`);
  }
  console.log(`\n${files.length} test files, ${runs} compile/encode/run checks passed`);
}

main().catch((err) => {
  console.error(err?.stack || err);
  process.exit(1);
});
