#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

const ROOT = path.resolve(__dirname, '..');
const JS_EXTENSIONS = new Set(['.js', '.ts']);
const SKIP_DIRS = new Set(['.git', 'node_modules', 'target', 'pkg', '.issues', '.vendor']);
const DEFAULT_OPCODE_NAMES = [
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
  'LOAD_LOCAL',
  'STORE_LOCAL',
  'LOAD_UNDEFINED',
  'LOAD_NULL',
  'LOAD_TRUE',
  'LOAD_FALSE',
  'LOAD_INT_SMALL',
  'MEMBER_CONST',
  'STORE_MEMBER_CONST',
  'CALL_0',
  'CALL_2',
  'RETURN_REG',
  'RETURN_CONST',
  'JUMP_IF_FALSE_REG',
  'BINARY_REG_REG',
  'BINARY_REG_CONST',
  'LOAD_LOCAL_SMALL',
  'STORE_LOCAL_SMALL',
];
const DEFAULT_OPERAND_TAG_NAMES = [
  'register',
  'constant',
  'name',
  'local',
  'extern',
  'label',
  'count',
  'none',
  'function',
];
const DEFAULT_CONSTANT_TAG_NAMES = ['number', 'string', 'bool', 'null', 'undefined'];

function usage() {
  return [
    'Usage: node scripts/compile-runtime-package.js <folder> [options]',
    '',
    'Options:',
    '  --out <dir>       Output runtime package directory. Default: <folder>/js-vm-runtime',
    '  --entry <file>    Entry js/ts file relative to folder. Default: index/main lookup',
    '  --clean           Remove output directory before writing',
    '  --depth <n>       Runtime max call depth. Default: 128',
    '  --recursion <n>   Runtime max recursive call depth. Default: 8',
  ].join('\n');
}

function parseArgs(argv) {
  const options = {
    clean: false,
    maxCallDepth: 128,
    maxRecursiveCallDepth: 8,
  };
  const positional = [];
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') {
      options.help = true;
    } else if (arg === '--clean') {
      options.clean = true;
    } else if (arg === '--out') {
      options.out = argv[++index];
    } else if (arg === '--entry') {
      options.entry = argv[++index];
    } else if (arg === '--depth') {
      options.maxCallDepth = readPositiveInt(argv[++index], '--depth');
    } else if (arg === '--recursion') {
      options.maxRecursiveCallDepth = readPositiveInt(argv[++index], '--recursion');
    } else if (arg.startsWith('--out=')) {
      options.out = arg.slice('--out='.length);
    } else if (arg.startsWith('--entry=')) {
      options.entry = arg.slice('--entry='.length);
    } else if (arg.startsWith('--depth=')) {
      options.maxCallDepth = readPositiveInt(arg.slice('--depth='.length), '--depth');
    } else if (arg.startsWith('--recursion=')) {
      options.maxRecursiveCallDepth = readPositiveInt(arg.slice('--recursion='.length), '--recursion');
    } else if (arg.startsWith('-')) {
      throw new Error(`unknown option: ${arg}`);
    } else {
      positional.push(arg);
    }
  }
  if (positional.length > 1) {
    throw new Error(`expected one folder, got ${positional.length}`);
  }
  options.input = positional[0];
  return options;
}

function readPositiveInt(value, name) {
  const number = Number(value);
  if (!Number.isInteger(number) || number <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return number;
}

function toPosixPath(value) {
  return value.split(path.sep).join('/');
}

function normalizeVirtualPath(value) {
  const normalized = path.posix.normalize(String(value).replace(/\\/g, '/'));
  return normalized === '.' ? '' : normalized.replace(/^\/+/, '');
}

function ensureInsideRoot(root, file) {
  const resolved = path.resolve(root, file);
  const relative = path.relative(root, resolved);
  if (relative.startsWith('..') || path.isAbsolute(relative)) {
    throw new Error(`path escapes input folder: ${file}`);
  }
  return resolved;
}

function listSourceFiles(root, outputDir) {
  const files = [];
  const outputRelative = path.relative(root, outputDir);
  const skipOutput = outputRelative && !outputRelative.startsWith('..') && !path.isAbsolute(outputRelative)
    ? outputRelative.split(path.sep)[0]
    : '';

  function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (entry.name.startsWith('.') && entry.name !== '.well-known') continue;
      if (entry.isDirectory()) {
        if (SKIP_DIRS.has(entry.name) || entry.name === skipOutput) continue;
        walk(path.join(dir, entry.name));
        continue;
      }
      if (!entry.isFile()) continue;
      const ext = path.extname(entry.name);
      if (!JS_EXTENSIONS.has(ext)) continue;
      files.push(toPosixPath(path.relative(root, path.join(dir, entry.name))));
    }
  }

  walk(root);
  files.sort((left, right) => left.localeCompare(right));
  return files;
}

