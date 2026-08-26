//! JS VM 命令行工具。
//!
//! CLI 负责多端构建和站点/目录 VM 化打包：
//! - `wasm`：构建 compiler、browser runtime、node runtime wasm 包。
//! - `package`：读取目录下 JS/TS/HTML，输出 wrapper `.js`、bytecode `.bin`、source map 和运行时包。
//! - `dump-bytecode`：按 seed 反解 `.bin`，辅助定位运行时 pc 错误。
//!
//! 语义拆分原则：CLI 只处理文件系统、路径、平台和复制；单文件 import/export 拆解、
//! wrapper 生成和 bytecode 生成都委托给 compiler crate，保证 Web Preview 与 CLI 行为一致。

use js_token_core::{BytecodeModule, EncodingConfig};
use js_vm_compiler::{
    ModuleImportRewrite, PackagedModuleOptions, analyze_module_source, check_source_syntax,
    compile_source_to_artifact_with_source_file, package_module_source,
};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const RUNTIME_WASM_STACK_SIZE: usize = 64 * 1024 * 1024;

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "pkg",
    "dist",
    "vendor",
    "js-vm-runtime",
    ".issues",
    ".tmp",
    ".vendor",
];
const GENERATED_RUNTIME_DIR: &str = "js-vm-runtime";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 打包目标平台。
enum Platform {
    /// 浏览器/H5 环境。
    Web,
    /// Node 环境。
    Node,
}

/// `package` 命令选项。
#[derive(Debug)]
struct PackageOptions {
    /// 输入目录。
    input: PathBuf,
    /// 输出目录。
    output: PathBuf,
    /// 可选入口文件；为空时从 HTML module script 或常见入口名推导。
    entry: Option<String>,
    /// 是否先清理输出目录。
    clean: bool,
    /// 是否复制非 JS 静态资源。
    copy_static: bool,
    /// 输出平台。
    platforms: Vec<Platform>,
    /// runtime 最大调用深度。
    max_call_depth: u32,
    /// runtime 同函数递归深度。
    max_recursive_call_depth: u32,
    /// runtime 最大执行步数。
    max_execution_steps: u32,
}

/// `wasm` 命令选项。
#[derive(Debug)]
struct WasmOptions {
    /// wasm-bindgen target，如 web/nodejs。
    target: String,
    /// 是否使用 release 构建。
    release: bool,
    /// 是否跳过 wasm-opt 压缩。
    skip_opt: bool,
}

#[derive(Debug)]
struct CheckOptions {
    /// 待检查目录。
    input: PathBuf,
}

/// 单个模块输出统计。
#[derive(Debug)]
struct ModuleOutput {
    /// 原始虚拟文件路径。
    file: String,
    /// 输出 wrapper JS 路径。
    source_path: String,
    /// 原始源码。
    source: String,
    /// 输出 bin 路径。
    bin: String,
    /// 与 bytecode 绑定的 seed。
    seed: String,
    /// runtime env import 路径。
    env_specifier: String,
    /// 带 cache version 的 bin import 路径。
    bin_specifier: String,
    /// 本地 import 重写规则。
    import_rewrites: Vec<ModuleImportRewrite>,
    /// extern slot 顺序。
    extern_slots: Vec<String>,
    /// 是否延迟执行。
    defer_execution: bool,
    /// `.bin` 字节数。
    bytes: usize,
    /// `.bin.map` 字节数。
    source_map_bytes: usize,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("ERROR {err}");
        std::process::exit(1);
    }
}

/// 解析一级命令并分发。
fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let command = if args.first().is_some_and(|arg| !arg.starts_with('-')) {
        args.remove(0)
    } else {
        "help".to_string()
    };
    match command.as_str() {
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        "wasm" => build_wasm(parse_wasm_options(&args)?),
        "package" => compile_runtime_package(parse_package_options(&args)?),
        "check" => check_sources(parse_check_options(&args)?),
        "dump-bytecode" => dump_bytecode(&args),
        "all" => {
            let split = args.iter().position(|arg| arg == "--");
            let (wasm_args, package_args) = match split {
                Some(index) => (&args[..index], &args[index + 1..]),
                None => (&[][..], &args[..]),
            };
            build_wasm(parse_wasm_options(wasm_args)?)?;
            compile_runtime_package(parse_package_options(package_args)?)
        }
        other => Err(format!("unknown command: {other}")),
    }
}

fn print_help() {
    println!(
        "{}",
        [
            "Usage:",
            "  js-vm wasm [--target web|bundler|nodejs] [--dev] [--skip-opt]",
            "  js-vm package <folder> [--out <dir>] [--entry <file>] [--platform web|node|all] [--clean]",
            "  js-vm check <folder>",
            "  js-vm dump-bytecode <file.bin> --seed <seed> [--around <pc>]",
            "  js-vm all [wasm options] -- <folder> [package options]",
            "",
            "Package options:",
            "  --out <dir>          Output runtime package directory. Default: <folder>/js-vm-runtime",
            "  --entry <file>       Entry js/ts file relative to folder. Default: index/main lookup",
            "  --platform <value>   web/browser/h5,node,nodejs,all. Default: web",
            "  --clean              Remove output directory before writing",
            "  --no-static          Do not copy non-JS static files from the input site",
            "  --no-native-framework  Deprecated no-op; all JS/TS files are VM-compiled",
            "  --depth <n>          Runtime max call depth. Default: 2048",
            "  --recursion <n>      Runtime max recursive call depth. Default: 128",
            "  --steps <n>          Runtime max execution steps. 0 disables the step budget. Default: 0",
        ]
        .join("\n")
    );
}

/// 反解 bytecode 文本。
///
/// 运行时错误通常只给出 pc，`dump-bytecode --around <pc>` 可以快速查看附近指令。
fn dump_bytecode(args: &[String]) -> Result<(), String> {
    let mut file = None;
    let mut seed = None;
    let mut around = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--seed" => {
                index += 1;
                seed = Some(read_arg(args, index, "--seed")?.to_string());
            }
            value if value.starts_with("--seed=") => {
                seed = Some(value["--seed=".len()..].to_string());
            }
            "--around" => {
                index += 1;
                around = Some(
                    read_arg(args, index, "--around")?
                        .parse::<usize>()
                        .map_err(|_| {
                            "--around must be a non-negative instruction index".to_string()
                        })?,
                );
            }
            value if value.starts_with("--around=") => {
                around = Some(value["--around=".len()..].parse::<usize>().map_err(|_| {
                    "--around must be a non-negative instruction index".to_string()
                })?);
            }
            value if value.starts_with('-') => return Err(format!("unknown dump option: {value}")),
            value => {
                if file.is_some() {
                    return Err("expected one bytecode file".to_string());
                }
                file = Some(PathBuf::from(value));
            }
        }
        index += 1;
    }
    let file = file.ok_or_else(|| "missing bytecode file".to_string())?;
    let seed = seed.ok_or_else(|| "missing --seed".to_string())?;
    let bytes = fs::read(&file).map_err(|err| format!("{}: {err}", file.display()))?;
    let module = BytecodeModule::from_bytes_with_seed(&bytes, &seed)
        .map_err(|err| format!("decode bytecode: {err}"))?;
    let text = module.to_text();
    if let Some(pc) = around {
        let start = pc.saturating_sub(24);
        let end = pc.saturating_add(24);
        for line in text.lines() {
            if let Some(line_pc) = bytecode_text_pc(line) {
                if (start..=end).contains(&line_pc) {
                    println!("{line}");
                }
            } else if line.starts_with('.') {
                println!("{line}");
            }
        }
    } else {
        print!("{text}");
    }
    Ok(())
}

fn bytecode_text_pc(line: &str) -> Option<usize> {
    let (pc, _) = line.split_once(' ')?;
    pc.parse().ok()
}

fn parse_wasm_options(args: &[String]) -> Result<WasmOptions, String> {
    let mut options = WasmOptions {
        target: "web".to_string(),
        release: true,
        skip_opt: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--target" => {
                index += 1;
                options.target = read_arg(args, index, "--target")?.to_string();
            }
            value if value.starts_with("--target=") => {
                options.target = value["--target=".len()..].to_string();
            }
            "--dev" => options.release = false,
            "--release" => options.release = true,
            "--skip-opt" => options.skip_opt = true,
            other => return Err(format!("unknown wasm option: {other}")),
        }
        index += 1;
    }
    Ok(options)
}

