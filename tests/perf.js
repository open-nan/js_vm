#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const log = require('./support/logger.js');
const {
  ROOT,
  relative,
  createDefaultHostEnvironment,
} = require('./support/vm_chain.js');

const DEFAULT_PROFILE_DIR = path.join(ROOT, 'pkg/executor-node-profile');

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

  if (args.buildProfile) {
    buildProfileRuntime(args);
  }

  const target = resolveTarget(args);
  const runtime = loadProfileRuntime(args.profilePkg);
  const externValues = createExternValues(target.externSlots, args);
  const sourceMap = loadSourceMap(target.map);

  log.step(`Profiling ${relative(target.bin)}`);
  log.info(`bytes=${formatBytes(target.bytes.length)} steps=${formatNumber(args.steps)} repeat=${args.repeat} warmup=${args.warmup}`);
  if (target.wrapper) log.info(`wrapper=${relative(target.wrapper)}`);
  if (target.map) log.info(`sourceMap=${relative(target.map)}`);

  const runs = [];
  let lastResult = null;
  for (let index = 0; index < args.warmup + args.repeat; index += 1) {
    const measured = index >= args.warmup;
    const runIndex = measured ? index - args.warmup + 1 : index + 1;
    const started = process.hrtime.bigint();
    const result = runtime.js_profile_execute_bytes_with_seed_and_runtime_limits(
      target.bytes,
      target.seed,
      externValues,
      args.callDepth,
      args.recursion,
      args.steps,
    );
    const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
    lastResult = result;
    if (measured) {
      runs.push(elapsedMs);
      log.jest(
        'PASS',
        `run ${runIndex}/${args.repeat}`,
        `time=${formatMs(elapsedMs)} ok=${Boolean(result.ok)} steps=${formatNumber(result.profile?.instructionCount || 0)}`,
      );
    } else {
      log.jest('RUN', `warmup ${runIndex}/${args.warmup}`, `time=${formatMs(elapsedMs)} ok=${Boolean(result.ok)}`);
    }
  }

  if (!lastResult?.profile) {
    throw new Error('profile runtime did not return profile data');
  }

  const stats = summarizeRuns(runs);
  const report = buildReport({
    args,
    target,
    result: lastResult,
    runs,
    stats,
    sourceMap,
  });
  printReport(report, args);

  if (args.json) {
    fs.mkdirSync(path.dirname(args.json), { recursive: true });
    fs.writeFileSync(args.json, `${JSON.stringify(report, null, 2)}\n`);
    log.ok(`Wrote perf report ${relative(args.json)}`);
  }

  if (args.dumpTop > 0) {
    dumpHotPcs(target, report.hotPcs.slice(0, args.dumpTop));
  }
}

