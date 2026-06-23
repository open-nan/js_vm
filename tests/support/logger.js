const crypto = require('node:crypto');
const fs = require('node:fs');

const startedAt = Date.now();
const useColor =
  !process.env.NO_COLOR && (process.env.FORCE_COLOR || Boolean(process.stdout.isTTY));

const colors = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  gray: '\x1b[90m',
};

const levelColors = {
  OK: 'green',
  PASS: 'green',
  FAIL: 'red',
  ERROR: 'red',
  WARN: 'yellow',
  SKIP: 'yellow',
  RUN: 'cyan',
  CASE: 'cyan',
  STEP: 'cyan',
  INFO: 'gray',
  DONE: 'magenta',
};

function step(message) {
  log('STEP', message);
}

function info(message) {
  log('INFO', message);
}

function ok(message) {
  log('OK', message);
}

function warn(message) {
  log('WARN', message);
}

function error(message) {
  log('ERROR', message);
}

function progress(label, current, total, message) {
  log(label, `${progressText(current, total)} ${message}`);
}

function finish(message) {
  ok(`${message} (${formatDuration(Date.now() - startedAt)})`);
}

function timed(label, fn) {
  const started = Date.now();
  step(label);
  const result = fn();
  ok(`${label} completed in ${formatDuration(Date.now() - started)}`);
  return result;
}

function log(level, message) {
  const label = colorize(level.padEnd(5), levelColors[level] || 'reset');
  console.log(`${colorize(`[${timestamp()}]`, 'gray')} ${label} ${message}`);
}

function jest(status, file, details = '') {
  const label = colorize(status.padStart(5), levelColors[status] || 'reset');
  const suffix = details ? ` ${colorize(details, 'gray')}` : '';
  console.log(`${label} ${file}${suffix}`);
}

function summary(rows) {
  console.log('');
  for (const [label, value] of rows) {
    console.log(`${colorize(`${label}:`.padEnd(12), 'bold')} ${value}`);
  }
}

function progressText(current, total) {
  return `[${current}/${total}]`;
}

function md5Text(text) {
  return crypto.createHash('md5').update(text).digest('hex');
}

function md5File(file) {
  return md5Text(fs.readFileSync(file));
}

function colorize(value, color) {
  if (!useColor || !colors[color]) return value;
  return `${colors[color]}${value}${colors.reset}`;
}

function timestamp(date = new Date()) {
  return date.toTimeString().slice(0, 8);
}

function formatDuration(ms) {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = ((ms % 60_000) / 1000).toFixed(1).padStart(4, '0');
  return `${minutes}m${seconds}s`;
}

module.exports = {
  step,
  info,
  ok,
  warn,
  error,
  progress,
  finish,
  timed,
  jest,
  summary,
  progressText,
  md5Text,
  md5File,
  formatDuration,
};
