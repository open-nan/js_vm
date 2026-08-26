//! 编译器主体。
//!
//! 该模块面向 Rust/CLI 和 wasm 绑定提供统一编译能力。它不直接负责文件系统遍历，
//! 只处理单个源码字符串以及“单模块包装”场景：
//!
//! - `Compiler`：页面内增量编译使用的状态对象。
//! - `compile_source_to_artifact_with_source_file`：CLI/测试使用的 native 编译入口。
//! - `package_module_source`：把原始 ES module 拆成 wrapper JS 和 VM 内部源码。
//! - `RuntimeFeatureManifest`：根据源码/IR 推导运行时 feature 包。

use crate::parse::{
    LoweringContext, check_source_syntax as parse_check_source_syntax, parse_source,
};
use js_token_core::{
    BytecodeModule, BytecodeOperand, EncodingConfig, IrConst, IrModule, IrModuleKind,
};
use std::collections::{BTreeMap, BTreeSet};
use swc_common::{Span, Spanned};
use swc_ecma_ast::*;
use wasm_bindgen::prelude::*;

/// 检查源码语法，不生成 IR 或 bytecode。
pub fn check_source_syntax(source: &str, source_file: &str) -> Result<(), String> {
    parse_check_source_syntax(source, source_file)
}

/// 单源码编译器。
///
/// 构造时完成 SWC 解析和 IR lowering，后续可重复输出 IR、feature、bytecode artifact。
pub struct Compiler {
    /// 降低后的 IR。
    ir: IrModule,
    /// 原始源码，用于 source map 和 feature 推导。
    source: String,
}

impl Compiler {
    /// 解析源码并构建编译器。
    pub fn new(source: &str) -> Result<Compiler, JsValue> {
        let ir = compile_to_ir(source).map_err(|err| JsValue::from_str(&err))?;
        Ok(Compiler {
            ir,
            source: source.to_string(),
        })
    }

    /// 返回当前源码需要宿主传入的 extern slot。
    pub fn extern_slots(&self) -> Vec<String> {
        self.ir.extern_slots.clone()
    }

    /// 返回 IR 可读文本。
    pub fn to_text(&self) -> String {
        self.ir.to_text()
    }

    /// 推导当前源码需要启用的 runtime feature。
    pub fn runtime_features(&self) -> Vec<String> {
        runtime_features_for_source_and_ir(&self.source, &self.ir)
    }

    /// 生成 runtime feature manifest。
    pub fn runtime_feature_manifest(&self, compact_errors: bool) -> RuntimeFeatureManifest {
        RuntimeFeatureManifest::from_features(self.runtime_features(), compact_errors)
    }

    /// 生成完整 bytecode artifact。
    ///
    /// `seed` 决定 opcode/tag 混淆表，`extern_slots` 决定 extern operand 在 bytes 中的槽位。
    pub fn to_bytecode_artifact(
        &self,
        seed: Option<String>,
        extern_slots: Box<[JsValue]>,
    ) -> Result<CompilerArtifact, String> {
        let module = self.bytecode_module_with_extern_slots(extern_slots)?;
        let encoding = match seed.as_deref() {
            Some(seed) if !seed.is_empty() => {
                EncodingConfig::from_seed(seed).map_err(|err| err.to_string())?
            }
            _ => EncodingConfig::default(),
        };
        let bytes = module
            .to_bytes_with_encoding(&encoding)
            .map_err(|err| err.to_string())?;
        let bytes_profile_text = module
            .bytes_profile_text_with_encoding(&encoding)
            .map_err(|err| err.to_string())?;
        let source_map = source_map_json(&self.source, "input.js", &module, &encoding, &bytes)?;
        Ok(CompilerArtifact {
            bytecode_text: module.to_text(),
            bytes_profile_text,
            source_map,
            bytes,
        })
    }

    /// 生成 source map JSON。
    pub fn source_map(
        &self,
        seed: Option<String>,
        extern_slots: Box<[JsValue]>,
        source_file: &str,
    ) -> Result<String, String> {
        let module = self.bytecode_module_with_extern_slots(extern_slots)?;
        let encoding = match seed.as_deref() {
            Some(seed) if !seed.is_empty() => {
                EncodingConfig::from_seed(seed).map_err(|err| err.to_string())?
            }
            _ => EncodingConfig::default(),
        };
        let bytes = module
            .to_bytes_with_encoding(&encoding)
            .map_err(|err| err.to_string())?;
        source_map_json(&self.source, source_file, &module, &encoding, &bytes)
    }

    /// 根据 UI/调用方传入的 extern slot 顺序重建 bytecode 模块。
    ///
    /// 当 `extern_slots` 为空时使用编译期默认顺序；非空时必须和 IR 收集到的 extern 集合一致。
    fn bytecode_module_with_extern_slots(
        &self,
        extern_slots: Box<[JsValue]>,
    ) -> Result<BytecodeModule, String> {
        let extern_slots = js_values_to_strings(&extern_slots);
        let mut module = self.ir.to_bytecode();
        if extern_slots.is_empty() {
            return Ok(module);
        }
        if extern_slots.len() != self.ir.extern_slots.len() {
            return Err(format!(
                "extern slot count mismatch: expected {}, got {}",
                self.ir.extern_slots.len(),
                extern_slots.len()
            ));
        }
        remap_external_operands(&mut module, &self.ir.extern_slots, &extern_slots)?;
        module.extern_slots = extern_slots;
        Ok(module)
    }
}

/// 把 bytecode 中的 extern operand 从原始 slot 顺序重映射到新顺序。
///
/// 这允许 UI 通过 extern slot 表做混淆，同时执行器仍只按数组下标访问外部对象。
fn remap_external_operands(
    module: &mut BytecodeModule,
    original_slots: &[String],
    extern_slots: &[String],
) -> Result<(), String> {
    let mut remapped_slots = BTreeMap::new();
    for (index, name) in extern_slots.iter().enumerate() {
        if remapped_slots.insert(name.as_str(), index as u32).is_some() {
            return Err(format!("duplicate extern slot {name}"));
        }
    }
    for name in original_slots {
        if !remapped_slots.contains_key(name.as_str()) {
            return Err(format!("missing extern slot {name}"));
        }
    }
    for instruction in &mut module.instructions {
        for operand in &mut instruction.operands {
            if let BytecodeOperand::External(index) = operand {
                let name = original_slots
                    .get(*index as usize)
                    .ok_or_else(|| format!("bad extern operand slot {index}"))?;
                *index = *remapped_slots
                    .get(name.as_str())
                    .ok_or_else(|| format!("missing extern slot {name}"))?;
            }
        }
    }
    Ok(())
}

/// 单次编译的聚合产物。
///
/// wasm UI 用这个结构替代多个零散旧接口，避免 bytes、source map、profile 使用不同配置生成。
pub struct CompilerArtifact {
    bytecode_text: String,
    bytes_profile_text: String,
    source_map: String,
    bytes: Vec<u8>,
}

impl CompilerArtifact {
    /// 返回 bytecode 调试文本。
    pub fn bytecode_text(&self) -> String {
        self.bytecode_text.clone()
    }

    /// 返回 bytes profile 调试文本。
    pub fn bytes_profile_text(&self) -> String {
        self.bytes_profile_text.clone()
    }

    /// 返回 source map JSON。
    pub fn source_map(&self) -> String {
        self.source_map.clone()
    }

    /// 返回可执行 bytes。
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// native/CLI 编译产物。
///
/// 相比 wasm `CompilerArtifact`，这里额外暴露 extern slots 和 runtime feature 信息，
/// 便于 CLI 生成运行时包、manifest 和 wrapper。
pub struct NativeCompilerArtifact {
    /// IR 可读文本。
    pub ir_text: String,
    /// bytecode 可读文本。
    pub bytecode_text: String,
    /// bytes 体积分布文本。
    pub bytes_profile_text: String,
    /// source map JSON。
    pub source_map: String,
    /// 可执行 bytecode bytes。
    pub bytes: Vec<u8>,
    /// 编译期识别出的 extern slot 名称。
    pub extern_slots: Vec<String>,
    /// 当前源码需要的 runtime feature。
    pub runtime_features: Vec<String>,
    /// feature 的规范化字符串。
    pub runtime_feature_canonical: String,
    /// feature 规范串 md5。
    pub runtime_feature_md5: String,
    /// 对应 runtime 包名。
    pub runtime_feature_package: String,
}

/// 单条 import 信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImportInfo {
    /// import 来源，如 `./a.js`。
    pub specifier: String,
    /// 当前 import 声明引入的本地名字。
    pub locals: Vec<String>,
}

/// 模块静态分析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSourceAnalysis {
    /// 静态 import 列表。
    pub imports: Vec<ModuleImportInfo>,
    /// 动态 import specifier 列表。
    pub dynamic_imports: Vec<String>,
    /// 命名导出名。
    pub export_names: Vec<String>,
    /// 是否包含 default export。
    pub has_default_export: bool,
}

/// import specifier 重写规则。
///
/// CLI 打包多文件时把源码里的 `./a.js` 重写成 VM wrapper 对应路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImportRewrite {
    /// 原始 specifier。
    pub specifier: String,
    /// 替换后的 specifier。
    pub replacement: String,
}

/// 单模块包装选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedModuleOptions {
    /// 原始源码文件名，用于 source map 和模块语义。
    pub source_file: String,
    /// 输出 wrapper JS 路径。
    pub wrapper_path: String,
    /// 输出 bin 路径。
    pub bin_path: String,
    /// bytecode seed。
    pub seed: String,
    /// wrapper 中导入运行时环境包的路径。
    pub env_specifier: String,
    /// wrapper 中加载 bin 的路径。
    pub bin_specifier: String,
    /// import 重写规则。
    pub import_rewrites: Vec<ModuleImportRewrite>,
    /// extern slot 顺序。
    pub extern_slots: Vec<String>,
    /// 是否延迟执行。
    ///
    /// 动态 import 目标模块会设置为 true，避免静态导入 wrapper 时提前触发模块副作用。
    pub defer_execution: bool,
}

/// 单模块包装结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedModuleSource {
    /// 去掉 import/export 边界、供 VM 编译的源码。
    pub vm_source: String,
    /// 保留 ES module import/export 语义、负责加载 bin 并调用 VM 的 wrapper。
    pub wrapper_source: String,
    /// 原始 import 分析结果。
    pub imports: Vec<ModuleImportInfo>,
    /// 命名导出列表。
    pub export_names: Vec<String>,
    /// 是否有 default export。
    pub has_default_export: bool,
    /// wrapper 需要传入 VM 的 import 本地值。
    pub imported_locals: Vec<String>,
}

/// 编译源码为 native artifact，使用默认 source file 名称。
pub fn compile_source_to_artifact(
    source: &str,
    seed: Option<&str>,
    extern_slots: &[String],
) -> Result<NativeCompilerArtifact, String> {
    compile_source_to_artifact_with_source_file(source, seed, extern_slots, "input.js")
}