function parseArgs(argv) {
  const args = {
    bin: '',
    seed: '',
    wrapper: '',
    map: '',
    externs: '',
    externsFile: '',
    profilePkg: DEFAULT_PROFILE_DIR,
    steps: 1_000_000,
    repeat: 10,
    warmup: 1,
    callDepth: 2048,
    recursion: 128,
    top: 20,
    dumpTop: 5,
    json: '',
    strictExterns: false,
    buildProfile: false,
    help: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const value = () => {
      const next = argv[++index];
      if (!next) throw new Error(`${arg} requires a value`);
      return next;
    };

    if (arg === '-h' || arg === '--help') args.help = true;
    else if (arg === '--bin') args.bin = path.resolve(value());
    else if (arg.startsWith('--bin=')) args.bin = path.resolve(arg.slice(6));
    else if (arg === '--seed') args.seed = value();
    else if (arg.startsWith('--seed=')) args.seed = arg.slice(7);
    else if (arg === '--wrapper') args.wrapper = path.resolve(value());
    else if (arg.startsWith('--wrapper=')) args.wrapper = path.resolve(arg.slice(10));
    else if (arg === '--map') args.map = path.resolve(value());
    else if (arg.startsWith('--map=')) args.map = path.resolve(arg.slice(6));
    else if (arg === '--externs') args.externs = value();
    else if (arg.startsWith('--externs=')) args.externs = arg.slice(10);
    else if (arg === '--externs-file') args.externsFile = path.resolve(value());
    else if (arg.startsWith('--externs-file=')) args.externsFile = path.resolve(arg.slice(15));
    else if (arg === '--profile-pkg') args.profilePkg = path.resolve(value());
    else if (arg.startsWith('--profile-pkg=')) args.profilePkg = path.resolve(arg.slice(14));
    else if (arg === '--steps') args.steps = parsePositiveInt(value(), 'steps');
    else if (arg.startsWith('--steps=')) args.steps = parsePositiveInt(arg.slice(8), 'steps');
    else if (arg === '--repeat') args.repeat = parsePositiveInt(value(), 'repeat');
    else if (arg.startsWith('--repeat=')) args.repeat = parsePositiveInt(arg.slice(9), 'repeat');
    else if (arg === '--warmup') args.warmup = parseNonNegativeInt(value(), 'warmup');
    else if (arg.startsWith('--warmup=')) args.warmup = parseNonNegativeInt(arg.slice(9), 'warmup');
    else if (arg === '--call-depth') args.callDepth = parsePositiveInt(value(), 'call-depth');
    else if (arg.startsWith('--call-depth=')) args.callDepth = parsePositiveInt(arg.slice(13), 'call-depth');
    else if (arg === '--recursion') args.recursion = parsePositiveInt(value(), 'recursion');
    else if (arg.startsWith('--recursion=')) args.recursion = parsePositiveInt(arg.slice(12), 'recursion');
    else if (arg === '--top') args.top = parsePositiveInt(value(), 'top');
    else if (arg.startsWith('--top=')) args.top = parsePositiveInt(arg.slice(6), 'top');
    else if (arg === '--dump-top') args.dumpTop = parseNonNegativeInt(value(), 'dump-top');
    else if (arg.startsWith('--dump-top=')) args.dumpTop = parseNonNegativeInt(arg.slice(11), 'dump-top');
    else if (arg === '--no-dump') args.dumpTop = 0;
    else if (arg === '--json') args.json = path.resolve(value());
    else if (arg.startsWith('--json=')) args.json = path.resolve(arg.slice(7));
    else if (arg === '--strict-externs') args.strictExterns = true;
    else if (arg === '--build-profile') args.buildProfile = true;
    else if (!args.wrapper && /\.m?js(?:\?.*)?$/.test(arg)) args.wrapper = path.resolve(arg);
    else if (!args.bin && /\.bin(?:\?.*)?$/.test(arg)) args.bin = path.resolve(arg);
    else throw new Error(`unknown perf option: ${arg}`);
  }

  args.profilePkg = path.resolve(args.profilePkg);
  return args;
}

function resolveTarget(args) {
  const wrapperInfo = args.wrapper ? parseWrapper(args.wrapper) : {};
  const bin = args.bin || wrapperInfo.bin;
  const seed = args.seed || wrapperInfo.seed;
  const map = args.map || wrapperInfo.map || defaultMapForBin(bin);
  const externSlots = readExternSlots(args, wrapperInfo.externSlots || []);

  if (!bin) throw new Error('missing --bin or --wrapper');
  if (!seed) throw new Error('missing --seed or wrapper seed');
  if (!fs.existsSync(bin)) throw new Error(`bin not found: ${bin}`);

  return {
    bin,
    seed,
    map: map && fs.existsSync(map) ? map : '',
    wrapper: args.wrapper || '',
    externSlots,
    bytes: fs.readFileSync(bin),
  };
}

function parseWrapper(file) {
  if (!fs.existsSync(file)) throw new Error(`wrapper not found: ${file}`);
  const source = fs.readFileSync(file, 'utf8');
  const seed = matchString(source, /const\s+__jsVmSeed\s*=\s*"([^"]+)"/);
  const binSpecifier = matchString(source, /const\s+__jsVmBinUrl\s*=\s*new\s+URL\("([^"]+)"/);
  const mapSpecifier = matchString(source, /const\s+__jsVmSourceMapUrl\s*=\s*new\s+URL\("([^"]+)"/);
  const externSlotsSource = matchString(source, /externSlots:\s*(\[[^\]]*\])/);
  return {
    seed,
    bin: binSpecifier ? resolveWrapperResource(file, binSpecifier) : '',
    map: mapSpecifier ? resolveWrapperResource(file, mapSpecifier) : '',
    externSlots: externSlotsSource ? JSON.parse(externSlotsSource) : [],
  };
}

function matchString(source, pattern) {
  return source.match(pattern)?.[1] || '';
}