fn parse_package_options(args: &[String]) -> Result<PackageOptions, String> {
    let mut input = None;
    let mut output = None;
    let mut entry = None;
    let mut clean = false;
    let mut copy_static = true;
    let mut platforms = vec![Platform::Web];
    let mut max_call_depth = 2048;
    let mut max_recursive_call_depth = 128;
    let mut max_execution_steps = 0;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--out" => {
                index += 1;
                output = Some(PathBuf::from(read_arg(args, index, "--out")?));
            }
            value if value.starts_with("--out=") => {
                output = Some(PathBuf::from(&value["--out=".len()..]));
            }
            "--entry" => {
                index += 1;
                entry = Some(normalize_virtual_path(read_arg(args, index, "--entry")?));
            }
            value if value.starts_with("--entry=") => {
                entry = Some(normalize_virtual_path(&value["--entry=".len()..]));
            }
            "--platform" => {
                index += 1;
                platforms = parse_platforms(read_arg(args, index, "--platform")?)?;
            }
            value if value.starts_with("--platform=") => {
                platforms = parse_platforms(&value["--platform=".len()..])?;
            }
            "--clean" => clean = true,
            "--no-static" => copy_static = false,
            "--no-native-framework" => {}
            "--depth" => {
                index += 1;
                max_call_depth = read_positive_u32(read_arg(args, index, "--depth")?, "--depth")?;
            }
            value if value.starts_with("--depth=") => {
                max_call_depth = read_positive_u32(&value["--depth=".len()..], "--depth")?;
            }
            "--recursion" => {
                index += 1;
                max_recursive_call_depth =
                    read_positive_u32(read_arg(args, index, "--recursion")?, "--recursion")?;
            }
            value if value.starts_with("--recursion=") => {
                max_recursive_call_depth =
                    read_positive_u32(&value["--recursion=".len()..], "--recursion")?;
            }
            "--steps" => {
                index += 1;
                max_execution_steps =
                    read_non_negative_u32(read_arg(args, index, "--steps")?, "--steps")?;
            }
            value if value.starts_with("--steps=") => {
                max_execution_steps = read_non_negative_u32(&value["--steps=".len()..], "--steps")?;
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown package option: {value}"));
            }
            value => {
                if input.is_some() {
                    return Err("expected one input folder".to_string());
                }
                input = Some(PathBuf::from(value));
            }
        }
        index += 1;
    }
    let input = input.ok_or_else(|| "missing input folder".to_string())?;
    let input = absolute_path(&input).map_err(|err| format!("invalid input folder: {err}"))?;
    let output = output
        .map(|path| absolute_path(&path))
        .transpose()
        .map_err(|err| format!("invalid output folder: {err}"))?
        .unwrap_or_else(|| input.join("js-vm-runtime"));
    Ok(PackageOptions {
        input,
        output,
        entry,
        clean,
        copy_static,
        platforms,
        max_call_depth,
        max_recursive_call_depth,
        max_execution_steps,
    })
}

fn read_arg<'a>(args: &'a [String], index: usize, name: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("{name} requires a value"))
}

fn read_positive_u32(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{name} must be a positive integer"))
}

fn read_non_negative_u32(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("{name} must be a non-negative integer"))
}

fn parse_platforms(value: &str) -> Result<Vec<Platform>, String> {
    let mut platforms = Vec::new();
    for part in value.split(',') {
        match part.trim() {
            "web" | "browser" | "h5" => platforms.push(Platform::Web),
            "node" | "nodejs" => platforms.push(Platform::Node),
            "all" => {
                platforms.push(Platform::Web);
                platforms.push(Platform::Node);
            }
            "" => {}
            other => return Err(format!("unknown platform: {other}")),
        }
    }
    platforms.sort_by_key(|platform| match platform {
        Platform::Web => 0,
        Platform::Node => 1,
    });
    platforms.dedup();
    if platforms.is_empty() {
        return Err("platform cannot be empty".to_string());
    }
    Ok(platforms)
}

fn parse_check_options(args: &[String]) -> Result<CheckOptions, String> {
    let mut input = None;
    for value in args {
        if value.starts_with('-') {
            return Err(format!("unknown check option: {value}"));
        }
        if input.is_some() {
            return Err("expected one input folder".to_string());
        }
        input = Some(PathBuf::from(value));
    }
    let input = input.unwrap_or_else(|| PathBuf::from("."));
    let input = absolute_path(&input).map_err(|err| format!("invalid input folder: {err}"))?;
    Ok(CheckOptions { input })
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

/// 构建 wasm 包。
///
/// 当前会同时输出 compiler、browser executor、node executor，并在可用时运行 wasm-opt。
fn build_wasm(options: WasmOptions) -> Result<(), String> {
    let root = workspace_root()?;
    let started = Instant::now();
    println!("STEP Building wasm packages target={}", options.target);
    remove_dir_if_exists(&root.join("pkg/compiler"))?;
    remove_dir_if_exists(&root.join("pkg/executor"))?;
    remove_dir_if_exists(&root.join("pkg/executor-node"))?;
    let mut common = vec!["build".to_string()];
    if options.release {
        common.push("--release".to_string());
    } else {
        common.push("--dev".to_string());
    }
    run_command(
        &root,
        "wasm-pack",
        common
            .iter()
            .chain([
                &"crates/compiler".to_string(),
                &"--target".to_string(),
                &options.target,
                &"--out-dir".to_string(),
                &"../../pkg/compiler".to_string(),
            ])
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )?;
    let runtime_rustflags = runtime_wasm_rustflags();
    run_command_with_env(
        &root,
        "wasm-pack",
        common
            .iter()
            .chain([
                &"crates/runtime/bin/browser".to_string(),
                &"--target".to_string(),
                &options.target,
                &"--out-dir".to_string(),
                &"../../../../pkg/executor".to_string(),
                &"--no-default-features".to_string(),
                &"--features".to_string(),
                &"full".to_string(),
            ])
            .map(String::as_str)
            .collect::<Vec<_>>(),
        &[("RUSTFLAGS", runtime_rustflags.as_str())],
    )?;
    run_command_with_env(
        &root,
        "wasm-pack",
        common
            .iter()
            .chain([
                &"crates/runtime/bin/node".to_string(),
                &"--target".to_string(),
                &"nodejs".to_string(),
                &"--out-dir".to_string(),
                &"../../../../pkg/executor-node".to_string(),
                &"--no-default-features".to_string(),
                &"--features".to_string(),
                &"full".to_string(),
            ])
            .map(String::as_str)
            .collect::<Vec<_>>(),
        &[("RUSTFLAGS", runtime_rustflags.as_str())],
    )?;
    if !options.skip_opt {
        if let Some(wasm_opt) = find_command("wasm-opt") {
            optimize_wasm(&root, &wasm_opt, "pkg/compiler/js_vm_compiler_bg.wasm")?;
            optimize_wasm(&root, &wasm_opt, "pkg/executor/js_vm_runtime_bg.wasm")?;
            optimize_wasm(
                &root,
                &wasm_opt,
                "pkg/executor-node/js_vm_runtime_node_bg.wasm",
            )?;
        } else {
            println!("WARN wasm-opt not found; wasm output was built but not post-optimized");
        }
    }
    patch_wasm_bindgen_js(&root.join("pkg/compiler/js_vm_compiler.js"))?;
    patch_wasm_bindgen_js(&root.join("pkg/executor/js_vm_runtime.js"))?;
    patch_wasm_bindgen_js(&root.join("pkg/executor-node/js_vm_runtime_node.js"))?;
    print_size(&root, "pkg/compiler/js_vm_compiler_bg.wasm")?;
    print_size(&root, "pkg/compiler/js_vm_compiler.js")?;
    print_size(&root, "pkg/executor/js_vm_runtime_bg.wasm")?;
    print_size(&root, "pkg/executor/js_vm_runtime.js")?;
    print_size(&root, "pkg/executor-node/js_vm_runtime_node_bg.wasm")?;
    print_size(&root, "pkg/executor-node/js_vm_runtime_node.js")?;
    println!(
        "OK Wasm build completed in {}ms",
        started.elapsed().as_millis()
    );
    Ok(())
}

/// 编译一个目录为 VM 运行时包。
///
/// 输出结构保留原有静态资源和 HTML，JS/TS 文件会变成 wrapper `.js` + `.bin` + `.bin.map`。
/// wrapper 负责加载 runtime/env/bin，并按模块依赖顺序执行 VM。
fn compile_runtime_package(options: PackageOptions) -> Result<(), String> {
    // CLI 只负责目录级 orchestration：找入口、复制静态资源、写 wrapper/bin/map。
    // 单文件的 import/export 拆解和 wrapper 生成全部交给 compiler crate，
    // 这样 Web Preview、下载包和命令行打包不会出现两套语义。
    if !options.input.is_dir() {
        return Err(format!(
            "input folder does not exist: {}",
            options.input.display()
        ));
    }
    let root = workspace_root()?;
    ensure_runtime_exists(&root, &options.platforms)?;
    let files = list_source_files(&options.input, &options.output)?;
    if files.is_empty() {
        return Err(format!(
            "no .js or .ts files found under {}",
            options.input.display()
        ));
    }
    let entry = choose_entry(&options.input, &files, options.entry.as_deref())?;
    let sources = read_sources(&options.input, &files)?;
    let order = runtime_execution_order(&entry, &sources);
    let static_targets = local_static_import_targets(&sources);
    let dynamic_targets = local_dynamic_import_targets(&sources);
    if options.clean {
        remove_dir_if_exists(&options.output)?;
    }
    fs::create_dir_all(&options.output).map_err(|err| err.to_string())?;
    copy_runtime(&root, &options.output, &options.platforms)?;

    let mut modules = Vec::new();
    let mut script_versions = BTreeMap::new();
    let base_seed = EncodingConfig::default()
        .paired_seed(&[])
        .map_err(|err| err.to_string())?;
    for file in &order {
        // 第一次包装/编译使用空 seed 产出 bytes；拿到 bytes 后再生成与内容绑定的 seed，
        // 第二次包装把最终 seed 和 extern slot 写进 wrapper，保证运行时先校验再执行。
        let source = sources
            .get(file)
            .ok_or_else(|| format!("missing source: {file}"))?;
        let source_path = module_js_path(file);
        let bin = module_bin_path(file);
        let import_rewrites = module_import_rewrites(file, &sources);
        let defer_execution =
            dynamic_targets.contains(file) && !static_targets.contains(file) && file != &entry;
        let env_file = match options.platforms.first().copied().unwrap_or(Platform::Web) {
            Platform::Web => "js_vm_env_browser.js",
            Platform::Node => "js_vm_env_node.js",
        };
        let env_specifier = relative_import_specifier(&source_path, env_file);
        let bin_specifier = relative_import_specifier(&source_path, &bin);
        let prepared = package_module_source(
            source,
            PackagedModuleOptions {
                source_file: file.clone(),
                wrapper_path: source_path.clone(),
                bin_path: bin.clone(),
                seed: String::new(),
                env_specifier: env_specifier.clone(),
                bin_specifier: bin_specifier.clone(),
                import_rewrites: import_rewrites.clone(),
                extern_slots: Vec::new(),
                defer_execution,
            },
        )
        .map_err(|err| format!("{file}: {err}"))?;
        let artifact = compile_source_to_artifact_with_source_file(
            &prepared.vm_source,
            Some(&base_seed),
            &[],
            file,
        )
        .map_err(|err| format!("{file}: {err}"))?;
        let seed = EncodingConfig::default()
            .paired_seed(&artifact.bytes)
            .map_err(|err| err.to_string())?;
        let bin_cache_version = seed_cache_version(&seed);
        let versioned_bin_specifier = if bin_cache_version.is_empty() {
            bin_specifier
        } else {
            format!("{bin_specifier}?v={bin_cache_version}")
        };
        let packaged_for_version = package_module_source(
            source,
            PackagedModuleOptions {
                source_file: file.clone(),
                wrapper_path: source_path.clone(),
                bin_path: bin.clone(),
                seed: seed.clone(),
                env_specifier: env_specifier.clone(),
                bin_specifier: versioned_bin_specifier.clone(),
                import_rewrites: import_rewrites.clone(),
                extern_slots: artifact.extern_slots.clone(),
                defer_execution,
            },
        )
        .map_err(|err| format!("{file}: {err}"))?;
        write_output(&options.output, &bin, &artifact.bytes)?;
        let source_map_path = format!("{bin}.map");
        write_output(
            &options.output,
            &source_map_path,
            artifact.source_map.as_bytes(),
        )?;
        script_versions.insert(
            source_path.clone(),
            content_cache_version(packaged_for_version.wrapper_source.as_bytes()),
        );
        modules.push(ModuleOutput {
            file: file.clone(),
            source_path,
            source: source.clone(),
            bin,
            seed,
            env_specifier,
            bin_specifier: versioned_bin_specifier,
            import_rewrites,
            extern_slots: artifact.extern_slots.clone(),
            defer_execution,
            bytes: artifact.bytes.len(),
            source_map_bytes: artifact.source_map.len(),
        });
    }

    for module in &modules {
        let versioned_rewrites = version_module_import_rewrites(
            &module.source_path,
            &module.import_rewrites,
            &script_versions,
        );
        let packaged = package_module_source(
            &module.source,
            PackagedModuleOptions {
                source_file: module.file.clone(),
                wrapper_path: module.source_path.clone(),
                bin_path: module.bin.clone(),
                seed: module.seed.clone(),
                env_specifier: module.env_specifier.clone(),
                bin_specifier: module.bin_specifier.clone(),
                import_rewrites: versioned_rewrites,
                extern_slots: module.extern_slots.clone(),
                defer_execution: module.defer_execution,
            },
        )
        .map_err(|err| format!("{}: {err}", module.file))?;
        write_output(
            &options.output,
            &module.source_path,
            packaged.wrapper_source.as_bytes(),
        )?;
    }

    let static_files = if options.copy_static {
        copy_static_site(
            &options.input,
            &options.output,
            &options.platforms,
            &script_versions,
        )?
    } else {
        0
    };

    for platform in &options.platforms {
        match platform {
            Platform::Web => write_output(
                &options.output,
                "js_vm_env_browser.js",
                browser_env_code(&options).as_bytes(),
            )?,
            Platform::Node => write_output(
                &options.output,
                "js_vm_env_node.js",
                node_env_code(&options).as_bytes(),
            )?,
        }
    }

    let total_bytes = modules.iter().map(|module| module.bytes).sum::<usize>();
    let total_source_map_bytes = modules
        .iter()
        .map(|module| module.source_map_bytes)
        .sum::<usize>();
    println!(
        "Compiled {} module(s) from {}",
        modules.len(),
        options.input.display()
    );
    println!("Entry: {entry}");
    println!("Output: {}", options.output.display());
    println!("Platforms: {}", platform_names(&options.platforms));
    if options.copy_static {
        println!("Static files: {static_files}");
    }
    println!("Bytecode: {total_bytes} bytes");
    println!("Source maps: {total_source_map_bytes} bytes");
    Ok(())
}

fn check_sources(options: CheckOptions) -> Result<(), String> {
    if !options.input.is_dir() {
        return Err(format!(
            "check input folder does not exist: {}",
            options.input.display()
        ));
    }
    let started = Instant::now();
    let mut files = Vec::new();
    walk_check_files(&options.input, &options.input, &mut files)?;
    files.sort();

    let mut stats = BTreeMap::<&'static str, usize>::new();
    let mut errors = Vec::new();
    for file in &files {
        match check_one_file(&options.input, file) {
            Ok(kind) => {
                *stats.entry(kind).or_insert(0) += 1;
            }
            Err(err) => errors.push(err),
        }
    }

    if !errors.is_empty() {
        return Err(format!(
            "syntax check failed for {} file(s):\n{}",
            errors.len(),
            errors.join("\n")
        ));
    }

    println!(
        "OK Checked {} file(s) in {}ms: js/ts={}, html={}, css={}, md={}",
        files.len(),
        started.elapsed().as_millis(),
        stats.get("js/ts").copied().unwrap_or(0),
        stats.get("html").copied().unwrap_or(0),
        stats.get("css").copied().unwrap_or(0),
        stats.get("md").copied().unwrap_or(0),
    );
    Ok(())
}

fn walk_check_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_check_files(root, &path, files)?;
        } else if file_type.is_file() && is_checkable_file(&path) {
            files.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(())
}