function chooseEntry(files, requestedEntry) {
  if (requestedEntry) {
    const entry = normalizeVirtualPath(requestedEntry);
    if (!files.includes(entry)) {
      throw new Error(`entry not found in input files: ${entry}`);
    }
    return entry;
  }
  const candidates = [
    'index.ts',
    'index.js',
    'main.ts',
    'main.js',
    'src/index.ts',
    'src/index.js',
    'src/main.ts',
    'src/main.js',
  ];
  return candidates.find((candidate) => files.includes(candidate)) || files[0];
}

function dirname(file) {
  const index = file.lastIndexOf('/');
  return index < 0 ? '' : file.slice(0, index);
}

function resolveVirtualImport(fromFile, specifier, files) {
  if (!specifier.startsWith('./') && !specifier.startsWith('../')) return null;
  const base = normalizeVirtualPath(`${dirname(fromFile)}/${specifier}`);
  const candidates = [
    base,
    `${base}.ts`,
    `${base}.js`,
    `${base}/index.ts`,
    `${base}/index.js`,
  ];
  return candidates.find((candidate) => files.has(candidate)) || null;
}

function localImportSpecifiers(source) {
  const specifiers = [];
  const importRe = /^\s*import(?:\s+[\s\S]*?\s+from)?\s*["']([^"']+)["'];?\s*$/gm;
  for (const match of source.matchAll(importRe)) {
    specifiers.push(match[1]);
  }
  return specifiers;
}

function orderedFiles(entry, sources) {
  const ordered = [];
  const seen = new Set();
  const visiting = new Set();
  const files = new Set(Object.keys(sources));

  const visit = (file) => {
    if (seen.has(file)) return;
    if (visiting.has(file)) return;
    const source = sources[file];
    if (source == null) {
      throw new Error(`missing source: ${file}`);
    }
    visiting.add(file);
    for (const specifier of localImportSpecifiers(source)) {
      const resolved = resolveVirtualImport(file, specifier, files);
      if (resolved) visit(resolved);
    }
    visiting.delete(file);
    seen.add(file);
    ordered.push(file);
  };

  visit(entry);
  for (const file of Object.keys(sources).sort((left, right) => left.localeCompare(right))) {
    visit(file);
  }
  return ordered;
}

function importBindingLines(clause, moduleVariable) {
  const lines = [];
  if (clause.startsWith('* as ')) {
    lines.push(`const ${clause.slice(5).trim()} = ${moduleVariable};`);
    return lines;
  }
  const namedStart = clause.indexOf('{');
  if (namedStart >= 0) {
    const defaultPart = clause.slice(0, namedStart).replace(/,$/, '').trim();
    const namedPart = clause.slice(namedStart + 1, clause.lastIndexOf('}'));
    if (defaultPart) {
      lines.push(`const ${defaultPart} = ${moduleVariable}.default;`);
    }
    for (const part of namedPart.split(',')) {
      const item = part.trim();
      if (!item) continue;
      const [imported, local = imported] = item.split(/\s+as\s+/);
      lines.push(`const ${local.trim()} = ${moduleVariable}[${JSON.stringify(imported.trim())}];`);
    }
    return lines;
  }
  lines.push(`const ${clause.trim()} = ${moduleVariable}.default;`);
  return lines;
}