function resolveWrapperResource(wrapper, specifier) {
  const clean = String(specifier).replace(/[?#].*$/, '');
  return path.resolve(path.dirname(wrapper), clean);
}

function defaultMapForBin(bin) {
  return bin ? `${bin}.map` : '';
}

function readExternSlots(args, fallback) {
  if (args.externsFile) {
    return JSON.parse(fs.readFileSync(args.externsFile, 'utf8'));
  }
  if (args.externs) {
    const trimmed = args.externs.trim();
    if (trimmed.startsWith('[')) return JSON.parse(trimmed);
    return trimmed.split(',').map((item) => item.trim()).filter(Boolean);
  }
  return fallback;
}

function loadProfileRuntime(profilePkg) {
  const runtimePath = path.join(profilePkg, 'js_vm_runtime_node.js');
  const wasmPath = path.join(profilePkg, 'js_vm_runtime_node_bg.wasm');
  if (!fs.existsSync(runtimePath) || !fs.existsSync(wasmPath)) {
    throw new Error(
      `runtime profile package is missing at ${relative(profilePkg)}; run npm run perf -- --build-profile first`,
    );
  }
  const runtime = require(runtimePath);
  if (typeof runtime.js_profile_execute_bytes_with_seed_and_runtime_limits !== 'function') {
    throw new Error(`${relative(runtimePath)} was not built with runtime-profile feature`);
  }
  return runtime;
}

function buildProfileRuntime(args) {
  log.step('Building runtime-profile node package');
  const result = spawnSync(
    'wasm-pack',
    [
      'build',
      '--release',
      'crates/runtime/bin/node',
      '--target',
      'nodejs',
      '--out-dir',
      path.relative(path.join(ROOT, 'crates/runtime/bin/node'), args.profilePkg),
      '--no-default-features',
      '--features',
      'full,runtime-profile',
    ],
    {
      cwd: ROOT,
      env: process.env,
      stdio: 'inherit',
      shell: process.platform === 'win32',
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`build runtime-profile failed with code ${result.status}`);
  }
}

function createExternValues(externSlots, args) {
  const baseEnvironment = createDefaultHostEnvironment();
  const stubCache = new Map();
  const makeStub = (name) => {
    if (stubCache.has(name)) return stubCache.get(name);
    let proxy;
    const target = function () { return proxy; };
    proxy = new Proxy(target, {
      get(_target, prop) {
        if (prop === Symbol.toPrimitive) return () => 0;
        if (prop === 'then') return undefined;
        if (prop === 'valueOf') return () => 0;
        if (prop === 'toString') return () => `[stub ${name}]`;
        if (prop === 'length') return 0;
        return makeStub(`${name}.${String(prop)}`);
      },
      set() { return true; },
      has() { return true; },
      apply() { return proxy; },
      construct() { return proxy; },
      ownKeys() { return []; },
      getOwnPropertyDescriptor() {
        return { configurable: true, enumerable: false, value: undefined };
      },
    });
    stubCache.set(name, proxy);
    return proxy;
  };
  const makeElement = (tagName = 'div') => {
    const listeners = new Map();
    const style = {};
    const attributes = {};
    const element = {
      nodeType: 1,
      nodeName: String(tagName).toUpperCase(),
      tagName: String(tagName).toUpperCase(),
      childNodes: [],
      children: [],
      parentNode: null,
      ownerDocument: null,
      style,
      className: '',
      textContent: '',
      innerHTML: '',
      dataset: {},
      attributes,
      appendChild(child) {
        if (child && typeof child === 'object') child.parentNode = element;
        element.childNodes.push(child);
        element.children.push(child);
        return child;
      },
      insertBefore(child, reference) {
        if (child && typeof child === 'object') child.parentNode = element;
        const index = element.childNodes.indexOf(reference);
        if (index >= 0) {
          element.childNodes.splice(index, 0, child);
          element.children.splice(index, 0, child);
        } else {
          element.childNodes.push(child);
          element.children.push(child);
        }
        return child;
      },
      removeChild(child) {
        element.childNodes = element.childNodes.filter((item) => item !== child);
        element.children = element.children.filter((item) => item !== child);
        if (child && typeof child === 'object') child.parentNode = null;
        return child;
      },
      setAttribute(name, value) {
        attributes[String(name)] = String(value);
      },
      getAttribute(name) {
        return Object.prototype.hasOwnProperty.call(attributes, String(name))
          ? attributes[String(name)]
          : null;
      },
      removeAttribute(name) {
        delete attributes[String(name)];
      },
      addEventListener(type, listener) {
        const key = String(type);
        const values = listeners.get(key) || [];
        values.push(listener);
        listeners.set(key, values);
      },
      removeEventListener(type, listener) {
        const key = String(type);
        listeners.set(key, (listeners.get(key) || []).filter((item) => item !== listener));
      },
      dispatchEvent(event) {
        for (const listener of listeners.get(String(event?.type || '')) || []) {
          if (typeof listener === 'function') listener.call(element, event);
        }
        return true;
      },
      querySelector() { return null; },
      querySelectorAll() { return []; },
      getBoundingClientRect() {
        return { x: 0, y: 0, top: 0, right: 0, bottom: 0, left: 0, width: 0, height: 0 };
      },
      cloneNode() {
        return makeElement(tagName);
      },
    };
    return element;
  };
  const documentElement = makeElement('html');
  const head = makeElement('head');
  const body = makeElement('body');
  const documentStub = {
    nodeType: 9,
    nodeName: '#document',
    documentElement,
    head,
    body,
    defaultView: null,
    readyState: 'complete',
    createElement(tagName) {
      const element = makeElement(tagName);
      element.ownerDocument = documentStub;
      return element;
    },
    createElementNS(_namespace, tagName) {
      const element = makeElement(tagName);
      element.ownerDocument = documentStub;
      return element;
    },
    createTextNode(text) {
      return { nodeType: 3, nodeName: '#text', textContent: String(text), data: String(text), parentNode: null };
    },
    querySelector() { return null; },
    querySelectorAll() { return []; },
    getElementById() { return null; },
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() { return true; },
  };
  documentElement.ownerDocument = documentStub;
  head.ownerDocument = documentStub;
  body.ownerDocument = documentStub;
  class ElementStub {}
  class HTMLElementStub extends ElementStub {}
  class SVGElementStub extends ElementStub {}
  class DocumentStub {}
  class WindowStub {}
  class StorageStub {
    constructor() {
      this.store = Object.create(null);
    }
    get length() {
      return Object.keys(this.store).length;
    }
    key(index) {
      return Object.keys(this.store)[index] || null;
    }
    getItem(key) {
      const name = String(key);
      return Object.prototype.hasOwnProperty.call(this.store, name) ? this.store[name] : null;
    }
    setItem(key, value) {
      this.store[String(key)] = String(value);
    }
    removeItem(key) {
      delete this.store[String(key)];
    }
    clear() {
      this.store = Object.create(null);
    }
  }
  class ObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() { return []; }
  }
  const localStorage = new StorageStub();
  const historyStub = {
    length: 1,
    state: null,
    pushState() {},
    replaceState() {},
    back() {},
    forward() {},
    go() {},
  };
  const locationStub = {
    href: 'http://js-vm.perf/',
    origin: 'http://js-vm.perf',
    protocol: 'http:',
    host: 'js-vm.perf',
    hostname: 'js-vm.perf',
    pathname: '/',
    search: '',
    hash: '',
    assign(value) { this.href = String(value); },
    replace(value) { this.href = String(value); },
    reload() {},
    toString() { return this.href; },
  };
  const windowStub = {
    console: baseEnvironment.console,
    document: documentStub,
    history: historyStub,
    location: locationStub,
    localStorage,
    sessionStorage: new StorageStub(),
    navigator: { userAgent: 'js-vm-perf' },
    innerHeight: 768,
    innerWidth: 1024,
    pageXOffset: 0,
    pageYOffset: 0,
    scrollX: 0,
    scrollY: 0,
    scrollTo(leftOrOptions, top) {
      if (typeof leftOrOptions === 'object' && leftOrOptions) {
        this.scrollX = Number(leftOrOptions.left || 0);
        this.scrollY = Number(leftOrOptions.top || 0);
      } else {
        this.scrollX = Number(leftOrOptions || 0);
        this.scrollY = Number(top || 0);
      }
      this.pageXOffset = this.scrollX;
      this.pageYOffset = this.scrollY;
    },
    scrollBy(leftOrOptions, top) {
      if (typeof leftOrOptions === 'object' && leftOrOptions) {
        this.scrollTo(
          this.scrollX + Number(leftOrOptions.left || 0),
          this.scrollY + Number(leftOrOptions.top || 0),
        );
      } else {
        this.scrollTo(this.scrollX + Number(leftOrOptions || 0), this.scrollY + Number(top || 0));
      }
    },
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() { return true; },
    getComputedStyle() { return {}; },
    matchMedia() {
      return { matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} };
    },
  };
  windowStub.window = windowStub;
  windowStub.self = windowStub;
  windowStub.globalThis = windowStub;
  windowStub.parent = windowStub;
  windowStub.top = windowStub;
  documentStub.defaultView = windowStub;

  const environment = {
    ...baseEnvironment,
    global: globalThis,
    process,
    Buffer,
    URL,
    Uint8Array,
    Uint16Array,
    Int32Array,
    Float32Array,
    TextDecoder,
    Blob: globalThis.Blob || makeStub('Blob'),
    Event: globalThis.Event || class EventStub {
      constructor(type, options = {}) {
        this.type = String(type);
        this.bubbles = Boolean(options.bubbles);
        this.cancelable = Boolean(options.cancelable);
      }
    },
    CustomEvent: globalThis.CustomEvent || class CustomEventStub {
      constructor(type, options = {}) {
        this.type = String(type);
        this.detail = options.detail;
      }
    },
    Document: globalThis.Document || DocumentStub,
    Window: globalThis.Window || WindowStub,
    Intl,
    innerHeight: 768,
    innerWidth: 1024,
    pageXOffset: 0,
    pageYOffset: 0,
    scrollX: 0,
    scrollY: 0,
    scrollTo: windowStub.scrollTo.bind(windowStub),
    scrollBy: windowStub.scrollBy.bind(windowStub),
    history: historyStub,
    location: locationStub,
    document: documentStub,
    requestAnimationFrame: () => 0,
    setTimeout: () => 0,
    clearTimeout: () => undefined,
    Element: globalThis.Element || ElementStub,
    HTMLElement: globalThis.HTMLElement || HTMLElementStub,
    MathMLElement: globalThis.HTMLElement || HTMLElementStub,
    SVGElement: globalThis.SVGElement || SVGElementStub,
    MutationObserver: globalThis.MutationObserver || ObserverStub,
    ResizeObserver: globalThis.ResizeObserver || ObserverStub,
    Storage: globalThis.Storage || StorageStub,
    StorageEvent: globalThis.StorageEvent || class StorageEventStub {},
    WorkerGlobalScope: globalThis.WorkerGlobalScope || class WorkerGlobalScopeStub {},
    import: () => Promise.resolve({}),
  };
  environment.window = windowStub;
  environment.window.console = environment.console;
  environment.window.document = environment.document;
  environment.window.location = environment.location;
  environment.self = environment.window;
  environment.globalThis = environment.window;

  return externSlots.map((name) => {
    if (Object.prototype.hasOwnProperty.call(environment, name)) {
      const value = environment[name];
      if (value !== undefined || args.strictExterns) return value;
    }
    if (Object.prototype.hasOwnProperty.call(globalThis, name)) {
      const value = globalThis[name];
      if (value !== undefined || args.strictExterns) return value;
    }
    const value = resolvePath(environment, name);
    if (value !== undefined || args.strictExterns) return value;
    return makeStub(name);
  });
}

