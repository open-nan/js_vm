use js_token_core::EncodingConfig;
use js_vm_compiler::compile_source_to_artifact;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "pkg",
    ".issues",
    ".vendor",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Web,
    Node,
}

#[derive(Debug)]
struct PackageOptions {
    input: PathBuf,
    output: PathBuf,
    entry: Option<String>,
    clean: bool,
    platforms: Vec<Platform>,
    max_call_depth: u32,
    max_recursive_call_depth: u32,
}

#[derive(Debug)]
struct WasmOptions {
    target: String,
    release: bool,
    skip_opt: bool,
}

#[derive(Debug)]
struct ModuleOutput {
    file: String,
    bin: String,
    source: String,
    seed: String,
    externs: Vec<String>,
    bytes: usize,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("ERROR {err}");
        std::process::exit(1);
    }
}

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
            "  js-vm all [wasm options] -- <folder> [package options]",
            "",
            "Package options:",
            "  --out <dir>          Output runtime package directory. Default: <folder>/js-vm-runtime",
            "  --entry <file>       Entry js/ts file relative to folder. Default: index/main lookup",
            "  --platform <value>   web, node, or all. Default: web",
            "  --clean              Remove output directory before writing",
            "  --depth <n>          Runtime max call depth. Default: 128",
            "  --recursion <n>      Runtime max recursive call depth. Default: 8",
        ]
        .join("\n")
    );
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
    let mut platforms = vec![Platform::Web];
    let mut max_call_depth = 128;
    let mut max_recursive_call_depth = 8;
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
        platforms,
        max_call_depth,
        max_recursive_call_depth,
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

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn build_wasm(options: WasmOptions) -> Result<(), String> {
    let root = workspace_root()?;
    let started = Instant::now();
    println!("STEP Building wasm packages target={}", options.target);
    remove_dir_if_exists(&root.join("pkg/compiler"))?;
    remove_dir_if_exists(&root.join("pkg/executor"))?;
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
    run_command(
        &root,
        "wasm-pack",
        common
            .iter()
            .chain([
                &"crates/runtime".to_string(),
                &"--target".to_string(),
                &options.target,
                &"--out-dir".to_string(),
                &"../../pkg/executor".to_string(),
            ])
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )?;
    if !options.skip_opt {
        if let Some(wasm_opt) = find_command("wasm-opt") {
            optimize_wasm(&root, &wasm_opt, "pkg/compiler/js_vm_compiler_bg.wasm")?;
            optimize_wasm(&root, &wasm_opt, "pkg/executor/js_vm_runtime_bg.wasm")?;
        } else {
            println!("WARN wasm-opt not found; wasm output was built but not post-optimized");
        }
    }
    patch_wasm_bindgen_js(&root.join("pkg/compiler/js_vm_compiler.js"))?;
    patch_wasm_bindgen_js(&root.join("pkg/executor/js_vm_runtime.js"))?;
    print_size(&root, "pkg/compiler/js_vm_compiler_bg.wasm")?;
    print_size(&root, "pkg/compiler/js_vm_compiler.js")?;
    print_size(&root, "pkg/executor/js_vm_runtime_bg.wasm")?;
    print_size(&root, "pkg/executor/js_vm_runtime.js")?;
    println!(
        "OK Wasm build completed in {}ms",
        started.elapsed().as_millis()
    );
    Ok(())
}

