const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

const ROOT = path.resolve(__dirname, '../..');

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
  'ENTER_SCOPE',
  'LEAVE_SCOPE',
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

class JsVmAdapter {
  constructor(packages) {
    Object.assign(this, packages);
  }

  static async load() {
    const packages = await loadVmPackages();
    return new JsVmAdapter(packages);
  }

  runSource(source, options = {}) {
    return runVmSource(this, source, options);
  }
}

function relative(file) {
  return path.relative(ROOT, file);
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

function seedRows(index, baseSeed = 1337) {
  if (index === 0) {
    return {
      label: 'default',
      opcodes: OPCODES,
      operandTags: OPERAND_TAGS,
      constantTags: CONSTANT_TAGS,
    };
  }
  const rand = makePrng((baseSeed + index * 0x9e3779b9) >>> 0);
  return {
    label: `random-${index}`,
    opcodes: shuffled(OPCODES, rand),
    operandTags: shuffled(OPERAND_TAGS, rand),
    constantTags: shuffled(CONSTANT_TAGS, rand),
  };
}

function externSlotsForIteration(externs, iteration, baseSeed = 1337) {
  if (iteration === 0 || externs.length < 2) return externs;
  const rand = makePrng((baseSeed ^ (iteration * 0x85ebca6b)) >>> 0);
  return shuffled(externs, rand);
}

async function loadVm() {
  return JsVmAdapter.load();
}

async function loadVmPackages() {
  const compilerPath = path.join(ROOT, 'pkg/compiler/js_vm_compiler.js');
  const compilerWasmPath = path.join(ROOT, 'pkg/compiler/js_vm_compiler_bg.wasm');
  const runtimePath = path.join(ROOT, 'pkg/executor/js_vm_runtime.js');
  const runtimeWasmPath = path.join(ROOT, 'pkg/executor/js_vm_runtime_bg.wasm');

  for (const file of [compilerPath, compilerWasmPath, runtimePath, runtimeWasmPath]) {
    if (!fs.existsSync(file)) {
      throw new Error(`${relative(file)} is missing; run npm run build:wasm first`);
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

function runVmSource(vm, source, options = {}) {
  if (vm instanceof JsVmAdapter) {
    return runVmSourceWithPackages(vm, source, options);
  }
  return runVmSourceWithPackages(vm, source, options);
}

function runVmSourceWithPackages(vm, source, options = {}) {
  const seeds = options.seeds ?? 2;
  const baseSeed = options.baseSeed ?? 1337;
  const iterations = Math.max(1, seeds + 1);
  const compiler = new vm.Compiler(source);
  const externs = Array.from(compiler.extern_slots());
  const results = [];
  const coverage = createCoverage();

  try {
    for (let iteration = 0; iteration < iterations; iteration += 1) {
      const rows = seedRows(iteration, baseSeed);
      const configSeed = vm.js_encoding_seed_from_rows(
        rows.opcodes,
        rows.operandTags,
        rows.constantTags,
        new Uint8Array(),
      );
      const externSlots = externSlotsForIteration(externs, iteration, baseSeed);
      const artifact = compiler.to_bytecode_artifact(configSeed, externSlots);
      try {
        const bytes = artifact.bytes();
        if (options.coverage) {
          collectBytecodeCoverage(coverage, artifact.bytecode_text(), bytes.length);
        }
        const seed = vm.js_encoding_seed_for_seed_and_bytes(configSeed, bytes);
        const logEntries = [];
        const previousHostLog = globalThis.__jsVmHostLog;
        if (options.captureLogs) {
          globalThis.__jsVmHostLog = (level, message) => {
            if (message === undefined) {
              logEntries.push({ level: 'log', message: String(level) });
            } else {
              logEntries.push({ level: String(level), message: String(message) });
            }
          };
        }
        let result;
        try {
          result = vm.js_execute_bytes_with_seed(bytes, seed);
        } finally {
          if (options.captureLogs) {
            globalThis.__jsVmHostLog = previousHostLog || (() => {});
          }
        }
        results.push({
          result,
          seed,
          label: rows.label,
          logs: logEntries.map((entry) => entry.message),
          logEntries,
        });

        if (options.expect !== undefined && result !== options.expect) {
          throw new Error(
            `${options.id || 'source'} failed on ${rows.label}: expected ${JSON.stringify(
              options.expect,
            )}, got ${JSON.stringify(result)}\nseed=${seed}`,
          );
        }
      } finally {
        artifact.free();
      }
    }
  } finally {
    compiler.free();
  }

  return {
    iterations,
    results,
    lastResult: results.at(-1)?.result,
    logs: results.at(-1)?.logs || [],
    logEntries: results.at(-1)?.logEntries || [],
    coverage: finalizeCoverage(coverage),
  };
}

function createCoverage() {
  return {
    opcodes: new Set(),
    sections: new Set(),
    maxByteLength: 0,
    artifacts: 0,
  };
}

function collectBytecodeCoverage(coverage, text, byteLength) {
  coverage.artifacts += 1;
  coverage.maxByteLength = Math.max(coverage.maxByteLength, byteLength);
  for (const line of text.split(/\r?\n/)) {
    const section = line.match(/^\.(\w+)/);
    if (section) coverage.sections.add(section[1]);
    const op = line.match(/^\d+\s+([A-Z][A-Z0-9_]*)\b(?:\s+(.*))?$/);
    if (!op) continue;
    coverage.opcodes.add(op[1]);
    for (const specialized of specializedCoverageOpcodes(op[1], op[2] || '')) {
      coverage.opcodes.add(specialized);
    }
  }
}

function specializedCoverageOpcodes(op, operands) {
  if (op === 'LOAD_CONST' && /^r\d+,\s+c\d+\(/.test(operands)) {
    return ['LOAD_CONST_CONST'];
  }
  if (op === 'POP' && /^r\d+\b/.test(operands)) {
    return ['POP_REG'];
  }
  if (op === 'CALL' && /,\s+#1,/.test(operands)) {
    return ['CALL_1'];
  }
  return [];
}

function finalizeCoverage(coverage) {
  return {
    opcodes: Array.from(coverage.opcodes).sort(),
    sections: Array.from(coverage.sections).sort(),
    maxByteLength: coverage.maxByteLength,
    artifacts: coverage.artifacts,
  };
}

module.exports = {
  ROOT,
  OPCODES,
  OPERAND_TAGS,
  CONSTANT_TAGS,
  relative,
  makePrng,
  shuffled,
  seedRows,
  externSlotsForIteration,
  JsVmAdapter,
  loadVm,
  runVmSource,
};
