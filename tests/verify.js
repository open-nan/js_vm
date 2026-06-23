#!/usr/bin/env node

const { spawnSync } = require('node:child_process');
const path = require('node:path');
const log = require('./support/logger.js');

const ROOT = path.resolve(__dirname, '..');

const steps = [
  ['Rust unit tests', 'cargo', ['test']],
  ['Build wasm packages', process.execPath, ['scripts/build-wasm.js']],
  ['Run JS VM corpus', process.execPath, ['tests/E2E.js']],
  ['Run differential tests', process.execPath, ['tests/Differential.js']],
];

log.step('Starting full verification');
steps.forEach(([label, command, args], index) => {
  log.jest('RUN', label, log.progressText(index + 1, steps.length));
  run(label, command, args);
});
log.finish('Full verification passed');

function run(label, command, args) {
  const started = Date.now();
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: process.env,
    stdio: 'inherit',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with code ${result.status}`);
  }
  log.jest('PASS', label, `time=${log.formatDuration(Date.now() - started)}`);
}