fn compile_runtime_package(options: PackageOptions) -> Result<(), String> {
    if !options.input.is_dir() {
        return Err(format!(
            "input folder does not exist: {}",
            options.input.display()
        ));
    }
    let root = workspace_root()?;
    ensure_runtime_exists(&root)?;
    let files = list_source_files(&options.input, &options.output)?;
    if files.is_empty() {
        return Err(format!(
            "no .js or .ts files found under {}",
            options.input.display()
        ));
    }
    let entry = choose_entry(&files, options.entry.as_deref())?;
    let sources = read_sources(&options.input, &files)?;
    let order = ordered_files(&entry, &sources);
    if options.clean {
        remove_dir_if_exists(&options.output)?;
    }
    fs::create_dir_all(&options.output).map_err(|err| err.to_string())?;
    copy_runtime(&root, &options.output)?;

    let mut modules = Vec::new();
    let base_seed = EncodingConfig::default()
        .paired_seed(&[])
        .map_err(|err| err.to_string())?;
    for file in &order {
        let source = sources
            .get(file)
            .ok_or_else(|| format!("missing source: {file}"))?;
        let transformed = transform_runtime_module(file, source, &sources);
        let artifact = compile_source_to_artifact(&transformed, Some(&base_seed), &[])?;
        let seed = EncodingConfig::default()
            .paired_seed(&artifact.bytes)
            .map_err(|err| err.to_string())?;
        let bin = package_path("bytecode", file, ".bin");
        let source_path = package_path("sources", file, "");
        write_output(&options.output, &bin, &artifact.bytes)?;
        write_output(&options.output, &source_path, source.as_bytes())?;
        modules.push(ModuleOutput {
            file: file.clone(),
            bin,
            source: source_path,
            seed,
            externs: artifact.extern_slots,
            bytes: artifact.bytes.len(),
        });
    }

    let manifest = manifest_json(&entry, &modules);
    write_output(&options.output, "manifest.json", manifest.as_bytes())?;
    for platform in &options.platforms {
        match platform {
            Platform::Web => write_output(
                &options.output,
                "js-vm-loader.web.js",
                web_loader_code(&options).as_bytes(),
            )?,
            Platform::Node => write_output(
                &options.output,
                "js-vm-loader.node.mjs",
                node_loader_code(&options).as_bytes(),
            )?,
        }
    }
    match options.platforms.first() {
        Some(Platform::Web) => write_output(
            &options.output,
            "js-vm-loader.js",
            web_loader_code(&options).as_bytes(),
        )?,
        Some(Platform::Node) => write_output(
            &options.output,
            "js-vm-loader.js",
            node_loader_code(&options).as_bytes(),
        )?,
        None => {}
    }

    let total_bytes = modules.iter().map(|module| module.bytes).sum::<usize>();
    println!(
        "Compiled {} module(s) from {}",
        modules.len(),
        options.input.display()
    );
    println!("Entry: {entry}");
    println!("Output: {}", options.output.display());
    println!("Platforms: {}", platform_names(&options.platforms));
    println!("Bytecode: {total_bytes} bytes");
    Ok(())
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
    println!("RUN {command} {}", args.join(" "));
    let status = Command::new(command)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|err| format!("{command}: {err}"))?;
    if !status.success() {
        return Err(format!("{command} exited with {status}"));
    }
    Ok(())
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

fn ensure_runtime_exists(root: &Path) -> Result<(), String> {
    for file in [
        "pkg/executor/js_vm_runtime.js",
        "pkg/executor/js_vm_runtime_bg.wasm",
    ] {
        if !root.join(file).is_file() {
            return Err(format!("{file} is missing; run npm run build:wasm first"));
        }
    }
    Ok(())
}

fn copy_runtime(root: &Path, output: &Path) -> Result<(), String> {
    for (from, to) in [
        ("pkg/executor/js_vm_runtime.js", "js_vm_runtime.js"),
        (
            "pkg/executor/js_vm_runtime_bg.wasm",
            "js_vm_runtime_bg.wasm",
        ),
        ("pkg/executor/package.json", "package.json"),
    ] {
        let source = root.join(from);
        if source.is_file() {
            fs::copy(&source, output.join(to))
                .map_err(|err| format!("copy {}: {err}", source.display()))?;
        }
    }
    Ok(())
}