fn check_one_file(root: &Path, file: &Path) -> Result<&'static str, String> {
    let full_path = root.join(file);
    let source =
        fs::read_to_string(&full_path).map_err(|err| format!("{}: {err}", file.display()))?;
    let source_file = file.to_string_lossy();
    match lower_extension(file).as_deref() {
        Some("js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx") => {
            check_source_syntax(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            Ok("js/ts")
        }
        Some("vue") => {
            check_html_syntax(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            check_embedded_blocks(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            Ok("html")
        }
        Some("html") => {
            check_html_syntax(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            check_embedded_blocks(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            Ok("html")
        }
        Some("css") => {
            check_css_syntax(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            Ok("css")
        }
        Some("md") => {
            check_markdown_syntax(&source, &source_file)
                .map_err(|err| format!("{}: {err}", file.display()))?;
            Ok("md")
        }
        _ => Ok("other"),
    }
}

fn is_checkable_file(path: &Path) -> bool {
    matches!(
        lower_extension(path).as_deref(),
        Some("js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "vue" | "html" | "css" | "md")
    )
}

fn lower_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|extension| extension.to_ascii_lowercase())
}

fn check_embedded_blocks(source: &str, source_file: &str) -> Result<(), String> {
    for block in raw_tag_blocks(source, "script")? {
        let attrs = block.attrs.to_ascii_lowercase();
        if attrs.contains(" src=")
            || attrs.contains(" type=\"application/json\"")
            || attrs.contains(" type='application/json'")
            || attrs.contains(" type=\"importmap\"")
            || attrs.contains(" type='importmap'")
            || attrs.contains(" type=\"speculationrules\"")
            || attrs.contains(" type='speculationrules'")
        {
            continue;
        }
        let virtual_file = if attrs.contains("lang=\"tsx\"") || attrs.contains("lang='tsx'") {
            format!("{source_file}:script.tsx")
        } else if attrs.contains("lang=\"ts\"") || attrs.contains("lang='ts'") {
            format!("{source_file}:script.ts")
        } else if attrs.contains("lang=\"jsx\"") || attrs.contains("lang='jsx'") {
            format!("{source_file}:script.jsx")
        } else {
            format!("{source_file}:script.js")
        };
        check_source_syntax(block.body, &virtual_file)
            .map_err(|err| format!("embedded <script> at {}: {err}", block.location))?;
    }
    for block in raw_tag_blocks(source, "style")? {
        check_css_syntax(block.body, source_file)
            .map_err(|err| format!("embedded <style> at {}: {err}", block.location))?;
    }
    Ok(())
}

struct RawTagBlock<'a> {
    attrs: &'a str,
    body: &'a str,
    location: String,
}

fn raw_tag_blocks<'a>(source: &'a str, tag: &str) -> Result<Vec<RawTagBlock<'a>>, String> {
    let lower = source.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}");
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find(&open) {
        let start = cursor + offset;
        let Some(tag_end) = find_markup_tag_end(source, start) else {
            return Err(format!("unclosed <{tag}> at {}", line_col(source, start)));
        };
        let attrs = &source[start + open.len()..tag_end - 1];
        if attrs.trim_end().ends_with('/') {
            cursor = tag_end;
            continue;
        }
        let Some(close_offset) = lower[tag_end..].find(&close) else {
            return Err(format!("missing </{tag}> for {}", line_col(source, start)));
        };
        let body_start = tag_end;
        let close_start = tag_end + close_offset;
        let Some(close_end) = find_markup_tag_end(source, close_start) else {
            return Err(format!(
                "unclosed </{tag}> at {}",
                line_col(source, close_start)
            ));
        };
        blocks.push(RawTagBlock {
            attrs,
            body: &source[body_start..close_start],
            location: line_col(source, start),
        });
        cursor = close_end;
    }
    Ok(blocks)
}

