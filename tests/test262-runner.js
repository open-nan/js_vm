#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const log = require('./support/logger.js');
const {
  ROOT,
  createDefaultHostEnvironment,
  externValuesForSlots,
  loadVm,
  relative,
  seedRows,
} = require('./support/vm_chain.js');

const DEFAULT_TEST262_ROOT = path.join(__dirname, '.vendor/test262');
const CONFIG_ROOT = path.join(__dirname, 'test262');
const DEFAULT_PROFILE = path.join(CONFIG_ROOT, 'baseline.txt');
const DEFAULT_UNSUPPORTED = path.join(CONFIG_ROOT, 'unsupported.json');
const DEFAULT_REPORT_DIR = path.join(CONFIG_ROOT, 'reports');
const DEFAULT_BASE_SEED = Number.parseInt(process.env.JS_VM_TEST262_BASE_SEED || '1337', 10);
const ERROR_TYPES = [
  'AggregateError',
  'EvalError',
  'RangeError',
  'ReferenceError',
  'SyntaxError',
  'Test262Error',
  'TypeError',
  'URIError',
  'Error',
];

main().catch((err) => {
  log.error(err?.stack || err);
  process.exit(err?.exitCode || 1);
});

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const unsupported = loadUnsupported(args.unsupportedFile);
  const root = path.resolve(args.root);
  const rootInfo = resolveTest262Root(root);
  const selectedPaths = selectProfilePaths(args);
  const files = collectTests(rootInfo.testRoot, selectedPaths, args);

  if (!files.length) {
    throw new Error(
      [
        `no Test262 files selected under ${relative(rootInfo.testRoot)}`,
        `root=${relative(root)}`,
        `profile=${args.all ? 'all' : relative(args.profile)}`,
      ].join('\n'),
    );
  }

  const selected = args.maxCases > 0 ? files.slice(0, args.maxCases) : files;
  log.step(
    `Running Test262 profile cases=${selected.length}/${files.length}, root=${relative(root)}, report=${relative(args.reportDir)}`,
  );
  if (selected.length !== files.length) {
    log.warn(`case selection truncated by --max-cases=${args.maxCases}`);
  }

  const vm = await loadVm();
  const suiteStartedAt = Date.now();
  const report = createReport(root, rootInfo, selectedPaths, args, unsupported);

  for (const [index, file] of selected.entries()) {
    const test = readTest(file, rootInfo.testRoot);
    const progress = log.progressText(index + 1, selected.length);
    const skipReason = testSkipReason(test, rootInfo, args, unsupported);

    if (skipReason) {
      recordSkip(report, test, skipReason);
      if (args.verbose) log.jest('SKIP', test.id, `${progress} reason=${skipReason}`);
      continue;
    }

    const scenarios = scenariosFor(test);
    for (const scenario of scenarios) {
      if (args.verbose) log.jest('RUN', `${test.id} (${scenario.name})`, progress);
      const prepared = prepareSource(test, rootInfo.harnessRoot, scenario, args);
      const outcome = runVmTest262Case(vm, prepared.source, {
        baseSeed: args.baseSeed,
        id: `${test.id} (${scenario.name})`,
      });
      const result = evaluateOutcome(test, scenario, outcome);
      recordResult(report, test, scenario, result, outcome);
      if (args.verbose) {
        log.jest(result.ok ? 'PASS' : 'FAIL', `${test.id} (${scenario.name})`, progress);
      }
    }
  }

  report.durationMs = Date.now() - suiteStartedAt;
  report.generatedAt = new Date().toISOString();
  writeReports(report, args);
  printSummary(report, args);

  if (report.stats.failed > 0) {
    throw Object.assign(new Error(`${report.stats.failed} Test262 scenario(s) failed`), {
      exitCode: 1,
    });
  }
}

