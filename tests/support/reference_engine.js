const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

let cachedV8Path = null;

function resolveJsvuV8Path(explicitPath = '') {
  if (explicitPath) {
    const normalized = normalizeJsvuV8Path(explicitPath);
    if (normalized) return normalized;
  }
  if (cachedV8Path && isExecutableFile(cachedV8Path)) return cachedV8Path;

  const candidates = [
    process.env.JSVU_V8_PATH,
    process.env.JSVU_V8,
    process.env.JS_VM_V8_PATH,
    process.env.JS_VM_V8,
    path.join(os.homedir(), '.jsvu/engines/v8/v8'),
    path.join(os.homedir(), '.jsvu/bin/v8'),
    commandPath('v8'),
  ].filter(Boolean).map(normalizeJsvuV8Path).filter(Boolean);

  cachedV8Path = candidates.find(isExecutableFile) || '';
  return cachedV8Path;
}

function requireJsvuV8Path(explicitPath = '') {
  const v8Path = resolveJsvuV8Path(explicitPath);
  if (v8Path) return v8Path;
  throw new Error(
    [
      'jsvu V8 engine is not installed or not found.',
      'Install it with: npx jsvu --engines=v8',
      'In Linux CI use: npx jsvu --os=linux64 --engines=v8',
      'Or set JSVU_V8_PATH=/path/to/v8.',
    ].join('\n'),
  );
}

function v8EvalArgs(source, v8Path = '') {
  const snapshot = v8SnapshotArg(v8Path);
  return snapshot ? [snapshot, '-e', source] : ['-e', source];
}

function v8ReferencePrelude() {
  return [
    'const __jsVmV8Print = print;',
    'const __jsVmV8PrintErr = typeof printErr === "function" ? printErr : print;',
    'globalThis.console = globalThis.console || {};',
    'for (const k of ["log","info","debug"]) console[k] = (...args) => __jsVmV8Print(args.map(String).join(" "));',
    'for (const k of ["warn","error"]) console[k] = (...args) => __jsVmV8PrintErr(args.map(String).join(" "));',
    'for (const k of ["time","timeEnd","timeLog","count","countReset","trace","assert","group","groupEnd","groupCollapsed","clear","table","dir","dirxml","profile","profileEnd"]) console[k] = function() {};',
    'globalThis.__vmPrint = (...args) => __jsVmV8Print(args.map(String).join(" "));',
    '',
  ].join('\n');
}

function commandPath(command) {
  const result = spawnSync('which', [command], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  return result.status === 0 ? result.stdout.trim() : '';
}

function normalizeJsvuV8Path(file) {
  if (!file) return '';
  const resolved = path.resolve(file);
  const wrapperTarget = jsvuWrapperTarget(resolved);
  if (wrapperTarget && isExecutableFile(wrapperTarget)) return wrapperTarget;
  return isExecutableFile(resolved) ? resolved : '';
}

function jsvuWrapperTarget(file) {
  const normalized = file.split(path.sep).join('/');
  if (!normalized.endsWith('/.jsvu/bin/v8')) return '';
  const enginePath = path.join(path.dirname(path.dirname(file)), 'engines/v8/v8');
  if (isExecutableFile(enginePath)) return enginePath;

  try {
    const wrapper = fs.readFileSync(file, 'utf8');
    const match = wrapper.match(/"([^"]+\/engines\/v8\/v8)"/);
    return match ? match[1] : '';
  } catch {
    return '';
  }
}

function v8SnapshotArg(v8Path) {
  if (!v8Path) return '';
  const snapshot = path.join(path.dirname(path.resolve(v8Path)), 'snapshot_blob.bin');
  return fs.existsSync(snapshot) ? `--snapshot_blob=${snapshot}` : '';
}

function isExecutableFile(file) {
  if (!file) return false;
  try {
    fs.accessSync(path.resolve(file), fs.constants.X_OK);
    return fs.statSync(path.resolve(file)).isFile();
  } catch {
    return false;
  }
}

module.exports = {
  resolveJsvuV8Path,
  requireJsvuV8Path,
  v8EvalArgs,
  v8ReferencePrelude,
};
