/* tslint:disable */
/* eslint-disable */

/**
 * wasm 侧编译器对象。
 *
 * 构造时完成源码解析和 IR lowering，后续方法都基于同一份 IR 输出不同产物。
 */
export class Compiler {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * 返回编译器识别出的 extern slot 名称。
     *
     * 未在当前作用域声明、需要宿主环境提供的根名字会进入该列表。
     */
    extern_slots(): string[];
    /**
     * 创建编译器并立即把源码降低到 IR。
     */
    constructor(source: string);
    /**
     * 返回 feature 的规范化字符串。
     */
    runtime_feature_canonical(compact_errors: boolean): string;
    /**
     * 返回 runtime feature manifest JSON。
     *
     * `compact_errors` 会选择是否启用紧凑错误信息特性，从而影响 runtime 包名。
     */
    runtime_feature_manifest(compact_errors: boolean): string;
    /**
     * 返回当前 feature 组合对应的 runtime 包名。
     */
    runtime_feature_package(compact_errors: boolean): string;
    /**
     * 返回当前源码需要的 runtime feature 列表。
     */
    runtime_features(): string[];
    /**
     * 单独生成 source map。
     *
     * 该接口用于调试器或下载包只需要 map 的场景；普通页面构建优先用 `to_bytecode_artifact`。
     */
    source_map(seed: string | null | undefined, extern_slots: any[], source_file: string): string;
    /**
     * 生成完整 bytecode artifact。
     *
     * `seed` 为空时使用默认编码；非空时先从 seed 恢复编码表。
     * `extern_slots` 非空时会按指定顺序重排 extern operand。
     */
    to_bytecode_artifact(seed: string | null | undefined, extern_slots: any[]): CompilerArtifact;
    /**
     * 返回 IR 文本。
     */
    to_text(): string;
}

/**
 * wasm 侧编译产物。
 *
 * 它是页面和下载包共同使用的聚合结果：既包含可读调试文本，也包含真正运行的 bytes。
 */
export class CompilerArtifact {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * 返回可读 Bytecode 文本，供 UI 展示和调试。
     */
    bytecode_text(): string;
    /**
     * 返回最终可执行 bytecode bytes。
     */
    bytes(): Uint8Array;
    /**
     * 返回 bytes 体积分布文本，供压缩分析使用。
     */
    bytes_profile_text(): string;
    /**
     * 返回紧凑 source map JSON。
     */
    source_map(): string;
}

/**
 * 分析 ES module 的 import/export 边界。
 */
export function js_analyze_module_source(source: string): any;

/**
 * 从 seed 还原 UI 表格需要的名称行。
 */
export function js_encoding_rows_from_seed(seed: string): string[];

/**
 * 根据已有 seed 和 bytes 重新生成配对 seed。
 *
 * 用于 bytes 改变后同步 seed 指纹。
 */
export function js_encoding_seed_for_seed_and_bytes(seed: string, bytes: Uint8Array): string;

/**
 * 根据 UI 表格中的 opcode/operand/constant tag 行生成与 bytes 绑定的 seed。
 */
export function js_encoding_seed_from_rows(opcode_names: any[], operand_tag_names: any[], constant_tag_names: any[], bytes: Uint8Array): string;

/**
 * 把一个模块源码包装成 VM wrapper + VM 内部源码。
 *
 * Web 预览和 CLI 打包应共用该入口，保证模块语义、extern slots、bin 加载方式一致。
 */
export function js_package_module_source(source: string, options: any): any;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_compiler_free: (a: number, b: number) => void;
    readonly __wbg_compilerartifact_free: (a: number, b: number) => void;
    readonly compiler_extern_slots: (a: number, b: number) => void;
    readonly compiler_new: (a: number, b: number, c: number) => void;
    readonly compiler_runtime_feature_canonical: (a: number, b: number, c: number) => void;
    readonly compiler_runtime_feature_manifest: (a: number, b: number, c: number) => void;
    readonly compiler_runtime_feature_package: (a: number, b: number, c: number) => void;
    readonly compiler_runtime_features: (a: number, b: number) => void;
    readonly compiler_source_map: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly compiler_to_bytecode_artifact: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly compiler_to_text: (a: number, b: number) => void;
    readonly compilerartifact_bytecode_text: (a: number, b: number) => void;
    readonly compilerartifact_bytes: (a: number, b: number) => void;
    readonly compilerartifact_bytes_profile_text: (a: number, b: number) => void;
    readonly compilerartifact_source_map: (a: number, b: number) => void;
    readonly js_analyze_module_source: (a: number, b: number, c: number) => void;
    readonly js_encoding_rows_from_seed: (a: number, b: number, c: number) => void;
    readonly js_encoding_seed_for_seed_and_bytes: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly js_encoding_seed_from_rows: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly js_package_module_source: (a: number, b: number, c: number, d: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
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