/// 编译源码为 native artifact。
///
/// 这是 CLI 和测试的主入口，保证 IR、bytecode、bytes、source map、feature manifest
/// 使用同一份 seed 和 extern slot 配置生成。
pub fn compile_source_to_artifact_with_source_file(
    source: &str,
    seed: Option<&str>,
    extern_slots: &[String],
    source_file: &str,
) -> Result<NativeCompilerArtifact, String> {
    let ir = compile_to_ir(source)?;
    let mut module = ir.to_bytecode();
    if !extern_slots.is_empty() {
        if extern_slots.len() != ir.extern_slots.len() {
            return Err(format!(
                "extern slot count mismatch: expected {}, got {}",
                ir.extern_slots.len(),
                extern_slots.len()
            ));
        }
        remap_external_operands(&mut module, &ir.extern_slots, extern_slots)?;
        module.extern_slots = extern_slots.to_vec();
    }
    let encoding = match seed {
        Some(seed) if !seed.is_empty() => {
            EncodingConfig::from_seed(seed).map_err(|err| err.to_string())?
        }
        _ => EncodingConfig::default(),
    };
    let bytes = module
        .to_bytes_with_encoding(&encoding)
        .map_err(|err| err.to_string())?;
    let bytes_profile_text = module
        .bytes_profile_text_with_encoding(&encoding)
        .map_err(|err| err.to_string())?;
    let source_map = source_map_json(source, source_file, &module, &encoding, &bytes)?;
    let runtime_manifest = RuntimeFeatureManifest::from_features(
        runtime_features_for_source_and_ir(source, &ir),
        false,
    );
    let final_extern_slots = if extern_slots.is_empty() {
        ir.extern_slots.clone()
    } else {
        extern_slots.to_vec()
    };
    Ok(NativeCompilerArtifact {
        ir_text: ir.to_text(),
        bytecode_text: module.to_text(),
        bytes_profile_text,
        source_map,
        bytes,
        extern_slots: final_extern_slots,
        runtime_features: runtime_manifest.features,
        runtime_feature_canonical: runtime_manifest.canonical,
        runtime_feature_md5: runtime_manifest.md5,
        runtime_feature_package: runtime_manifest.package_name,
    })
}

/// 分析模块的 import/export 边界。
pub fn analyze_module_source(source: &str) -> Result<ModuleSourceAnalysis, String> {
    let program = parse_source(source)?;
    let Program::Module(module) = program else {
        return Ok(ModuleSourceAnalysis {
            imports: Vec::new(),
            dynamic_imports: dynamic_import_specifiers(source),
            export_names: Vec::new(),
            has_default_export: false,
        });
    };
    Ok(ModuleSourceAnalysis {
        imports: module
            .body
            .iter()
            .filter_map(|item| match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(decl)) => Some(ModuleImportInfo {
                    specifier: decl.src.value.to_string(),
                    locals: decl
                        .specifiers
                        .iter()
                        .filter_map(import_local_name)
                        .collect(),
                }),
                _ => None,
            })
            .collect(),
        dynamic_imports: dynamic_import_specifiers(source),
        export_names: module_export_names_from_ast(&module),
        has_default_export: module_has_default_export_from_ast(&module),
    })
}

/// 把单个模块源码拆成 VM 内部源码和外层 wrapper。
///
/// wrapper 负责保留 ES module 的 import/export 形态、加载 `.bin`、传入 externs 并调用执行器。
/// VM 内部源码则移除 import/export 声明，避免执行器重复处理宿主模块加载。
pub fn package_module_source(
    source: &str,
    options: PackagedModuleOptions,
) -> Result<PackagedModuleSource, String> {
    // 这是“多文件 VM 化”的核心边界：
    //
    // 原始模块依旧由浏览器/Node 的 ES module loader 负责加载依赖，因此静态 import
    // 不能放进 VM 内部执行，否则会丢失模块链接、循环依赖、live binding 等宿主语义。
    // 编译器把 import/export 拆到外层 wrapper，VM 内部只执行去掉模块边界后的主体代码。
    let imports = static_imports_with_spans(source)?;
    let rewrites = options
        .import_rewrites
        .iter()
        .map(|rewrite| (rewrite.specifier.as_str(), rewrite.replacement.as_str()))
        .collect::<BTreeMap<_, _>>();
    let analysis = analyze_module_source(source)?;
    let dynamic_imports = dynamic_import_specifiers(source)
        .iter()
        .filter_map(|specifier| {
            rewrites
                .get(specifier.as_str())
                .map(|replacement| ((*replacement).to_string(), specifier.clone()))
        })
        .collect::<Vec<_>>();
    let mut imported_locals = BTreeSet::new();
    let mut wrapper_imports = Vec::new();
    for import in &imports {
        imported_locals.extend(import.locals.iter().cloned());
        // CLI 打包时每个 JS 文件都会变成 wrapper + bin。这里重写 import specifier，
        // 让 wrapper 之间继续按 ESM 方式互相引用，而不是把所有文件合成一个大 bin。
        let statement = rewrites
            .get(import.specifier.as_str())
            .map(|replacement| rewrite_import_specifier(&import.statement, replacement))
            .unwrap_or_else(|| import.statement.clone());
        wrapper_imports.push(statement);
    }
    // VM 源码只保留运行主体。动态 import 会被改写成 extern `import` 调用，
    // wrapper 负责把它解析到对应的 VM wrapper 模块。
    let mut vm_source = remove_spans(
        source,
        imports.iter().map(|import| (import.start, import.end)),
    );
    vm_source = collapse_empty_vite_preload_imports(&vm_source);
    vm_source = rewrite_dynamic_imports_with_rewrites(&vm_source, &rewrites);
    vm_source = rewrite_hot_numeric_intrinsics(&vm_source);
    let eager_dynamic_imports = awaited_dynamic_import_specifiers(&vm_source);
    let wrapper_source = module_wrapper_source(
        &options,
        &wrapper_imports,
        &dynamic_imports,
        &eager_dynamic_imports,
        &analysis.export_names,
        analysis.has_default_export,
        &imported_locals,
    );
    Ok(PackagedModuleSource {
        vm_source,
        wrapper_source,
        imports: analysis.imports,
        export_names: analysis.export_names,
        has_default_export: analysis.has_default_export,
        imported_locals: imported_locals.into_iter().collect(),
    })
}

/// source map 中去重后的源码区间。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DebugSourceSpan {
    start: usize,
    end: usize,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
}

/// 连续 pc 到 byte range/source span 的紧凑映射。
///
/// 相邻指令如果函数、源码区间和 byte range 连续，会合并成一个 range，降低 `.bin.map` 体积。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactPcRange {
    start_pc: usize,
    end_pc: usize,
    byte_start: usize,
    byte_end: usize,
    function_id: Option<usize>,
    source_span_id: Option<usize>,
}

/// 连续 pc 到 opcode 名称的紧凑映射。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactOpRange {
    start_pc: usize,
    end_pc: usize,
    op_id: usize,
}

fn source_map_json(
    source: &str,
    source_file: &str,
    module: &BytecodeModule,
    encoding: &EncodingConfig,
    bytes: &[u8],
) -> Result<String, String> {
    // source map 使用数组编码而不是对象编码，减少大型 bundle 下的 map 体积。
    // `sources`、`functions`、`pcRanges`、`pcOps` 都按下标互相引用。
    //
    // 这里生成的是标准 source map 外壳 + `x_js_vm` 扩展字段。浏览器 devtools
    // 可以先识别标准字段，VM 调试器再读取扩展字段，把 runtime 报错里的 pc 映射回源码位置。
    let spans = collect_debug_source_spans(source)?;
    let ranges = module
        .instruction_byte_ranges_with_encoding(encoding)
        .map_err(|err| err.to_string())?;
    let instruction_count = module.instructions.len();
    let mut functions_json = Vec::new();
    for function in &module.functions {
        let name = function
            .name
            .and_then(|name| module.names.get(name as usize))
            .map(String::as_str)
            .unwrap_or("");
        functions_json.push(format!(
            "[{},{},{},{},{},{}]",
            json_string(name),
            function.body_start,
            function.body_end,
            function.flags,
            u8::from(function.has_return),
            function.params.len()
        ));
    }

    let mut source_span_ids = BTreeMap::new();
    let mut source_spans = Vec::<DebugSourceSpan>::new();
    let mut opcode_ids = BTreeMap::new();
    let mut opcodes = Vec::<String>::new();
    let mut pc_ranges = Vec::<CompactPcRange>::new();
    let mut pc_ops = Vec::<CompactOpRange>::new();
    for (pc, instruction) in module.instructions.iter().enumerate() {
        let (byte_start, byte_end) = ranges.get(pc).copied().unwrap_or((0, 0));
        let function_id = function_id_for_pc(module, pc);
        let source_span_id = source_span_for_pc(pc, instruction_count, &spans).map(|span| {
            if let Some(id) = source_span_ids.get(span).copied() {
                id
            } else {
                let id = source_spans.len();
                source_span_ids.insert(span.clone(), id);
                source_spans.push(span.clone());
                id
            }
        });
        push_compact_pc_range(
            &mut pc_ranges,
            pc,
            byte_start,
            byte_end,
            function_id,
            source_span_id,
        );

        let opcode = instruction.op.mnemonic().to_string();
        let op_id = if let Some(id) = opcode_ids.get(&opcode).copied() {
            id
        } else {
            let id = opcodes.len();
            opcode_ids.insert(opcode.clone(), id);
            opcodes.push(opcode);
            id
        };
        push_compact_op_range(&mut pc_ops, pc, op_id);
    }

    Ok(format!(
        "{{\"version\":3,\"file\":{},\"sources\":[{}],\"sourcesContent\":[{}],\"names\":[],\"mappings\":\"\",\"x_js_vm\":{{\"version\":2,\"bytecodeHash\":{},\"byteLength\":{},\"instructionCount\":{},\"externSlots\":[{}],\"functions\":[{}],\"sourceSpans\":[{}],\"opcodes\":[{}],\"pcRanges\":[{}],\"pcOps\":[{}]}}}}",
        json_string(&source_map_output_file(source_file)),
        json_string(source_file),
        json_string(source),
        json_string(&md5_hex_12(bytes)),
        bytes.len(),
        instruction_count,
        module
            .extern_slots
            .iter()
            .map(|slot| json_string(slot))
            .collect::<Vec<_>>()
            .join(","),
        functions_json.join(","),
        source_spans
            .iter()
            .map(format_source_span_compact)
            .collect::<Vec<_>>()
            .join(","),
        opcodes
            .iter()
            .map(|opcode| json_string(opcode))
            .collect::<Vec<_>>()
            .join(","),
        pc_ranges
            .iter()
            .map(format_pc_range_compact)
            .collect::<Vec<_>>()
            .join(","),
        pc_ops
            .iter()
            .map(format_op_range_compact)
            .collect::<Vec<_>>()
            .join(",")
    ))
}