function resolvePath(environment, name) {
  const parts = String(name).split('.');
  let current = environment;
  for (const part of parts) {
    if (current == null) return undefined;
    current = current[part];
  }
  return current;
}

function loadSourceMap(file) {
  if (!file || !fs.existsSync(file)) return null;
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (err) {
    log.warn(`Cannot parse source map ${relative(file)}: ${err.message}`);
    return null;
  }
}

function summarizeRuns(runs) {
  const sorted = runs.slice().sort((a, b) => a - b);
  const avg = runs.reduce((sum, value) => sum + value, 0) / Math.max(1, runs.length);
  return {
    minMs: sorted[0] || 0,
    maxMs: sorted.at(-1) || 0,
    avgMs: avg,
    p50Ms: percentile(sorted, 0.50),
    p90Ms: percentile(sorted, 0.90),
    p99Ms: percentile(sorted, 0.99),
    stepsPerSecond: avg > 0 ? Math.round(1_000_000 / (avg / 1000)) : 0,
  };
}

function percentile(sorted, p) {
  if (!sorted.length) return 0;
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return sorted[index];
}

function buildReport({ args, target, result, runs, stats, sourceMap }) {
  const profile = result.profile;
  const hotPcs = (profile.hotPcs || []).slice(0, args.top).map((entry) => ({
    pc: Number(entry.pc),
    op: entry.op,
    count: Number(entry.count),
    source: sourceFrame(sourceMap, Number(entry.pc)),
  }));
  const functions = (profile.functions || []).slice(0, args.top).map((entry) => ({
    name: entry.name || '<anonymous>',
    bodyStart: Number(entry.bodyStart),
    bodyEnd: Number(entry.bodyEnd),
    callCount: Number(entry.callCount || 0),
    instructionCount: Number(entry.instructionCount || 0),
    source: sourceFrame(sourceMap, Number(entry.bodyStart)),
  }));
  const hotCallbacks = (profile.callbacks || []).slice(0, args.top).map((entry) => ({
    label: entry.label || '<unknown>',
    callCount: Number(entry.callCount || 0),
    instructionCount: Number(entry.instructionCount || 0),
  }));
  return {
    generatedAt: new Date().toISOString(),
    target: {
      bin: target.bin,
      wrapper: target.wrapper || null,
      map: target.map || null,
      byteLength: target.bytes.length,
      md5: log.md5File(target.bin),
      externSlots: target.externSlots,
    },
    settings: {
      steps: args.steps,
      repeat: args.repeat,
      warmup: args.warmup,
      callDepth: args.callDepth,
      recursion: args.recursion,
    },
    result: {
      ok: Boolean(result.ok),
      value: result.value,
      error: result.error,
    },
    runsMs: runs,
    stats,
    hostBridge: {
      get: Number(profile.hostGetCount || 0),
      set: Number(profile.hostSetCount || 0),
      call: Number(profile.hostCallCount || 0),
      construct: Number(profile.hostConstructCount || 0),
    },
    callbacks: {
      total: Number(profile.callbackCount || 0),
      fastPath: Number(profile.callbackFastPathCount || 0),
    },
    inlineCache: {
      loadNameHits: Number(profile.loadNameCacheHitCount || 0),
      loadNameMisses: Number(profile.loadNameCacheMissCount || 0),
      memberConstHits: Number(profile.memberConstCacheHitCount || 0),
      memberConstMisses: Number(profile.memberConstCacheMissCount || 0),
      callOneHits: Number(profile.callOneCacheHitCount || 0),
      callOneMisses: Number(profile.callOneCacheMissCount || 0),
      memberCallHits: Number(profile.memberCallCacheHitCount || profile.memberCallOneCacheHitCount || 0),
      memberCallMisses: Number(profile.memberCallCacheMissCount || profile.memberCallOneCacheMissCount || 0),
    },
    fused: {
      binaryBranch: Number(profile.fusedBinaryBranchCount || 0),
      moveBranch: Number(profile.fusedMoveBranchCount || 0),
      regBranchJump: Number(profile.fusedRegBranchJumpCount || 0),
      fastBinaryRegConst: Number(profile.fastBinaryRegConstCount || 0),
      fastBinaryRegReg: Number(profile.fastBinaryRegRegCount || 0),
    },
    instructionCount: Number(profile.instructionCount || 0),
    lastPc: profile.lastPc == null ? null : Number(profile.lastPc),
    callbackStack: Array.from(profile.callbackStack || []),
    opcodes: (profile.opcodes || []).slice(0, args.top).map((entry) => ({
      name: entry.name,
      count: Number(entry.count),
    })),
    functions,
    hotCallbacks,
    hotPcs,
  };
}

