mod compiler;
mod parse;

use compiler::{encoding_names_from_seed, encoding_seed_for_seed_and_bytes};
use js_token_core::{EncodingConfig, EncodingNames};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Compiler {
    inner: compiler::Compiler,
}

#[wasm_bindgen]
pub struct CompilerArtifact {
    inner: compiler::CompilerArtifact,
}

#[wasm_bindgen]
impl CompilerArtifact {
    pub fn bytecode_text(&self) -> String {
        self.inner.bytecode_text()
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.inner.bytes()
    }
}

#[wasm_bindgen]
impl Compiler {
    #[wasm_bindgen(constructor)]
    pub fn new(source: &str) -> Result<Compiler, JsValue> {
        Ok(Self {
            inner: compiler::Compiler::new(source)?,
        })
    }

    pub fn extern_slots(&self) -> Vec<String> {
        self.inner.extern_slots()
    }

    pub fn to_text(&self) -> String {
        self.inner.to_text()
    }

    pub fn to_bytecode_artifact(
        &self,
        seed: Option<String>,
        extern_slots: Box<[JsValue]>,
    ) -> Result<CompilerArtifact, String> {
        Ok(CompilerArtifact {
            inner: self.inner.to_bytecode_artifact(seed, extern_slots)?,
        })
    }
}

#[wasm_bindgen]
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
pub fn js_encoding_seed_for_seed_and_bytes(seed: &str, bytes: &[u8]) -> Result<String, String> {
    encoding_seed_for_seed_and_bytes(seed, bytes)
}

#[wasm_bindgen]
pub fn js_encoding_rows_from_seed(seed: &str) -> Result<Vec<String>, String> {
    encoding_names_from_seed(seed)
}

fn js_values_to_strings(values: &[JsValue]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}
