#!/usr/bin/env node

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const ROOT = path.resolve(__dirname, '..');
const OUT_ROOT = path.join(ROOT, 'pkg', 'runtime-features');
const RUNTIME_CRATE = 'crates/runtime/bin/browser';

const FEATURE_ALIASES = {
  full: [
    'bigint',
    'generator',
    'host-builtins',
    'module',
    'proxy',
    'regexp',
    'test262-compat',
  ],
  'host-builtins': [
    'array-builtins',
    'function-builtins',
    'object-builtins',
    'string-builtins',
  ],
  'test262-compat': ['test262-eval'],
};

const FEATURE_IMPLICATIONS = {
  debugger: ['source-map'],
};

const KNOWN_FEATURES = [
  ...Object.keys(FEATURE_ALIASES),
  'array-builtins',
  'bigint',
  'compact-errors',
  'debugger',
  'function-builtins',
  'generator',
  'host-builtins',
  'module',
  'object-builtins',
  'proxy',
  'regexp',
  'source-map',
  'string-builtins',
  'test262-compat',
  'test262-eval',
];

const NON_RECURSIVE_FEATURES = new Set(['source-map', 'debugger']);

const CANONICAL_FEATURES = KNOWN_FEATURES
  .filter((feature) => !Object.prototype.hasOwnProperty.call(FEATURE_ALIASES, feature))
  .filter((feature) => !NON_RECURSIVE_FEATURES.has(feature))
  .sort();

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  if (args.command === 'resolve') {
    console.log(JSON.stringify(resolveFeatureSet(args.features), null, 2));
    return;
  }

  const map = createFeatureMap(args);
  if (args.command === 'list') {
    for (const item of map.packages) {
      console.log(`${item.packageName} ${item.canonical || '<empty>'}`);
    }
    return;
  }

  if (args.command === 'pack') {
    buildFeaturePackageByMd5(map, args);
    return;
  }

  writeManifest(map, args.out);
  if (args.command === 'map') {
    printMapSummary(map, args.out);
    return;
  }

  buildFeatureMap(map, args);
}

function parseArgs(argv) {
  const args = {
    command: 'resolve',
    dryRun: false,
    features: [],
    help: false,
    includeCompactErrors: true,
    limit: Infinity,
    md5: '',
    offset: 0,
    out: path.join('pkg', 'runtime-features', 'manifest.json'),
    release: true,
    skipOpt: false,
    target: 'web',
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '-h' || arg === '--help') args.help = true;
    else if (['resolve', 'list', 'map', 'build', 'pack'].includes(arg)) args.command = arg;
    else if (arg === '--dry-run') args.dryRun = true;
    else if (arg === '--features') args.features.push(...splitFeatures(argv[++index]));
    else if (arg.startsWith('--features=')) args.features.push(...splitFeatures(arg.slice(11)));
    else if (arg === '--md5') args.md5 = argv[++index] || '';
    else if (arg.startsWith('--md5=')) args.md5 = arg.slice(6);
    else if (arg === '--package') args.md5 = argv[++index] || '';
    else if (arg.startsWith('--package=')) args.md5 = arg.slice(10);
    else if (arg === '--no-compact-errors') args.includeCompactErrors = false;
    else if (arg === '--include-compact-errors') args.includeCompactErrors = true;
    else if (arg === '--limit') args.limit = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--limit=')) args.limit = Number.parseInt(arg.slice(8), 10);
    else if (arg === '--offset') args.offset = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--offset=')) args.offset = Number.parseInt(arg.slice(9), 10);
    else if (arg === '--out') args.out = argv[++index];
    else if (arg.startsWith('--out=')) args.out = arg.slice(6);
    else if (arg === '--skip-opt') args.skipOpt = true;
    else if (arg === '--target') args.target = argv[++index];
    else if (arg.startsWith('--target=')) args.target = arg.slice(9);
    else if (arg === '--dev') args.release = false;
    else if (args.command === 'pack' && !args.md5) args.md5 = arg;
    else args.features.push(...splitFeatures(arg));
  }

  if (!Number.isFinite(args.limit) || args.limit < 0) args.limit = Infinity;
  if (!Number.isFinite(args.offset) || args.offset < 0) args.offset = 0;
  return args;
}