fn push_compact_pc_range(
    ranges: &mut Vec<CompactPcRange>,
    pc: usize,
    byte_start: usize,
    byte_end: usize,
    function_id: Option<usize>,
    source_span_id: Option<usize>,
) {
    if let Some(last) = ranges.last_mut() {
        if last.end_pc == pc
            && last.byte_end == byte_start
            && last.function_id == function_id
            && last.source_span_id == source_span_id
        {
            last.end_pc = pc + 1;
            last.byte_end = byte_end;
            return;
        }
    }
    ranges.push(CompactPcRange {
        start_pc: pc,
        end_pc: pc + 1,
        byte_start,
        byte_end,
        function_id,
        source_span_id,
    });
}

fn push_compact_op_range(ranges: &mut Vec<CompactOpRange>, pc: usize, op_id: usize) {
    if let Some(last) = ranges.last_mut() {
        if last.end_pc == pc && last.op_id == op_id {
            last.end_pc = pc + 1;
            return;
        }
    }
    ranges.push(CompactOpRange {
        start_pc: pc,
        end_pc: pc + 1,
        op_id,
    });
}

fn format_source_span_compact(span: &DebugSourceSpan) -> String {
    format!(
        "[{},{},{},{},{},{}]",
        span.start, span.end, span.line, span.column, span.end_line, span.end_column
    )
}

fn format_pc_range_compact(range: &CompactPcRange) -> String {
    format!(
        "[{},{},{},{},{},{}]",
        range.start_pc,
        range.end_pc,
        range.byte_start,
        range.byte_end,
        range
            .function_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-1".to_string()),
        range
            .source_span_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-1".to_string())
    )
}

fn format_op_range_compact(range: &CompactOpRange) -> String {
    format!("[{},{},{}]", range.start_pc, range.end_pc, range.op_id)
}

fn source_map_output_file(source_file: &str) -> String {
    let trimmed = source_file.trim();
    if trimmed.is_empty() {
        return "bytecode.bin".to_string();
    }
    if let Some(stripped) = trimmed.strip_suffix(".js") {
        return format!("{stripped}.bin");
    }
    if let Some(stripped) = trimmed.strip_suffix(".mjs") {
        return format!("{stripped}.bin");
    }
    format!("{trimmed}.bin")
}

fn function_id_for_pc(module: &BytecodeModule, pc: usize) -> Option<usize> {
    module
        .functions
        .iter()
        .enumerate()
        .find_map(|(index, function)| {
            let start = function.body_start as usize;
            let end = function.body_end as usize;
            (pc >= start && pc < end).then_some(index)
        })
}

fn source_span_for_pc(
    pc: usize,
    instruction_count: usize,
    spans: &[DebugSourceSpan],
) -> Option<&DebugSourceSpan> {
    if spans.is_empty() {
        return None;
    }
    if instruction_count <= 1 {
        return spans.first();
    }
    spans
        .get((pc.saturating_mul(spans.len()) / instruction_count).min(spans.len() - 1))
        .or_else(|| spans.last())
}

fn collect_debug_source_spans(source: &str) -> Result<Vec<DebugSourceSpan>, String> {
    let program = parse_source(source)?;
    let mut spans = Vec::new();
    match &program {
        Program::Script(script) => {
            for stmt in &script.body {
                collect_stmt_debug_spans(stmt, source, &mut spans);
            }
        }
        Program::Module(module) => {
            for item in &module.body {
                collect_module_item_debug_spans(item, source, &mut spans);
            }
        }
    }
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup_by_key(|span| (span.start, span.end));
    Ok(spans)
}

fn collect_module_item_debug_spans(
    item: &ModuleItem,
    source: &str,
    out: &mut Vec<DebugSourceSpan>,
) {
    push_debug_span(item.span(), source, out);
    match item {
        ModuleItem::Stmt(stmt) => collect_stmt_debug_spans(stmt, source, out),
        ModuleItem::ModuleDecl(decl) => collect_module_decl_debug_spans(decl, source, out),
    }
}

fn collect_module_decl_debug_spans(
    decl: &ModuleDecl,
    source: &str,
    out: &mut Vec<DebugSourceSpan>,
) {
    match decl {
        ModuleDecl::ExportDecl(decl) => collect_decl_debug_spans(&decl.decl, source, out),
        ModuleDecl::ExportDefaultDecl(decl) => match &decl.decl {
            DefaultDecl::Class(class) => {
                push_debug_span(class.class.span, source, out);
            }
            DefaultDecl::Fn(function) => {
                push_debug_span(function.function.span, source, out);
                collect_function_body_debug_spans(&function.function, source, out);
            }
            DefaultDecl::TsInterfaceDecl(_) => {}
        },
        _ => {}
    }
}

fn collect_decl_debug_spans(decl: &Decl, source: &str, out: &mut Vec<DebugSourceSpan>) {
    match decl {
        Decl::Class(decl) => push_debug_span(decl.class.span, source, out),
        Decl::Fn(decl) => {
            push_debug_span(decl.function.span, source, out);
            collect_function_body_debug_spans(&decl.function, source, out);
        }
        Decl::Var(decl) => {
            push_debug_span(decl.span, source, out);
            for var_decl in &decl.decls {
                push_debug_span(var_decl.span, source, out);
            }
        }
        _ => {}
    }
}

