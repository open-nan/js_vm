// JS VM node environment package.
import fs from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import * as runtime from './js_vm_runtime_node.js';

const maxCallDepth = 2048;
const maxRecursiveCallDepth = 128;
const maxExecutionSteps = 1000000;
const here = dirname(fileURLToPath(import.meta.url));

if (typeof globalThis.__jsVmHostLog !== 'function') {
  globalThis.__jsVmHostLog = (level, message) => {
    const method = console && typeof console[level] === 'function' ? console[level] : console.log;
    method.call(console, message);
  };
}

export async function loadBin(url) {
  const file = url instanceof URL ? fileURLToPath(url) : join(here, String(url));
  return new Uint8Array(await fs.readFile(file));
}

export async function loadSourceMap(url) {
  const file = url instanceof URL ? fileURLToPath(url) : join(here, String(url));
  return JSON.parse(await fs.readFile(file, 'utf8'));
}

export function resolveExternal(name) {
  return String(name).split('.').reduce((value, part) => value == null ? undefined : value[part], globalThis);
}

export async function ready() {}

export function executeScript(bytes, seed, externs = []) {
  const run = typeof runtime.js_execute_void_bytes_with_seed_and_runtime_limits === 'function'
    ? runtime.js_execute_void_bytes_with_seed_and_runtime_limits
    : runtime.js_execute_void_bytes_with_seed;
  if (run === runtime.js_execute_void_bytes_with_seed_and_runtime_limits) {
    run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  } else {
    run(bytes, seed, externs);
  }
}

export function executeModule(bytes, seed, externs = []) {
  const run = typeof runtime.js_execute_module_bytes_with_seed_and_runtime_limits === 'function'
    ? runtime.js_execute_module_bytes_with_seed_and_runtime_limits
    : runtime.js_execute_module_bytes_with_seed;
  if (run === runtime.js_execute_module_bytes_with_seed_and_runtime_limits) {
    return run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }
  return run(bytes, seed, externs);
}

export function executeDebug(bytes, seed, externs = []) {
  const run = typeof runtime.js_execute_bytes_with_seed_debug_and_runtime_limits === 'function'
    ? runtime.js_execute_bytes_with_seed_debug_and_runtime_limits
    : runtime.js_execute_bytes_with_seed_debug;
  if (typeof run !== 'function') {
    throw new Error('JS VM runtime was not built with source-map feature');
  }
  if (run === runtime.js_execute_bytes_with_seed_debug_and_runtime_limits) {
    return run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }
  return run(bytes, seed, externs);
}

export function createDebugSession(bytes, seed, externs = []) {
  const Session = runtime.JsVmDebugSession;
  if (typeof Session !== 'function') {
    throw new Error('JS VM runtime was not built with debugger feature');
  }
  if (typeof Session.new_with_runtime_limits === 'function') {
    return Session.new_with_runtime_limits(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }
  return new Session(bytes, seed, externs);
}

export function breakpointPcs(sourceMap, breakpoints = []) {
  const list = Array.isArray(breakpoints) ? breakpoints : [breakpoints];
  const vm = sourceMap?.x_js_vm || {};
  const ranges = Array.isArray(vm.pcRanges) ? vm.pcRanges : [];
  const spans = Array.isArray(vm.sourceSpans) ? vm.sourceSpans : [];
  const pcs = new Set();
  for (const breakpoint of list) {
    if (typeof breakpoint === 'number' && Number.isFinite(breakpoint)) {
      pcs.add(Math.max(0, Math.trunc(breakpoint)));
      continue;
    }
    if (!breakpoint || typeof breakpoint !== 'object') continue;
    if (Number.isFinite(Number(breakpoint.pc))) {
      pcs.add(Math.max(0, Math.trunc(Number(breakpoint.pc))));
      continue;
    }
    const line = Number(breakpoint.line);
    const column = Number.isFinite(Number(breakpoint.column)) ? Number(breakpoint.column) : 0;
    if (!Number.isFinite(line)) continue;
    for (const range of ranges) {
      const span = range[5] >= 0 ? spans[range[5]] : null;
      if (!span) continue;
      const inLine = line >= span[2] && line <= span[4];
      const afterStart = line !== span[2] || column >= span[3];
      const beforeEnd = line !== span[4] || column <= span[5];
      if (inLine && afterStart && beforeEnd) {
        pcs.add(range[0]);
        break;
      }
    }
  }
  return Array.from(pcs).sort((a, b) => a - b);
}

export function sourceFrame(sourceMap, pc) {
  const vm = sourceMap?.x_js_vm || {};
  if (Array.isArray(vm.pcRanges)) {
    const range = vm.pcRanges.find((item) => pc >= item[0] && pc < item[1]);
    if (!range) return null;
    const opRange = (vm.pcOps || []).find((item) => pc >= item[0] && pc < item[1]);
    const span = range[5] >= 0 ? vm.sourceSpans?.[range[5]] : null;
    const source = span ? {
      source: 0,
      start: span[0],
      end: span[1],
      line: span[2],
      column: span[3],
      endLine: span[4],
      endColumn: span[5],
    } : null;
    return {
      pc,
      pcStart: range[0],
      pcEnd: range[1],
      op: opRange ? vm.opcodes?.[opRange[2]] : undefined,
      byteStart: range[2],
      byteEnd: range[3],
      function: range[4] >= 0 ? range[4] : null,
      source,
      sourceFile: sourceMap.sources?.[0],
    };
  }
  const frames = vm.pcMap || [];
  const frame = frames.find((item) => item.pc === pc);
  return frame ? { ...frame, sourceFile: sourceMap.sources?.[frame.source?.source ?? 0] } : null;
}

export function decorateDebugEvent(sourceMap, event) {
  const pc = Number(event?.pc);
  const frame = Number.isFinite(pc) ? sourceFrame(sourceMap, pc) : null;
  const callStack = (event?.callStack || []).map((item) => {
    const framePc = Number(item?.pc);
    return {
      ...item,
      source: Number.isFinite(framePc) ? sourceFrame(sourceMap, framePc) : null,
    };
  });
  return { ...event, frame, source: frame?.source || null, callStack };
}

export const execute = executeModule;
