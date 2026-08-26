//! 编译器的 wasm 绑定入口。
//!
//! 这一层把 Rust 编译器能力导出给 Web UI 和 H5 打包流程使用。核心流程是：
//! JavaScript/TypeScript 源码 -> SWC AST -> IR -> Bytecode -> bytes/source map/runtime feature。
//!
//! 注意：公开给 JS 的接口尽量保持聚合形态，例如 `to_bytecode_artifact` 一次性返回
//! bytecode 文本、bytes profile、source map 和 bytes，避免 UI 侧拼接多个旧接口。

mod compiler;
mod parse;

pub use compiler::{
    ModuleImportInfo, ModuleImportRewrite, ModuleSourceAnalysis, NativeCompilerArtifact,
    PackagedModuleOptions, PackagedModuleSource, RuntimeFeatureManifest, analyze_module_source,
    check_source_syntax, compile_source_to_artifact, compile_source_to_artifact_with_source_file,
    encoding_names_from_seed, encoding_seed_for_seed_and_bytes, package_module_source,
};
use js_sys::{Array, Object, Reflect};
use js_token_core::{EncodingConfig, EncodingNames};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
/// wasm 侧编译器对象。
///
/// 构造时完成源码解析和 IR lowering，后续方法都基于同一份 IR 输出不同产物。
pub struct Compiler {
    inner: compiler::Compiler,
}

#[wasm_bindgen]
/// wasm 侧编译产物。
///
/// 它是页面和下载包共同使用的聚合结果：既包含可读调试文本，也包含真正运行的 bytes。
pub struct CompilerArtifact {
    inner: compiler::CompilerArtifact,
}

#[wasm_bindgen]
impl CompilerArtifact {
    /// 返回可读 Bytecode 文本，供 UI 展示和调试。
    pub fn bytecode_text(&self) -> String {
        self.inner.bytecode_text()
    }

    /// 返回 bytes 体积分布文本，供压缩分析使用。
    pub fn bytes_profile_text(&self) -> String {
        self.inner.bytes_profile_text()
    }

    /// 返回紧凑 source map JSON。
    pub fn source_map(&self) -> String {
        self.inner.source_map()
    }

    /// 返回最终可执行 bytecode bytes。
    pub fn bytes(&self) -> Vec<u8> {
        self.inner.bytes()
    }
}

#[wasm_bindgen]
impl Compiler {
    #[wasm_bindgen(constructor)]
    /// 创建编译器并立即把源码降低到 IR。
    pub fn new(source: &str) -> Result<Compiler, JsValue> {
        Ok(Self {
            inner: compiler::Compiler::new(source)?,
        })
    }

    /// 返回编译器识别出的 extern slot 名称。
    ///
    /// 未在当前作用域声明、需要宿主环境提供的根名字会进入该列表。
    pub fn extern_slots(&self) -> Vec<String> {
        self.inner.extern_slots()
    }

    /// 返回 IR 文本。
    pub fn to_text(&self) -> String {
        self.inner.to_text()
    }

    /// 返回当前源码需要的 runtime feature 列表。
    pub fn runtime_features(&self) -> Vec<String> {
        self.inner.runtime_features()
    }

    /// 返回 runtime feature manifest JSON。
    ///
    /// `compact_errors` 会选择是否启用紧凑错误信息特性，从而影响 runtime 包名。
    pub fn runtime_feature_manifest(&self, compact_errors: bool) -> String {
        self.inner
            .runtime_feature_manifest(compact_errors)
            .to_json()
    }

    /// 返回 feature 的规范化字符串。
    pub fn runtime_feature_canonical(&self, compact_errors: bool) -> String {
        self.inner
            .runtime_feature_manifest(compact_errors)
            .canonical
    }

    /// 返回当前 feature 组合对应的 runtime 包名。
    pub fn runtime_feature_package(&self, compact_errors: bool) -> String {
        self.inner
            .runtime_feature_manifest(compact_errors)
            .package_name
    }

