/* tslint:disable */
/* eslint-disable */

export function js_execute_bytes(bytes: Uint8Array, seed: string, externals: any[], options: any): string;

/**
 * 执行 bytecode 并返回字符串化结果。
 */
export function js_execute_bytes_with_seed(bytes: Uint8Array, seed: string, externals: any[]): string;

/**
 * 执行 bytecode，并允许设置调用深度限制。
 */
export function js_execute_bytes_with_seed_and_limits(bytes: Uint8Array, seed: string, externals: any[], max_call_depth: number, max_recursive_call_depth: number): string;

/**
 * 执行 bytecode，并允许设置调用深度、递归深度和步数预算。
 */
export function js_execute_bytes_with_seed_and_runtime_limits(bytes: Uint8Array, seed: string, externals: any[], max_call_depth: number, max_recursive_call_depth: number, max_execution_steps: number): string;

/**
 * 执行 ES module bytecode，返回模块 namespace 对象。
 */
export function js_execute_module_bytes_with_seed(bytes: Uint8Array, seed: string, externals: any[]): any;

/**
 * 执行 ES module bytecode，返回模块 namespace 对象，并使用自定义运行限制。
 */
export function js_execute_module_bytes_with_seed_and_runtime_limits(bytes: Uint8Array, seed: string, externals: any[], max_call_depth: number, max_recursive_call_depth: number, max_execution_steps: number): any;

/**
 * 执行 bytecode，返回原生 `JsValue`。
 */
export function js_execute_value_bytes_with_seed(bytes: Uint8Array, seed: string, externals: any[]): any;

export function js_execute_value_bytes_with_seed_and_runtime_limits(bytes: Uint8Array, seed: string, externals: any[], max_call_depth: number, max_recursive_call_depth: number, max_execution_steps: number): any;

/**
 * 执行 bytecode，忽略返回值。
 *
 * 适合打包后的页面入口脚本，避免无意义的字符串转换。
 */
export function js_execute_void_bytes_with_seed(bytes: Uint8Array, seed: string, externals: any[]): void;

/**
 * 执行 bytecode，忽略返回值，并使用自定义运行限制。
 */
export function js_execute_void_bytes_with_seed_and_runtime_limits(bytes: Uint8Array, seed: string, externals: any[], max_call_depth: number, max_recursive_call_depth: number, max_execution_steps: number): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly js_execute_bytes: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly js_execute_bytes_with_seed: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly js_execute_bytes_with_seed_and_limits: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly js_execute_bytes_with_seed_and_runtime_limits: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly js_execute_module_bytes_with_seed: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly js_execute_module_bytes_with_seed_and_runtime_limits: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly js_execute_value_bytes_with_seed: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly js_execute_value_bytes_with_seed_and_runtime_limits: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly js_execute_void_bytes_with_seed: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly js_execute_void_bytes_with_seed_and_runtime_limits: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly __wasm_bindgen_func_elem_987: (a: number, b: number, c: number, d: number) => number;
    readonly __wasm_bindgen_func_elem_1436: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_296: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export5: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
