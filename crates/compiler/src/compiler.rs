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

#[cfg(test)]
mod tests {
    use super::{compile_to_ir, remap_external_operands};
    use js_token_core::{BytecodeModule, EncodingConfig};
    use js_vm_runtime::Executor;

    const MULTIPLE_FUNCTIONS_FIB_ADD: &str =
        include_str!("../../../tests/regressions/multiple-functions-fib-add.test.js");

    fn run_source(source: &str) -> Result<String, String> {
        let ir = compile_to_ir(source)?;
        let module = ir.to_bytecode();
        let bytes = module.to_bytes();
        let seed = EncodingConfig::default()
            .paired_seed(&bytes)
            .map_err(|err| err.to_string())?;
        let module =
            BytecodeModule::from_bytes_with_seed(&bytes, &seed).map_err(|err| err.to_string())?;
        Executor::run(&module)
            .map(|value| value.to_string())
            .map_err(|err| err.to_string())
    }

    fn count_subslice(bytes: &[u8], needle: &[u8]) -> usize {
        bytes
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
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
        assert_eq!(count_subslice(&bytes, b"console"), 1);
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
    fn regression_multiple_functions_fib_add_executes() {
        assert_eq!(run_source(MULTIPLE_FUNCTIONS_FIB_ADD).as_deref(), Ok("21"));
    }

    #[test]
    fn class_methods_compile_and_execute() {
        let cases = [
            (
                "class A { value() { return 7; } } const a = new A(); a.value();",
                "7",
            ),
            ("class A { static value() { return 9; } } A.value();", "9"),
            (
                "class Box { constructor(v) { this.v = v; } value() { return this.v; } } const box = new Box(11); box.value();",
                "11",
            ),
            (
                "class Point { x = 3; y = 4; sum() { return this.x + this.y; } } const p = new Point(); p.sum();",
                "7",
            ),
        ];

        for (source, expected) in cases {
            assert_eq!(run_source(source).as_deref(), Ok(expected), "{source}");
        }
    }

    #[test]
    fn compiler_runtime_class_method_fuzz_cases() {
        let method_names = ["m", "value", "compute"];
        let values = [0, 1, 2, 7, 13];

        for (method_index, method) in method_names.iter().enumerate() {
            for value in values {
                let class_name = format!("C{method_index}_{value}");
                let source = format!(
                    "class {class_name} {{ {method}() {{ return {value}; }} }} const obj = new {class_name}(); obj.{method}();"
                );
                let expected = value.to_string();
                assert_eq!(
                    run_source(&source).as_deref(),
                    Ok(expected.as_str()),
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn compiler_runtime_expression_fuzz_cases() {
        let cases = [
            ("const a = [1, 2, 3]; a.length;", "3"),
            ("const o = { value() { return 5; } }; o.value();", "5"),
            ("function add(a, b) { return a + b; } add(2, 3);", "5"),
            ("let x = 1; x = x + 4; x;", "5"),
            ("const s = `a${1 + 2}`; s;", "a3"),
            ("try { throw 6; } catch (err) { err + 1; }", "7"),
        ];

        for (source, expected) in cases {
            assert_eq!(run_source(source).as_deref(), Ok(expected), "{source}");
        }
    }
}