fn collect_stmt_debug_spans(stmt: &Stmt, source: &str, out: &mut Vec<DebugSourceSpan>) {
    push_debug_span(stmt.span(), source, out);
    match stmt {
        Stmt::Block(block) => collect_block_debug_spans(block, source, out),
        Stmt::Decl(decl) => collect_decl_debug_spans(decl, source, out),
        Stmt::DoWhile(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        Stmt::For(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        Stmt::ForIn(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        Stmt::ForOf(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        Stmt::If(stmt) => {
            collect_stmt_debug_spans(&stmt.cons, source, out);
            if let Some(alt) = &stmt.alt {
                collect_stmt_debug_spans(alt, source, out);
            }
        }
        Stmt::Labeled(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        Stmt::Switch(stmt) => {
            for case in &stmt.cases {
                for cons in &case.cons {
                    collect_stmt_debug_spans(cons, source, out);
                }
            }
        }
        Stmt::Try(stmt) => {
            collect_block_debug_spans(&stmt.block, source, out);
            if let Some(handler) = &stmt.handler {
                collect_block_debug_spans(&handler.body, source, out);
            }
            if let Some(finalizer) = &stmt.finalizer {
                collect_block_debug_spans(finalizer, source, out);
            }
        }
        Stmt::While(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        Stmt::With(stmt) => collect_stmt_debug_spans(&stmt.body, source, out),
        _ => {}
    }
}

fn collect_block_debug_spans(block: &BlockStmt, source: &str, out: &mut Vec<DebugSourceSpan>) {
    for stmt in &block.stmts {
        collect_stmt_debug_spans(stmt, source, out);
    }
}

fn collect_function_body_debug_spans(
    function: &Function,
    source: &str,
    out: &mut Vec<DebugSourceSpan>,
) {
    if let Some(body) = &function.body {
        collect_block_debug_spans(body, source, out);
    }
}

fn push_debug_span(span: Span, source: &str, out: &mut Vec<DebugSourceSpan>) {
    let start = byte_pos_to_offset(span.lo.0, source.len());
    let end = byte_pos_to_offset(span.hi.0, source.len());
    if start >= end {
        return;
    }
    let (line, column) = line_column_for_offset(source, start);
    let (end_line, end_column) = line_column_for_offset(source, end);
    out.push(DebugSourceSpan {
        start,
        end,
        line,
        column,
        end_line,
        end_column,
    });
}

fn line_column_for_offset(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    (line, offset.saturating_sub(line_start))
}

/// Runtime feature manifest。
///
/// 编译器从源码和 IR 中推导运行所需 feature，规范化后生成 md5 和 runtime 包名。
/// `[generator, bigint]` 与 `[bigint, generator]` 会归一成同一个 canonical，因此对应同一个包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFeatureManifest {
    /// 去重、排序后的 feature 列表。
    pub features: Vec<String>,
    /// 规范化 feature 字符串。
    pub canonical: String,
    /// `canonical` 的 md5 短标识。
    pub md5: String,
    /// 对应 runtime 包名。
    pub package_name: String,
}

impl RuntimeFeatureManifest {
    /// 根据 feature 列表生成 manifest。
    ///
    /// 这里会自动展开隐含依赖，例如某些 builtins 可能需要 host bridge 支持。
    pub fn from_features(features: Vec<String>, compact_errors: bool) -> Self {
        let mut set = BTreeSet::new();
        for feature in features {
            expand_runtime_feature(&feature, &mut set);
        }
        if compact_errors {
            set.insert("compact-errors".to_string());
        }
        let features = set.into_iter().collect::<Vec<_>>();
        let canonical = features.join(",");
        let md5 = md5_hex_12(canonical.as_bytes());
        let package_name = format!("runtime-feature-{md5}");
        Self {
            features,
            canonical,
            md5,
            package_name,
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"features\":[{}],\"canonical\":{},\"md5\":{},\"packageName\":{}}}",
            self.features
                .iter()
                .map(|feature| json_string(feature))
                .collect::<Vec<_>>()
                .join(","),
            json_string(&self.canonical),
            json_string(&self.md5),
            json_string(&self.package_name),
        )
    }
}

fn runtime_features_for_source_and_ir(source: &str, ir: &IrModule) -> Vec<String> {
    // feature 推导决定运行时包的粒度：编译器只把源码实际触达的语义能力放进列表，
    // 页面/CLI 再据此选择 `runtime-feature-<md5>` 包。这里宁可轻微多报，也不要漏报，
    // 因为漏报会让生成产物在用户环境里直接缺 runtime 能力。
    let mut features = BTreeSet::new();
    if ir.kind == IrModuleKind::Module || !ir.imports.is_empty() || !ir.exports.is_empty() {
        features.insert("module".to_string());
    }
    if ir
        .constants
        .iter()
        .any(|constant| matches!(constant, IrConst::BigInt(_)))
        || contains_bigint_literal(source)
        || source.contains("BigInt")
    {
        features.insert("bigint".to_string());
    }
    if ir
        .constants
        .iter()
        .any(|constant| matches!(constant, IrConst::Regex { .. }))
        || contains_regex_dependency(source)
    {
        features.insert("regexp".to_string());
    }
    if ir
        .functions
        .iter()
        .any(|function| function.flags.is_generator)
        || source.contains("function*")
        || source.contains("yield")
    {
        features.insert("generator".to_string());
    }
    if source.contains("Proxy") || source.contains("Reflect.") {
        features.insert("proxy".to_string());
    }
    if source.contains("eval(") || source.contains("(eval)") {
        features.insert("test262-eval".to_string());
    }
    if contains_member_call(source, ARRAY_BUILTIN_METHODS) || source.contains("Array.") {
        features.insert("array-builtins".to_string());
    }
    if contains_member_call(source, STRING_BUILTIN_METHODS) || source.contains("String.") {
        features.insert("string-builtins".to_string());
    }
    if contains_member_call(source, FUNCTION_BUILTIN_METHODS) || source.contains("Function.") {
        features.insert("function-builtins".to_string());
    }
    if contains_member_call(source, OBJECT_BUILTIN_METHODS) || source.contains("Object.") {
        features.insert("object-builtins".to_string());
    }
    features.into_iter().collect()
}

const ARRAY_BUILTIN_METHODS: &[&str] = &[
    "at",
    "concat",
    "copyWithin",
    "entries",
    "every",
    "fill",
    "filter",
    "find",
    "findIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "pop",
    "push",
    "reduce",
    "reduceRight",
    "reverse",
    "shift",
    "slice",
    "some",
    "sort",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "unshift",
    "values",
    "with",
];

const STRING_BUILTIN_METHODS: &[&str] = &[
    "charAt",
    "charCodeAt",
    "codePointAt",
    "endsWith",
    "includes",
    "indexOf",
    "lastIndexOf",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "split",
    "startsWith",
    "substring",
    "toLowerCase",
    "toUpperCase",
    "trim",
    "trimEnd",
    "trimStart",
];

const FUNCTION_BUILTIN_METHODS: &[&str] = &["apply", "bind", "call"];

const OBJECT_BUILTIN_METHODS: &[&str] = &[
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

fn contains_member_call(source: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        source.contains(&format!(".{name}("))
            || source.contains(&format!("['{name}']("))
            || source.contains(&format!("[\"{name}\"]("))
    })
}

fn contains_bigint_literal(source: &str) -> bool {
    let mut prev_is_ident = false;
    let mut digits = false;
    for ch in source.chars() {
        if ch.is_ascii_digit() {
            digits = true;
            prev_is_ident = false;
            continue;
        }
        if ch == 'n' && digits && !prev_is_ident {
            return true;
        }
        prev_is_ident = ch == '_' || ch == '$' || ch.is_ascii_alphabetic();
        digits = false;
    }
    false
}

fn contains_regex_dependency(source: &str) -> bool {
    source.contains("RegExp")
        || source.contains(".match(")
        || source.contains(".matchAll(")
        || source.contains(".replace(")
        || source.contains(".replaceAll(")
        || source.contains(".search(")
        || source.contains(".split(")
        || source.contains(".test(")
}

fn expand_runtime_feature(feature: &str, out: &mut BTreeSet<String>) {
    match feature {
        "full" => {
            for item in [
                "bigint",
                "generator",
                "host-builtins",
                "module",
                "proxy",
                "regexp",
                "test262-compat",
            ] {
                expand_runtime_feature(item, out);
            }
        }
        "host-builtins" => {
            for item in [
                "array-builtins",
                "function-builtins",
                "object-builtins",
                "string-builtins",
            ] {
                expand_runtime_feature(item, out);
            }
        }
        "test262-compat" => expand_runtime_feature("test262-eval", out),
        "" => {}
        other => {
            out.insert(other.to_string());
        }
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn md5_hex_12(input: &[u8]) -> String {
    let mut state = [
        0x67452301_u32,
        0xefcdab89_u32,
        0x98badcfe_u32,
        0x10325476_u32,
    ];
    let bit_len = (input.len() as u64) * 8;
    let mut data = input.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in data.chunks_exact(64) {
        let mut words = [0_u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }

        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | ((!b) & d), i)
            } else if i < 32 {
                ((d & b) | ((!d) & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | (!d)), (7 * i) % 16)
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(MD5_K[i])
                    .wrapping_add(words[g])
                    .rotate_left(MD5_S[i]),
            );
            a = tmp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut hex = String::with_capacity(32);
    for word in state {
        for byte in word.to_le_bytes() {
            hex.push_str(&format!("{byte:02x}"));
        }
    }
    hex.truncate(12);
    hex
}

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

pub fn encoding_names_from_seed(seed: &str) -> Result<Vec<String>, String> {
    let encoding = EncodingConfig::from_seed(seed).map_err(|err| err.to_string())?;
    Ok(encoding.names().flatten())
}

pub fn encoding_seed_for_seed_and_bytes(seed: &str, bytes: &[u8]) -> Result<String, String> {
    let encoding = EncodingConfig::from_seed(seed).map_err(|err| err.to_string())?;
    encoding.paired_seed(bytes).map_err(|err| err.to_string())
}

#[derive(Debug, Clone)]
struct StaticImportSpan {
    start: usize,
    end: usize,
    statement: String,
    specifier: String,
    locals: Vec<String>,
}

fn static_imports_with_spans(source: &str) -> Result<Vec<StaticImportSpan>, String> {
    let program = parse_source(source)?;
    let Program::Module(module) = program else {
        return Ok(Vec::new());
    };
    let mut imports = module
        .body
        .iter()
        .filter_map(|item| match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(decl)) => {
                let start = byte_pos_to_offset(decl.span.lo.0, source.len());
                let end = byte_pos_to_offset(decl.span.hi.0, source.len());
                Some(StaticImportSpan {
                    start,
                    end,
                    statement: source
                        .get(start..end)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    specifier: decl.src.value.to_string(),
                    locals: decl
                        .specifiers
                        .iter()
                        .filter_map(import_local_name)
                        .collect(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    imports.sort_by_key(|import| import.start);
    Ok(imports)
}

fn byte_pos_to_offset(pos: u32, source_len: usize) -> usize {
    (pos as usize).saturating_sub(1).min(source_len)
}

fn remove_spans(source: &str, spans: impl IntoIterator<Item = (usize, usize)>) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for (start, end) in spans {
        if start < cursor || start > source.len() || end > source.len() || start > end {
            continue;
        }
        out.push_str(&source[cursor..start]);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        cursor = end;
    }
    out.push_str(&source[cursor..]);
    out
}

fn module_export_names_from_ast(module: &Module) -> Vec<String> {
    let mut names = BTreeSet::new();
    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(decl)) => {
                names.extend(decl_names_for_export(&decl.decl));
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(decl)) => {
                for specifier in &decl.specifiers {
                    if let Some((_, exported)) = export_entry_for_wrapper(specifier) {
                        if exported != "default" {
                            names.insert(exported);
                        }
                    }
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(_))
            | ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(_)) => {
                names.insert("default".to_string());
            }
            _ => {}
        }
    }
    names.into_iter().collect()
}

fn module_has_default_export_from_ast(module: &Module) -> bool {
    module.body.iter().any(|item| match item {
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(_))
        | ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(_)) => true,
        ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(decl)) => {
            decl.specifiers.iter().any(|specifier| {
                export_entry_for_wrapper(specifier)
                    .map(|(_, exported)| exported == "default")
                    .unwrap_or(false)
            })
        }
        _ => false,
    })
}

fn decl_names_for_export(decl: &Decl) -> Vec<String> {
    match decl {
        Decl::Class(decl) => vec![decl.ident.sym.to_string()],
        Decl::Fn(decl) => vec![decl.ident.sym.to_string()],
        Decl::Var(decl) => decl
            .decls
            .iter()
            .flat_map(|decl| pat_bound_names_for_export(&decl.name))
            .collect(),
        _ => Vec::new(),
    }
}

fn pat_bound_names_for_export(pat: &Pat) -> Vec<String> {
    let mut names = Vec::new();
    collect_pat_bound_names_for_export(pat, &mut names);
    names
}

fn collect_pat_bound_names_for_export(pat: &Pat, names: &mut Vec<String>) {
    match pat {
        Pat::Ident(ident) => names.push(ident.id.sym.to_string()),
        Pat::Array(array) => {
            for element in array.elems.iter().flatten() {
                collect_pat_bound_names_for_export(element, names);
            }
        }
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::KeyValue(prop) => {
                        collect_pat_bound_names_for_export(&prop.value, names)
                    }
                    ObjectPatProp::Assign(prop) => names.push(prop.key.sym.to_string()),
                    ObjectPatProp::Rest(prop) => {
                        collect_pat_bound_names_for_export(&prop.arg, names)
                    }
                }
            }
        }
        Pat::Rest(rest) => collect_pat_bound_names_for_export(&rest.arg, names),
        Pat::Assign(assign) => collect_pat_bound_names_for_export(&assign.left, names),
        Pat::Expr(_) | Pat::Invalid(_) => {}
    }
}

fn export_entry_for_wrapper(specifier: &ExportSpecifier) -> Option<(String, String)> {
    match specifier {
        ExportSpecifier::Named(named) => {
            let local = module_export_name_for_wrapper(&named.orig);
            let exported = named
                .exported
                .as_ref()
                .map(module_export_name_for_wrapper)
                .unwrap_or_else(|| local.clone());
            Some((local, exported))
        }
        ExportSpecifier::Default(default) => {
            Some(("default".to_string(), default.exported.sym.to_string()))
        }
        ExportSpecifier::Namespace(namespace) => {
            let exported = module_export_name_for_wrapper(&namespace.name);
            Some((exported.clone(), exported))
        }
    }
}

fn module_export_name_for_wrapper(name: &ModuleExportName) -> String {
    name.atom().to_string()
}

fn import_local_name(specifier: &ImportSpecifier) -> Option<String> {
    match specifier {
        ImportSpecifier::Named(named) => Some(named.local.sym.to_string()),
        ImportSpecifier::Default(default) => Some(default.local.sym.to_string()),
        ImportSpecifier::Namespace(namespace) => Some(namespace.local.sym.to_string()),
    }
}

fn rewrite_import_specifier(statement: &str, replacement: &str) -> String {
    let Some(quote_index) = statement.find(['"', '\'']) else {
        return statement.to_string();
    };
    let quote = statement.as_bytes()[quote_index] as char;
    let rest = &statement[quote_index + 1..];
    let Some(end) = rest.find(quote) else {
        return statement.to_string();
    };
    format!(
        "{}{}{}{}",
        &statement[..quote_index + 1],
        replacement,
        quote,
        &rest[end + quote.len_utf8()..]
    )
}

fn rewrite_dynamic_imports_with_rewrites(source: &str, rewrites: &BTreeMap<&str, &str>) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("import(") {
        let start = cursor + offset;
        let quote_start = start + "import(".len();
        let Some(quote) = source[quote_start..].chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            out.push_str(&source[cursor..quote_start]);
            cursor = quote_start;
            continue;
        }
        let specifier_start = quote_start + quote.len_utf8();
        let Some(specifier_end_offset) = source[specifier_start..].find(quote) else {
            break;
        };
        let specifier_end = specifier_start + specifier_end_offset;
        let after_quote = specifier_end + quote.len_utf8();
        let rest = source[after_quote..].trim_start();
        if !rest.starts_with(')') {
            out.push_str(&source[cursor..after_quote]);
            cursor = after_quote;
            continue;
        }
        let specifier = &source[specifier_start..specifier_end];
        let Some(replacement) = rewrites.get(specifier) else {
            out.push_str(&source[cursor..after_quote]);
            cursor = after_quote;
            continue;
        };
        out.push_str(&source[cursor..start]);
        out.push_str(&format!("import({})", json_string(replacement)));
        cursor = after_quote + (source[after_quote..].len() - rest.len()) + 1;
    }
    out.push_str(&source[cursor..]);
    out
}

