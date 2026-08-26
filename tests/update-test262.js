#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const log = require('./support/logger.js');

const ROOT = path.resolve(__dirname, '..');
const VENDOR_ROOT = path.join(ROOT, 'tests/.vendor');
const TARGET = path.join(VENDOR_ROOT, 'test262');
const TEST262_REPO = 'https://github.com/tc39/test262.git';
const TEST262_REF = process.env.TEST262_REF || process.env.JS_VM_TEST262_REF || 'main';

main();

function main() {
  log.step(`Updating tests/.vendor/test262 from ${TEST262_REPO} ${TEST262_REF}`);
  fs.mkdirSync(VENDOR_ROOT, { recursive: true });

  if (fs.existsSync(path.join(TARGET, '.git'))) {
    run('fetch Test262', 'git', ['-C', TARGET, 'fetch', '--depth=1', 'origin', TEST262_REF]);
    run('reset Test262 checkout', 'git', ['-C', TARGET, 'reset', '--hard', 'FETCH_HEAD']);
  } else {
    fs.rmSync(TARGET, { recursive: true, force: true });
    run('clone Test262', 'git', [
      'clone',
      '--filter=blob:none',
      '--depth=1',
      '--branch',
      TEST262_REF,
      TEST262_REPO,
      TARGET,
    ]);
  }

  const revision = gitOutput(['-C', TARGET, 'rev-parse', 'HEAD']);
  const version = readPackageVersion();
  fs.writeFileSync(
    path.join(TARGET, '.upstream'),
    [
      `repo=${TEST262_REPO}`,
      `ref=${TEST262_REF}`,
      `revision=${revision}`,
      `version=${version}`,
      `updated_at=${new Date().toISOString()}`,
      '',
    ].join('\n'),
  );

  log.finish(`Updated tests/.vendor/test262 at ${revision.slice(0, 12)} version=${version || 'unknown'}`);
}

function run(label, command, args) {
  const started = Date.now();
  log.jest('RUN', label);
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

function gitOutput(args) {
  const result = spawnSync('git', args, {
    cwd: ROOT,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`git ${args.join(' ')} failed: ${result.stderr.trim()}`);
  }
  return result.stdout.trim();
}

function readPackageVersion() {
  const packageFile = path.join(TARGET, 'package.json');
  if (!fs.existsSync(packageFile)) return '';
  try {
    return JSON.parse(fs.readFileSync(packageFile, 'utf8')).version || '';
  } catch {
    return '';
  }
}