fn list_source_files(input: &Path, output: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    let output_top = output
        .strip_prefix(input)
        .ok()
        .and_then(|path| path.components().next())
        .and_then(|component| component.as_os_str().to_str())
        .map(str::to_string);
    walk_sources(input, input, output_top.as_deref(), &mut files)?;
    files.sort();
    Ok(files)
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
        path.extension().and_then(OsStr::to_str),
        Some("js") | Some("ts")
    )
}

fn choose_entry(files: &[String], requested: Option<&str>) -> Result<String, String> {
    if let Some(entry) = requested {
        if files.iter().any(|file| file == entry) {
            return Ok(entry.to_string());
        }
        return Err(format!("entry not found in input files: {entry}"));
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

fn read_sources(input: &Path, files: &[String]) -> Result<BTreeMap<String, String>, String> {
    files
        .iter()
        .map(|file| {
            let full_path = input.join(file);
            let source = fs::read_to_string(&full_path)
                .map_err(|err| format!("{}: {err}", full_path.display()))?;
            Ok((file.clone(), source))
        })
        .collect()
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
    source
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with("import") {
                return None;
            }
            quoted_specifier(trimmed)
        })
        .collect()
}

fn quoted_specifier(line: &str) -> Option<String> {
    let quote_index = line.find(['"', '\''])?;
    let quote = line.as_bytes()[quote_index] as char;
    let rest = &line[quote_index + 1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn resolve_virtual_import(
    from_file: &str,
    specifier: &str,
    sources: &BTreeMap<String, String>,
) -> Option<String> {
    if !specifier.starts_with("./") && !specifier.starts_with("../") {
        return None;
    }
    let base = normalize_virtual_path(&format!("{}/{}", dirname(from_file), specifier));
    [
        base.clone(),
        format!("{base}.ts"),
        format!("{base}.js"),
        format!("{base}/index.ts"),
        format!("{base}/index.js"),
    ]
    .into_iter()
    .find(|candidate| sources.contains_key(candidate))
}

fn transform_runtime_module(
    file: &str,
    source: &str,
    sources: &BTreeMap<String, String>,
) -> String {
    let module_key = js_string(file);
    let mut out = vec![
        "globalThis.__JS_VM_MODULES__ = globalThis.__JS_VM_MODULES__ || {};".to_string(),
        format!("globalThis.__JS_VM_MODULES__[{module_key}] = (() => {{"),
        "  const __vm_exports = {};".to_string(),
    ];
    let mut deferred_exports = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") {
            if let Some(specifier) = quoted_specifier(trimmed) {
                if let Some(resolved) = resolve_virtual_import(file, &specifier, sources) {
                    if let Some(clause) = import_clause(trimmed) {
                        for binding in import_binding_lines(&clause, &resolved) {
                            out.push(format!("  {binding}"));
                        }
                    }
                    continue;
                }
            }
        }
        if let Some((kind, name, rest)) = export_variable(line) {
            out.push(format!("{kind} {name}{rest}"));
            out.push(format!("  __vm_exports[{}] = {name};", js_string(&name)));
            continue;
        }
        if let Some((kind, name, rest)) = export_named_decl(line) {
            out.push(format!("{kind} {name}{rest}"));
            deferred_exports.push(format!("__vm_exports[{}] = {name};", js_string(&name)));
            continue;
        }
        if let Some(expr) = trimmed.strip_prefix("export default ") {
            out.push(format!(
                "  const __vm_default = {};",
                expr.trim_end_matches(';')
            ));
            out.push("  __vm_exports.default = __vm_default;".to_string());
            continue;
        }
        if let Some(list) = trimmed
            .strip_prefix("export {")
            .and_then(|value| value.strip_suffix("};").or_else(|| value.strip_suffix('}')))
        {
            for part in list.split(',') {
                let item = part.trim();
                if item.is_empty() {
                    continue;
                }
                let mut pieces = item.split(" as ");
                let local = pieces.next().unwrap_or("").trim();
                let exported = pieces.next().unwrap_or(local).trim();
                out.push(format!(
                    "  __vm_exports[{}] = {local};",
                    js_string(exported)
                ));
            }
            continue;
        }
        out.push(format!("  {line}"));
    }
    for export in deferred_exports {
        out.push(format!("  {export}"));
    }
    out.push("  return __vm_exports;".to_string());
    out.push("})();".to_string());
    out.push(format!("globalThis.__JS_VM_MODULES__[{module_key}];"));
    out.join("\n")
}