fn rewrite_hot_numeric_intrinsics(source: &str) -> String {
    let source = rewrite_deflate_table_intrinsics(source);
    rewrite_bit_reverse_table_intrinsics(&source)
}

fn rewrite_deflate_table_intrinsics(source: &str) -> String {
    const NEEDLE: &str = "=function(s,i){for(var a=new ";
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(NEEDLE) {
        let eq = cursor + offset;
        let Some(name_start) = assignment_name_start(source, eq) else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        };
        let name = &source[name_start..eq];
        let Some((uint16_ctor, after_uint16)) = read_js_identifier(source, eq + NEEDLE.len())
        else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        };
        const MID: &str = "(31),n=0;n<31;++n)a[n]=i+=1<<s[n-1];for(var e=new ";
        if !source[after_uint16..].starts_with(MID) {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        }
        let int32_start = after_uint16 + MID.len();
        let Some((int32_ctor, _)) = read_js_identifier(source, int32_start) else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        };
        let candidate = format!(
            "{name}=function(s,i){{for(var a=new {uint16_ctor}(31),n=0;n<31;++n)a[n]=i+=1<<s[n-1];for(var e=new {int32_ctor}(a[30]),n=1;n<30;++n)for(var t=a[n];t<a[n+1];++t)e[t]=t-a[n]<<5|n;return{{b:a,r:e}}}}"
        );
        if source[name_start..].starts_with(&candidate) {
            out.push_str(&source[cursor..name_start]);
            out.push_str(&format!(
                "{name}=__jsVmIntrinsicDeflateTable({uint16_ctor},{int32_ctor})"
            ));
            cursor = name_start + candidate.len();
        } else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
        }
    }
    out.push_str(&source[cursor..]);
    out
}

fn rewrite_bit_reverse_table_intrinsics(source: &str) -> String {
    const NEEDLE: &str = "=new ";
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(NEEDLE) {
        let eq = cursor + offset;
        let Some(table_start) = assignment_name_start(source, eq) else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        };
        let table = &source[table_start..eq];
        let Some((uint16_ctor, after_uint16)) = read_js_identifier(source, eq + NEEDLE.len())
        else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        };
        const MID: &str = "(32768);for(var ";
        if !source[after_uint16..].starts_with(MID) {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        }
        let index_start = after_uint16 + MID.len();
        let Some((index, _)) = read_js_identifier(source, index_start) else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
            continue;
        };
        let candidate = format!(
            "{table}=new {uint16_ctor}(32768);for(var {index}=0;{index}<32768;++{index}){{var pa=({index}&43690)>>1|({index}&21845)<<1;pa=(pa&52428)>>2|(pa&13107)<<2,pa=(pa&61680)>>4|(pa&3855)<<4,{table}[{index}]=((pa&65280)>>8|(pa&255)<<8)>>1}}"
        );
        if source[table_start..].starts_with(&candidate) {
            out.push_str(&source[cursor..table_start]);
            out.push_str(&format!(
                "{table}=__jsVmIntrinsicBitReverseTable({uint16_ctor});"
            ));
            cursor = table_start + candidate.len();
        } else {
            out.push_str(&source[cursor..eq + 1]);
            cursor = eq + 1;
        }
    }
    out.push_str(&source[cursor..]);
    out
}

fn assignment_name_start(source: &str, eq: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut end = eq;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_js_identifier_part_byte(bytes[start - 1]) {
        start -= 1;
    }
    (start < end && is_valid_js_identifier(&source[start..end])).then_some(start)
}

fn read_js_identifier(source: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    let mut end = start;
    while end < bytes.len() && is_js_identifier_part_byte(bytes[end]) {
        end += 1;
    }
    (start < end && is_valid_js_identifier(&source[start..end]))
        .then_some((&source[start..end], end))
}

fn is_js_identifier_part_byte(value: u8) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, b'_' | b'$')
}

fn collapse_empty_vite_preload_imports(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("(()=>import(") {
        let call_open = cursor + offset;
        let Some(identifier_start) = vite_preload_identifier_start(source, call_open) else {
            out.push_str(&source[cursor..call_open + 1]);
            cursor = call_open + 1;
            continue;
        };
        let import_arg_start = call_open + "(()=>import(".len();
        let Some(quote) = source[import_arg_start..].chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            out.push_str(&source[cursor..import_arg_start]);
            cursor = import_arg_start;
            continue;
        }
        let specifier_start = import_arg_start + quote.len_utf8();
        let Some(specifier_end_offset) = source[specifier_start..].find(quote) else {
            break;
        };
        let specifier_end = specifier_start + specifier_end_offset;
        let after_quote = specifier_end + quote.len_utf8();
        let Some((call_end, specifier)) =
            empty_vite_preload_suffix(source, after_quote, &source[specifier_start..specifier_end])
        else {
            out.push_str(&source[cursor..after_quote]);
            cursor = after_quote;
            continue;
        };
        out.push_str(&source[cursor..identifier_start]);
        out.push_str(&format!("import({})", json_string(specifier)));
        cursor = call_end;
    }
    out.push_str(&source[cursor..]);
    out
}

fn vite_preload_identifier_start(source: &str, call_open: usize) -> Option<usize> {
    if call_open == 0 || source.as_bytes().get(call_open)? != &b'(' {
        return None;
    }
    let mut cursor = call_open;
    while cursor > 0 {
        let prev = source[..cursor].chars().next_back()?;
        if prev.is_whitespace() {
            cursor -= prev.len_utf8();
            continue;
        }
        break;
    }
    let mut start = cursor;
    while start > 0 {
        let prev = source[..start].chars().next_back()?;
        if prev == '_' || prev == '$' || prev.is_ascii_alphanumeric() {
            start -= prev.len_utf8();
            continue;
        }
        break;
    }
    if start == cursor {
        return None;
    }
    let before = source[..start].chars().next_back();
    if matches!(before, Some('.') | Some('"') | Some('\'')) {
        return None;
    }
    Some(start)
}