    /// 生成完整 bytecode artifact。
    ///
    /// `seed` 为空时使用默认编码；非空时先从 seed 恢复编码表。
    /// `extern_slots` 非空时会按指定顺序重排 extern operand。
    pub fn to_bytecode_artifact(
        &self,
        seed: Option<String>,
        extern_slots: Box<[JsValue]>,
    ) -> Result<CompilerArtifact, String> {
        Ok(CompilerArtifact {
            inner: self.inner.to_bytecode_artifact(seed, extern_slots)?,
        })
    }

    /// 单独生成 source map。
    ///
    /// 该接口用于调试器或下载包只需要 map 的场景；普通页面构建优先用 `to_bytecode_artifact`。
    pub fn source_map(
        &self,
        seed: Option<String>,
        extern_slots: Box<[JsValue]>,
        source_file: &str,
    ) -> Result<String, String> {
        self.inner.source_map(seed, extern_slots, source_file)
    }
}

#[wasm_bindgen]
/// 根据 UI 表格中的 opcode/operand/constant tag 行生成与 bytes 绑定的 seed。
pub fn js_encoding_seed_from_rows(
    opcode_names: Box<[JsValue]>,
    operand_tag_names: Box<[JsValue]>,
    constant_tag_names: Box<[JsValue]>,
    bytes: &[u8],
) -> Result<String, String> {
    let names = EncodingNames {
        opcodes: js_values_to_strings(&opcode_names),
        operand_tags: js_values_to_strings(&operand_tag_names),
        constant_tags: js_values_to_strings(&constant_tag_names),
    };
    let encoding = EncodingConfig::from_names(&names).map_err(|err| err.to_string())?;
    encoding.paired_seed(bytes).map_err(|err| err.to_string())
}

#[wasm_bindgen]
/// 根据已有 seed 和 bytes 重新生成配对 seed。
///
/// 用于 bytes 改变后同步 seed 指纹。
pub fn js_encoding_seed_for_seed_and_bytes(seed: &str, bytes: &[u8]) -> Result<String, String> {
    encoding_seed_for_seed_and_bytes(seed, bytes)
}

#[wasm_bindgen]
/// 从 seed 还原 UI 表格需要的名称行。
pub fn js_encoding_rows_from_seed(seed: &str) -> Result<Vec<String>, String> {
    encoding_names_from_seed(seed)
}

#[wasm_bindgen]
/// 分析 ES module 的 import/export 边界。
pub fn js_analyze_module_source(source: &str) -> Result<JsValue, String> {
    let analysis = analyze_module_source(source)?;
    module_analysis_to_js_value(&analysis)
}

#[wasm_bindgen]
/// 把一个模块源码包装成 VM wrapper + VM 内部源码。
///
/// Web 预览和 CLI 打包应共用该入口，保证模块语义、extern slots、bin 加载方式一致。
pub fn js_package_module_source(source: &str, options: JsValue) -> Result<JsValue, String> {
    let package = package_module_source(source, packaged_options_from_js(&options)?)?;
    packaged_module_to_js_value(&package)
}

fn js_values_to_strings(values: &[JsValue]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}

fn packaged_options_from_js(value: &JsValue) -> Result<PackagedModuleOptions, String> {
    Ok(PackagedModuleOptions {
        source_file: js_prop_string(value, "sourceFile", "example.js")?,
        wrapper_path: js_prop_string(value, "wrapperPath", "example.js")?,
        bin_path: js_prop_string(value, "binPath", "example.bin")?,
        seed: js_prop_string(value, "seed", "")?,
        env_specifier: js_prop_string(value, "envSpecifier", "./js_vm_env_browser.js")?,
        bin_specifier: js_prop_string(value, "binSpecifier", "./example.bin")?,
        import_rewrites: js_prop_import_rewrites(value, "importRewrites")?,
        extern_slots: js_prop_string_array(value, "externSlots")?,
        defer_execution: js_prop_bool(value, "deferExecution", false)?,
    })
}

fn js_prop(value: &JsValue, name: &str) -> Result<JsValue, String> {
    Reflect::get(value, &JsValue::from_str(name))
        .map_err(|err| format!("cannot read option {name}: {err:?}"))
}