function transformRuntimeModule(file, source, files) {
  const moduleKey = JSON.stringify(file);
  const exportsName = '__vm_exports';
  const deferredExports = [];
  const lines = source.split(/\r?\n/);
  const out = [
    'globalThis.__JS_VM_MODULES__ = globalThis.__JS_VM_MODULES__ || {};',
    `globalThis.__JS_VM_MODULES__[${moduleKey}] = (() => {`,
    `  const ${exportsName} = {};`,
  ];

  for (const line of lines) {
    const importMatch = line.match(/^\s*import\s+([\s\S]+?)\s+from\s+["']([^"']+)["'];?\s*$/);
    const sideEffectImportMatch = line.match(/^\s*import\s+["']([^"']+)["'];?\s*$/);
    if (importMatch) {
      const resolved = resolveVirtualImport(file, importMatch[2], files);
      if (!resolved) {
        out.push(`  ${line}`);
      } else {
        out.push(...importBindingLines(
          importMatch[1].trim(),
          `globalThis.__JS_VM_MODULES__[${JSON.stringify(resolved)}]`,
        ).map((value) => `  ${value}`));
      }
      continue;
    }
    if (sideEffectImportMatch) {
      const resolved = resolveVirtualImport(file, sideEffectImportMatch[1], files);
      if (!resolved) out.push(`  ${line}`);
      continue;
    }
    const exportDecl = line.match(/^(\s*)export\s+(const|let|var)\s+([A-Za-z_$][\w$]*)([\s\S]*)$/);
    if (exportDecl) {
      out.push(`${exportDecl[1]}${exportDecl[2]} ${exportDecl[3]}${exportDecl[4]}`);
      out.push(`${exportDecl[1]}${exportsName}[${JSON.stringify(exportDecl[3])}] = ${exportDecl[3]};`);
      continue;
    }
    const exportNamedFn = line.match(/^(\s*)export\s+(function|class)\s+([A-Za-z_$][\w$]*)([\s\S]*)$/);
    if (exportNamedFn) {
      out.push(`${exportNamedFn[1]}${exportNamedFn[2]} ${exportNamedFn[3]}${exportNamedFn[4]}`);
      deferredExports.push(`${exportsName}[${JSON.stringify(exportNamedFn[3])}] = ${exportNamedFn[3]};`);
      continue;
    }
    const exportDefault = line.match(/^(\s*)export\s+default\s+([\s\S]+?);?\s*$/);
    if (exportDefault) {
      out.push(`${exportDefault[1]}const __vm_default = ${exportDefault[2].replace(/;$/, '')};`);
      out.push(`${exportDefault[1]}${exportsName}.default = __vm_default;`);
      continue;
    }
    const exportList = line.match(/^\s*export\s+\{([\s\S]+)\};?\s*$/);
    if (exportList) {
      out.push(...exportList[1].split(',').map((part) => {
        const [local, exported = local] = part.trim().split(/\s+as\s+/);
        return `  ${exportsName}[${JSON.stringify(exported.trim())}] = ${local.trim()};`;
      }));
      continue;
    }
    out.push(`  ${line}`);
  }

  out.push(...deferredExports.map((value) => `  ${value}`));
  out.push(`  return ${exportsName};`);
  out.push('})();');
  out.push(`globalThis.__JS_VM_MODULES__[${moduleKey}];`);
  return out.join('\n');
}

function packagePath(prefix, file, suffix = '') {
  return `${prefix}/${normalizeVirtualPath(file)}${suffix}`;
}

function runtimeLoaderCode(options) {
  return `// JS VM multi-bin runtime loader.
// Keep this file next to manifest.json, js_vm_runtime.js and js_vm_runtime_bg.wasm.
import init, {
  js_execute_bytes_with_seed,
  js_execute_bytes_with_seed_and_limits,
} from './js_vm_runtime.js';

const maxCallDepth = ${JSON.stringify(options.maxCallDepth)};
const maxRecursiveCallDepth = ${JSON.stringify(options.maxRecursiveCallDepth)};

async function loadJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(\`failed to load \${url}: \${response.status} \${response.statusText}\`);
  }
  return response.json();
}

async function loadBin(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(\`failed to load \${url}: \${response.status} \${response.statusText}\`);
  }
  return new Uint8Array(await response.arrayBuffer());
}

function resolveExternal(name) {
  const parts = String(name).split('.');
  let current = globalThis;
  for (const part of parts) {
    if (current == null) return undefined;
    current = current[part];
  }
  return current;
}

globalThis.__JS_VM_MODULES__ = globalThis.__JS_VM_MODULES__ || {};

await init(new URL('./js_vm_runtime_bg.wasm', import.meta.url));
const manifest = await loadJson(new URL('./manifest.json', import.meta.url));
const execute = typeof js_execute_bytes_with_seed_and_limits === 'function'
  ? (bytes, seed, externs) => js_execute_bytes_with_seed_and_limits(
    bytes,
    seed,
    externs,
    maxCallDepth,
    maxRecursiveCallDepth,
  )
  : (bytes, seed, externs) => js_execute_bytes_with_seed(bytes, seed, externs);

for (const module of manifest.modules) {
  const bytes = await loadBin(new URL(module.bin, import.meta.url));
  const externs = module.externs.map(resolveExternal);
  module.result = execute(bytes, module.seed, externs);
}

console.log('[js-vm]', {
  entry: manifest.entry,
  modules: manifest.modules.length,
  results: manifest.modules.map((module) => ({ file: module.file, result: module.result })),
});

export default globalThis.__JS_VM_MODULES__[manifest.entry];
`;
}

