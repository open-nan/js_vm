import { executeModule, executeScript, executeDebug, createDebugSession, loadBin, loadSourceMap, resolveExternal, sourceFrame, breakpointPcs, decorateDebugEvent } from "./js_vm_env_node.js";
const __jsVmDynamicImports = null;
function __jsVmAsyncValue(read) {
  const value = Object.create(null);
  Object.defineProperty(value, '__js_vm_async_resolved', { value: true });
  Object.defineProperty(value, '__js_vm_value', { get: read });
  Object.defineProperty(value, 'then', { value(onFulfilled, onRejected) {
    try {
      const resolved = read();
      const next = typeof onFulfilled === 'function' ? onFulfilled(resolved) : resolved;
      return next && typeof next === 'object' && next.__js_vm_async_resolved ? next : __jsVmAsyncResolved(next);
    } catch (error) {
      if (typeof onRejected === 'function') return __jsVmAsyncResolved(onRejected(error));
      throw error;
    }
  } });
  Object.defineProperty(value, 'catch', { value() { return value; } });
  Object.defineProperty(value, 'finally', { value(onFinally) {
    if (typeof onFinally === 'function') onFinally();
    return value;
  } });
  return value;
}
function __jsVmAsyncResolved(value) {
  return __jsVmAsyncValue(() => value);
}
function __jsVmDynamicModuleValue(entry) {
  if (typeof entry === 'function') {
    return entry().then((module) => {
      try {
        if (module && typeof module.__jsVmLoadModule === 'function') module.__jsVmLoadModule();
      } catch (error) {
        console.error(error && error.message ? error.message : error);
        throw error;
      }
      return module;
    });
  }
  return __jsVmAsyncValue(() => {
    if (entry && typeof entry.__jsVmLoadModule === 'function') entry.__jsVmLoadModule();
    return entry;
  });
}
function __jsVmResolveDynamicImport(specifier) {
  const key = String(specifier);
  const entry = __jsVmDynamicImports && __jsVmDynamicImports.get(key);
  return entry ? __jsVmDynamicModuleValue(entry) : import(key);
}
const __jsVmSeed = "JSTKSEED2-45d5ff2fc16913ef-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_~!$@*()[]{}+#.012345678.012345";
const __jsVmBinUrl = new URL("./jquery.bin?v=45d5ff2fc16913ef", import.meta.url);
const __jsVmSourceMapUrl = new URL("./jquery.bin.map?v=45d5ff2fc16913ef", import.meta.url);
const __jsVmBin = await loadBin(__jsVmBinUrl);
function __jsVmCreateExterns() { return [resolveExternal("module"), resolveExternal("window"), resolveExternal("Array"), resolveExternal("Date"), resolveExternal("Error"), resolveExternal("JSON"), resolveExternal("Math"), resolveExternal("Object"), resolveExternal("RegExp"), resolveExternal("String"), resolveExternal("Symbol"), resolveExternal("TypeError"), resolveExternal("define"), resolveExternal("encodeURIComponent"), resolveExternal("isFinite"), resolveExternal("parseFloat"), resolveExternal("parseInt"), resolveExternal("r"), resolveExternal("i"), resolveExternal("e"), resolveExternal("o"), resolveExternal("l"), resolveExternal("s"), resolveExternal("c")]; }
export const __jsVmDebugInfo = Object.freeze({ source: "jquery.js", bin: __jsVmBinUrl.href, map: __jsVmSourceMapUrl.href, seed: __jsVmSeed, externSlots: ["module", "window", "Array", "Date", "Error", "JSON", "Math", "Object", "RegExp", "String", "Symbol", "TypeError", "define", "encodeURIComponent", "isFinite", "parseFloat", "parseInt", "r", "i", "e", "o", "l", "s", "c"] });
export async function __jsVmDebug(pc) {
  const map = await loadSourceMap(__jsVmSourceMapUrl);
  const __jsVmExterns = __jsVmCreateExterns();
  const result = executeDebug(__jsVmBin, __jsVmSeed, __jsVmExterns);
  const stack = (result.stack || []).map((frame) => ({ ...frame, source: sourceFrame(map, frame.pc) }));
  const selectedPc = Number(pc);
  return { ...result, info: __jsVmDebugInfo, map, stack, frame: Number.isFinite(selectedPc) ? sourceFrame(map, selectedPc) : null };
}
export async function __jsVmDebugSession(breakpoints = []) {
  const map = await loadSourceMap(__jsVmSourceMapUrl);
  const __jsVmExterns = __jsVmCreateExterns();
  const session = createDebugSession(__jsVmBin, __jsVmSeed, __jsVmExterns);
  let pcs = [];
  const applyBreakpoints = (next) => {
    pcs = breakpointPcs(map, next);
    session.set_breakpoints(pcs);
    return pcs;
  };
  applyBreakpoints(breakpoints);
  const decorate = (event) => ({ ...decorateDebugEvent(map, event), info: __jsVmDebugInfo, map, breakpoints: pcs });
  return {
    info: __jsVmDebugInfo,
    map,
    raw: session,
    setBreakpoints(next) { return applyBreakpoints(next); },
    pcFor(next) { return breakpointPcs(map, next); },
    frame(pc) { return sourceFrame(map, Number(pc)); },
    resume() { return decorate(session.resume()); },
    step() { return decorate(session.step()); },
    inspect() { return decorate(session.inspect()); },
  };
}
let __jsVmExecuted = false;
function __jsVmRunModule() {
  if (!__jsVmExecuted) {
    executeScript(__jsVmBin, __jsVmSeed, __jsVmCreateExterns());
    __jsVmExecuted = true;
  }
  return undefined;
}
export function __jsVmLoadModule() { return __jsVmRunModule(); }
__jsVmRunModule();
export default undefined;
// source: jquery.js