function printReport(report, args) {
  log.summary([
    ['Target', relative(report.target.bin)],
    ['Bytes', `${formatBytes(report.target.byteLength)} md5=${report.target.md5.slice(0, 12)}`],
    ['Result', report.result.ok ? 'ok' : `error: ${report.result.error || '-'}`],
    ['Runs', report.runsMs.map(formatMs).join(', ')],
    ['Latency', `p50=${formatMs(report.stats.p50Ms)} p90=${formatMs(report.stats.p90Ms)} avg=${formatMs(report.stats.avgMs)}`],
    ['Throughput', `${formatNumber(report.stats.stepsPerSecond)} steps/s`],
    ['HostBridge', `get=${formatNumber(report.hostBridge.get)} set=${formatNumber(report.hostBridge.set)} call=${formatNumber(report.hostBridge.call)} construct=${formatNumber(report.hostBridge.construct)}`],
    ['Callbacks', `total=${formatNumber(report.callbacks.total)} fast=${formatNumber(report.callbacks.fastPath)}`],
    ['Last PC', report.lastPc == null ? '-' : String(report.lastPc)],
    ['Callback Stack', report.callbackStack.length ? report.callbackStack.join(' <- ') : '-'],
    ['InlineCache', `loadName hit=${formatNumber(report.inlineCache.loadNameHits)} miss=${formatNumber(report.inlineCache.loadNameMisses)} rate=${percent(report.inlineCache.loadNameHits, report.inlineCache.loadNameHits + report.inlineCache.loadNameMisses)} | memberConst hit=${formatNumber(report.inlineCache.memberConstHits)} miss=${formatNumber(report.inlineCache.memberConstMisses)} rate=${percent(report.inlineCache.memberConstHits, report.inlineCache.memberConstHits + report.inlineCache.memberConstMisses)} | call1 hit=${formatNumber(report.inlineCache.callOneHits)} miss=${formatNumber(report.inlineCache.callOneMisses)} rate=${percent(report.inlineCache.callOneHits, report.inlineCache.callOneHits + report.inlineCache.callOneMisses)} | memberCall hit=${formatNumber(report.inlineCache.memberCallHits)} miss=${formatNumber(report.inlineCache.memberCallMisses)} rate=${percent(report.inlineCache.memberCallHits, report.inlineCache.memberCallHits + report.inlineCache.memberCallMisses)}`],
    ['Fused', `binaryBranch=${formatNumber(report.fused.binaryBranch)} moveBranch=${formatNumber(report.fused.moveBranch)} regBranchJump=${formatNumber(report.fused.regBranchJump)} fastBinaryRegConst=${formatNumber(report.fused.fastBinaryRegConst)} fastBinaryRegReg=${formatNumber(report.fused.fastBinaryRegReg)}`],
  ]);

  printTable('Top Opcodes', ['#', 'opcode', 'count', '%'], report.opcodes.map((entry, index) => [
    index + 1,
    entry.name,
    formatNumber(entry.count),
    percent(entry.count, report.instructionCount),
  ]));

  printTable('Hot Functions', ['#', 'function', 'body', 'calls', 'instr', '%', 'source'], report.functions.map((entry, index) => [
    index + 1,
    entry.name,
    `${entry.bodyStart}..${entry.bodyEnd}`,
    formatNumber(entry.callCount),
    formatNumber(entry.instructionCount),
    percent(entry.instructionCount, report.instructionCount),
    formatSource(entry.source),
  ]));

  printTable('Hot Callbacks', ['#', 'callback', 'calls', 'instr', '%'], report.hotCallbacks.map((entry, index) => [
    index + 1,
    entry.label,
    formatNumber(entry.callCount),
    formatNumber(entry.instructionCount),
    percent(entry.instructionCount, report.instructionCount),
  ]));

  printTable('Hot PCs', ['#', 'pc', 'op', 'count', 'source'], report.hotPcs.map((entry, index) => [
    index + 1,
    entry.pc,
    entry.op,
    formatNumber(entry.count),
    formatSource(entry.source),
  ]));

  if (!args.dumpTop) {
    log.info('Use --dump-top=<n> to dump bytecode around hot PCs');
  }
}