fn check_html_syntax(source: &str, source_file: &str) -> Result<(), String> {
    let lower = source.to_ascii_lowercase();
    let mut stack = Vec::<(String, usize)>::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find('<') {
        let start = cursor + offset;
        if source[start..].starts_with("<!--") {
            let Some(end) = source[start + 4..].find("-->") else {
                return Err(format!("unclosed comment at {}", line_col(source, start)));
            };
            cursor = start + 4 + end + 3;
            continue;
        }
        let Some(end) = find_markup_tag_end(source, start) else {
            return Err(format!("unclosed tag at {}", line_col(source, start)));
        };
        let tag = source[start + 1..end - 1].trim();
        if tag.is_empty() || tag.starts_with('!') || tag.starts_with('?') {
            cursor = end;
            continue;
        }
        let closing = tag.starts_with('/');
        let tag_name = html_tag_name(if closing { &tag[1..] } else { tag });
        if tag_name.is_empty() {
            cursor = end;
            continue;
        }
        if closing {
            if let Some(index) = stack.iter().rposition(|(name, _)| name == &tag_name) {
                stack.truncate(index);
            } else if !is_optional_html_tag(&tag_name) {
                return Err(format!(
                    "unexpected closing </{tag_name}> at {}",
                    line_col(source, start)
                ));
            }
            cursor = end;
            continue;
        }
        let self_closing = tag.ends_with('/') || is_void_html_tag(&tag_name);
        if !self_closing {
            if is_optional_html_tag(&tag_name) {
                while stack
                    .last()
                    .is_some_and(|(open_name, _)| open_name == &tag_name)
                {
                    stack.pop();
                }
            }
            stack.push((tag_name.clone(), start));
        }
        if tag_name == "script" || tag_name == "style" {
            let close = format!("</{tag_name}");
            if let Some(close_offset) = lower[end..].find(&close) {
                let close_start = end + close_offset;
                let Some(close_end) = find_markup_tag_end(source, close_start) else {
                    return Err(format!(
                        "unclosed </{tag_name}> at {}",
                        line_col(source, close_start)
                    ));
                };
                stack.pop();
                cursor = close_end;
                continue;
            }
            return Err(format!(
                "missing </{tag_name}> for {} in {source_file}",
                line_col(source, start)
            ));
        }
        cursor = end;
    }
    if let Some((tag, start)) = stack.last() {
        return Err(format!(
            "unclosed <{tag}> opened at {}",
            line_col(source, *start)
        ));
    }
    Ok(())
}

fn find_markup_tag_end(source: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, ch) in source[start..].char_indices() {
        match quote {
            Some(current) if ch == current => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '>' => return Some(start + offset + 1),
            None => {}
        }
    }
    None
}

fn html_tag_name(tag: &str) -> String {
    tag.trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_void_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_optional_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "body" | "html" | "head" | "li" | "p" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
    )
}

fn check_css_syntax(source: &str, _source_file: &str) -> Result<(), String> {
    let mut stack = Vec::<(char, usize)>::new();
    let mut chars = source.char_indices().peekable();
    let mut quote = None;
    while let Some((index, ch)) = chars.next() {
        if let Some(current) = quote {
            if ch == '\\' {
                chars.next();
            } else if ch == current {
                quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '*') {
            chars.next();
            let mut closed = false;
            while let Some((_, comment_ch)) = chars.next() {
                if comment_ch == '*' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err(format!("unclosed comment at {}", line_col(source, index)));
            }
            continue;
        }
        match ch {
            '{' | '(' | '[' => stack.push((ch, index)),
            '}' | ')' | ']' => {
                let Some((open, open_index)) = stack.pop() else {
                    return Err(format!("unexpected `{ch}` at {}", line_col(source, index)));
                };
                if !matching_delimiter(open, ch) {
                    return Err(format!(
                        "mismatched `{open}` at {} and `{ch}` at {}",
                        line_col(source, open_index),
                        line_col(source, index)
                    ));
                }
            }
            _ => {}
        }
    }
    if let Some(current) = quote {
        return Err(format!("unclosed string `{current}`"));
    }
    if let Some((open, index)) = stack.last() {
        return Err(format!(
            "unclosed `{open}` opened at {}",
            line_col(source, *index)
        ));
    }
    Ok(())
}

fn matching_delimiter(open: char, close: char) -> bool {
    matches!((open, close), ('{', '}') | ('(', ')') | ('[', ']'))
}

fn check_markdown_syntax(source: &str, _source_file: &str) -> Result<(), String> {
    let mut fence = None::<(char, usize, usize)>;
    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        let Some(marker) = marker else {
            continue;
        };
        let width = trimmed.chars().take_while(|ch| *ch == marker).count();
        if width < 3 {
            continue;
        }
        match fence {
            Some((open_marker, open_width, _)) if open_marker == marker && width >= open_width => {
                fence = None;
            }
            None => {
                fence = Some((marker, width, line_index + 1));
            }
            _ => {}
        }
    }
    if let Some((marker, width, line)) = fence {
        return Err(format!(
            "unclosed markdown fence {} opened at line {line}",
            marker.to_string().repeat(width)
        ));
    }
    Ok(())
}

fn line_col(source: &str, index: usize) -> String {
    let mut line = 1;
    let mut col = 1;
    for ch in source[..index.min(source.len())].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    format!("{line}:{col}")
}

fn workspace_root() -> Result<PathBuf, String> {
    let mut dir = env::current_dir().map_err(|err| err.to_string())?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("cannot find workspace root".to_string());
        }
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|err| format!("remove {}: {err}", path.display()))?;
    }
    Ok(())
}

fn run_command(root: &Path, command: &str, args: Vec<&str>) -> Result<(), String> {
    run_command_with_env(root, command, args, &[])
}

fn run_command_with_env(
    root: &Path,
    command: &str,
    args: Vec<&str>,
    envs: &[(&str, &str)],
) -> Result<(), String> {
    println!("RUN {command} {}", args.join(" "));
    let mut process = Command::new(command);
    process.args(args).current_dir(root);
    for (key, value) in envs {
        process.env(key, value);
    }
    let status = process
        .status()
        .map_err(|err| format!("{command}: {err}"))?;
    if !status.success() {
        return Err(format!("{command} exited with {status}"));
    }
    Ok(())
}

fn runtime_wasm_rustflags() -> String {
    let stack_arg = format!("-C link-arg=-zstack-size={RUNTIME_WASM_STACK_SIZE}");
    match env::var("RUSTFLAGS") {
        Ok(existing) if !existing.trim().is_empty() => {
            if existing.contains("-zstack-size=") {
                existing
            } else {
                format!("{existing} {stack_arg}")
            }
        }
        _ => stack_arg,
    }
}

fn find_command(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(command))
        .find(|path| path.is_file())
}

fn optimize_wasm(root: &Path, wasm_opt: &Path, file: &str) -> Result<(), String> {
    run_command(
        root,
        &wasm_opt.display().to_string(),
        vec![
            file,
            "-Oz",
            "--enable-bulk-memory",
            "--enable-nontrapping-float-to-int",
            "-o",
            file,
        ],
    )
}

fn patch_wasm_bindgen_js(path: &Path) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let patched = source
        .replace(
            "let deferred5_0;\n    let deferred5_1;",
            "let deferred5_0 = 0;\n    let deferred5_1 = 0;",
        )
        .replace(
            "let deferred6_0;\n    let deferred6_1;",
            "let deferred6_0 = 0;\n    let deferred6_1 = 0;",
        );
    if patched != source {
        fs::write(path, patched).map_err(|err| format!("{}: {err}", path.display()))?;
        println!(
            "INFO Patched wasm-bindgen deferred frees in {}",
            path.display()
        );
    }
    Ok(())
}

fn print_size(root: &Path, file: &str) -> Result<(), String> {
    let size = fs::metadata(root.join(file))
        .map_err(|err| format!("{file}: {err}"))?
        .len();
    println!("INFO {size:>8} {file}");
    Ok(())
}

fn ensure_runtime_exists(root: &Path, platforms: &[Platform]) -> Result<(), String> {
    let mut files = Vec::new();
    if platforms.contains(&Platform::Web) {
        files.extend([
            "pkg/executor/js_vm_runtime.js",
            "pkg/executor/js_vm_runtime_bg.wasm",
        ]);
    }
    if platforms.contains(&Platform::Node) {
        files.extend([
            "pkg/executor-node/js_vm_runtime_node.js",
            "pkg/executor-node/js_vm_runtime_node_bg.wasm",
        ]);
    }
    for file in files {
        if !root.join(file).is_file() {
            return Err(format!("{file} is missing; run npm run build:wasm first"));
        }
    }
    Ok(())
}

fn copy_runtime(root: &Path, output: &Path, platforms: &[Platform]) -> Result<(), String> {
    // runtime 包按平台拆分：浏览器页面只拿 browser executor，Node 产物只拿 node executor。
    // 后续继续做 feature 包时，这里会替换为按 manifest/md5 复制最小 runtime。
    let mut files = Vec::new();
    if platforms.contains(&Platform::Web) {
        files.extend([
            ("pkg/executor/js_vm_runtime.js", "js_vm_runtime_browser.js"),
            (
                "pkg/executor/js_vm_runtime_bg.wasm",
                "js_vm_runtime_browser_bg.wasm",
            ),
            ("pkg/executor/package.json", "package.json"),
        ]);
    }
    if platforms.contains(&Platform::Node) {
        files.extend([
            (
                "pkg/executor-node/js_vm_runtime_node.js",
                "js_vm_runtime_node.js",
            ),
            (
                "pkg/executor-node/js_vm_runtime_node_bg.wasm",
                "js_vm_runtime_node_bg.wasm",
            ),
        ]);
    }
    for (from, to) in files {
        let source = root.join(from);
        if source.is_file() {
            fs::copy(&source, output.join(to))
                .map_err(|err| format!("copy {}: {err}", source.display()))?;
        }
    }
    Ok(())
}

fn copy_static_site(
    input: &Path,
    output: &Path,
    platforms: &[Platform],
    script_versions: &BTreeMap<String, String>,
) -> Result<usize, String> {
    let output_top = output_top_dir(input, output);
    let mut copied = 0;
    walk_static_site(
        input,
        input,
        output,
        output_top.as_deref(),
        platforms.contains(&Platform::Web),
        script_versions,
        &mut copied,
    )?;
    Ok(copied)
}

