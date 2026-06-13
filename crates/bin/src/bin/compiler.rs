use std::{
    env, fs,
    io::{self, Read},
};

use js_token_bin::{
    compile_to_bytecode_bytes, compile_to_bytecode_bytes_with_encoding_yaml,
    compile_to_bytecode_text, compile_to_bytecode_with_externals, compile_to_ir_text,
    compile_to_ir_with_externals,
};

enum EmitKind {
    Ir,
    Bytecode,
    Bytes,
}

fn main() {
    let mut emit = EmitKind::Bytecode;
    let mut input_path = None;
    let mut encoding_path = None;
    let mut externals = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--emit" => {
                let Some(kind) = args.next() else {
                    eprintln!("missing value for --emit; expected ir, bytecode, or bytes");
                    std::process::exit(1);
                };
                emit = match kind.as_str() {
                    "ir" => EmitKind::Ir,
                    "bytecode" => EmitKind::Bytecode,
                    "bytes" => EmitKind::Bytes,
                    _ => {
                        eprintln!("unknown emit kind {kind:?}; expected ir, bytecode, or bytes");
                        std::process::exit(1);
                    }
                };
            }
            "--encoding" => {
                let Some(path) = args.next() else {
                    eprintln!("missing value for --encoding; expected encoding yaml path");
                    std::process::exit(1);
                };
                encoding_path = Some(path);
            }
            "--extern" => {
                let Some(name) = args.next() else {
                    eprintln!("missing value for --extern; expected external global name");
                    std::process::exit(1);
                };
                externals.push(name);
            }
            "--help" | "-h" => {
                print_usage();
                return;
            }
            path => input_path = Some(path.to_string()),
        }
    }

    let source = match input_path {
        Some(path) => fs::read_to_string(&path).unwrap_or_else(|err| {
            eprintln!("failed to read {path}: {err}");
            std::process::exit(1);
        }),
        None => {
            let mut source = String::new();
            io::stdin()
                .read_to_string(&mut source)
                .unwrap_or_else(|err| {
                    eprintln!("failed to read stdin: {err}");
                    std::process::exit(1);
                });
            source
        }
    };

    let encoding_yaml = encoding_path.map(|path| {
        fs::read_to_string(&path).unwrap_or_else(|err| {
            eprintln!("failed to read encoding yaml {path}: {err}");
            std::process::exit(1);
        })
    });

    let result = match emit {
        EmitKind::Ir if externals.is_empty() => {
            compile_to_ir_text(&source).map(|text| text.into_bytes())
        }
        EmitKind::Ir => compile_to_ir_with_externals(&source, &externals)
            .map(|module| module.to_text().into_bytes()),
        EmitKind::Bytecode if externals.is_empty() => {
            compile_to_bytecode_text(&source).map(|text| text.into_bytes())
        }
        EmitKind::Bytecode => compile_to_bytecode_with_externals(&source, &externals)
            .map(|module| module.to_text().into_bytes()),
        EmitKind::Bytes => match encoding_yaml.as_deref() {
            Some(yaml) if externals.is_empty() => {
                compile_to_bytecode_bytes_with_encoding_yaml(&source, yaml)
            }
            Some(yaml) => {
                let encoding =
                    js_token_core::EncodingConfig::from_yaml(yaml).map_err(|err| err.to_string());
                encoding.and_then(|encoding| {
                    compile_to_bytecode_with_externals(&source, &externals).and_then(|module| {
                        module
                            .to_bytes_with_encoding(&encoding)
                            .map_err(|err| err.to_string())
                    })
                })
            }
            None if externals.is_empty() => compile_to_bytecode_bytes(&source),
            None => compile_to_bytecode_with_externals(&source, &externals)
                .map(|module| module.to_bytes()),
        },
    };

    match result {
        Ok(output) => {
            use std::io::Write;
            io::stdout().write_all(&output).unwrap_or_else(|err| {
                eprintln!("failed to write stdout: {err}");
                std::process::exit(1);
            });
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: js-compiler [--emit ir|bytecode|bytes] [--encoding encoding.yaml] [--extern name] [input.js]"
    );
}