fn import_clause(line: &str) -> Option<String> {
    let from_index = line.find(" from ")?;
    Some(line["import ".len()..from_index].trim().to_string())
}

fn import_binding_lines(clause: &str, resolved: &str) -> Vec<String> {
    let module = format!("globalThis.__JS_VM_MODULES__[{}]", js_string(resolved));
    let clause = clause.trim();
    if let Some(rest) = clause.strip_prefix("* as ") {
        return vec![format!("const {} = {module};", rest.trim())];
    }
    if let (Some(start), Some(end)) = (clause.find('{'), clause.rfind('}')) {
        let mut lines = Vec::new();
        let default_part = clause[..start].trim().trim_end_matches(',').trim();
        if !default_part.is_empty() {
            lines.push(format!("const {default_part} = {module}.default;"));
        }
        for part in clause[start + 1..end].split(',') {
            let item = part.trim();
            if item.is_empty() {
                continue;
            }
            let mut pieces = item.split(" as ");
            let imported = pieces.next().unwrap_or("").trim();
            let local = pieces.next().unwrap_or(imported).trim();
            lines.push(format!(
                "const {local} = {module}[{}];",
                js_string(imported)
            ));
        }
        return lines;
    }
    vec![format!("const {clause} = {module}.default;")]
}

fn export_variable(line: &str) -> Option<(String, String, String)> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    for kind in ["const", "let", "var"] {
        let prefix = format!("export {kind} ");
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let name = read_ident(rest)?;
            return Some((
                format!("{indent}{kind}"),
                name.to_string(),
                rest[name.len()..].to_string(),
            ));
        }
    }
    None
}

fn export_named_decl(line: &str) -> Option<(String, String, String)> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    for kind in ["function", "class"] {
        let prefix = format!("export {kind} ");
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let name = read_ident(rest)?;
            return Some((
                format!("{indent}{kind}"),
                name.to_string(),
                rest[name.len()..].to_string(),
            ));
        }
    }
    None
}

fn read_ident(value: &str) -> Option<&str> {
    let len = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '$')
        .last()
        .map(|(index, ch)| index + ch.len_utf8())?;
    Some(&value[..len])
}

fn manifest_json(entry: &str, modules: &[ModuleOutput]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"format\": \"js-vm-runtime-package\",\n");
    out.push_str("  \"version\": 1,\n");
    out.push_str(&format!("  \"entry\": {},\n", js_string(entry)));
    out.push_str(&format!("  \"moduleCount\": {},\n", modules.len()));
    out.push_str("  \"modules\": [\n");
    for (index, module) in modules.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"file\": {},\n", js_string(&module.file)));
        out.push_str(&format!("      \"bin\": {},\n", js_string(&module.bin)));
        out.push_str(&format!(
            "      \"source\": {},\n",
            js_string(&module.source)
        ));
        out.push_str(&format!("      \"seed\": {},\n", js_string(&module.seed)));
        out.push_str("      \"externs\": [");
        for (extern_index, external) in module.externs.iter().enumerate() {
            if extern_index > 0 {
                out.push_str(", ");
            }
            out.push_str(&js_string(external));
        }
        out.push_str("]\n");
        out.push_str("    }");
        if index + 1 < modules.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

