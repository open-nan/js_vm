const fs = require('node:fs');
const path = require('node:path');
const { ROOT, relative } = require('./vm_chain.js');

const CORPUS_ROOT = path.join(ROOT, 'tests/corpus');
const TEST_FILE_RE = /\.test\.(?:js|ts|jsx|tsx|vue|vue\.tsx|vue\.jsx)$/;
const SKIP_DIRS = new Set(['.vendor', 'node_modules']);

function readCorpusFiles(dir = CORPUS_ROOT) {
  if (!fs.existsSync(dir)) return [];
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  return entries
    .flatMap((entry) => {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        return SKIP_DIRS.has(entry.name) ? [] : readCorpusFiles(fullPath);
      }
      return [fullPath];
    })
    .filter((file) => TEST_FILE_RE.test(file))
    .sort();
}

function readCorpusCase(file) {
  const rawSource = fs.readFileSync(file, 'utf8');
  const meta = parseMeta(rawSource, file);
  return {
    file,
    rawSource,
    source: sourceForCompiler(rawSource, file),
    meta,
  };
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
    seeds: meta.seeds ? Number.parseInt(meta.seeds, 10) : undefined,
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

function hostRunnable(file) {
  return !/\.(?:ts|tsx|jsx|vue\.tsx|vue\.jsx)$/.test(file);
}

function observableHostSource(source, host = 'node') {
  const lines = source.split(/\r?\n/);
  const index = findLastExpressionLine(lines);
  if (index < 0) return null;
  const expression = lines[index].trim().replace(/;$/, '');
  lines[index] = `__vmObserve(${expression});`;
  return `${observablePrelude(host)}\n${lines.join('\n')}`;
}

function findLastExpressionLine(lines) {
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = lines[index].trim();
    if (!line || line.startsWith('//') || line.startsWith('<!--')) continue;
    if (line === '}' || line === '};') return -1;
    if (!line.endsWith(';')) return -1;
    const expression = line.slice(0, -1).trim();
    if (!expression || /^(?:const|let|var|function|class|if|for|while|switch|try|catch|finally|throw|return|export|import)\b/.test(expression)) {
      return -1;
    }
    return index;
  }
  return -1;
}

function observablePrelude(host) {
  const consoleShim =
    host === 'd8'
      ? `
if (typeof console === "undefined") {
  globalThis.console = { log: (...args) => print(args.map(__vmFormat).join(" ")) };
}`
      : '';
  return `
function __vmFormat(value) {
  if (typeof value === "number") {
    if (Number.isNaN(value)) return "NaN";
    if (Object.is(value, -0)) return "-0";
  }
  return String(value);
}
${consoleShim}
function __vmObserve(value) {
  console.log(__vmFormat(value));
}`;
}

module.exports = {
  CORPUS_ROOT,
  readCorpusFiles,
  readCorpusCase,
  parseMeta,
  sourceForCompiler,
  hostRunnable,
  observableHostSource,
};
