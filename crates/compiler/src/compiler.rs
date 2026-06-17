use js_token_core::{
    BytecodeModule, EncodingConfig, IrModule,
};
use wasm_bindgen::prelude::*;
use swc_ecma_ast::*;
use crate::parse::{LoweringContext, parse_source};


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
        let bytes = match seed.as_deref() {
            Some(seed) if !seed.is_empty() => {
                let encoding = EncodingConfig::from_seed(seed).map_err(|err| err.to_string())?;
                module
                    .to_bytes_with_encoding(&encoding)
                    .map_err(|err| err.to_string())?
            }
            _ => module.to_bytes(),
        };
        Ok(CompilerArtifact {
            bytecode_text: module.to_text(),
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
        module.extern_slots = extern_slots;
        Ok(module)
    }
}

pub struct CompilerArtifact {
    bytecode_text: String,
    bytes: Vec<u8>,
}

impl CompilerArtifact {
    pub fn bytecode_text(&self) -> String {
        self.bytecode_text.clone()
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
    match program {
        Program::Module(module) => ctx.lower_module(&module),
        Program::Script(script) => {
            for stmt in &script.body {
                ctx.predeclare_stmt(stmt);
            }
            for stmt in &script.body {
                ctx.lower_stmt(stmt);
            }
        }
    }

    Ok(ctx.into_module())
}

fn js_values_to_strings(values: &[JsValue]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}
