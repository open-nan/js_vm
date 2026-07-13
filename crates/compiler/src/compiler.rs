use crate::parse::{LoweringContext, parse_source};
use js_token_core::{BytecodeModule, BytecodeOperand, EncodingConfig, IrModule};
use std::collections::BTreeMap;
use swc_ecma_ast::*;
use wasm_bindgen::prelude::*;

pub struct Compiler {
    ir: IrModule,
}

impl Compiler {
    pub fn new(source: &str) -> Result<Compiler, JsValue> {
        let ir = compile_to_ir(source).map_err(|err| JsValue::from_str(&err))?;
        Ok(Compiler { ir })
    }

    pub fn extern_slots(&self) -> Vec<String> {
        self.ir.extern_slots.clone()
    }

    pub fn to_text(&self) -> String {
        self.ir.to_text()
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
            for stmt in &script.body {
                ctx.predeclare_stmt(stmt);
            }
            for stmt in &script.body {
                ctx.lower_stmt(stmt);
            }
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
    use super::{compile_to_ir, remap_external_operands};
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
}