function splitFeatures(value) {
  return String(value || '')
    .split(/[,\s]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function createFeatureMap(args) {
  const packages = enumerateFeatureSets(args.includeCompactErrors)
    .slice(args.offset, Number.isFinite(args.limit) ? args.offset + args.limit : undefined)
    .map(featurePackageItem);
  return {
    generatedAt: new Date().toISOString(),
    canonicalFeatures: CANONICAL_FEATURES,
    includeCompactErrors: args.includeCompactErrors,
    aliases: FEATURE_ALIASES,
    implications: FEATURE_IMPLICATIONS,
    count: packages.length,
    packages,
    byCanonical: Object.fromEntries(packages.map((item) => [item.canonical, item.packageName])),
    byMd5: Object.fromEntries(packages.map((item) => [item.md5, item.packageName])),
  };
}

function resolveFeatureSet(features) {
  const expanded = expandFeatures(features);
  const canonicalFeatures = Array.from(expanded).sort();
  const canonical = canonicalFeatures.join(',');
  const md5 = crypto.createHash('md5').update(canonical).digest('hex').slice(0, 12);
  return {
    input: features,
    features: canonicalFeatures,
    canonical,
    md5,
    packageName: `runtime-feature-${md5}`,
  };
}

function expandFeatures(features) {
  const out = new Set();
  const visit = (feature) => {
    if (!KNOWN_FEATURES.includes(feature)) {
      throw new Error(`unknown runtime feature: ${feature}`);
    }
    const aliases = FEATURE_ALIASES[feature];
    if (aliases) {
      for (const alias of aliases) visit(alias);
      return;
    }
    out.add(feature);
    for (const implied of FEATURE_IMPLICATIONS[feature] || []) visit(implied);
  };
  for (const feature of features) visit(feature);
  return out;
}

function enumerateFeatureSets(includeCompactErrors = true) {
  const base = CANONICAL_FEATURES.filter(
    (feature) => includeCompactErrors || feature !== 'compact-errors',
  );
  const out = [];
  walkFeatureSets(base, 0, [], out);
  return out;
}

function walkFeatureSets(features, index, selected, out) {
  if (index >= features.length) {
    out.push(resolveFeatureSet(selected));
    return;
  }
  walkFeatureSets(features, index + 1, selected, out);
  selected.push(features[index]);
  walkFeatureSets(features, index + 1, selected, out);
  selected.pop();
}

function writeManifest(map, out) {
  const outPath = path.resolve(ROOT, out);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(map, null, 2)}\n`);
}

function buildFeatureMap(map, args) {
  fs.mkdirSync(OUT_ROOT, { recursive: true });
  printMapSummary(map, args.out);
  for (const [index, item] of map.packages.entries()) {
    const progress = `[${index + 1}/${map.packages.length}]`;
    console.log(`RUN ${progress} ${item.packageName} ${item.canonical || '<empty>'}`);
    if (args.dryRun) continue;
    buildOne(item, args);
  }
  if (!args.dryRun) writeManifest(map, args.out);
  console.log(`OK runtime feature build ${args.dryRun ? 'planned' : 'completed'} count=${map.count}`);
}

function buildFeaturePackageByMd5(map, args) {
  if (args.features.length) {
    const item = featurePackageItem(resolveFeatureSet(args.features));
    printMapSummary(map, args.out);
    console.log(`RUN ${item.packageName} ${item.canonical || '<empty>'}`);
    if (!args.dryRun) {
      fs.mkdirSync(OUT_ROOT, { recursive: true });
      buildOne(item, args);
      writeManifest(map, args.out);
    }
    console.log(`OK runtime feature package ${args.dryRun ? 'planned' : 'built'} ${item.packageName}`);
    return;
  }

  const md5 = normalizePackageMd5(args.md5);
  if (!md5) {
    throw new Error('pack requires a md5, package name, or --features list, for example: pack 1f6c5a7a67ff');
  }
  const item = map.packages.find((candidate) => candidate.md5 === md5);
  if (!item) {
    throw new Error(
      `runtime feature package not found for md5 ${md5}; try again with --include-compact-errors or --no-compact-errors`,
    );
  }
  printMapSummary(map, args.out);
  console.log(`RUN ${item.packageName} ${item.canonical || '<empty>'}`);
  if (!args.dryRun) {
    fs.mkdirSync(OUT_ROOT, { recursive: true });
    buildOne(item, args);
    writeManifest(map, args.out);
  }
  console.log(`OK runtime feature package ${args.dryRun ? 'planned' : 'built'} ${item.packageName}`);
}

function featurePackageItem(item) {
  return {
    ...item,
    dir: `pkg/runtime-features/${item.packageName}`,
    js: `pkg/runtime-features/${item.packageName}/js_vm_runtime.js`,
    wasm: `pkg/runtime-features/${item.packageName}/js_vm_runtime_bg.wasm`,
  };
}

function normalizePackageMd5(value) {
  const normalized = String(value || '')
    .trim()
    .replace(/^runtime-feature-/i, '')
    .toLowerCase();
  if (!normalized) return '';
  if (!/^[a-f0-9]{12}$/.test(normalized)) {
    throw new Error(`invalid runtime feature md5: ${value}`);
  }
  return normalized;
}

function buildOne(item, args) {
  const outDir = path.join(ROOT, item.dir);
  fs.rmSync(outDir, { recursive: true, force: true });
  const commandArgs = ['build'];
  commandArgs.push(args.release ? '--release' : '--dev');
  commandArgs.push(RUNTIME_CRATE, '--target', args.target, '--out-dir', path.relative(path.join(ROOT, RUNTIME_CRATE), outDir));
  commandArgs.push('--no-default-features');
  if (item.features.length) commandArgs.push('--features', item.features.join(','));
  run('wasm-pack', commandArgs);

  const jsPath = path.join(outDir, 'js_vm_runtime.js');
  const wasmPath = path.join(outDir, 'js_vm_runtime_bg.wasm');
  patchWasmBindgenJs(jsPath);
  if (!args.skipOpt) optimizeWasm(wasmPath);
}

function patchWasmBindgenJs(file) {
  if (!fs.existsSync(file)) return;
  const source = fs.readFileSync(file, 'utf8');
  const patched = source
    .replace(
      'let deferred5_0;\n    let deferred5_1;',
      'let deferred5_0 = 0;\n    let deferred5_1 = 0;',
    )
    .replace(
      'let deferred6_0;\n    let deferred6_1;',
      'let deferred6_0 = 0;\n    let deferred6_1 = 0;',
    );
  if (patched !== source) fs.writeFileSync(file, patched);
}

function optimizeWasm(file) {
  const wasmOpt = findCommand('wasm-opt');
  if (!wasmOpt || !fs.existsSync(file)) return;
  run(wasmOpt, [
    file,
    '-Oz',
    '--enable-bulk-memory',
    '--enable-nontrapping-float-to-int',
    '-o',
    file,
  ]);
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    stdio: 'inherit',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with code ${result.status}`);
  }
}

