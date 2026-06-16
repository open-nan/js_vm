use std::{
    env, fs,
    io::{self, Read},
};

use js_token_bin::Executor;
use js_token_core::BytecodeModule;

fn main() {
    let mut input_path = None;
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

    let Some(seed) = seed else {
        eprintln!("missing --seed; bytecode execution requires a paired seed");
        std::process::exit(1);
    };
    let module = BytecodeModule::from_bytes_with_seed(&bytes, &seed).unwrap_or_else(|err| {
        eprintln!("failed to decode bytecode with seed: {err}");
        std::process::exit(1);
    });

    let value = Executor::run_with_external_names(&module, &externals).unwrap_or_else(|err| {
        eprintln!("execution failed: {err}");
        std::process::exit(1);
    });
    println!("{value}");
}

fn print_usage() {
    eprintln!("usage: js-executor --seed seed [--extern name] [input.bytecode]");
}