fn js_prop_string(value: &JsValue, name: &str, default: &str) -> Result<String, String> {
    let prop = js_prop(value, name)?;
    if prop.is_null() || prop.is_undefined() {
        return Ok(default.to_string());
    }
    prop.as_string()
        .ok_or_else(|| format!("option {name} must be a string"))
}

fn js_prop_bool(value: &JsValue, name: &str, default: bool) -> Result<bool, String> {
    let prop = js_prop(value, name)?;
    if prop.is_null() || prop.is_undefined() {
        return Ok(default);
    }
    prop.as_bool()
        .ok_or_else(|| format!("option {name} must be a boolean"))
}

fn js_prop_string_array(value: &JsValue, name: &str) -> Result<Vec<String>, String> {
    let prop = js_prop(value, name)?;
    if prop.is_null() || prop.is_undefined() {
        return Ok(Vec::new());
    }
    if !Array::is_array(&prop) {
        return Err(format!("option {name} must be an array"));
    }
    let array = Array::from(&prop);
    let mut out = Vec::new();
    for item in array.iter() {
        out.push(
            item.as_string()
                .ok_or_else(|| format!("option {name} entries must be strings"))?,
        );
    }
    Ok(out)
}

fn js_prop_import_rewrites(
    value: &JsValue,
    name: &str,
) -> Result<Vec<ModuleImportRewrite>, String> {
    let prop = js_prop(value, name)?;
    if prop.is_null() || prop.is_undefined() {
        return Ok(Vec::new());
    }
    if !Array::is_array(&prop) {
        return Err(format!("option {name} must be an array"));
    }
    let array = Array::from(&prop);
    let mut out = Vec::new();
    for item in array.iter() {
        out.push(ModuleImportRewrite {
            specifier: js_prop_string(&item, "specifier", "")?,
            replacement: js_prop_string(&item, "replacement", "")?,
        });
    }
    Ok(out)
}

fn module_analysis_to_js_value(analysis: &ModuleSourceAnalysis) -> Result<JsValue, String> {
    let object = Object::new();
    set_js_prop(&object, "imports", imports_to_js_array(&analysis.imports)?)?;
    set_js_prop(
        &object,
        "dynamicImports",
        string_array(&analysis.dynamic_imports),
    )?;
    set_js_prop(&object, "exportNames", string_array(&analysis.export_names))?;
    set_js_prop(
        &object,
        "hasDefaultExport",
        JsValue::from_bool(analysis.has_default_export),
    )?;
    Ok(object.into())
}

fn packaged_module_to_js_value(package: &PackagedModuleSource) -> Result<JsValue, String> {
    let object = Object::new();
    set_js_prop(&object, "vmSource", JsValue::from_str(&package.vm_source))?;
    set_js_prop(
        &object,
        "wrapperSource",
        JsValue::from_str(&package.wrapper_source),
    )?;
    set_js_prop(&object, "imports", imports_to_js_array(&package.imports)?)?;
    set_js_prop(&object, "exportNames", string_array(&package.export_names))?;
    set_js_prop(
        &object,
        "hasDefaultExport",
        JsValue::from_bool(package.has_default_export),
    )?;
    set_js_prop(
        &object,
        "importedLocals",
        string_array(&package.imported_locals),
    )?;
    Ok(object.into())
}

fn imports_to_js_array(imports: &[ModuleImportInfo]) -> Result<JsValue, String> {
    let array = Array::new();
    for import in imports {
        let object = Object::new();
        set_js_prop(&object, "specifier", JsValue::from_str(&import.specifier))?;
        set_js_prop(&object, "locals", string_array(&import.locals))?;
        array.push(&object);
    }
    Ok(array.into())
}

fn string_array(values: &[String]) -> JsValue {
    let array = Array::new();
    for value in values {
        array.push(&JsValue::from_str(value));
    }
    array.into()
}

fn set_js_prop(object: &Object, name: &str, value: JsValue) -> Result<(), String> {
    Reflect::set(object, &JsValue::from_str(name), &value)
        .map(|_| ())
        .map_err(|err| format!("cannot set result {name}: {err:?}"))
}