function printTable(title, headers, rows) {
  console.log(`\n${title}`);
  const widths = headers.map((header, index) => Math.max(
    String(header).length,
    ...rows.map((row) => String(row[index] ?? '').length),
  ));
  const formatRow = (row) => row.map((cell, index) => String(cell ?? '').padEnd(widths[index])).join('  ');
  console.log(formatRow(headers));
  console.log(widths.map((width) => '-'.repeat(width)).join('  '));
  for (const row of rows) console.log(formatRow(row));
}

function dumpHotPcs(target, hotPcs) {
  if (!hotPcs.length) return;
  console.log('\nBytecode Around Hot PCs');
  for (const entry of hotPcs) {
    console.log(`\n# pc ${entry.pc} ${entry.op} count=${formatNumber(entry.count)} ${formatSource(entry.source)}`);
    const result = spawnSync(
      'cargo',
      ['run', '-p', 'js_vm_cli', '--', 'dump-bytecode', target.bin, '--seed', target.seed, '--around', String(entry.pc)],
      {
        cwd: ROOT,
        env: process.env,
        encoding: 'utf8',
        shell: process.platform === 'win32',
      },
    );
    if (result.error || result.status !== 0) {
      console.log(`dump failed: ${result.error?.message || result.stderr || result.status}`);
      continue;
    }
    console.log(formatDumpOutput(result.stdout));
  }
}