fn walk_static_site(
    root: &Path,
    dir: &Path,
    output: &Path,
    output_top: Option<&str>,
    rewrite_html: bool,
    script_versions: &BTreeMap<String, String>,
    copied: &mut usize,
) -> Result<(), String> {
    for dir_entry in fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let dir_entry = dir_entry.map_err(|err| err.to_string())?;
        let path = dir_entry.path();
        let name = dir_entry.file_name();
        let name = name.to_string_lossy();
        let file_type = dir_entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) || Some(name.as_ref()) == output_top {
                continue;
            }
            walk_static_site(
                root,
                &path,
                output,
                output_top,
                rewrite_html,
                script_versions,
                copied,
            )?;
        } else if file_type.is_file() && !is_js_ts(&path) {
            let relative = path.strip_prefix(root).map_err(|err| err.to_string())?;
            let target = output.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
            }
            if is_html(&path) && rewrite_html {
                let source = fs::read_to_string(&path)
                    .map_err(|err| format!("{}: {err}", path.display()))?;
                fs::write(&target, rewrite_html_for_vm(&source, script_versions))
                    .map_err(|err| format!("{}: {err}", target.display()))?;
            } else {
                fs::copy(&path, &target)
                    .map_err(|err| format!("copy {}: {err}", path.display()))?;
            }
            *copied += 1;
        }
    }
    Ok(())
}

fn list_source_files(input: &Path, output: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    let output_top = output_top_dir(input, output);
    walk_sources(input, input, output_top.as_deref(), &mut files)?;
    files.sort();
    Ok(files)
}

fn output_top_dir(input: &Path, output: &Path) -> Option<String> {
    output
        .strip_prefix(input)
        .ok()
        .and_then(|path| path.components().next())
        .and_then(|component| component.as_os_str().to_str())
        .map(str::to_string)
}

fn walk_sources(
    root: &Path,
    dir: &Path,
    output_top: Option<&str>,
    files: &mut Vec<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) || Some(name.as_ref()) == output_top {
                continue;
            }
            walk_sources(root, &path, output_top, files)?;
        } else if file_type.is_file() && is_js_ts(&path) {
            files.push(to_virtual_path(
                path.strip_prefix(root).map_err(|err| err.to_string())?,
            ));
        }
    }
    Ok(())
}

fn is_js_ts(path: &Path) -> bool {
    matches!(
        lower_extension(path).as_deref(),
        Some("js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx")
    )
}

fn is_html(path: &Path) -> bool {
    matches!(lower_extension(path).as_deref(), Some("html"))
}

fn rewrite_html_for_vm(source: &str, script_versions: &BTreeMap<String, String>) -> String {
    // 原站点里构建工具可能生成 modulepreload 预加载原 JS。
    // VM 化后入口 wrapper 会自己加载 bin，保留旧 preload 可能导致原 JS 和 VM JS 双执行。
    add_module_script_versions(&remove_js_modulepreload_links(source), script_versions)
}

fn remove_js_modulepreload_links(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("<link") {
        let start = cursor + offset;
        out.push_str(&source[cursor..start]);
        let Some(end) = find_tag_end(source, start) else {
            out.push_str(&source[start..]);
            return out;
        };
        let tag = &source[start..end];
        let lower = tag.to_ascii_lowercase();
        if lower.contains(".js")
            && (lower.contains("rel=\"modulepreload\"")
                || lower.contains("rel='modulepreload'")
                || lower.contains("rel=modulepreload"))
        {
            cursor = end;
        } else {
            out.push_str(tag);
            cursor = end;
        }
    }
    out.push_str(&source[cursor..]);
    out
}

fn find_tag_end(source: &str, start: usize) -> Option<usize> {
    source[start..].find('>').map(|offset| start + offset + 1)
}

fn add_module_script_versions(source: &str, versions: &BTreeMap<String, String>) -> String {
    if versions.is_empty() {
        return source.to_string();
    }
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("<script") {
        let start = cursor + offset;
        out.push_str(&source[cursor..start]);
        let Some(end) = find_tag_end(source, start) else {
            out.push_str(&source[start..]);
            return out;
        };
        let tag = &source[start..end];
        out.push_str(&versioned_module_script_tag(tag, versions));
        cursor = end;
    }
    out.push_str(&source[cursor..]);
    out
}

fn versioned_module_script_tag(tag: &str, versions: &BTreeMap<String, String>) -> String {
    let lower = tag.to_ascii_lowercase();
    if !(lower.contains("type=\"module\"")
        || lower.contains("type='module'")
        || lower.contains("type=module"))
    {
        return tag.to_string();
    }
    let Some(src) = tag_attr(tag, "src") else {
        return tag.to_string();
    };
    let normalized = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src.as_str())
        .trim_start_matches('/');
    if !is_js_like_virtual_path(normalized) {
        return tag.to_string();
    }
    let Some(version) = versions.get(normalized) else {
        return tag.to_string();
    };
    replace_src_attr(tag, &src, &versioned_src(&src, version))
}

fn replace_src_attr(tag: &str, old: &str, new: &str) -> String {
    for quote in ['"', '\''] {
        let needle = format!("src={quote}{old}{quote}");
        if tag.contains(&needle) {
            return tag.replacen(&needle, &format!("src={quote}{new}{quote}"), 1);
        }
    }
    let needle = format!("src={old}");
    tag.replacen(&needle, &format!("src={new}"), 1)
}

fn versioned_src(src: &str, version: &str) -> String {
    let hash_start = src.find('#').unwrap_or(src.len());
    let (path_and_query, hash) = src.split_at(hash_start);
    let path_end = path_and_query.find('?').unwrap_or(path_and_query.len());
    let path = &path_and_query[..path_end];
    format!("{path}?v={version}{hash}")
}

fn choose_entry(input: &Path, files: &[String], requested: Option<&str>) -> Result<String, String> {
    if let Some(entry) = requested {
        if files.iter().any(|file| file == entry) {
            return Ok(entry.to_string());
        }
        return Err(format!("entry not found in input files: {entry}"));
    }
    if let Some(entry) = html_module_script_entry(input, files)? {
        return Ok(entry);
    }
    for candidate in [
        "index.ts",
        "index.js",
        "main.ts",
        "main.js",
        "src/index.ts",
        "src/index.js",
        "src/main.ts",
        "src/main.js",
    ] {
        if files.iter().any(|file| file == candidate) {
            return Ok(candidate.to_string());
        }
    }
    files
        .first()
        .cloned()
        .ok_or_else(|| "no source files".to_string())
}

fn html_module_script_entry(input: &Path, files: &[String]) -> Result<Option<String>, String> {
    let mut entries = Vec::new();
    collect_html_module_script_entries(input, input, &mut entries)?;
    for entry in entries {
        if files.iter().any(|file| file == &entry) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

fn collect_html_module_script_entries(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_html_module_script_entries(root, &path, entries)?;
        } else if file_type.is_file() && is_html(&path) {
            let source =
                fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            entries.extend(html_module_script_srcs(&source));
        }
    }
    for entry in entries.iter_mut() {
        *entry = normalize_virtual_path(entry);
    }
    entries.sort();
    entries.dedup();
    let _ = root;
    Ok(())
}

fn html_module_script_srcs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("<script") {
        let start = cursor + offset;
        let Some(end) = find_tag_end(source, start) else {
            break;
        };
        let tag = &source[start..end];
        let lower = tag.to_ascii_lowercase();
        if lower.contains("type=\"module\"")
            || lower.contains("type='module'")
            || lower.contains("type=module")
        {
            if let Some(src) = tag_attr(tag, "src") {
                let src = src
                    .split(['?', '#'])
                    .next()
                    .unwrap_or(src.as_str())
                    .trim_start_matches('/');
                if is_js_like_virtual_path(src) {
                    out.push(src.to_string());
                }
            }
        }
        cursor = end;
    }
    out
}

fn tag_attr(tag: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{name}={quote}");
        if let Some(start) = tag.find(&needle) {
            let value_start = start + needle.len();
            let value_end = tag[value_start..].find(quote)?;
            return Some(tag[value_start..value_start + value_end].to_string());
        }
    }
    let needle = format!("{name}=");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == '>')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn is_js_like_virtual_path(value: &str) -> bool {
    value.ends_with(".js")
        || value.ends_with(".mjs")
        || value.ends_with(".cjs")
        || value.ends_with(".ts")
        || value.ends_with(".tsx")
        || value.ends_with(".jsx")
}

