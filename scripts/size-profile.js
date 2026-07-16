#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');
const { spawnSync } = require('node:child_process');

const ROOT = path.resolve(__dirname, '..');

const TARGETS = [
  {
    key: 'runtime-browser',
    label: 'Runtime Browser',
    wasm: 'pkg/executor/js_vm_runtime_bg.wasm',
    js: 'pkg/executor/js_vm_runtime.js',
  },
  {
    key: 'runtime-node',
    label: 'Runtime Node',
    wasm: 'pkg/executor-node/js_vm_runtime_node_bg.wasm',
    js: 'pkg/executor-node/js_vm_runtime_node.js',
  },
  {
    key: 'compiler',
    label: 'Compiler',
    wasm: 'pkg/compiler/js_vm_compiler_bg.wasm',
    js: 'pkg/compiler/js_vm_compiler.js',
  },
];

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  if (args.build) {
    run('npm', ['run', 'build:wasm']);
  }

  const selected = args.targets.size
    ? TARGETS.filter((target) => args.targets.has(target.key))
    : TARGETS;
  if (selected.length === 0) {
    throw new Error(`no size profile target matched: ${Array.from(args.targets).join(', ')}`);
  }

  const report = {
    generatedAt: new Date().toISOString(),
    targets: selected.map(profileTarget),
  };

  printReport(report);

  if (args.out) {
    const outPath = path.resolve(ROOT, args.out);
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
    console.log(`\nWrote ${relative(outPath)}`);
  }

  if (args.twiggy) {
    writeTwiggyReports(selected, args.twiggyTop);
  }
}

function parseArgs(argv) {
  const args = {
    build: false,
    help: false,
    out: null,
    targets: new Set(),
    twiggy: false,
    twiggyTop: 40,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') args.help = true;
    else if (arg === '--build') args.build = true;
    else if (arg === '--twiggy') args.twiggy = true;
    else if (arg === '--out') args.out = argv[++index];
    else if (arg.startsWith('--out=')) args.out = arg.slice('--out='.length);
    else if (arg === '--target') args.targets.add(argv[++index]);
    else if (arg.startsWith('--target=')) args.targets.add(arg.slice('--target='.length));
    else if (arg === '--top') args.twiggyTop = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--top=')) args.twiggyTop = Number.parseInt(arg.slice('--top='.length), 10);
    else throw new Error(`unknown option: ${arg}`);
  }

  if (!Number.isFinite(args.twiggyTop) || args.twiggyTop <= 0) {
    args.twiggyTop = 40;
  }
  return args;
}

function profileTarget(target) {
  const wasmPath = path.join(ROOT, target.wasm);
  const jsPath = path.join(ROOT, target.js);
  const wasm = readArtifact(wasmPath);
  const js = readArtifact(jsPath);
  const totalRaw = wasm.rawBytes + js.rawBytes;
  const totalGzip = wasm.gzipBytes + js.gzipBytes;

  return {
    key: target.key,
    label: target.label,
    wasm: {
      path: target.wasm,
      ...wasm,
    },
    js: {
      path: target.js,
      ...js,
    },
    totalRawBytes: totalRaw,
    totalGzipBytes: totalGzip,
  };
}

function readArtifact(file) {
  if (!fs.existsSync(file)) {
    return {
      exists: false,
      rawBytes: 0,
      gzipBytes: 0,
      mtimeMs: 0,
    };
  }
  const bytes = fs.readFileSync(file);
  return {
    exists: true,
    rawBytes: bytes.length,
    gzipBytes: zlib.gzipSync(bytes, { level: 9 }).length,
    mtimeMs: Math.round(fs.statSync(file).mtimeMs),
  };
}

function printReport(report) {
  console.log(`JS VM size profile (${report.generatedAt})`);
  console.log('');
  console.log(
    [
      pad('Target', 18),
      pad('WASM raw', 12),
      pad('WASM gzip', 12),
      pad('JS raw', 10),
      pad('JS gzip', 10),
      pad('Total raw', 12),
      pad('Total gzip', 12),
    ].join('  '),
  );
  console.log('-'.repeat(94));
  for (const target of report.targets) {
    console.log(
      [
        pad(target.label, 18),
        pad(formatBytes(target.wasm.rawBytes), 12),
        pad(formatBytes(target.wasm.gzipBytes), 12),
        pad(formatBytes(target.js.rawBytes), 10),
        pad(formatBytes(target.js.gzipBytes), 10),
        pad(formatBytes(target.totalRawBytes), 12),
        pad(formatBytes(target.totalGzipBytes), 12),
      ].join('  '),
    );
  }
}

function writeTwiggyReports(targets, top) {
  const twiggy = findCommand('twiggy');
  if (!twiggy) {
    console.log('\nWARN twiggy not found; skipped twiggy reports');
    return;
  }

  const outDir = path.join(ROOT, 'tests', 'reports', 'size');
  fs.mkdirSync(outDir, { recursive: true });

  for (const target of targets) {
    const wasmPath = path.join(ROOT, target.wasm);
    if (!fs.existsSync(wasmPath)) continue;
    const result = spawnSync(twiggy, ['top', '-n', String(top), wasmPath], {
      cwd: ROOT,
      encoding: 'utf8',
    });
    const output = result.status === 0 ? result.stdout : `${result.stdout}\n${result.stderr}`;
    const outPath = path.join(outDir, `${target.key}-twiggy-top.txt`);
    fs.writeFileSync(outPath, output.trimEnd() + '\n');
    console.log(`Wrote ${relative(outPath)}`);
  }
}

function run(command, args) {
  console.log(`RUN ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, {
    cwd: ROOT,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with code ${result.status}`);
  }
}

function findCommand(command) {
  const result = spawnSync('which', [command], {
    encoding: 'utf8',
  });
  return result.status === 0 ? result.stdout.trim() : null;
}

function formatBytes(bytes) {
  if (!bytes) return '-';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
}

function pad(value, width) {
  value = String(value);
  return value.length >= width ? value : value + ' '.repeat(width - value.length);
}

function relative(file) {
  return path.relative(ROOT, file);
}

function printHelp() {
  console.log(`Usage: node scripts/size-profile.js [options]

Options:
  --build                 Run npm run build:wasm before profiling.
  --target <name>         Profile one target. Repeatable.
                          Names: runtime-browser, runtime-node, compiler.
  --out <file>            Write JSON report.
  --twiggy                Write twiggy top reports to tests/reports/size.
  --top <n>               Number of twiggy rows. Default: 40.
  -h, --help              Show this help.
`);
}

main();
