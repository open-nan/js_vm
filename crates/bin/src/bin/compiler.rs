use std::{
    env, fs,
    io::{self, Read},
};

use js_token_bin::{CompileOptions, compile_source_with_options};

enum EmitKind {
    Ir,
    Bytecode,
    Bytes,
}

fn main() {
    let mut emit = EmitKind::Bytecode;
    let mut input_path = None;
    let mut encoding_seed = None;
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
            "--seed" => {
                let Some(seed) = args.next() else {
                    eprintln!("missing value for --seed; expected encoding seed");
                    std::process::exit(1);
                };
                encoding_seed = Some(seed);
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

    let result = compile_source_with_options(
        &source,
        &CompileOptions {
            externals,
            encoding_seed,
            extern_slots: None,
        },
    )
    .map(|output| match emit {
        EmitKind::Ir => output.ir_text().into_bytes(),
        EmitKind::Bytecode => output.bytecode_text().into_bytes(),
        EmitKind::Bytes => output.bytes,
    });

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
        "usage: js-compiler [--emit ir|bytecode|bytes] [--seed encoding-seed] [--extern name] [input.js]"
    );
}