fn empty_vite_preload_suffix<'a>(
    source: &'a str,
    after_quote: usize,
    specifier: &'a str,
) -> Option<(usize, &'a str)> {
    let mut cursor = skip_ascii_ws(source, after_quote);
    if source.as_bytes().get(cursor)? != &b')' {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_ws(source, cursor);
    if source.as_bytes().get(cursor)? != &b',' {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_ws(source, cursor);
    if source.as_bytes().get(cursor)? != &b'[' {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_ws(source, cursor);
    if source.as_bytes().get(cursor)? != &b']' {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_ws(source, cursor);
    if source.as_bytes().get(cursor)? != &b')' {
        return None;
    }
    Some((cursor + 1, specifier))
}

fn skip_ascii_ws(source: &str, mut cursor: usize) -> usize {
    while let Some(byte) = source.as_bytes().get(cursor) {
        if !byte.is_ascii_whitespace() {
            break;
        }
        cursor += 1;
    }
    cursor
}

fn dynamic_import_specifiers(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("import(") {
        let start = cursor + offset;
        let quote_start = start + "import(".len();
        let Some(quote) = source[quote_start..].chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            cursor = quote_start;
            continue;
        }
        let specifier_start = quote_start + quote.len_utf8();
        let Some(specifier_end_offset) = source[specifier_start..].find(quote) else {
            break;
        };
        let specifier_end = specifier_start + specifier_end_offset;
        let after_quote = specifier_end + quote.len_utf8();
        let rest = source[after_quote..].trim_start();
        if rest.starts_with(')') {
            let specifier = source[specifier_start..specifier_end].to_string();
            if seen.insert(specifier.clone()) {
                out.push(specifier);
            }
            cursor = after_quote + (source[after_quote..].len() - rest.len()) + 1;
        } else {
            cursor = after_quote;
        }
    }
    out
}

fn awaited_dynamic_import_specifiers(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("await") {
        let await_start = cursor + offset;
        let before = source[..await_start].chars().next_back();
        if matches!(before, Some(ch) if ch == '_' || ch == '$' || ch.is_ascii_alphanumeric()) {
            cursor = await_start + "await".len();
            continue;
        }
        let mut import_start = skip_ascii_ws(source, await_start + "await".len());
        if source[import_start..].starts_with('(') {
            import_start = skip_ascii_ws(source, import_start + 1);
        }
        if !source[import_start..].starts_with("import(") {
            cursor = await_start + "await".len();
            continue;
        }
        let quote_start = import_start + "import(".len();
        let Some(quote) = source[quote_start..].chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            cursor = quote_start;
            continue;
        }
        let specifier_start = quote_start + quote.len_utf8();
        let Some(specifier_end_offset) = source[specifier_start..].find(quote) else {
            break;
        };
        let specifier_end = specifier_start + specifier_end_offset;
        let after_quote = specifier_end + quote.len_utf8();
        let rest_start = skip_ascii_ws(source, after_quote);
        if source[rest_start..].starts_with(')') {
            out.insert(source[specifier_start..specifier_end].to_string());
            cursor = rest_start + 1;
        } else {
            cursor = after_quote;
        }
    }
    out
}

fn module_wrapper_source(
    options: &PackagedModuleOptions,
    imports: &[String],
    dynamic_imports: &[(String, String)],
    eager_dynamic_imports: &BTreeSet<String>,
    export_names: &[String],
    has_default_export: bool,
    imported_locals: &BTreeSet<String>,
) -> String {
    // wrapper 的职责是“像普通 ESM 一样存在”，同时把真实执行交给 VM：
    //
    // - import 语句保持在 wrapper 顶层，继续享受宿主 loader 的依赖解析。
    // - externs 数组按编译器记录的 slot 顺序传给执行器。
    // - module 模式把 VM 的 namespace 同步回 wrapper export binding。
    // - script 模式只执行副作用，不暴露无意义的 default 值。
    let mut out = Vec::new();
    out.push(format!(
        "import {{ executeModule, executeScript, executeDebug, createDebugSession, loadBin, loadSourceMap, resolveExternal, sourceFrame, breakpointPcs, decorateDebugEvent }} from {};",
        json_string(&options.env_specifier)
    ));
    out.extend(imports.iter().cloned());
    for (index, (specifier, _)) in dynamic_imports.iter().enumerate() {
        if eager_dynamic_imports.contains(specifier) {
            out.push(format!(
                "import * as __jsVmDynamicImport{index} from {};",
                json_string(specifier)
            ));
        }
    }
    if dynamic_imports.is_empty() {
        out.push("const __jsVmDynamicImports = null;".to_string());
    } else {
        out.push(format!(
            "const __jsVmDynamicImports = new Map([{}]);",
            dynamic_imports
                .iter()
                .enumerate()
                .map(|(index, (specifier, original))| {
                    let entry = if eager_dynamic_imports.contains(specifier) {
                        format!("__jsVmDynamicImport{index}")
                    } else {
                        format!("() => import({})", json_string(specifier))
                    };
                    if specifier == original {
                        format!("[{}, {entry}]", json_string(specifier))
                    } else {
                        format!(
                            "[{}, {entry}], [{}, {entry}]",
                            json_string(specifier),
                            json_string(original)
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push(
        "function __jsVmAsyncValue(read) {\n  const value = Object.create(null);\n  Object.defineProperty(value, '__js_vm_async_resolved', { value: true });\n  Object.defineProperty(value, '__js_vm_value', { get: read });\n  Object.defineProperty(value, 'then', { value(onFulfilled, onRejected) {\n    try {\n      const resolved = read();\n      const next = typeof onFulfilled === 'function' ? onFulfilled(resolved) : resolved;\n      return next && typeof next === 'object' && next.__js_vm_async_resolved ? next : __jsVmAsyncResolved(next);\n    } catch (error) {\n      if (typeof onRejected === 'function') return __jsVmAsyncResolved(onRejected(error));\n      throw error;\n    }\n  } });\n  Object.defineProperty(value, 'catch', { value() { return value; } });\n  Object.defineProperty(value, 'finally', { value(onFinally) {\n    if (typeof onFinally === 'function') onFinally();\n    return value;\n  } });\n  return value;\n}\nfunction __jsVmAsyncResolved(value) {\n  return __jsVmAsyncValue(() => value);\n}\nfunction __jsVmDynamicModuleValue(load) {\n  return load().then((module) => {\n    if (module && typeof module.__jsVmLoadModule === 'function') module.__jsVmLoadModule();\n    return module;\n  });\n}\nfunction __jsVmResolveDynamicImport(specifier) {\n  const key = String(specifier);\n  const load = __jsVmDynamicImports && __jsVmDynamicImports.get(key);\n  return load ? __jsVmDynamicModuleValue(load) : import(key);\n}".to_string(),
    );
    if let Some(dynamic_import_helpers) = out.last_mut() {
        *dynamic_import_helpers = dynamic_import_helpers.replace(
            r#"function __jsVmDynamicModuleValue(load) {
  return load().then((module) => {
    if (module && typeof module.__jsVmLoadModule === 'function') module.__jsVmLoadModule();
    return module;
  });
}
function __jsVmResolveDynamicImport(specifier) {
  const key = String(specifier);
  const load = __jsVmDynamicImports && __jsVmDynamicImports.get(key);
  return load ? __jsVmDynamicModuleValue(load) : import(key);
}"#,
            r#"function __jsVmThenable(value) {
  return value && typeof value === 'object' && value.__js_vm_async_resolved ? value : __jsVmAsyncResolved(value);
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
  return __jsVmThenable((() => {
    if (entry && typeof entry.__jsVmLoadModule === 'function') entry.__jsVmLoadModule();
    return entry;
  })());
}
function __jsVmResolveDynamicImport(specifier) {
  const key = String(specifier);
  const entry = __jsVmDynamicImports && __jsVmDynamicImports.get(key);
  return entry ? __jsVmDynamicModuleValue(entry) : import(key);
}"#,
        );
    }
    out.push(format!(
        "const __jsVmSeed = {};",
        json_string(&options.seed)
    ));
    out.push(format!(
        "const __jsVmBinUrl = new URL({}, import.meta.url);",
        json_string(&options.bin_specifier)
    ));
    out.push(format!(
        "const __jsVmSourceMapUrl = new URL({}, import.meta.url);",
        json_string(&source_map_specifier_from_bin_specifier(
            &options.bin_specifier
        ))
    ));
    out.push(format!("const __jsVmBin = await loadBin(__jsVmBinUrl);"));
    let externs = options
        .extern_slots
        .iter()
        .map(|name| {
            if imported_locals.contains(name) {
                name.clone()
            } else if name == "import" {
                "__jsVmResolveDynamicImport".to_string()
            } else {
                format!("resolveExternal({})", json_string(name))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    out.push(format!(
        "function __jsVmCreateExterns() {{ return [{externs}]; }}"
    ));
    out.push(format!(
        "export const __jsVmDebugInfo = Object.freeze({{ source: {}, bin: __jsVmBinUrl.href, map: __jsVmSourceMapUrl.href, seed: __jsVmSeed, externSlots: [{}] }});",
        json_string(&options.source_file),
        options
            .extern_slots
            .iter()
            .map(|slot| json_string(slot))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push(
        "export async function __jsVmDebug(pc) {\n  const map = await loadSourceMap(__jsVmSourceMapUrl);\n  const __jsVmExterns = __jsVmCreateExterns();\n  const result = executeDebug(__jsVmBin, __jsVmSeed, __jsVmExterns);\n  const stack = (result.stack || []).map((frame) => ({ ...frame, source: sourceFrame(map, frame.pc) }));\n  const selectedPc = Number(pc);\n  return { ...result, info: __jsVmDebugInfo, map, stack, frame: Number.isFinite(selectedPc) ? sourceFrame(map, selectedPc) : null };\n}".to_string(),
    );
    out.push(
        "export async function __jsVmDebugSession(breakpoints = []) {\n  const map = await loadSourceMap(__jsVmSourceMapUrl);\n  const __jsVmExterns = __jsVmCreateExterns();\n  const session = createDebugSession(__jsVmBin, __jsVmSeed, __jsVmExterns);\n  let pcs = [];\n  const applyBreakpoints = (next) => {\n    pcs = breakpointPcs(map, next);\n    session.set_breakpoints(pcs);\n    return pcs;\n  };\n  applyBreakpoints(breakpoints);\n  const decorate = (event) => ({ ...decorateDebugEvent(map, event), info: __jsVmDebugInfo, map, breakpoints: pcs });\n  return {\n    info: __jsVmDebugInfo,\n    map,\n    raw: session,\n    setBreakpoints(next) { return applyBreakpoints(next); },\n    pcFor(next) { return breakpointPcs(map, next); },\n    frame(pc) { return sourceFrame(map, Number(pc)); },\n    resume() { return decorate(session.resume()); },\n    step() { return decorate(session.step()); },\n    inspect() { return decorate(session.inspect()); },\n  };\n}".to_string(),
    );
    if has_default_export || !export_names.is_empty() {
        out.push("let __jsVmModule;".to_string());
        out.push("let __jsVmExecuted = false;".to_string());
        for name in export_names {
            if is_valid_js_identifier(name) && name != "default" {
                out.push(format!("export let {name};"));
            }
        }
        out.push("let __jsVmDefaultExport;".to_string());
        out.push(format!(
            "function __jsVmApplyExports(module) {{\n{}\n  __jsVmDefaultExport = {};\n}}",
            export_names
                .iter()
                .filter(|name| is_valid_js_identifier(name) && name.as_str() != "default")
                .map(|name| format!("  {name} = module[{}];", json_string(name)))
                .collect::<Vec<_>>()
                .join("\n"),
            if has_default_export {
                "module.default".to_string()
            } else {
                "module".to_string()
            }
        ));
        out.push(
            "function __jsVmRunModule() {\n  if (!__jsVmExecuted) {\n    __jsVmModule = executeModule(__jsVmBin, __jsVmSeed, __jsVmCreateExterns());\n    __jsVmExecuted = true;\n    __jsVmApplyExports(__jsVmModule);\n  }\n  return __jsVmModule;\n}".to_string(),
        );
        out.push("export function __jsVmLoadModule() { return __jsVmRunModule(); }".to_string());
        out.push("export { __jsVmDefaultExport as default };".to_string());
        if !options.defer_execution {
            out.push("__jsVmRunModule();".to_string());
        }
    } else {
        out.push("let __jsVmExecuted = false;".to_string());
        out.push(
            "function __jsVmRunModule() {\n  if (!__jsVmExecuted) {\n    executeScript(__jsVmBin, __jsVmSeed, __jsVmCreateExterns());\n    __jsVmExecuted = true;\n  }\n  return undefined;\n}".to_string(),
        );
        out.push("export function __jsVmLoadModule() { return __jsVmRunModule(); }".to_string());
        if !options.defer_execution {
            out.push("__jsVmRunModule();".to_string());
        }
        out.push("export default undefined;".to_string());
    }
    out.push(format!("// source: {}", options.source_file));
    out.join("\n")
}

fn source_map_specifier_from_bin_specifier(specifier: &str) -> String {
    let split = specifier.find(['?', '#']).unwrap_or(specifier.len());
    let (path, suffix) = specifier.split_at(split);
    if let Some(stem) = path.strip_suffix(".bin") {
        format!("{stem}.bin.map{suffix}")
    } else {
        format!("{path}.map{suffix}")
    }
}

fn is_valid_js_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

fn compile_to_ir(source: &str) -> Result<IrModule, String> {
    compile_to_ir_with_externals(source, &[])
}

fn compile_to_ir_with_externals(source: &str, externals: &[String]) -> Result<IrModule, String> {
    // 编译入口只关心“源码 -> IR”。外部槽顺序可以由调用方预置，
    // 这样 Web UI 手动调整 extern 表或 CLI 多文件打包时，都能生成稳定的 slot 索引。
    let program = parse_source(source)?;

    let mut ctx = LoweringContext::with_externals(externals);
    let kind = match program {
        Program::Module(module) => {
            ctx.lower_module(&module);
            js_token_core::IrModuleKind::Module
        }
        Program::Script(script) => {
            ctx.lower_script(&script);
            js_token_core::IrModuleKind::Script
        }
    };

    let mut module = ctx.into_module();
    module.kind = kind;
    Ok(module)
}

fn js_values_to_strings(values: &[JsValue]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ModuleImportRewrite, PackagedModuleOptions, RuntimeFeatureManifest,
        compile_source_to_artifact, compile_to_ir, md5_hex_12, package_module_source,
        remap_external_operands, rewrite_hot_numeric_intrinsics,
        runtime_features_for_source_and_ir,
    };
    use js_token_core::{BytecodeOp, IrConst};

    fn count_subslice(bytes: &[u8], needle: &[u8]) -> usize {
        bytes
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    #[test]
    fn repeated_literals_share_ir_constants() {
        let ir = compile_to_ir("const a = 1; const b = 1; const c = 'x'; const d = 'x'; a + b;")
            .unwrap();
        let ones = ir
            .constants
            .iter()
            .filter(|constant| matches!(constant, IrConst::Int(1)))
            .count();
        let strings = ir
            .constants
            .iter()
            .filter(|constant| matches!(constant, IrConst::String(value) if value == "x"))
            .count();

        assert_eq!(ones, 1, "{:#?}", ir.constants);
        assert_eq!(strings, 1, "{:#?}", ir.constants);
    }

    #[test]
    fn vue_getter_member_declares_use_fused_local_member_opcode() {
        let source = r#"
            class Wr {
                get(i, a, n) {
                    if (a === "__v_skip") return i.__v_skip;
                    const e = this._isReadonly, t = this._isShallow;
                    if (a === "__v_isReactive") return !e;
                    return t;
                }
            }
            Wr;
        "#;
        let ir = compile_to_ir(source).unwrap();
        let bytecode = ir.to_bytecode();
        let text = bytecode.to_text();

        assert!(
            bytecode
                .instructions
                .iter()
                .any(|instruction| instruction.op == BytecodeOp::StoreLocalMemberConst),
            "{text}"
        );
        assert!(!text.contains("MEMBER_CONST r0, name#"), "{text}");
    }

    #[test]
    fn function_this_lowers_to_local_slot() {
        let ir = compile_to_ir(
            r#"
            function read() {
                return this.value;
            }
            read;
            "#,
        )
        .unwrap();
        let module = ir.to_bytecode();
        let text = module.to_text();

        assert!(text.contains("local#1"), "{text}");
        assert!(!text.contains("(\"this\")"), "{text}");
    }

    #[test]
    fn source_map_exposes_pc_byte_ranges_and_source_locations() {
        let source = "const value = 1 + 2;\nvalue;";
        let artifact = compile_source_to_artifact(source, None, &[]).unwrap();
        let map = artifact.source_map;

        assert!(map.contains("\"version\":3"), "{map}");
        assert!(
            map.contains("\"sourcesContent\":[\"const value = 1 + 2;\\nvalue;\"]"),
            "{map}"
        );
        assert!(map.contains("\"x_js_vm\""), "{map}");
        assert!(map.contains("\"version\":2"), "{map}");
        assert!(map.contains("\"bytecodeHash\""), "{map}");
        assert!(map.contains("\"sourceSpans\""), "{map}");
        assert!(map.contains("\"opcodes\""), "{map}");
        assert!(map.contains("\"pcRanges\""), "{map}");
        assert!(map.contains("\"pcOps\""), "{map}");
        assert!(map.contains("\"instructionCount\":3"), "{map}");
        assert!(map.contains("[0,1,"), "{map}");
        assert!(map.contains("[0,20,1,0,1,20]"), "{map}");
        assert!(map.contains("[21,27,2,0,2,6]"), "{map}");
    }

    #[test]
    fn external_root_names_compile_to_extern_operands() {
        let ir = compile_to_ir(r#"console.log("ass");"#).unwrap();
        assert_eq!(ir.extern_slots, vec!["console"]);

        let module = ir.to_bytecode();
        let text = module.to_text();
        let bytes = module.to_bytes();

        assert!(module.names.is_empty(), "{text}");
        assert!(!text.contains(".names"), "{text}");
        assert!(
            text.contains("LOAD_NAME r0, extern#0(\"console\")"),
            "{text}"
        );
        assert!(!text.contains("name#0(\"console\")"), "{text}");
        assert_eq!(count_subslice(&bytes, b"console"), 0);
    }

    #[test]
    fn external_operands_remap_when_extern_slots_are_reordered() {
        let ir = compile_to_ir("console.log(window);").unwrap();
        let original_slots = ir.extern_slots.clone();
        let mut module = ir.to_bytecode();
        let reordered_slots = vec!["window".to_string(), "console".to_string()];

        assert_eq!(original_slots, vec!["console", "window"]);
        remap_external_operands(&mut module, &original_slots, &reordered_slots).unwrap();
        module.extern_slots = reordered_slots;
        let text = module.to_text();

        assert_eq!(module.extern_slots, vec!["window", "console"]);
        assert!(text.contains("extern#1(\"console\")"), "{text}");
        assert!(text.contains("extern#0(\"window\")"), "{text}");
    }

    #[test]
    fn destructuring_declared_names_do_not_become_externals() {
        let ir = compile_to_ir(
            r#"
            const source = { immediate: 1, deep: 2, items: [3] };
            const { immediate: n, deep: e, items: [a] } = source;
            n + e + a;
            "#,
        )
        .unwrap();
        let text = ir.to_text();

        assert!(ir.extern_slots.is_empty(), "{text}");
        assert!(!text.contains("external"), "{text}");
    }

    #[test]
    fn destructuring_function_params_are_bound_inside_body() {
        let ir = compile_to_ir(
            r#"
            function uk({ record: s }) {
              return s.name;
            }
            uk({ record: { name: "route" } });
            "#,
        )
        .unwrap();
        let text = ir.to_text();

        assert!(ir.extern_slots.is_empty(), "{text}");
        assert!(text.contains("__js_vm_param_0"), "{text}");
        assert!(text.contains("name=s"), "{text}");
    }

    #[test]
    fn underscored_member_access_is_not_numeric_separator_literal() {
        let ir = compile_to_ir("globalThis.__JS_VM_MODULES__;").unwrap();
        let text = ir.to_text();
        assert!(text.contains("__JS_VM_MODULES__"), "{text}");
    }

    #[test]
    fn regex_character_class_is_not_numeric_separator_literal() {
        let ir = compile_to_ir("const re = /[a-zA-Z0-9_]/; re.test('a');").unwrap();
        let text = ir.to_text();
        assert!(text.contains("extern#0 = RegExp"), "{text}");
        assert!(text.contains("const#0 = \"[a-zA-Z0-9_]\""), "{text}");
        assert!(text.contains("r0 = new extern#0"), "{text}");
    }

    #[test]
    fn object_spread_does_not_emit_unsupported_opcode() {
        let artifact = compile_source_to_artifact(
            "const base = { a: 1, b: 2 }; const value = { ...base, b: 3 }; value.b;",
            None,
            &[],
        )
        .unwrap();
        assert!(
            !artifact.bytecode_text.contains("UNSUPPORTED"),
            "{}",
            artifact.bytecode_text
        );
    }

    #[test]
    fn accessors_do_not_emit_unsupported_opcode() {
        let artifact = compile_source_to_artifact(
            r#"
            const ref = {
              _value: 1,
              get value() { return this._value; },
              set value(next) { this._value = next; }
            };
            class Box {
              constructor(value) { this._value = value; }
              get value() { return this._value; }
              set value(next) { this._value = next; }
            }
            ref.value = new Box(2).value;
            ref.value;
            "#,
            None,
            &[],
        )
        .unwrap();
        assert!(
            !artifact.bytecode_text.contains("UNSUPPORTED"),
            "{}",
            artifact.bytecode_text
        );
    }

    #[test]
    fn optional_member_checks_nullish_before_property_read() {
        let artifact = compile_source_to_artifact(
            r#"
            const qs = undefined;
            function read(options = {}) {
              const { window: target = qs } = options;
              return target?.localStorage;
            }
            read();
            "#,
            None,
            &[],
        )
        .unwrap();
        let local_storage_line = artifact
            .bytecode_text
            .lines()
            .find(|line| line.contains("MEMBER") && line.contains("localStorage"))
            .expect("missing guarded localStorage member read");
        let local_storage_index = artifact
            .bytecode_text
            .find(local_storage_line)
            .expect("missing guarded localStorage member read");
        let before_member = &artifact.bytecode_text[..local_storage_index];
        assert!(
            before_member.contains("JUMP_IF_FALSE") || before_member.contains("JUMP_IF_TRUE"),
            "{}",
            artifact.bytecode_text
        );
    }

    #[test]
    fn for_in_checks_nullish_before_object_keys() {
        let artifact = compile_source_to_artifact(
            r#"
            for (const key in undefined) {
              key;
            }
            7;
            "#,
            None,
            &[],
        )
        .unwrap();
        let code = artifact
            .bytecode_text
            .split(".code")
            .nth(1)
            .unwrap_or(&artifact.bytecode_text);
        let keys_index = code.find("keys").expect("missing Object.keys lowering");
        let before_keys = &code[..keys_index];

        assert!(before_keys.contains("null"), "{}", artifact.bytecode_text);
        assert!(
            before_keys.contains("undefined"),
            "{}",
            artifact.bytecode_text
        );
        assert!(before_keys.contains("JUMP"), "{}", artifact.bytecode_text);
    }

    #[test]
    fn optional_call_checks_callee_before_calling() {
        let artifact = compile_source_to_artifact(
            r#"
            const plugin = {};
            plugin.enhance?.({ app: 1 });
            7;
            "#,
            None,
            &[],
        )
        .unwrap();
        let call_index = artifact
            .bytecode_text
            .find("CALL")
            .expect("missing guarded call");
        let before_call = &artifact.bytecode_text[..call_index];

        assert!(
            before_call.contains("JUMP_IF_FALSE") || before_call.contains("JUMP_IF_TRUE"),
            "{}",
            artifact.bytecode_text
        );
        assert!(
            before_call.contains("undefined"),
            "{}",
            artifact.bytecode_text
        );
    }

    #[test]
    fn optional_chain_continues_across_following_member() {
        let ir = compile_to_ir(
            r#"
            function current() { return undefined; }
            const found = (current())?.appContext.components;
            found;
            "#,
        )
        .unwrap();
        let text = ir.to_text();

        assert!(text.contains("opt_member_read"), "{text}");
        assert!(text.contains("chain_member_read"), "{text}");
        assert!(text.contains("components"), "{text}");
    }

    #[test]
    fn async_function_flag_is_encoded() {
        let artifact = compile_source_to_artifact(
            r#"
            async function createApp() {
              return { app: 1 };
            }
            createApp().then;
            "#,
            None,
            &[],
        )
        .unwrap();

        assert!(
            artifact.bytecode_text.contains("flags:5")
                || artifact.bytecode_text.contains("flags:4"),
            "{}",
            artifact.bytecode_text
        );
    }

    #[test]
    fn super_constructor_call_does_not_become_external() {
        let artifact = compile_source_to_artifact(
            r#"
            class Base {
              constructor(value) { this.value = value; }
            }
            class Child extends Base {
              constructor(value) { super(value); }
            }
            new Child(7).value;
            "#,
            None,
            &[],
        )
        .unwrap();
        assert!(
            !artifact.bytecode_text.contains("extern"),
            "{}",
            artifact.bytecode_text
        );
    }

    #[test]
    fn runtime_feature_md5_matches_node_manifest_rule() {
        assert_eq!(md5_hex_12(b""), "d41d8cd98f00");
        assert_eq!(md5_hex_12(b"bigint,generator"), "1f6c5a7a67ff");

        let left = RuntimeFeatureManifest::from_features(
            vec!["generator".to_string(), "bigint".to_string()],
            false,
        );
        let right = RuntimeFeatureManifest::from_features(
            vec!["bigint".to_string(), "generator".to_string()],
            false,
        );
        assert_eq!(left, right);
        assert_eq!(left.canonical, "bigint,generator");
        assert_eq!(left.package_name, "runtime-feature-1f6c5a7a67ff");
    }

    #[test]
    fn runtime_features_are_detected_from_source_and_ir() {
        let source = r#"
            import value from "./dep.js";
            function* gen() { yield 1n; }
            /a/.test("abc");
            new Proxy({}, Reflect);
            eval("1");
            [1, 2].map((item) => item);
            " abc ".trim();
            (function fn() {}).call(null);
            ({ a: 1 }).hasOwnProperty("a");
            value;
        "#;
        let ir = compile_to_ir(source).unwrap();
        let features = runtime_features_for_source_and_ir(source, &ir);
        for feature in [
            "array-builtins",
            "bigint",
            "function-builtins",
            "generator",
            "module",
            "object-builtins",
            "proxy",
            "regexp",
            "string-builtins",
            "test262-eval",
        ] {
            assert!(features.contains(&feature.to_string()), "{features:?}");
        }
    }

    #[test]
    fn packaged_module_uses_ast_import_export_boundaries() {
        let source = r#"import{a as t,b}from"./app.js";const c=t()+b;function m(){return c}const g={path:"/"};export{m as comp,g as data};"#;
        let packaged = package_module_source(
            source,
            PackagedModuleOptions {
                source_file: "assets/page.js".to_string(),
                wrapper_path: "assets/page.js".to_string(),
                bin_path: "assets/page.bin".to_string(),
                seed: "seed".to_string(),
                env_specifier: "../js_vm_env_browser.js".to_string(),
                bin_specifier: "./page.bin?v=seed".to_string(),
                import_rewrites: vec![ModuleImportRewrite {
                    specifier: "./app.js".to_string(),
                    replacement: "./app.js".to_string(),
                }],
                extern_slots: vec!["t".to_string(), "b".to_string()],
                defer_execution: false,
            },
        )
        .unwrap();

        assert!(!packaged.vm_source.contains("from\"./app.js\""));
        assert!(
            packaged.vm_source.contains("const c=t()+b"),
            "{}",
            packaged.vm_source
        );
        assert!(packaged.vm_source.contains("export{m as comp,g as data}"));
        assert!(
            packaged
                .wrapper_source
                .contains("import{a as t,b}from\"./app.js\";")
        );
        assert!(
            packaged.wrapper_source.contains(
                "import { executeModule, executeScript, executeDebug, createDebugSession, loadBin, loadSourceMap, resolveExternal, sourceFrame, breakpointPcs, decorateDebugEvent }"
            ),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged.wrapper_source.contains(
                "const __jsVmSourceMapUrl = new URL(\"./page.bin.map?v=seed\", import.meta.url);"
            ),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("export const __jsVmDebugInfo = Object.freeze"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("export async function __jsVmDebug(pc)"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("export async function __jsVmDebugSession(breakpoints = [])"),
            "{}",
            packaged.wrapper_source
        );
        assert!(packaged.wrapper_source.contains("export let comp;"));
        assert!(packaged.wrapper_source.contains("export let data;"));
        assert!(packaged.wrapper_source.contains("comp = module[\"comp\"];"));
        assert!(packaged.wrapper_source.contains("data = module[\"data\"];"));
    }

    #[test]
    fn packaged_module_maps_local_dynamic_imports() {
        let source = r#"const route=()=>import("./route.js");export async function load(){const mod=await import("./photo.js");return mod.default()+route;}"#;
        let packaged = package_module_source(
            source,
            PackagedModuleOptions {
                source_file: "assets/app.js".to_string(),
                wrapper_path: "assets/app.js".to_string(),
                bin_path: "assets/app.bin".to_string(),
                seed: "seed".to_string(),
                env_specifier: "../js_vm_env_browser.js".to_string(),
                bin_specifier: "./app.bin?v=seed".to_string(),
                import_rewrites: vec![
                    ModuleImportRewrite {
                        specifier: "./photo.js".to_string(),
                        replacement: "./photo.vm.js".to_string(),
                    },
                    ModuleImportRewrite {
                        specifier: "./route.js".to_string(),
                        replacement: "./route.vm.js".to_string(),
                    },
                ],
                extern_slots: vec!["import".to_string()],
                defer_execution: true,
            },
        )
        .unwrap();

        assert!(
            packaged.vm_source.contains("import(\"./photo.vm.js\")"),
            "{}",
            packaged.vm_source
        );
        assert!(
            packaged.vm_source.contains("import(\"./route.vm.js\")"),
            "{}",
            packaged.vm_source
        );
        assert!(
            packaged.wrapper_source.contains(" from \"./photo.vm.js\";"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            !packaged.wrapper_source.contains(" from \"./route.vm.js\";"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("[\"./photo.js\", __jsVmDynamicImport"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("[\"./route.js\", () => import(\"./route.vm.js\")]"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged.wrapper_source.contains(
                "function __jsVmCreateExterns() { return [__jsVmResolveDynamicImport]; }"
            ),
            "{}",
            packaged.wrapper_source
        );
    }

    #[test]
    fn packaged_module_collapses_empty_vite_preload_dynamic_imports() {
        let source = r#"const G=(loader,deps)=>Promise.all(deps).then(()=>loader());const routes={home:{loader:()=>G(()=>import("./home.js"),[])}};export async function load(){return (await routes.home.loader()).data.value;}"#;
        let packaged = package_module_source(
            source,
            PackagedModuleOptions {
                source_file: "assets/app.js".to_string(),
                wrapper_path: "assets/app.js".to_string(),
                bin_path: "assets/app.bin".to_string(),
                seed: "seed".to_string(),
                env_specifier: "../js_vm_env_browser.js".to_string(),
                bin_specifier: "./app.bin?v=seed".to_string(),
                import_rewrites: vec![ModuleImportRewrite {
                    specifier: "./home.js".to_string(),
                    replacement: "./home.vm.js".to_string(),
                }],
                extern_slots: vec!["Promise".to_string(), "import".to_string()],
                defer_execution: true,
            },
        )
        .unwrap();

        assert!(
            packaged
                .vm_source
                .contains(r#"loader:()=>import("./home.vm.js")"#),
            "{}",
            packaged.vm_source
        );
        assert!(!packaged.vm_source.contains("G(()=>import("));
        assert!(
            packaged
                .wrapper_source
                .contains("[\"./home.js\", () => import(\"./home.vm.js\")]"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            !packaged
                .wrapper_source
                .contains("import * as __jsVmDynamicImport")
        );
    }

    #[test]
    fn packaged_module_keeps_callback_dynamic_imports_lazy() {
        let source = r#"const routes={home:{loader:()=>Promise.resolve().then(()=>import("./home.js"))}};export async function load(){const mod=await routes.home.loader();return mod.default();}"#;
        let packaged = package_module_source(
            source,
            PackagedModuleOptions {
                source_file: "assets/app.js".to_string(),
                wrapper_path: "assets/app.js".to_string(),
                bin_path: "assets/app.bin".to_string(),
                seed: "seed".to_string(),
                env_specifier: "../js_vm_env_browser.js".to_string(),
                bin_specifier: "./app.bin?v=seed".to_string(),
                import_rewrites: vec![ModuleImportRewrite {
                    specifier: "./home.js".to_string(),
                    replacement: "./home.vm.js".to_string(),
                }],
                extern_slots: vec!["Promise".to_string(), "import".to_string()],
                defer_execution: true,
            },
        )
        .unwrap();

        assert!(
            packaged
                .vm_source
                .contains(r#"then(()=>import("./home.vm.js"))"#),
            "{}",
            packaged.vm_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("[\"./home.vm.js\", () => import(\"./home.vm.js\")]"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            packaged
                .wrapper_source
                .contains("[\"./home.js\", () => import(\"./home.vm.js\")]"),
            "{}",
            packaged.wrapper_source
        );
        assert!(
            !packaged
                .wrapper_source
                .contains("import * as __jsVmDynamicImport"),
            "{}",
            packaged.wrapper_source
        );
    }

    #[test]
    fn packaged_module_rewrites_hot_numeric_intrinsics() {
        let source = r#"var za=Uint16Array,MB=Int32Array,Fk,Vt;Fk=function(s,i){for(var a=new za(31),n=0;n<31;++n)a[n]=i+=1<<s[n-1];for(var e=new MB(a[30]),n=1;n<30;++n)for(var t=a[n];t<a[n+1];++t)e[t]=t-a[n]<<5|n;return{b:a,r:e}};Vt=new za(32768);for(var Ts=0;Ts<32768;++Ts){var pa=(Ts&43690)>>1|(Ts&21845)<<1;pa=(pa&52428)>>2|(pa&13107)<<2,pa=(pa&61680)>>4|(pa&3855)<<4,Vt[Ts]=((pa&65280)>>8|(pa&255)<<8)>>1}Fk;Vt;"#;
        let rewritten = rewrite_hot_numeric_intrinsics(source);
        let current_minified = r#"var ri=Uint8Array,Ua=Uint16Array,IB=Int32Array,fk,Ht;fk=function(s,i){for(var a=new Ua(31),n=0;n<31;++n)a[n]=i+=1<<s[n-1];for(var e=new IB(a[30]),n=1;n<30;++n)for(var t=a[n];t<a[n+1];++t)e[t]=t-a[n]<<5|n;return{b:a,r:e}};Ht=new Ua(32768);for(var _s=0;_s<32768;++_s){var pa=(_s&43690)>>1|(_s&21845)<<1;pa=(pa&52428)>>2|(pa&13107)<<2,pa=(pa&61680)>>4|(pa&3855)<<4,Ht[_s]=((pa&65280)>>8|(pa&255)<<8)>>1}fk;Ht;"#;
        let current_rewritten = rewrite_hot_numeric_intrinsics(current_minified);

        assert!(
            rewritten.contains("Fk=__jsVmIntrinsicDeflateTable(za,MB)"),
            "{}",
            rewritten
        );
        assert!(
            rewritten.contains("Vt=__jsVmIntrinsicBitReverseTable(za);"),
            "{}",
            rewritten
        );
        assert!(
            current_rewritten.contains("fk=__jsVmIntrinsicDeflateTable(Ua,IB)"),
            "{}",
            current_rewritten
        );
        assert!(
            current_rewritten.contains("Ht=__jsVmIntrinsicBitReverseTable(Ua);"),
            "{}",
            current_rewritten
        );

        let ir = compile_to_ir(
            "var za=Uint16Array,MB=Int32Array,Fk,Vt;Fk=__jsVmIntrinsicDeflateTable(za,MB);Vt=__jsVmIntrinsicBitReverseTable(za);",
        )
        .unwrap();
        assert!(
            ir.extern_slots
                .contains(&"__jsVmIntrinsicDeflateTable".to_string()),
            "{:?}",
            ir.extern_slots
        );
        assert!(
            ir.extern_slots
                .contains(&"__jsVmIntrinsicBitReverseTable".to_string()),
            "{:?}",
            ir.extern_slots
        );
    }
}
