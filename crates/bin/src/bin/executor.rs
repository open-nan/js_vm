use std::{
    env, fs,
    io::{self, Read},
};

use js_token_bin::Executor;
use js_token_core::{BytecodeModule, EncodingConfig};

fn main() {
    let mut input_path = None;
    let mut encoding_path = None;
    let mut seed = None;
    let mut externals = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => {
                let Some(value) = args.next() else {
                    eprintln!("missing value for --seed; expected opcode seed");
                    std::process::exit(1);
                };
                seed = Some(value);
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

    let bytes = match input_path {
        Some(path) => fs::read(&path).unwrap_or_else(|err| {
            eprintln!("failed to read {path}: {err}");
            std::process::exit(1);
        }),
        None => {
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input).unwrap_or_else(|err| {
                eprintln!("failed to read stdin: {err}");
                std::process::exit(1);
            });
            input
        }
    };

    let module = if let Some(seed) = seed {
        BytecodeModule::from_bytes_with_seed(&bytes, &seed).unwrap_or_else(|err| {
            eprintln!("failed to decode bytecode with seed: {err}");
            std::process::exit(1);
        })
    } else {
        let encoding = encoding_path
            .map(|path| {
                let yaml = fs::read_to_string(&path).unwrap_or_else(|err| {
                    eprintln!("failed to read encoding yaml {path}: {err}");
                    std::process::exit(1);
                });
                EncodingConfig::from_yaml(&yaml).unwrap_or_else(|err| {
                    eprintln!("failed to parse encoding yaml {path}: {err}");
                    std::process::exit(1);
                })
            })
            .unwrap_or_default();

        BytecodeModule::from_bytes_with_encoding(&bytes, &encoding).unwrap_or_else(|err| {
            eprintln!("failed to decode bytecode: {err}");
            std::process::exit(1);
        })
    };

    let value = Executor::run_with_external_names(&module, &externals).unwrap_or_else(|err| {
        eprintln!("execution failed: {err}");
        std::process::exit(1);
    });
    println!("{value}");
}

fn print_usage() {
    eprintln!(
        "usage: js-executor [--seed seed | --encoding encoding.yaml] [--extern name] [input.bytecode]"
    );
}