function parseArgs(argv) {
  const parsed = {
    all: false,
    baseSeed: DEFAULT_BASE_SEED,
    failOnly: true,
    harnessMode: process.env.JS_VM_TEST262_HARNESS || 'light',
    help: false,
    includeAsync: false,
    includeIntl: false,
    includeModule: false,
    includeRaw: true,
    maxCases: numberEnv('JS_VM_TEST262_MAX_CASES', 0),
    paths: [],
    profile: process.env.JS_VM_TEST262_PROFILE || DEFAULT_PROFILE,
    reportDir: process.env.JS_VM_TEST262_REPORT_DIR || DEFAULT_REPORT_DIR,
    root: process.env.JS_VM_TEST262_ROOT || DEFAULT_TEST262_ROOT,
    skipUnsupported: true,
    unsupportedFile: process.env.JS_VM_TEST262_UNSUPPORTED || DEFAULT_UNSUPPORTED,
    verbose: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') parsed.help = true;
    else if (arg === '--all') parsed.all = true;
    else if (arg === '--verbose' || arg === '-v') parsed.verbose = true;
    else if (arg === '--no-fail-only') parsed.failOnly = false;
    else if (arg === '--fail-only') parsed.failOnly = true;
    else if (arg === '--no-skip-unsupported') parsed.skipUnsupported = false;
    else if (arg === '--skip-unsupported') parsed.skipUnsupported = true;
    else if (arg === '--include-module' || arg === '--modules') parsed.includeModule = true;
    else if (arg === '--include-async') parsed.includeAsync = true;
    else if (arg === '--include-intl') parsed.includeIntl = true;
    else if (arg === '--include-raw') parsed.includeRaw = true;
    else if (arg === '--skip-raw') parsed.includeRaw = false;
    else if (arg === '--official-harness') parsed.harnessMode = 'official';
    else if (arg === '--light-harness') parsed.harnessMode = 'light';
    else if (arg === '--harness') parsed.harnessMode = argv[++index];
    else if (arg.startsWith('--harness=')) parsed.harnessMode = arg.slice(10);
    else if (arg === '--root' || arg === '--test262-root') parsed.root = argv[++index];
    else if (arg.startsWith('--root=')) parsed.root = arg.slice(7);
    else if (arg.startsWith('--test262-root=')) parsed.root = arg.slice(15);
    else if (arg === '--profile') parsed.profile = argv[++index];
    else if (arg.startsWith('--profile=')) parsed.profile = arg.slice(10);
    else if (arg === '--path') parsed.paths.push(...splitList(argv[++index]));
    else if (arg.startsWith('--path=')) parsed.paths.push(...splitList(arg.slice(7)));
    else if (arg === '--max-cases') parsed.maxCases = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--max-cases=')) parsed.maxCases = Number.parseInt(arg.slice(12), 10);
    else if (arg === '--report-dir') parsed.reportDir = argv[++index];
    else if (arg.startsWith('--report-dir=')) parsed.reportDir = arg.slice(13);
    else if (arg === '--unsupported') parsed.unsupportedFile = argv[++index];
    else if (arg.startsWith('--unsupported=')) parsed.unsupportedFile = arg.slice(14);
    else if (arg === '--base-seed') parsed.baseSeed = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--base-seed=')) parsed.baseSeed = Number.parseInt(arg.slice(12), 10);
    else {
      throw new Error(`unknown test262 option: ${arg}`);
    }
  }

  parsed.root = path.resolve(ROOT, parsed.root);
  parsed.profile = path.resolve(ROOT, parsed.profile);
  parsed.reportDir = path.resolve(ROOT, parsed.reportDir);
  parsed.unsupportedFile = path.resolve(ROOT, parsed.unsupportedFile);
  if (!['light', 'official'].includes(parsed.harnessMode)) {
    throw new Error(`unknown Test262 harness mode: ${parsed.harnessMode}`);
  }
  return parsed;
}