async function loadCompiler() {
  const compilerPath = path.join(ROOT, 'pkg/compiler/js_vm_compiler.js');
  const compilerWasmPath = path.join(ROOT, 'pkg/compiler/js_vm_compiler_bg.wasm');
  for (const file of [compilerPath, compilerWasmPath]) {
    if (!fs.existsSync(file)) {
      throw new Error(`${path.relative(ROOT, file)} is missing; run npm run build:wasm first`);
    }
  }
  const compilerPkg = await import(pathToFileURL(compilerPath).href);
  await compilerPkg.default({ module_or_path: fs.readFileSync(compilerWasmPath) });
  return compilerPkg;
}

function ensureCleanOutput(outputDir, clean) {
  if (clean) {
    fs.rmSync(outputDir, { recursive: true, force: true });
  }
  fs.mkdirSync(outputDir, { recursive: true });
}

function writeFile(outputDir, relativePath, data) {
  const file = path.join(outputDir, relativePath);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, data);
}

function copyRuntime(outputDir) {
  const runtimeFiles = [
    ['pkg/executor/js_vm_runtime.js', 'js_vm_runtime.js'],
    ['pkg/executor/js_vm_runtime_bg.wasm', 'js_vm_runtime_bg.wasm'],
  ];
  for (const [from, to] of runtimeFiles) {
    const source = path.join(ROOT, from);
    if (!fs.existsSync(source)) {
      throw new Error(`${from} is missing; run npm run build:wasm first`);
    }
    fs.copyFileSync(source, path.join(outputDir, to));
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help || !options.input) {
    console.log(usage());
    process.exit(options.help ? 0 : 1);
  }

  const inputDir = path.resolve(options.input);
  if (!fs.existsSync(inputDir) || !fs.statSync(inputDir).isDirectory()) {
    throw new Error(`input folder does not exist: ${inputDir}`);
  }
  const outputDir = path.resolve(options.out || path.join(inputDir, 'js-vm-runtime'));
  const files = listSourceFiles(inputDir, outputDir);
  if (!files.length) {
    throw new Error(`no .js or .ts files found under ${inputDir}`);
  }
  const entry = chooseEntry(files, options.entry);
  const sources = Object.fromEntries(files.map((file) => [
    file,
    fs.readFileSync(ensureInsideRoot(inputDir, file), 'utf8'),
  ]));
  const order = orderedFiles(entry, sources);
  const sourceFileSet = new Set(files);
  const compilerPkg = await loadCompiler();
  const baseSeed = compilerPkg.js_encoding_seed_from_rows(
    DEFAULT_OPCODE_NAMES,
    DEFAULT_OPERAND_TAG_NAMES,
    DEFAULT_CONSTANT_TAG_NAMES,
    new Uint8Array(),
  );

  ensureCleanOutput(outputDir, options.clean);
  copyRuntime(outputDir);

  const modules = [];
  for (const file of order) {
    const transformed = transformRuntimeModule(file, sources[file], sourceFileSet);
    const compiler = new compilerPkg.Compiler(transformed);
    const externs = Array.from(compiler.extern_slots());
    const artifact = compiler.to_bytecode_artifact(baseSeed, externs);
    try {
      const bytes = new Uint8Array(artifact.bytes());
      const seed = compilerPkg.js_encoding_seed_for_seed_and_bytes(baseSeed, bytes);
      const bin = packagePath('bytecode', file, '.bin');
      const sourcePath = packagePath('sources', file);
      writeFile(outputDir, bin, bytes);
      writeFile(outputDir, sourcePath, sources[file]);
      modules.push({
        file,
        bin,
        source: sourcePath,
        seed,
        externs,
        bytes: bytes.length,
      });
    } finally {
      artifact.free();
      compiler.free();
    }
  }

  const manifest = {
    format: 'js-vm-runtime-package',
    version: 1,
    entry,
    moduleCount: modules.length,
    modules: modules.map((module) => ({
      file: module.file,
      bin: module.bin,
      source: module.source,
      seed: module.seed,
      externs: module.externs,
    })),
  };

  writeFile(outputDir, 'manifest.json', `${JSON.stringify(manifest, null, 2)}\n`);
  writeFile(outputDir, 'js-vm-loader.js', runtimeLoaderCode(options));

  const totalBytes = modules.reduce((sum, module) => sum + module.bytes, 0);
  console.log(`Compiled ${modules.length} module(s) from ${path.relative(process.cwd(), inputDir) || '.'}`);
  console.log(`Entry: ${entry}`);
  console.log(`Output: ${path.relative(process.cwd(), outputDir) || '.'}`);
  console.log(`Bytecode: ${totalBytes} bytes`);
}

main().catch((err) => {
  console.error(`ERROR ${err.stack || err.message || err}`);
  process.exit(1);
});