function findCommand(command) {
  const result = spawnSync('which', [command], { encoding: 'utf8' });
  return result.status === 0 ? result.stdout.trim() : null;
}

function printMapSummary(map, out) {
  console.log(`Runtime feature map count=${map.count}`);
  console.log(`Manifest ${out}`);
  if (map.packages.length) {
    const first = map.packages[0];
    const last = map.packages[map.packages.length - 1];
    console.log(`First ${first.packageName} ${first.canonical || '<empty>'}`);
    console.log(`Last  ${last.packageName} ${last.canonical || '<empty>'}`);
  }
}

function printHelp() {
  console.log(`Usage:
  node scripts/runtime-features.js resolve bigint,generator
  node scripts/runtime-features.js resolve generator bigint
  node scripts/runtime-features.js map
  node scripts/runtime-features.js build --dry-run
  node scripts/runtime-features.js build --limit=8
  node scripts/runtime-features.js pack 1f6c5a7a67ff
  node scripts/runtime-features.js pack runtime-feature-1f6c5a7a67ff
  node scripts/runtime-features.js pack --features=full,debugger

Commands:
  resolve   Normalize one feature set and print its package name.
  list      Print package name + canonical features for every combination.
  map       Write a manifest for every recursive feature combination.
  build     Build every package in the manifest selection.
  pack      Build one package by md5 or runtime-feature-<md5> name.

Options:
  --md5=ID                    md5 to build for the pack command.
  --no-compact-errors        Exclude compact-errors from recursive combinations.
  --limit=N --offset=N       Build or map a slice.
  --dry-run                  Print build plan without compiling.
  --skip-opt                 Skip wasm-opt.

Feature sets are sorted, deduplicated, and alias-expanded before hashing.`);
}

if (require.main === module) {
  try {
    main();
  } catch (err) {
    console.error(err?.stack || err);
    process.exit(1);
  }
}

module.exports = {
  CANONICAL_FEATURES,
  FEATURE_ALIASES,
  FEATURE_IMPLICATIONS,
  KNOWN_FEATURES,
  createFeatureMap,
  enumerateFeatureSets,
  resolveFeatureSet,
};
