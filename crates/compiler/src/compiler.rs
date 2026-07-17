use crate::parse::{LoweringContext, parse_source};
use js_token_core::{
    BytecodeModule, BytecodeOperand, EncodingConfig, IrConst, IrModule, IrModuleKind,
};
use std::collections::{BTreeMap, BTreeSet};
use swc_ecma_ast::*;
use wasm_bindgen::prelude::*;

pub struct Compiler {
    ir: IrModule,
    source: String,
}

impl Compiler {
    pub fn new(source: &str) -> Result<Compiler, JsValue> {
        let ir = compile_to_ir(source).map_err(|err| JsValue::from_str(&err))?;
        Ok(Compiler {
            ir,
            source: source.to_string(),
        })
    }

    pub fn extern_slots(&self) -> Vec<String> {
        self.ir.extern_slots.clone()
    }

    pub fn to_text(&self) -> String {
        self.ir.to_text()
    }

    pub fn runtime_features(&self) -> Vec<String> {
        runtime_features_for_source_and_ir(&self.source, &self.ir)
    }

    pub fn runtime_feature_manifest(&self, compact_errors: bool) -> RuntimeFeatureManifest {
        RuntimeFeatureManifest::from_features(self.runtime_features(), compact_errors)
    }

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
        Ok(CompilerArtifact {
            bytecode_text: module.to_text(),
            bytes_profile_text,
            bytes,
        })
    }

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

pub struct CompilerArtifact {
    bytecode_text: String,
    bytes_profile_text: String,
    bytes: Vec<u8>,
}

impl CompilerArtifact {
    pub fn bytecode_text(&self) -> String {
        self.bytecode_text.clone()
    }

    pub fn bytes_profile_text(&self) -> String {
        self.bytes_profile_text.clone()
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

pub struct NativeCompilerArtifact {
    pub ir_text: String,
    pub bytecode_text: String,
    pub bytes_profile_text: String,
    pub bytes: Vec<u8>,
    pub extern_slots: Vec<String>,
    pub runtime_features: Vec<String>,
    pub runtime_feature_canonical: String,
    pub runtime_feature_md5: String,
    pub runtime_feature_package: String,
}

pub fn compile_source_to_artifact(
    source: &str,
    seed: Option<&str>,
    extern_slots: &[String],
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
        bytes,
        extern_slots: final_extern_slots,
        runtime_features: runtime_manifest.features,
        runtime_feature_canonical: runtime_manifest.canonical,
        runtime_feature_md5: runtime_manifest.md5,
        runtime_feature_package: runtime_manifest.package_name,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFeatureManifest {
    pub features: Vec<String>,
    pub canonical: String,
    pub md5: String,
    pub package_name: String,
}

impl RuntimeFeatureManifest {
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

fn compile_to_ir(source: &str) -> Result<IrModule, String> {
    compile_to_ir_with_externals(source, &[])
}

fn compile_to_ir_with_externals(source: &str, externals: &[String]) -> Result<IrModule, String> {
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
        RuntimeFeatureManifest, compile_to_ir, md5_hex_12, remap_external_operands,
        runtime_features_for_source_and_ir,
    };
    use js_token_core::IrConst;

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
}