fn web_loader_code(options: &PackageOptions) -> String {
    format!(
        r#"// JS VM web runtime loader.
import init, {{
  js_execute_bytes_with_seed,
  js_execute_bytes_with_seed_and_limits,
}} from './js_vm_runtime.js';

const maxCallDepth = {max_call_depth};
const maxRecursiveCallDepth = {max_recursive_call_depth};

async function loadJson(url) {{
  const response = await fetch(url);
  if (!response.ok) throw new Error(`failed to load ${{url}}: ${{response.status}} ${{response.statusText}}`);
  return response.json();
}}

async function loadBin(url) {{
  const response = await fetch(url);
  if (!response.ok) throw new Error(`failed to load ${{url}}: ${{response.status}} ${{response.statusText}}`);
  return new Uint8Array(await response.arrayBuffer());
}}

function resolveExternal(name) {{
  return String(name).split('.').reduce((value, part) => value == null ? undefined : value[part], globalThis);
}}

globalThis.__JS_VM_MODULES__ = globalThis.__JS_VM_MODULES__ || {{}};
await init(new URL('./js_vm_runtime_bg.wasm', import.meta.url));
const manifest = await loadJson(new URL('./manifest.json', import.meta.url));
const execute = typeof js_execute_bytes_with_seed_and_limits === 'function'
  ? (bytes, seed, externs) => js_execute_bytes_with_seed_and_limits(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth)
  : (bytes, seed, externs) => js_execute_bytes_with_seed(bytes, seed, externs);

for (const module of manifest.modules) {{
  const bytes = await loadBin(new URL(module.bin, import.meta.url));
  module.result = execute(bytes, module.seed, module.externs.map(resolveExternal));
}}

export default globalThis.__JS_VM_MODULES__[manifest.entry];
"#,
        max_call_depth = options.max_call_depth,
        max_recursive_call_depth = options.max_recursive_call_depth
    )
}

fn node_loader_code(options: &PackageOptions) -> String {
    format!(
        r#"// JS VM Node.js runtime loader.
import {{ readFile }} from 'node:fs/promises';
import init, {{
  js_execute_bytes_with_seed,
  js_execute_bytes_with_seed_and_limits,
}} from './js_vm_runtime.js';

const maxCallDepth = {max_call_depth};
const maxRecursiveCallDepth = {max_recursive_call_depth};

async function loadJson(url) {{
  return JSON.parse(await readFile(url, 'utf8'));
}}

async function loadBin(url) {{
  return new Uint8Array(await readFile(url));
}}

function resolveExternal(name) {{
  return String(name).split('.').reduce((value, part) => value == null ? undefined : value[part], globalThis);
}}

globalThis.__JS_VM_MODULES__ = globalThis.__JS_VM_MODULES__ || {{}};
await init({{ module_or_path: await readFile(new URL('./js_vm_runtime_bg.wasm', import.meta.url)) }});
const manifest = await loadJson(new URL('./manifest.json', import.meta.url));
const execute = typeof js_execute_bytes_with_seed_and_limits === 'function'
  ? (bytes, seed, externs) => js_execute_bytes_with_seed_and_limits(bytes, seed, externs, maxCallDepth, maxRecursiveCallDepth)
  : (bytes, seed, externs) => js_execute_bytes_with_seed(bytes, seed, externs);

for (const module of manifest.modules) {{
  const bytes = await loadBin(new URL(module.bin, import.meta.url));
  module.result = execute(bytes, module.seed, module.externs.map(resolveExternal));
}}

export default globalThis.__JS_VM_MODULES__[manifest.entry];
"#,
        max_call_depth = options.max_call_depth,
        max_recursive_call_depth = options.max_recursive_call_depth
    )
}

fn write_output(output: &Path, relative: &str, bytes: &[u8]) -> Result<(), String> {
    let path = output.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    fs::write(&path, bytes).map_err(|err| format!("{}: {err}", path.display()))
}

fn package_path(prefix: &str, file: &str, suffix: &str) -> String {
    format!("{prefix}/{}{suffix}", normalize_virtual_path(file))
}

fn dirname(file: &str) -> &str {
    file.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
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

fn js_string(value: &str) -> String {
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
