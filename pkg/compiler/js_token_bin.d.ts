/* tslint:disable */
/* eslint-disable */

export class Compiler {
    free(): void;
    [Symbol.dispose](): void;
    extern_slots(): string[];
    constructor(source: string);
    to_bytecode_artifact(seed: string | null | undefined, extern_slots: any[]): CompilerArtifact;
    to_text(): string;
}

export class CompilerArtifact {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    bytecode_text(): string;
    bytes(): Uint8Array;
}

export function js_encoding_rows_from_seed(seed: string): string[];

export function js_encoding_seed_for_seed_and_bytes(seed: string, bytes: Uint8Array): string;

export function js_encoding_seed_from_rows(opcode_names: any[], operand_tag_names: any[], constant_tag_names: any[], bytes: Uint8Array): string;

export function js_execute_bytes_with_seed(bytes: Uint8Array, seed: string): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_compiler_free: (a: number, b: number) => void;
    readonly __wbg_compilerartifact_free: (a: number, b: number) => void;
    readonly compiler_extern_slots: (a: number, b: number) => void;
    readonly compiler_new: (a: number, b: number, c: number) => void;
    readonly compiler_to_bytecode_artifact: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly compiler_to_text: (a: number, b: number) => void;
    readonly compilerartifact_bytecode_text: (a: number, b: number) => void;
    readonly compilerartifact_bytes: (a: number, b: number) => void;
    readonly js_encoding_rows_from_seed: (a: number, b: number, c: number) => void;
    readonly js_encoding_seed_for_seed_and_bytes: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly js_encoding_seed_from_rows: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly js_execute_bytes_with_seed: (a: number, b: number, c: number, d: number, e: number) => void;
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