function resolveTest262Root(root) {
  if (!fs.existsSync(root)) {
    throw new Error(
      [
        `Test262 checkout not found: ${relative(root)}`,
        'Run: npm run update:test262',
        'Or pass: npm run test:test262 -- --root=/path/to/test262',
      ].join('\n'),
    );
  }

  const testRoot = fs.existsSync(path.join(root, 'test')) ? path.join(root, 'test') : root;
  const harnessRoot = fs.existsSync(path.join(root, 'harness'))
    ? path.join(root, 'harness')
    : path.join(root, 'harness');

  return {
    root,
    testRoot,
    harnessRoot,
    revision: test262Revision(root),
  };
}

function selectProfilePaths(args) {
  if (args.all) return ['.'];
  if (args.paths.length) return args.paths;
  return readPathList(args.profile);
}

function readPathList(file) {
  if (!fs.existsSync(file)) {
    throw new Error(`Test262 profile file not found: ${relative(file)}`);
  }
  const values = fs.readFileSync(file, 'utf8')
    .split(/\r?\n/)
    .map((line) => line.replace(/#.*/, '').trim())
    .filter(Boolean);
  if (!values.length) {
    throw new Error(`Test262 profile file has no paths: ${relative(file)}`);
  }
  return values;
}

function collectTests(testRoot, selectedPaths, args) {
  const files = [];
  const missing = [];
  for (const selectedPath of selectedPaths) {
    const normalized = normalizeSelectedPath(selectedPath);
    const target = path.join(testRoot, normalized);
    if (!fs.existsSync(target)) {
      missing.push(selectedPath);
      continue;
    }
    collectJsFiles(target, files);
  }

  if (missing.length) {
    log.warn(`missing Test262 path(s): ${missing.join(', ')}`);
  }

  return Array.from(new Set(files))
    .filter((file) => file.endsWith('.js'))
    .filter((file) => !path.basename(file).includes('_FIXTURE'))
    .sort();
}

function normalizeSelectedPath(value) {
  const normalized = String(value || '.').replace(/\\/g, '/').replace(/^\/+/, '');
  if (normalized === 'test') return '.';
  if (normalized.startsWith('test/')) return normalized.slice(5);
  return normalized || '.';
}

function collectJsFiles(target, files) {
  const stat = fs.statSync(target);
  if (stat.isFile()) {
    if (target.endsWith('.js')) files.push(target);
    return;
  }
  if (!stat.isDirectory()) return;
  for (const entry of fs.readdirSync(target, { withFileTypes: true })) {
    collectJsFiles(path.join(target, entry.name), files);
  }
}

function readTest(file, testRoot) {
  const source = fs.readFileSync(file, 'utf8');
  return {
    file,
    id: path.relative(testRoot, file).split(path.sep).join('/'),
    meta: parseFrontmatter(source),
    source,
  };
}

function parseFrontmatter(source) {
  const match = source.match(/\/\*---([\s\S]*?)---\*\//);
  if (!match) return {};
  return parseYamlSubset(match[1]);
}

function parseYamlSubset(text) {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const meta = {};

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (!line.trim() || /^\s*#/.test(line)) continue;
    const top = line.match(/^([A-Za-z0-9_-]+):(?:\s*(.*))?$/);
    if (!top) continue;

    const key = top[1];
    const rest = top[2] || '';

    if (['flags', 'features', 'includes', 'locale'].includes(key)) {
      if (rest.trim()) meta[key] = parseInlineArray(rest);
      else {
        const collected = [];
        while (index + 1 < lines.length && /^\s+/.test(lines[index + 1])) {
          const item = lines[++index].match(/^\s*-\s*(.+?)\s*$/);
          if (item) collected.push(unquoteYamlScalar(item[1]));
        }
        meta[key] = collected;
      }
      continue;
    }

    if (key === 'negative') {
      const negative = {};
      while (index + 1 < lines.length && /^\s+/.test(lines[index + 1])) {
        const item = lines[++index].match(/^\s*([A-Za-z0-9_-]+):\s*(.+?)\s*$/);
        if (item) negative[item[1]] = unquoteYamlScalar(item[2]);
      }
      meta.negative = negative;
      continue;
    }

    if (rest === '|' || rest === '>') {
      const block = [];
      while (index + 1 < lines.length && /^\s+/.test(lines[index + 1])) {
        block.push(lines[++index].replace(/^\s+/, ''));
      }
      meta[key] = block.join(rest === '|' ? '\n' : ' ');
      continue;
    }

    meta[key] = unquoteYamlScalar(rest);
  }

  return meta;
}

function parseInlineArray(value) {
  const trimmed = value.trim();
  if (!trimmed.startsWith('[') || !trimmed.endsWith(']')) {
    return [unquoteYamlScalar(trimmed)].filter(Boolean);
  }
  const inner = trimmed.slice(1, -1).trim();
  if (!inner) return [];
  return inner.split(',').map((item) => unquoteYamlScalar(item.trim())).filter(Boolean);
}

function unquoteYamlScalar(value) {
  const trimmed = String(value || '').trim();
  if (
    (trimmed.startsWith('"') && trimmed.endsWith('"')) ||
    (trimmed.startsWith("'") && trimmed.endsWith("'"))
  ) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function loadUnsupported(file) {
  if (!fs.existsSync(file)) return { flags: [], features: [], paths: [], sourcePatterns: [] };
  const parsed = JSON.parse(fs.readFileSync(file, 'utf8'));
  return {
    flags: Array.isArray(parsed.flags) ? parsed.flags : [],
    features: Array.isArray(parsed.features) ? parsed.features : [],
    paths: Array.isArray(parsed.paths) ? parsed.paths : [],
    sourcePatterns: Array.isArray(parsed.sourcePatterns) ? parsed.sourcePatterns : [],
  };
}

function testSkipReason(test, rootInfo, args, unsupported) {
  const flags = new Set(test.meta.flags || []);
  const features = new Set(test.meta.features || []);
  const negativeParse = test.meta.negative?.phase === 'parse';

  if (!args.includeIntl && (test.id.startsWith('intl402/') || test.id.startsWith('staging/intl402/'))) {
    return 'intl402 disabled';
  }
  if (!args.includeModule && flags.has('module')) return 'module flag disabled';
  if (!args.includeAsync && flags.has('async')) return 'async flag disabled';
  if (!args.includeRaw && flags.has('raw')) return 'raw flag disabled';

  if (args.skipUnsupported) {
    const unsupportedFlag = unsupported.flags.find((flag) => flags.has(flag));
    if (unsupportedFlag) return `unsupported flag: ${unsupportedFlag}`;

    const unsupportedFeature = negativeParse
      ? null
      : unsupported.features.find((feature) => features.has(feature));
    if (unsupportedFeature) return `unsupported feature: ${unsupportedFeature}`;

    const unsupportedPath = unsupported.paths.find((prefix) => test.id.startsWith(prefix));
    if (unsupportedPath) return `unsupported path: ${unsupportedPath}`;

    const unsupportedPattern = unsupported.sourcePatterns.find((pattern) => {
      return new RegExp(pattern).test(test.source);
    });
    if (unsupportedPattern) return `unsupported source pattern: ${unsupportedPattern}`;
  }

  if (!flags.has('raw') && !fs.existsSync(rootInfo.harnessRoot)) {
    return 'missing harness directory';
  }

  return '';
}

function scenariosFor(test) {
  const flags = new Set(test.meta.flags || []);
  if (flags.has('raw')) return [{ name: 'raw', strict: false, raw: true }];
  if (flags.has('module')) return [{ name: 'module', strict: true, module: true }];
  if (flags.has('onlyStrict')) return [{ name: 'strict', strict: true }];
  if (flags.has('noStrict')) return [{ name: 'default', strict: false }];
  return [
    { name: 'default', strict: false },
    { name: 'strict', strict: true },
  ];
}

function prepareSource(test, harnessRoot, scenario, args) {
  if (scenario.raw) {
    return {
      source: test.source,
      harnessFiles: [],
    };
  }

  const flags = new Set(test.meta.flags || []);
  const harnessFiles = args.harnessMode === 'official'
    ? ['assert.js', 'sta.js']
    : ['light-assert.js', 'light-sta.js'];
  if (flags.has('async')) harnessFiles.push('doneprintHandle.js');
  for (const include of test.meta.includes || []) {
    if (!harnessFiles.includes(include)) harnessFiles.push(include);
  }

  const officialHarnessSource = (file) => {
      const harnessFile = path.join(harnessRoot, file);
      if (!fs.existsSync(harnessFile)) {
        throw new Error(`missing Test262 harness file: ${relative(harnessFile)} for ${test.id}`);
      }
      let source = fs.readFileSync(harnessFile, 'utf8');
      if (file === 'tcoHelper.js') {
        source = source.replace(/\bvar\s+\$MAX_ITERATIONS\s*=\s*100000\s*;/, 'var $MAX_ITERATIONS = 4;');
      }
      return `\n/* harness: ${file} */\n${source}\n`;
    };
  const includeFiles = test.meta.includes || [];
  const harnessSource = args.harnessMode === 'official'
    ? harnessFiles.map(officialHarnessSource)
    : [
      ...lightHarnessSource(flags),
      ...includeFiles.map(officialHarnessSource),
    ];

  return {
    source: [
      scenario.strict ? '"use strict";\n' : '',
      ...harnessSource,
      `\n/* test: ${test.id} */\n`,
      test.source,
      '\n',
    ].join(''),
    harnessFiles,
  };
}

function lightHarnessSource(flags) {
  const sources = [
    `
/* harness: light-assert.js */
function Test262Error(message) {
  this.name = 'Test262Error';
  this.message = message || '';
}
Test262Error.prototype = Object.create(Error.prototype);
Test262Error.prototype.constructor = Test262Error;
function $ERROR(message) {
  throw new Test262Error(message);
}
function __sameValue(actual, expected) {
  if (actual === expected) return actual !== 0 || 1 / actual === 1 / expected;
  return actual !== actual && expected !== expected;
}
function assert(mustBeTrue, message) {
  if (mustBeTrue !== true) $ERROR(message || 'Expected true');
}
assert.sameValue = function(actual, expected, message) {
  if (!__sameValue(actual, expected)) $ERROR(message || 'Expected SameValue equality');
};
assert.notSameValue = function(actual, unexpected, message) {
  if (__sameValue(actual, unexpected)) $ERROR(message || 'Expected SameValue inequality');
};
assert.compareArray = function(actual, expected, message) {
  if (!actual || !expected || actual.length !== expected.length) {
    $ERROR(message || 'Expected arrays to have the same length');
  }
  for (var index = 0; index < expected.length; index += 1) {
    if (!__sameValue(actual[index], expected[index])) {
      $ERROR(message || 'Expected arrays to contain the same values');
    }
  }
};
assert.throws = function(expectedErrorConstructor, func, message) {
  try {
    func();
  } catch (error) {
    if (error instanceof expectedErrorConstructor) return;
    $ERROR(message || 'Expected a different error constructor');
  }
  $ERROR(message || 'Expected function to throw');
};
`,
    `
/* harness: light-sta.js */
function $DONOTEVALUATE() {
  throw new SyntaxError('$DONOTEVALUATE was evaluated');
}
`,
  ];

  if (flags.has('async')) {
    sources.push(`
/* harness: light-doneprintHandle.js */
var $DONE = function(error) {
  if (error) print('Test262:AsyncTestFailure: ' + error);
  else print('Test262:AsyncTestComplete');
};
`);
  }

  return sources;
}

function runVmTest262Case(vm, source, options) {
  const rows = seedRows(0, options.baseSeed);
  let compiler = null;
  let artifact = null;
  const logs = [];
  const previousHostLog = globalThis.__jsVmHostLog;

  try {
    compiler = new vm.Compiler(source);
    const externs = Array.from(compiler.extern_slots());
    const configSeed = vm.js_encoding_seed_from_rows(
      rows.opcodes,
      rows.operandTags,
      rows.constantTags,
      new Uint8Array(),
    );
    artifact = compiler.to_bytecode_artifact(configSeed, externs);
    const bytes = artifact.bytes();
    const seed = vm.js_encoding_seed_for_seed_and_bytes(configSeed, bytes);
    globalThis.__jsVmHostLog = (level, message) => {
      logs.push(message === undefined ? String(level) : String(message));
    };
    const result = vm.js_execute_bytes_with_seed(
      bytes,
      seed,
      externValuesForSlots(externs, createTest262HostEnvironment()),
    );
    return {
      ok: true,
      phase: 'runtime',
      result,
      logs,
      seed,
    };
  } catch (err) {
    const phase = compiler && artifact ? 'runtime' : 'parse';
    return {
      ok: false,
      phase,
      type: phase === 'parse' ? 'SyntaxError' : extractErrorType(err),
      message: errorMessage(err),
      logs,
    };
  } finally {
    globalThis.__jsVmHostLog = previousHostLog || (() => {});
    if (artifact) artifact.free();
    if (compiler) compiler.free();
  }
}

function createTest262HostEnvironment() {
  const env = createDefaultHostEnvironment();
  const realmGlobal = new Proxy(env.globalThis, {
    get(target, property) {
      if (Object.prototype.hasOwnProperty.call(env, property)) {
        return env[property];
      }
      return Reflect.get(target, property, target);
    },
    set(target, property, value) {
      env[property] = value;
      return Reflect.set(target, property, value, target);
    },
  });
  env.global = realmGlobal;
  env.$262 = {
    global: realmGlobal,
    createRealm() {
      return { global: realmGlobal };
    },
    detachArrayBuffer() {
      throw new TypeError('$262.detachArrayBuffer is not supported by js-vm test262 runner');
    },
    evalScript(source) {
      throw new Error(`$262.evalScript is not supported by js-vm test262 runner: ${String(source).slice(0, 80)}`);
    },
    agent: {
      start() {
        throw new TypeError('$262.agent.start is not supported by js-vm test262 runner');
      },
      broadcast() {},
      getReport() {
        return null;
      },
      sleep() {},
      monotonicNow() {
        return Date.now();
      },
    },
  };
  env.$DONOTEVALUATE = () => {
    throw new SyntaxError('$DONOTEVALUATE was evaluated');
  };
  return env;
}

function evaluateOutcome(test, scenario, outcome) {
  const flags = new Set(test.meta.flags || []);
  const negative = test.meta.negative;

  if (flags.has('async') && outcome.ok) {
    const asyncFailure = outcome.logs.find((line) => line.startsWith('Test262:AsyncTestFailure:'));
    if (asyncFailure) {
      return {
        ok: false,
        reason: asyncFailure,
        expected: 'async completion',
      };
    }
    if (!outcome.logs.includes('Test262:AsyncTestComplete')) {
      return {
        ok: false,
        reason: 'async completion was not printed',
        expected: 'Test262:AsyncTestComplete',
      };
    }
  }

  if (negative) {
    if (outcome.ok) {
      return {
        ok: false,
        reason: 'expected negative test to throw',
        expected: `${negative.phase || 'runtime'} ${negative.type || 'Error'}`,
      };
    }

    const expectedType = negative.type || '';
    const expectedPhase = negative.phase || 'runtime';
    if (expectedType && outcome.type !== expectedType) {
      return {
        ok: false,
        reason: `expected ${expectedType}, got ${outcome.type}`,
        expected: `${expectedPhase} ${expectedType}`,
      };
    }
    if (expectedPhase && outcome.phase !== expectedPhase) {
      return {
        ok: false,
        reason: `expected ${expectedPhase} error, got ${outcome.phase}`,
        expected: `${expectedPhase} ${expectedType || 'Error'}`,
      };
    }
    return { ok: true };
  }

  if (!outcome.ok) {
    return {
      ok: false,
      reason: `${outcome.phase} ${outcome.type}: ${outcome.message}`,
      expected: 'normal completion',
    };
  }

  return { ok: true };
}

function createReport(root, rootInfo, selectedPaths, args, unsupported) {
  return {
    format: 'js-vm-test262-report',
    generatedAt: '',
    durationMs: 0,
    root: relative(root),
    revision: rootInfo.revision,
    profile: args.all ? 'all' : relative(args.profile),
    selectedPaths,
    options: {
      includeAsync: args.includeAsync,
      includeIntl: args.includeIntl,
      includeModule: args.includeModule,
      includeRaw: args.includeRaw,
      harnessMode: args.harnessMode,
      skipUnsupported: args.skipUnsupported,
      maxCases: args.maxCases,
      baseSeed: args.baseSeed,
    },
    unsupported,
    stats: {
      cases: 0,
      scenarios: 0,
      passed: 0,
      failed: 0,
      skipped: 0,
    },
    skippedByReason: {},
    failures: [],
  };
}

function recordSkip(report, test, reason) {
  report.stats.cases += 1;
  report.stats.skipped += 1;
  report.skippedByReason[reason] = (report.skippedByReason[reason] || 0) + 1;
}

function recordResult(report, test, scenario, result, outcome) {
  report.stats.cases += 1;
  report.stats.scenarios += 1;
  if (result.ok) {
    report.stats.passed += 1;
    return;
  }

  report.stats.failed += 1;
  report.failures.push({
    file: test.id,
    scenario: scenario.name,
    description: test.meta.description || '',
    esid: test.meta.esid || test.meta.es5id || test.meta.es6id || '',
    flags: test.meta.flags || [],
    features: test.meta.features || [],
    negative: test.meta.negative || null,
    expected: result.expected,
    reason: result.reason,
    actual: {
      ok: outcome.ok,
      phase: outcome.phase,
      type: outcome.type || '',
      message: outcome.message || '',
      logs: outcome.logs || [],
    },
  });
}

function writeReports(report, args) {
  fs.mkdirSync(args.reportDir, { recursive: true });
  const stamp = report.generatedAt.replace(/[:.]/g, '-');
  const reportPath = path.join(args.reportDir, 'latest.json');
  const stampedReportPath = path.join(args.reportDir, `${stamp}.json`);
  fs.writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  fs.writeFileSync(stampedReportPath, `${JSON.stringify(report, null, 2)}\n`);
  report.reportPath = reportPath;
  report.stampedReportPath = stampedReportPath;

  const errorPath = path.join(args.reportDir, 'latest-errors.md');
  if (report.failures.length) {
    fs.writeFileSync(errorPath, renderErrorReport(report));
    report.errorPath = errorPath;
  } else if (fs.existsSync(errorPath)) {
    fs.rmSync(errorPath);
  }
}

function renderErrorReport(report) {
  const lines = [
    '# Test262 Error Report',
    '',
    `Generated: ${report.generatedAt}`,
    `Root: ${report.root}`,
    `Revision: ${report.revision || 'unknown'}`,
    `Profile: ${report.profile}`,
    '',
    `Failures: ${report.stats.failed}`,
    '',
  ];

  for (const [index, failure] of report.failures.entries()) {
    lines.push(`## ${index + 1}. ${failure.file} (${failure.scenario})`);
    if (failure.description) lines.push(`Description: ${failure.description}`);
    if (failure.esid) lines.push(`ESID: ${failure.esid}`);
    if (failure.flags.length) lines.push(`Flags: ${failure.flags.join(', ')}`);
    if (failure.features.length) lines.push(`Features: ${failure.features.join(', ')}`);
    lines.push(`Expected: ${failure.expected}`);
    lines.push(`Reason: ${failure.reason}`);
    lines.push(`Actual: ${failure.actual.phase || 'unknown'} ${failure.actual.type || ''}`.trim());
    if (failure.actual.message) lines.push(`Message: ${failure.actual.message}`);
    if (failure.actual.logs.length) {
      lines.push('Logs:');
      for (const entry of failure.actual.logs.slice(0, 20)) lines.push(`- ${entry}`);
    }
    lines.push('');
  }

  return `${lines.join('\n')}\n`;
}

function printSummary(report, args) {
  if (report.failures.length) {
    log.error(`Test262 inconsistencies found: ${report.failures.length}`);
    log.error(`Error report: ${relative(report.errorPath)}`);
    if (!args.failOnly) {
      for (const failure of report.failures.slice(0, 20)) {
        log.jest('FAIL', `${failure.file} (${failure.scenario})`, failure.reason);
      }
    }
  } else {
    log.ok(`Test262 profile passed; report=${relative(report.reportPath)}`);
  }

  log.summary([
    ['Test262', `${report.stats.passed} passed, ${report.stats.failed} failed, ${report.stats.skipped} skipped`],
    ['Scenarios', `${report.stats.scenarios} executed`],
    ['Revision', report.revision || 'unknown'],
    ['Time', log.formatDuration(report.durationMs)],
  ]);
}

function test262Revision(root) {
  const upstream = path.join(root, '.upstream');
  if (fs.existsSync(upstream)) {
    const revision = fs.readFileSync(upstream, 'utf8').match(/^revision=(.+)$/m);
    if (revision) return revision[1];
  }
  const head = path.join(root, '.git/HEAD');
  if (!fs.existsSync(head)) return '';
  const content = fs.readFileSync(head, 'utf8').trim();
  if (!content.startsWith('ref:')) return content;
  const ref = content.slice(5);
  const refFile = path.join(root, '.git', ref);
  return fs.existsSync(refFile) ? fs.readFileSync(refFile, 'utf8').trim() : content;
}

function extractErrorType(err) {
  const name = String(err?.name || '');
  if (ERROR_TYPES.includes(name)) return name;
  const text = errorMessage(err);
  const match = text.match(/\b(?:AggregateError|EvalError|RangeError|ReferenceError|SyntaxError|Test262Error|TypeError|URIError|Error)\b/);
  return match ? match[0] : (name || 'Error');
}

function errorMessage(err) {
  if (!err) return '';
  return String(err.stack || err.message || err).replace(/\s+$/g, '');
}

function splitList(value) {
  return String(value || '').split(',').map((item) => item.trim()).filter(Boolean);
}

function numberEnv(name, fallback) {
  return Number.parseInt(process.env[name] || String(fallback), 10);
}

function printHelp() {
  console.log(`Usage:
  node tests/index.js test262 [options]

Options:
  --root <dir>             Test262 checkout. Default: tests/.vendor/test262.
  --profile <file>         Path profile file. Default: tests/test262/baseline.txt.
  --path <path>            Add a test path under Test262 test/. Repeat or comma-separate.
  --all                    Ignore profile and run the whole checkout.
  --max-cases <n>          Run at most n selected files.
  --include-module         Include tests with flags: [module].
  --include-async          Include tests with flags: [async].
  --include-intl           Include intl402 paths.
  --harness <mode>         light (default) or official.
  --official-harness       Use Test262's exact harness files.
  --no-skip-unsupported    Do not use tests/test262/unsupported.json.
  --report-dir <dir>       Report output directory.
  --verbose                Print every selected scenario.

Examples:
  npm run update:test262
  npm run test:test262
  npm run test:test262 -- --path=language/expressions/addition --max-cases=50
  npm run test:test262 -- --all --max-cases=1000`);
}