fn read_sources(input: &Path, files: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut sources = files
        .iter()
        .map(|file| {
            let full_path = input.join(file);
            let source = fs::read_to_string(&full_path)
                .map_err(|err| format!("{}: {err}", full_path.display()))?;
            Ok((file.clone(), source))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    recover_sources_from_generated_runtime(input, &mut sources)?;
    Ok(sources)
}

fn recover_sources_from_generated_runtime(
    input: &Path,
    sources: &mut BTreeMap<String, String>,
) -> Result<usize, String> {
    let mut recovered = 0;
    loop {
        let missing = missing_local_source_candidates(sources);
        let before = sources.len();
        for file in missing {
            if sources.contains_key(&file) {
                continue;
            }
            if let Some(source) = recovered_source_from_generated_runtime(input, &file)? {
                sources.insert(file, source);
                recovered += 1;
            }
        }
        if sources.len() == before {
            break;
        }
    }
    if recovered > 0 {
        println!(
            "INFO Recovered {recovered} source module(s) from {GENERATED_RUNTIME_DIR} source maps"
        );
    }
    Ok(recovered)
}

fn missing_local_source_candidates(sources: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut missing = BTreeSet::new();
    for (file, source) in sources {
        for specifier in local_import_specifiers(source) {
            if !specifier.starts_with("./") && !specifier.starts_with("../") {
                continue;
            }
            let candidates = virtual_import_candidates(file, &specifier);
            if candidates
                .iter()
                .any(|candidate| sources.contains_key(candidate))
            {
                continue;
            }
            missing.extend(candidates);
        }
    }
    missing
}

fn recovered_source_from_generated_runtime(
    input: &Path,
    file: &str,
) -> Result<Option<String>, String> {
    let source_map = input
        .join(GENERATED_RUNTIME_DIR)
        .join(format!("{}.map", module_bin_path(file)));
    if !source_map.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(&source_map)
        .map_err(|err| format!("{}: {err}", source_map.display()))?;
    first_sources_content(&content)
        .map(Some)
        .ok_or_else(|| format!("{}: missing sourcesContent[0]", source_map.display()))
}

fn ordered_files(entry: &str, sources: &BTreeMap<String, String>) -> Vec<String> {
    let mut ordered = Vec::new();
    let mut seen = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    visit_file(entry, sources, &mut seen, &mut visiting, &mut ordered);
    for file in sources.keys() {
        visit_file(file, sources, &mut seen, &mut visiting, &mut ordered);
    }
    ordered
}

fn runtime_execution_order(entry: &str, sources: &BTreeMap<String, String>) -> Vec<String> {
    let mut ordered = ordered_files(entry, sources);
    if let Some(index) = ordered.iter().position(|file| file == entry) {
        let entry = ordered.remove(index);
        ordered.push(entry);
    }
    ordered
}

fn module_import_rewrites(
    file: &str,
    sources: &BTreeMap<String, String>,
) -> Vec<ModuleImportRewrite> {
    let Some(source) = sources.get(file) else {
        return Vec::new();
    };
    let Ok(analysis) = analyze_module_source(source) else {
        return Vec::new();
    };
    analysis
        .imports
        .into_iter()
        .map(|import| import.specifier)
        .chain(analysis.dynamic_imports)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|specifier| {
            let resolved = resolve_virtual_import(file, &specifier, sources)?;
            let from = module_js_path(file);
            let to = module_js_path(&resolved);
            Some(ModuleImportRewrite {
                specifier,
                replacement: relative_import_specifier(&from, &to),
            })
        })
        .collect()
}

fn version_module_import_rewrites(
    from_file: &str,
    rewrites: &[ModuleImportRewrite],
    script_versions: &BTreeMap<String, String>,
) -> Vec<ModuleImportRewrite> {
    rewrites
        .iter()
        .map(|rewrite| {
            let normalized =
                normalize_virtual_path(&format!("{}/{}", dirname(from_file), rewrite.replacement));
            let replacement = script_versions
                .get(&normalized)
                .map(|version| versioned_src(&rewrite.replacement, version))
                .unwrap_or_else(|| rewrite.replacement.clone());
            ModuleImportRewrite {
                specifier: rewrite.specifier.clone(),
                replacement,
            }
        })
        .collect()
}

fn local_static_import_targets(sources: &BTreeMap<String, String>) -> BTreeSet<String> {
    local_import_targets_by(sources, |analysis| {
        analysis
            .imports
            .into_iter()
            .map(|import| import.specifier)
            .collect()
    })
}

fn local_dynamic_import_targets(sources: &BTreeMap<String, String>) -> BTreeSet<String> {
    local_import_targets_by(sources, |analysis| analysis.dynamic_imports)
}

fn local_import_targets_by(
    sources: &BTreeMap<String, String>,
    imports: impl Fn(js_vm_compiler::ModuleSourceAnalysis) -> Vec<String>,
) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for (file, source) in sources {
        let Ok(analysis) = analyze_module_source(source) else {
            continue;
        };
        for specifier in imports(analysis) {
            if let Some(resolved) = resolve_virtual_import(file, &specifier, sources) {
                targets.insert(resolved);
            }
        }
    }
    targets
}

fn visit_file(
    file: &str,
    sources: &BTreeMap<String, String>,
    seen: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) {
    if seen.contains(file) || visiting.contains(file) {
        return;
    }
    let Some(source) = sources.get(file) else {
        return;
    };
    visiting.insert(file.to_string());
    for specifier in local_import_specifiers(source) {
        if let Some(resolved) = resolve_virtual_import(file, &specifier, sources) {
            visit_file(&resolved, sources, seen, visiting, ordered);
        }
    }
    visiting.remove(file);
    seen.insert(file.to_string());
    ordered.push(file.to_string());
}

