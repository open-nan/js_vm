#!/usr/bin/env node

const { spawnSync } = require('node:child_process');
const path = require('node:path');
const log = require('./support/logger.js');

const ROOT = path.resolve(__dirname, '..');
const steps = [
  ['Unit and chain tests', 'npm', ['run', 'verify']],
  ['Fuzz smoke tests', 'npm', ['run', 'fuzz:quick']],
];

log.step('Running pre-commit checks');
for (const [index, [label, command, args]] of steps.entries()) {
  log.jest('RUN', label, log.progressText(index + 1, steps.length));
  run(label, command, args);
}
log.finish('Pre-commit checks passed');

function run(label, command, args) {
  const started = Date.now();
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: process.env,
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    log.jest('FAIL', label, `time=${log.formatDuration(Date.now() - started)}`);
    process.exit(result.status || 1);
  }
  log.jest('PASS', label, `time=${log.formatDuration(Date.now() - started)}`);
}
