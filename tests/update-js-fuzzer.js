#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const log = require('./support/logger.js');

const ROOT = path.resolve(__dirname, '..');
const VENDOR_ROOT = path.join(ROOT, 'tests/.vendor');
const TARGET = path.join(VENDOR_ROOT, 'js_fuzzer');
const WORKDIR = path.join(VENDOR_ROOT, '.update-js-fuzzer-workdir');
const V8_REPO = 'https://chromium.googlesource.com/v8/v8';
const V8_REF = process.env.JS_FUZZER_V8_REF || 'main';
const V8_TOOL_PATH = 'tools/clusterfuzz/js_fuzzer';
const PULL_TOOL_URL = 'https://github.com/open-nan/my_shell/blob/main/pull_v8_tool.sh';

main();

function main() {
  log.step(`Updating tests/.vendor/js_fuzzer from V8 ${V8_REF}`);
  log.info(`Mirrors pull_v8_tool.sh workflow: ${PULL_TOOL_URL}`);
  fs.mkdirSync(VENDOR_ROOT, { recursive: true });
  fs.rmSync(WORKDIR, { recursive: true, force: true });

  run('clone v8 sparse workspace', 'git', [
    'clone',
    '--filter=blob:none',
    '--sparse',
    '--depth=1',
    '--branch',
    V8_REF,
    V8_REPO,
    WORKDIR,
  ]);
  run('select js_fuzzer sparse path', 'git', [
    '-C',
    WORKDIR,
    'sparse-checkout',
    'set',
    V8_TOOL_PATH,
  ]);

  const source = path.join(WORKDIR, V8_TOOL_PATH);
  if (!fs.existsSync(source)) {
    throw new Error(`${V8_TOOL_PATH} was not found in cloned V8 workspace`);
  }

  const revision = gitOutput(['-C', WORKDIR, 'rev-parse', 'HEAD']);
  fs.rmSync(TARGET, { recursive: true, force: true });
  fs.cpSync(source, TARGET, { recursive: true });
  fs.writeFileSync(
    path.join(TARGET, '.upstream'),
    [
      `repo=${V8_REPO}`,
      `ref=${V8_REF}`,
      `revision=${revision}`,
      `path=${V8_TOOL_PATH}`,
      `pull_tool=${PULL_TOOL_URL}`,
      `updated_at=${new Date().toISOString()}`,
      '',
    ].join('\n'),
  );

  fs.rmSync(WORKDIR, { recursive: true, force: true });
  log.finish(`Updated tests/.vendor/js_fuzzer at ${revision.slice(0, 12)}`);
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
