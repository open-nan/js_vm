/* tslint:disable */
/* eslint-disable */

export class Compiler {
    free(): void;
    [Symbol.dispose](): void;
    encoding_seed(yaml: string, bytes: Uint8Array): string;
    execute(): string;
    execute_with_externals(externals: any[]): string;
    extern_slots(): string[];
    constructor(source: string);
    to_bytecode(): string;
    to_bytecode_bytes(): Uint8Array;
    to_bytecode_text(): string;
    to_bytecode_text_with_extern_slots(extern_slots: any[]): string;
    to_bytes(): Uint8Array;
    to_bytes_with_encoding(yaml: string): Uint8Array;
    to_bytes_with_encoding_and_extern_slots(yaml: string, extern_slots: any[]): Uint8Array;
    to_bytes_with_seed(seed: string): Uint8Array;
    to_text(): string;
    static with_externals(source: string, externals: any[]): Compiler;
}

export function js_encoding_seed_for_bytes(yaml: string, bytes: Uint8Array): string;

export function js_encoding_yaml_from_seed(seed: string): string;

export function js_execute(source: string): string;

export function js_execute_bytes(bytes: Uint8Array): string;

export function js_execute_bytes_with_encoding(bytes: Uint8Array, yaml: string): string;

export function js_execute_bytes_with_seed(bytes: Uint8Array, seed: string): string;

export function js_execute_with_externals(source: string, externals: any[]): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_compiler_free: (a: number, b: number) => void;
    readonly compiler_encoding_seed: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly compiler_execute: (a: number, b: number) => void;
    readonly compiler_execute_with_externals: (a: number, b: number, c: number, d: number) => void;
    readonly compiler_extern_slots: (a: number, b: number) => void;
    readonly compiler_new: (a: number, b: number, c: number) => void;
    readonly compiler_to_bytecode: (a: number, b: number) => void;
    readonly compiler_to_bytecode_bytes: (a: number, b: number) => void;
    readonly compiler_to_bytecode_text_with_extern_slots: (a: number, b: number, c: number, d: number) => void;
    readonly compiler_to_bytes_with_encoding: (a: number, b: number, c: number, d: number) => void;
    readonly compiler_to_bytes_with_encoding_and_extern_slots: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly compiler_to_bytes_with_seed: (a: number, b: number, c: number, d: number) => void;
    readonly compiler_to_text: (a: number, b: number) => void;
    readonly compiler_with_externals: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly js_encoding_seed_for_bytes: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly js_encoding_yaml_from_seed: (a: number, b: number, c: number) => void;
    readonly js_execute: (a: number, b: number, c: number) => void;
    readonly js_execute_bytes: (a: number, b: number, c: number) => void;
    readonly js_execute_bytes_with_encoding: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly js_execute_bytes_with_seed: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly js_execute_with_externals: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly compiler_to_bytecode_text: (a: number, b: number) => void;
    readonly compiler_to_bytes: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number) => void;
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
