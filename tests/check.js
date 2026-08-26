#!/usr/bin/env node

const { spawnSync } = require('node:child_process');
const log = require('./support/logger.js');
const { ROOT } = require('./support/vm_chain.js');

main();

function main() {
  log.step('Running syntax and type checks');
  runStep('Rust check', 'cargo', ['check', '--workspace', '--all-targets'], {
    RUSTFLAGS: appendRustflags(process.env.RUSTFLAGS, '-Dwarnings'),
  });
  runStep('JS/TS/HTML/CSS/MD syntax check', 'cargo', ['run', '-p', 'js_vm_cli', '--', 'check', '.']);
  log.finish('Syntax and type checks passed');
}

function runStep(label, command, args, extraEnv = {}) {
  const started = Date.now();
  log.jest('RUN', label, '');
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...extraEnv },
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    log.jest('FAIL', label, `time=${log.formatDuration(Date.now() - started)}`);
    process.exit(result.status || 1);
  }
  log.jest('PASS', label, `time=${log.formatDuration(Date.now() - started)}`);
}

function appendRustflags(current, value) {
  const flags = String(current || '').trim();
  if (!flags) return value;
  if (flags.split(/\s+/).includes(value)) return flags;
  return `${flags} ${value}`;
}