function formatDumpOutput(output) {
  const lines = output.split(/\r?\n/);
  const codeStart = lines.findIndex((line) => line.trim() === '.code');
  const codeLines = (codeStart >= 0 ? lines.slice(codeStart + 1) : lines)
    .filter((line) => /^\s*\d+\s+/.test(line));
  return codeLines.join('\n');
}

function sourceFrame(sourceMap, pc) {
  const vm = sourceMap?.x_js_vm;
  if (!vm || !Array.isArray(vm.pcRanges)) return null;
  const range = vm.pcRanges.find((item) => pc >= item[0] && pc < item[1]);
  if (!range) return null;
  const opRange = Array.isArray(vm.pcOps)
    ? vm.pcOps.find((item) => pc >= item[0] && pc < item[1])
    : null;
  const span = range[5] >= 0 ? vm.sourceSpans?.[range[5]] : null;
  return {
    sourceFile: sourceMap.sources?.[0] || '',
    line: span ? span[2] : null,
    column: span ? span[3] : null,
    endLine: span ? span[4] : null,
    endColumn: span ? span[5] : null,
    byteStart: range[2],
    byteEnd: range[3],
    pcStart: range[0],
    pcEnd: range[1],
    function: range[4] >= 0 ? range[4] : null,
    op: opRange ? vm.opcodes?.[opRange[2]] : undefined,
  };
}