fn local_import_specifiers(source: &str) -> Vec<String> {
    analyze_module_source(source)
        .map(|analysis| {
            analysis
                .imports
                .into_iter()
                .map(|import| import.specifier)
                .chain(analysis.dynamic_imports)
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_virtual_import(
    from_file: &str,
    specifier: &str,
    sources: &BTreeMap<String, String>,
) -> Option<String> {
    if !specifier.starts_with("./") && !specifier.starts_with("../") {
        return None;
    }
    virtual_import_candidates(from_file, specifier)
        .into_iter()
        .find(|candidate| sources.contains_key(candidate))
}

fn virtual_import_candidates(from_file: &str, specifier: &str) -> Vec<String> {
    let base = normalize_virtual_path(&format!("{}/{}", dirname(from_file), specifier));
    vec![
        base.clone(),
        format!("{base}.ts"),
        format!("{base}.js"),
        format!("{base}/index.ts"),
        format!("{base}/index.js"),
    ]
}

fn first_sources_content(source_map: &str) -> Option<String> {
    let key = "\"sourcesContent\"";
    let key_start = source_map.find(key)?;
    let after_key = key_start + key.len();
    let colon = source_map[after_key..].find(':')? + after_key;
    let bracket = source_map[colon + 1..].find('[')? + colon + 1;
    let mut cursor = bracket + 1;
    while source_map[cursor..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
    {
        cursor += source_map[cursor..].chars().next()?.len_utf8();
    }
    parse_json_string(source_map, cursor).map(|(value, _)| value)
}

fn parse_json_string(source: &str, start: usize) -> Option<(String, usize)> {
    if source[start..].chars().next()? != '"' {
        return None;
    }
    let mut out = String::new();
    let mut cursor = start + 1;
    while cursor < source.len() {
        let ch = source[cursor..].chars().next()?;
        cursor += ch.len_utf8();
        match ch {
            '"' => return Some((out, cursor)),
            '\\' => {
                let escaped = source[cursor..].chars().next()?;
                cursor += escaped.len_utf8();
                match escaped {
                    '"' | '\\' | '/' => out.push(escaped),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let end = cursor.checked_add(4)?;
                        let hex = source.get(cursor..end)?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                        cursor = end;
                    }
                    _ => return None,
                }
            }
            _ => out.push(ch),
        }
    }
    None
}

fn browser_env_code(options: &PackageOptions) -> String {
    format!(
        r#"// JS VM browser environment package.
import init, * as runtime from './js_vm_runtime_browser.js';

const maxCallDepth = {max_call_depth};
const maxRecursiveCallDepth = {max_recursive_call_depth};
const maxExecutionSteps = {max_execution_steps};
let runtimeReady;

if (typeof globalThis.__jsVmHostLog !== 'function') {{
  globalThis.__jsVmHostLog = (level, message) => {{
    const method = console && typeof console[level] === 'function' ? console[level] : console.log;
    method.call(console, message);
  }};
}}

function ensureRuntime() {{
  if (!runtimeReady) {{
    runtimeReady = init({{ module_or_path: new URL('./js_vm_runtime_browser_bg.wasm', import.meta.url) }});
  }}
  return runtimeReady;
}}

export async function loadBin(url) {{
  const response = await fetch(url, {{ cache: 'no-store' }});
  if (!response.ok) throw new Error(`failed to load ${{url}}: ${{response.status}} ${{response.statusText}}`);
  return new Uint8Array(await response.arrayBuffer());
}}

export async function loadSourceMap(url) {{
  const response = await fetch(url, {{ cache: 'no-store' }});
  if (!response.ok) throw new Error(`failed to load ${{url}}: ${{response.status}} ${{response.statusText}}`);
  return response.json();
}}

export function resolveExternal(name) {{
  if (name === '__jsVmIntrinsicDeflateTable') return __jsVmIntrinsicDeflateTable;
  if (name === '__jsVmIntrinsicBitReverseTable') return __jsVmIntrinsicBitReverseTable;
  return String(name).split('.').reduce((value, part) => value == null ? undefined : value[part], globalThis);
}}

function __jsVmIntrinsicDeflateTable(Uint16ArrayCtor, Int32ArrayCtor) {{
  return function(lengths, base) {{
    const bits = new Uint16ArrayCtor(31);
    for (let index = 0; index < 31; ++index) bits[index] = base += 1 << lengths[index - 1];
    const reverse = new Int32ArrayCtor(bits[30]);
    for (let index = 1; index < 30; ++index) {{
      for (let value = bits[index]; value < bits[index + 1]; ++value) {{
        reverse[value] = ((value - bits[index]) << 5) | index;
      }}
    }}
    return {{ b: bits, r: reverse }};
  }};
}}

function __jsVmIntrinsicBitReverseTable(Uint16ArrayCtor) {{
  const table = new Uint16ArrayCtor(32768);
  for (let value = 0; value < 32768; ++value) {{
    let reversed = ((value & 43690) >> 1) | ((value & 21845) << 1);
    reversed = ((reversed & 52428) >> 2) | ((reversed & 13107) << 2);
    reversed = ((reversed & 61680) >> 4) | ((reversed & 3855) << 4);
    table[value] = (((reversed & 65280) >> 8) | ((reversed & 255) << 8)) >> 1;
  }}
  return table;
}}

export async function ready() {{
  await ensureRuntime();
}}

function assertReady() {{
  if (!runtimeReady) {{
    throw new Error('JS VM runtime is not initialized; call ready() before execute()');
  }}
}}

export function executeScript(bytes, seed, externs = []) {{
  assertReady();
  const run = typeof runtime.js_execute_void_bytes_with_seed_and_runtime_limits === 'function'
    ? runtime.js_execute_void_bytes_with_seed_and_runtime_limits
    : runtime.js_execute_void_bytes_with_seed;
  if (run === runtime.js_execute_void_bytes_with_seed_and_runtime_limits) {{
    run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }} else {{
    run(bytes, seed, externs);
  }}
}}

export function executeModule(bytes, seed, externs = []) {{
  assertReady();
  const run = typeof runtime.js_execute_module_bytes_with_seed_and_runtime_limits === 'function'
    ? runtime.js_execute_module_bytes_with_seed_and_runtime_limits
    : runtime.js_execute_module_bytes_with_seed;
  if (run === runtime.js_execute_module_bytes_with_seed_and_runtime_limits) {{
    return run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }}
  return run(bytes, seed, externs);
}}

export function executeDebug(bytes, seed, externs = []) {{
  assertReady();
  const run = typeof runtime.js_execute_bytes_with_seed_debug_and_runtime_limits === 'function'
    ? runtime.js_execute_bytes_with_seed_debug_and_runtime_limits
    : runtime.js_execute_bytes_with_seed_debug;
  if (typeof run !== 'function') {{
    throw new Error('JS VM runtime was not built with source-map feature');
  }}
  if (run === runtime.js_execute_bytes_with_seed_debug_and_runtime_limits) {{
    return run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }}
  return run(bytes, seed, externs);
}}

export function createDebugSession(bytes, seed, externs = []) {{
  assertReady();
  const Session = runtime.JsVmDebugSession;
  if (typeof Session !== 'function') {{
    throw new Error('JS VM runtime was not built with debugger feature');
  }}
  if (typeof Session.new_with_runtime_limits === 'function') {{
    return Session.new_with_runtime_limits(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }}
  return new Session(bytes, seed, externs);
}}

export function breakpointPcs(sourceMap, breakpoints = []) {{
  const list = Array.isArray(breakpoints) ? breakpoints : [breakpoints];
  const vm = sourceMap?.x_js_vm || {{}};
  const ranges = Array.isArray(vm.pcRanges) ? vm.pcRanges : [];
  const spans = Array.isArray(vm.sourceSpans) ? vm.sourceSpans : [];
  const pcs = new Set();
  for (const breakpoint of list) {{
    if (typeof breakpoint === 'number' && Number.isFinite(breakpoint)) {{
      pcs.add(Math.max(0, Math.trunc(breakpoint)));
      continue;
    }}
    if (!breakpoint || typeof breakpoint !== 'object') continue;
    if (Number.isFinite(Number(breakpoint.pc))) {{
      pcs.add(Math.max(0, Math.trunc(Number(breakpoint.pc))));
      continue;
    }}
    const line = Number(breakpoint.line);
    const column = Number.isFinite(Number(breakpoint.column)) ? Number(breakpoint.column) : 0;
    if (!Number.isFinite(line)) continue;
    for (const range of ranges) {{
      const span = range[5] >= 0 ? spans[range[5]] : null;
      if (!span) continue;
      const inLine = line >= span[2] && line <= span[4];
      const afterStart = line !== span[2] || column >= span[3];
      const beforeEnd = line !== span[4] || column <= span[5];
      if (inLine && afterStart && beforeEnd) {{
        pcs.add(range[0]);
        break;
      }}
    }}
  }}
  return Array.from(pcs).sort((a, b) => a - b);
}}

export function sourceFrame(sourceMap, pc) {{
  const vm = sourceMap?.x_js_vm || {{}};
  if (Array.isArray(vm.pcRanges)) {{
    const range = vm.pcRanges.find((item) => pc >= item[0] && pc < item[1]);
    if (!range) return null;
    const opRange = (vm.pcOps || []).find((item) => pc >= item[0] && pc < item[1]);
    const span = range[5] >= 0 ? vm.sourceSpans?.[range[5]] : null;
    const source = span ? {{
      source: 0,
      start: span[0],
      end: span[1],
      line: span[2],
      column: span[3],
      endLine: span[4],
      endColumn: span[5],
    }} : null;
    return {{
      pc,
      pcStart: range[0],
      pcEnd: range[1],
      op: opRange ? vm.opcodes?.[opRange[2]] : undefined,
      byteStart: range[2],
      byteEnd: range[3],
      function: range[4] >= 0 ? range[4] : null,
      source,
      sourceFile: sourceMap.sources?.[0],
    }};
  }}
  const frames = vm.pcMap || [];
  const frame = frames.find((item) => item.pc === pc);
  return frame ? {{ ...frame, sourceFile: sourceMap.sources?.[frame.source?.source ?? 0] }} : null;
}}

export function decorateDebugEvent(sourceMap, event) {{
  const pc = Number(event?.pc);
  const frame = Number.isFinite(pc) ? sourceFrame(sourceMap, pc) : null;
  const callStack = (event?.callStack || []).map((item) => {{
    const framePc = Number(item?.pc);
    return {{
      ...item,
      source: Number.isFinite(framePc) ? sourceFrame(sourceMap, framePc) : null,
    }};
  }});
  return {{ ...event, frame, source: frame?.source || null, callStack }};
}}

export const execute = executeModule;

await ready();
"#,
        max_call_depth = options.max_call_depth,
        max_recursive_call_depth = options.max_recursive_call_depth,
        max_execution_steps = options.max_execution_steps
    )
}

fn node_env_code(options: &PackageOptions) -> String {
    format!(
        r#"// JS VM node environment package.
import fs from 'node:fs/promises';
import {{ fileURLToPath }} from 'node:url';
import {{ dirname, join }} from 'node:path';
import * as runtime from './js_vm_runtime_node.js';

const maxCallDepth = {max_call_depth};
const maxRecursiveCallDepth = {max_recursive_call_depth};
const maxExecutionSteps = {max_execution_steps};
const here = dirname(fileURLToPath(import.meta.url));

if (typeof globalThis.__jsVmHostLog !== 'function') {{
  globalThis.__jsVmHostLog = (level, message) => {{
    const method = console && typeof console[level] === 'function' ? console[level] : console.log;
    method.call(console, message);
  }};
}}

export async function loadBin(url) {{
  const file = url instanceof URL ? fileURLToPath(url) : join(here, String(url));
  return new Uint8Array(await fs.readFile(file));
}}

export async function loadSourceMap(url) {{
  const file = url instanceof URL ? fileURLToPath(url) : join(here, String(url));
  return JSON.parse(await fs.readFile(file, 'utf8'));
}}

export function resolveExternal(name) {{
  if (name === '__jsVmIntrinsicDeflateTable') return __jsVmIntrinsicDeflateTable;
  if (name === '__jsVmIntrinsicBitReverseTable') return __jsVmIntrinsicBitReverseTable;
  return String(name).split('.').reduce((value, part) => value == null ? undefined : value[part], globalThis);
}}

function __jsVmIntrinsicDeflateTable(Uint16ArrayCtor, Int32ArrayCtor) {{
  return function(lengths, base) {{
    const bits = new Uint16ArrayCtor(31);
    for (let index = 0; index < 31; ++index) bits[index] = base += 1 << lengths[index - 1];
    const reverse = new Int32ArrayCtor(bits[30]);
    for (let index = 1; index < 30; ++index) {{
      for (let value = bits[index]; value < bits[index + 1]; ++value) {{
        reverse[value] = ((value - bits[index]) << 5) | index;
      }}
    }}
    return {{ b: bits, r: reverse }};
  }};
}}

function __jsVmIntrinsicBitReverseTable(Uint16ArrayCtor) {{
  const table = new Uint16ArrayCtor(32768);
  for (let value = 0; value < 32768; ++value) {{
    let reversed = ((value & 43690) >> 1) | ((value & 21845) << 1);
    reversed = ((reversed & 52428) >> 2) | ((reversed & 13107) << 2);
    reversed = ((reversed & 61680) >> 4) | ((reversed & 3855) << 4);
    table[value] = (((reversed & 65280) >> 8) | ((reversed & 255) << 8)) >> 1;
  }}
  return table;
}}

export async function ready() {{}}

export function executeScript(bytes, seed, externs = []) {{
  const run = typeof runtime.js_execute_void_bytes_with_seed_and_runtime_limits === 'function'
    ? runtime.js_execute_void_bytes_with_seed_and_runtime_limits
    : runtime.js_execute_void_bytes_with_seed;
  if (run === runtime.js_execute_void_bytes_with_seed_and_runtime_limits) {{
    run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }} else {{
    run(bytes, seed, externs);
  }}
}}

export function executeModule(bytes, seed, externs = []) {{
  const run = typeof runtime.js_execute_module_bytes_with_seed_and_runtime_limits === 'function'
    ? runtime.js_execute_module_bytes_with_seed_and_runtime_limits
    : runtime.js_execute_module_bytes_with_seed;
  if (run === runtime.js_execute_module_bytes_with_seed_and_runtime_limits) {{
    return run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }}
  return run(bytes, seed, externs);
}}

export function executeDebug(bytes, seed, externs = []) {{
  const run = typeof runtime.js_execute_bytes_with_seed_debug_and_runtime_limits === 'function'
    ? runtime.js_execute_bytes_with_seed_debug_and_runtime_limits
    : runtime.js_execute_bytes_with_seed_debug;
  if (typeof run !== 'function') {{
    throw new Error('JS VM runtime was not built with source-map feature');
  }}
  if (run === runtime.js_execute_bytes_with_seed_debug_and_runtime_limits) {{
    return run(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }}
  return run(bytes, seed, externs);
}}

export function createDebugSession(bytes, seed, externs = []) {{
  const Session = runtime.JsVmDebugSession;
  if (typeof Session !== 'function') {{
    throw new Error('JS VM runtime was not built with debugger feature');
  }}
  if (typeof Session.new_with_runtime_limits === 'function') {{
    return Session.new_with_runtime_limits(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth, maxExecutionSteps);
  }}
  return new Session(bytes, seed, externs);
}}

export function breakpointPcs(sourceMap, breakpoints = []) {{
  const list = Array.isArray(breakpoints) ? breakpoints : [breakpoints];
  const vm = sourceMap?.x_js_vm || {{}};
  const ranges = Array.isArray(vm.pcRanges) ? vm.pcRanges : [];
  const spans = Array.isArray(vm.sourceSpans) ? vm.sourceSpans : [];
  const pcs = new Set();
  for (const breakpoint of list) {{
    if (typeof breakpoint === 'number' && Number.isFinite(breakpoint)) {{
      pcs.add(Math.max(0, Math.trunc(breakpoint)));
      continue;
    }}
    if (!breakpoint || typeof breakpoint !== 'object') continue;
    if (Number.isFinite(Number(breakpoint.pc))) {{
      pcs.add(Math.max(0, Math.trunc(Number(breakpoint.pc))));
      continue;
    }}
    const line = Number(breakpoint.line);
    const column = Number.isFinite(Number(breakpoint.column)) ? Number(breakpoint.column) : 0;
    if (!Number.isFinite(line)) continue;
    for (const range of ranges) {{
      const span = range[5] >= 0 ? spans[range[5]] : null;
      if (!span) continue;
      const inLine = line >= span[2] && line <= span[4];
      const afterStart = line !== span[2] || column >= span[3];
      const beforeEnd = line !== span[4] || column <= span[5];
      if (inLine && afterStart && beforeEnd) {{
        pcs.add(range[0]);
        break;
      }}
    }}
  }}
  return Array.from(pcs).sort((a, b) => a - b);
}}

export function sourceFrame(sourceMap, pc) {{
  const vm = sourceMap?.x_js_vm || {{}};
  if (Array.isArray(vm.pcRanges)) {{
    const range = vm.pcRanges.find((item) => pc >= item[0] && pc < item[1]);
    if (!range) return null;
    const opRange = (vm.pcOps || []).find((item) => pc >= item[0] && pc < item[1]);
    const span = range[5] >= 0 ? vm.sourceSpans?.[range[5]] : null;
    const source = span ? {{
      source: 0,
      start: span[0],
      end: span[1],
      line: span[2],
      column: span[3],
      endLine: span[4],
      endColumn: span[5],
    }} : null;
    return {{
      pc,
      pcStart: range[0],
      pcEnd: range[1],
      op: opRange ? vm.opcodes?.[opRange[2]] : undefined,
      byteStart: range[2],
      byteEnd: range[3],
      function: range[4] >= 0 ? range[4] : null,
      source,
      sourceFile: sourceMap.sources?.[0],
    }};
  }}
  const frames = vm.pcMap || [];
  const frame = frames.find((item) => item.pc === pc);
  return frame ? {{ ...frame, sourceFile: sourceMap.sources?.[frame.source?.source ?? 0] }} : null;
}}

export function decorateDebugEvent(sourceMap, event) {{
  const pc = Number(event?.pc);
  const frame = Number.isFinite(pc) ? sourceFrame(sourceMap, pc) : null;
  const callStack = (event?.callStack || []).map((item) => {{
    const framePc = Number(item?.pc);
    return {{
      ...item,
      source: Number.isFinite(framePc) ? sourceFrame(sourceMap, framePc) : null,
    }};
  }});
  return {{ ...event, frame, source: frame?.source || null, callStack }};
}}

export const execute = executeModule;
"#,
        max_call_depth = options.max_call_depth,
        max_recursive_call_depth = options.max_recursive_call_depth,
        max_execution_steps = options.max_execution_steps
    )
}

fn write_output(output: &Path, relative: &str, bytes: &[u8]) -> Result<(), String> {
    let path = output.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    fs::write(&path, bytes).map_err(|err| format!("{}: {err}", path.display()))
}

fn dirname(file: &str) -> &str {
    file.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn module_js_path(file: &str) -> String {
    replace_extension(file, "js")
}

fn module_bin_path(file: &str) -> String {
    replace_extension(file, "bin")
}

fn replace_extension(file: &str, extension: &str) -> String {
    let normalized = normalize_virtual_path(file);
    match normalized.rsplit_once('.') {
        Some((stem, _)) => format!("{stem}.{extension}"),
        None => format!("{normalized}.{extension}"),
    }
}

fn seed_cache_version(seed: &str) -> String {
    seed.split('-')
        .nth(1)
        .filter(|part| !part.is_empty())
        .unwrap_or(seed)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(32)
        .collect()
}

fn content_cache_version(bytes: &[u8]) -> String {
    content_cache_version_with_prefix(&[], bytes)
}

fn content_cache_version_with_prefix(prefix: &[u8], bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in prefix.iter().chain(bytes.iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn relative_import_specifier(from_file: &str, to_file: &str) -> String {
    let from_dir = dirname(from_file);
    let from_parts = if from_dir.is_empty() {
        Vec::new()
    } else {
        from_dir.split('/').collect::<Vec<_>>()
    };
    let to_parts = normalize_virtual_path(to_file)
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut rel = Vec::new();
    for _ in common..from_parts.len() {
        rel.push("..".to_string());
    }
    rel.extend(to_parts[common..].iter().cloned());
    let joined = rel.join("/");
    if joined.starts_with('.') {
        joined
    } else {
        format!("./{joined}")
    }
}

fn normalize_virtual_path(value: &str) -> String {
    let mut parts = Vec::new();
    let normalized = value.replace('\\', "/");
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn to_virtual_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn platform_names(platforms: &[Platform]) -> String {
    platforms
        .iter()
        .map(|platform| match platform {
            Platform::Web => "web",
            Platform::Node => "node",
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_map_sources_content_is_recovered() {
        let map =
            r#"{"version":3,"sourcesContent":["const text = \"a\\nb\";\nexport default text;"]}"#;

        assert_eq!(
            first_sources_content(map).as_deref(),
            Some("const text = \"a\\nb\";\nexport default text;")
        );
    }

    #[test]
    fn missing_local_import_candidates_are_detected() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "assets/app.js".to_string(),
            "const page = () => import('./index.html-hash.js');".to_string(),
        );

        let missing = missing_local_source_candidates(&sources);

        assert!(missing.contains("assets/index.html-hash.js"));
    }

    #[test]
    fn html_module_script_is_used_as_default_entry() {
        let root = env::temp_dir().join(format!(
            "js-vm-cli-entry-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(
            root.join("index.html"),
            r#"<script type="module" src="/assets/app-hash.js" defer></script>"#,
        )
        .unwrap();
        let files = vec![
            "assets/01.article.js".to_string(),
            "assets/app-hash.js".to_string(),
        ];

        let entry = choose_entry(&root, &files, None).unwrap();

        assert_eq!(entry, "assets/app-hash.js");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn html_module_script_gets_cache_version() {
        let mut versions = BTreeMap::new();
        versions.insert("assets/app-hash.js".to_string(), "abc123".to_string());
        let html = r#"<link rel="modulepreload" href="/assets/app-hash.js"><script type="module" src="/assets/app-hash.js" defer></script>"#;

        let rewritten = rewrite_html_for_vm(html, &versions);

        assert!(!rewritten.contains("modulepreload"));
        assert!(rewritten.contains(r#"src="/assets/app-hash.js?v=abc123""#));
    }

    #[test]
    fn vm_module_import_rewrites_get_matching_cache_versions() {
        let rewrites = vec![ModuleImportRewrite {
            specifier: "./app.js".to_string(),
            replacement: "./app.js".to_string(),
        }];
        let mut versions = BTreeMap::new();
        versions.insert("assets/app.js".to_string(), "abc123".to_string());

        let versioned = version_module_import_rewrites("assets/page.js", &rewrites, &versions);

        assert_eq!(versioned[0].specifier, "./app.js");
        assert_eq!(versioned[0].replacement, "./app.js?v=abc123");
    }

    #[test]
    fn runtime_order_keeps_dynamic_pages_vm_packaged() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "assets/app.js".to_string(),
            format!(
                "{}\n{}",
                r#"import "./vendor.js"; const page = () => import("./page.js"); createApp({});"#,
                "/* @vue/runtime-dom */".repeat(14_000)
            ),
        );
        sources.insert(
            "assets/vendor.js".to_string(),
            r#"export const runtime = "@vue/runtime-dom";"#.to_string(),
        );
        sources.insert(
            "assets/page.js".to_string(),
            r#"export default { render() { return "page"; } };"#.to_string(),
        );

        let order = runtime_execution_order("assets/app.js", &sources);
        let static_targets = local_static_import_targets(&sources);
        let dynamic_targets = local_dynamic_import_targets(&sources);
        let app_rewrites = module_import_rewrites("assets/app.js", &sources);

        assert!(order.contains(&"assets/app.js".to_string()));
        assert!(order.contains(&"assets/vendor.js".to_string()));
        assert!(order.contains(&"assets/page.js".to_string()));
        assert!(static_targets.contains("assets/vendor.js"));
        assert!(dynamic_targets.contains("assets/page.js"));
        assert!(app_rewrites.iter().any(|rewrite| {
            rewrite.specifier == "./page.js" && rewrite.replacement == "./page.js"
        }));
    }
}