function formatSource(source) {
  if (!source) return '-';
  const location = source.line == null ? '?' : `${source.line + 1}:${(source.column || 0) + 1}`;
  return `${source.sourceFile || '-'}:${location}`;
}

function percent(value, total) {
  if (!total) return '0.00%';
  return `${((value / total) * 100).toFixed(2)}%`;
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
}

function formatMs(ms) {
  return `${ms.toFixed(2)}ms`;
}

function formatNumber(value) {
  return Number(value).toLocaleString('en-US');
}

function parsePositiveInt(value, name) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${name} must be a positive integer`);
  return parsed;
}

function parseNonNegativeInt(value, name) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed < 0) throw new Error(`${name} must be a non-negative integer`);
  return parsed;
}

function printHelp() {
  console.log(`Usage:
  npm run perf -- --wrapper <compiled-wrapper.js> [options]
  npm run perf -- --bin <file.bin> --seed <seed> [options]

Options:
  --wrapper <file>       VM wrapper .js; auto reads seed/bin/map/extern slots.
  --bin <file>           Bytecode .bin file.
  --seed <seed>          Obfuscation seed paired with the bytecode.
  --map <file>           Source map. Default: <bin>.map or wrapper map URL.
  --externs <list|json>  Extern slot names, comma list or JSON array.
  --externs-file <file>  JSON array of extern slot names.
  --steps <n>            VM execution step budget per run. Default: 1000000.
  --repeat <n>           Measured repeats. Default: 10.
  --warmup <n>           Warmup runs before measuring. Default: 1.
  --call-depth <n>       Runtime max call depth. Default: 2048.
  --recursion <n>        Runtime max recursive call depth. Default: 128.
  --top <n>              Top opcode/hot pc rows. Default: 20.
  --dump-top <n>         Dump bytecode around top hot PCs. Default: 5.
  --no-dump              Do not run dump-bytecode.
  --json <file>          Write structured perf report JSON.
  --strict-externs       Missing externs stay undefined instead of stub proxies.
  --build-profile        Build pkg/executor-node-profile before running.
  --profile-pkg <dir>    Runtime profile package. Default: pkg/executor-node-profile.

Examples:
  npm run perf -- --wrapper /private/tmp/nan-blogs-vm-optimized/assets/app-9LyPQIdV.js --steps=1000000 --repeat=10
  npm run perf -- --bin ./dist/app.bin --seed JSTKSEED2-... --externs Object,Array,window --no-dump
`);
}
