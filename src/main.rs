// main.rs - CLI driver for Voxlang compiler. Supports check, build, run, test, update, index, clean, lsp, shell.

mod std;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::std::walkdir::WalkDir;

pub mod frontend {
    pub mod lexer;
    pub mod span;
    pub mod token;
}
pub mod bridge;
pub mod codegen;
pub mod comptime;
pub mod desugar;
pub mod diagnostic;
pub mod discovery;
pub mod import;
pub mod index;
pub mod lsp;
pub mod module;
pub mod parser;
pub mod refinement;
pub mod semantics;
pub mod shell;
pub mod ui;
pub mod update;
pub mod watch;

use codegen::CodegenEngine;
use core::cmp;
use desugar::desugar;
use diagnostic::{
    emit_phase_update, flush_logs, get_exit_code, set_global_debug, set_output_format,
    spawn_ui_thread, stop_ui_thread,
};
use frontend::lexer::Lexer;
use module::ModuleResolver;
use parser::ASTNode;
use parser::Parser;
use semantics::SemanticAnalyzer;

// -----------------------------------------------------------------------------
// Cache configuration
// -----------------------------------------------------------------------------
#[derive(Clone, Copy)]
pub struct CacheConfig {
    pub no_cache: bool,
    pub reuse_proofs: bool,
    pub reuse_bitcode: bool,
    pub offline: bool,
    pub trust_modules: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            no_cache: false,
            reuse_proofs: true,
            reuse_bitcode: true,
            offline: false,
            trust_modules: false,
        }
    }
}

// -----------------------------------------------------------------------------
// CLI Definition
// -----------------------------------------------------------------------------

#[derive(Debug)]
struct Cli {
    debug: bool,
    target: String,
    output_format: diagnostic::OutputFormat,
    gpu: Option<String>,
    gpu_arch: Option<String>,
    no_cache: bool,
    reuse_proofs: bool,
    reuse_bitcode: bool,
    offline: bool,
    trust_modules: bool,
    command: Commands,
}

#[derive(Debug)]
enum Commands {
    Check { file: String },
    Build { file: String },
    Run { file: String },
    Test { path: String },
    Update { write: bool, path: String },
    Index { path: String, watch: bool },
    Clean,
    Lsp,
    Shell,
}

fn host_triple() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (arch, os) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc".to_string(),
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu".to_string(),
        ("x86_64", "macos") => "x86_64-apple-darwin".to_string(),
        (_, _) => {
            eprintln!("Unsupported host platform: {} {}", arch, os);
            std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
        }
    }
}

fn print_usage() {
    eprintln!(
        r#"Usage: vox [OPTIONS] <COMMAND>

Commands:
  check   <file>          Only perform lexical, syntactic, and semantic analysis
  build   <file>          Compile to native binary
  run     <file>          Compile and execute
  test    [path]          Run conformance test suite (default: src/Examples)
  update  [--write] [path] Update remote dependencies (default: .)
  index   [--watch] [path] Generate symbol index (default: .)
  clean                   Remove target/ directory
  lsp                     Start Language Server Protocol server
  shell                   Start interactive REPL shell

Global Options:
  --debug                 Enable debug output (lexer, parser, codegen)
  --target <TRIPLE>       Target triple (default: host triple)
  --output-format <FORMAT> Specify terminal formatting ("pretty", "json", "auto")
  --gpu <BACKEND>         GPU backend: "cuda" or "hip"
  --gpu-arch <ARCH>       GPU architecture (e.g., sm_70, gfx1200)
  --no-cache              Ignore all caches
  --reuse-proofs          Reuse cached Z3 proofs (default: true)
  --reuse-bitcode         Reuse cached LLVM bitcode (default: true)
  --offline               Do not download remote modules; fail if not cached
  --trust-modules         Allow @comptime execution in imported modules (security risk)
"#
    );
}

fn parse_args() -> Cli {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut debug = false;
    let mut target = host_triple();
    let mut output_format = diagnostic::OutputFormat::Auto;
    let mut gpu: Option<String> = None;
    let mut gpu_arch: Option<String> = None;
    let mut no_cache = false;
    let mut reuse_proofs = true;
    let mut reuse_bitcode = true;
    let mut offline = false;
    let mut trust_modules = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--debug" {
            debug = true;
            i += 1;
        } else if arg == "--target" {
            if i + 1 >= args.len() {
                eprintln!("Error: --target requires a value");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            target = args[i + 1].clone();
            i += 2;
        } else if arg == "--output-format" {
            if i + 1 >= args.len() {
                eprintln!("Error: --output-format requires a value (pretty, json, or auto)");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            let val = args[i + 1].as_str();
            output_format = match val {
                "pretty" => diagnostic::OutputFormat::Pretty,
                "json" => diagnostic::OutputFormat::Json,
                "auto" => diagnostic::OutputFormat::Auto,
                _ => {
                    eprintln!(
                        "Error: Invalid output format '{}'. Use 'pretty', 'json', or 'auto'.",
                        val
                    );
                    print_usage();
                    std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                }
            };
            i += 2;
        } else if arg == "--gpu" {
            if i + 1 >= args.len() {
                eprintln!("Error: --gpu requires a backend (cuda or hip)");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            let backend = args[i + 1].clone();
            if backend != "cuda" && backend != "hip" {
                eprintln!("Error: --gpu must be 'cuda' or 'hip', got '{}'", backend);
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            gpu = Some(backend);
            i += 2;
        } else if arg == "--gpu-arch" {
            if i + 1 >= args.len() {
                eprintln!("Error: --gpu-arch requires a value");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            gpu_arch = Some(args[i + 1].clone());
            i += 2;
        } else if arg == "--no-cache" {
            no_cache = true;
            i += 1;
        } else if arg == "--reuse-proofs" {
            if i + 1 < args.len() && args[i + 1] == "false" {
                reuse_proofs = false;
                i += 2;
            } else {
                reuse_proofs = true;
                i += 1;
            }
        } else if arg == "--reuse-bitcode" {
            if i + 1 < args.len() && args[i + 1] == "false" {
                reuse_bitcode = false;
                i += 2;
            } else {
                reuse_bitcode = true;
                i += 1;
            }
        } else if arg == "--offline" {
            offline = true;
            i += 1;
        } else if arg == "--trust-modules" {
            trust_modules = true;
            i += 1;
        } else if arg.starts_with('-') {
            eprintln!("Error: Unknown option '{}'", arg);
            print_usage();
            std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
        } else {
            break;
        }
    }

    let remaining = &args[i..];
    if remaining.is_empty() {
        eprintln!("Error: No command specified");
        print_usage();
        std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
    }

    let command_str = &remaining[0];
    let command_args = &remaining[1..];

    let command = match command_str.as_str() {
        "check" => {
            if command_args.is_empty() {
                eprintln!("Error: 'check' requires a file argument");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            let file = command_args[0].clone();
            if command_args.len() > 1 {
                eprintln!("Warning: extra arguments after file ignored");
            }
            Commands::Check { file }
        }
        "build" => {
            if command_args.is_empty() {
                eprintln!("Error: 'build' requires a file argument");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            let file = command_args[0].clone();
            if command_args.len() > 1 {
                eprintln!("Warning: extra arguments after file ignored");
            }
            Commands::Build { file }
        }
        "run" => {
            if command_args.is_empty() {
                eprintln!("Error: 'run' requires a file argument");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            let file = command_args[0].clone();
            if command_args.len() > 1 {
                eprintln!("Warning: extra arguments after file ignored");
            }
            Commands::Run { file }
        }
        "test" => {
            let path = if command_args.is_empty() {
                "src/Examples".to_string()
            } else {
                command_args[0].clone()
            };
            if command_args.len() > 1 {
                eprintln!("Warning: extra arguments after path ignored");
            }
            Commands::Test { path }
        }
        "update" => {
            let mut write = false;
            let mut path = ".".to_string();
            let mut j = 0;
            while j < command_args.len() {
                let arg = &command_args[j];
                if arg == "--write" {
                    write = true;
                    j += 1;
                } else if !arg.starts_with('-') {
                    path = arg.clone();
                    j += 1;
                } else {
                    eprintln!("Error: Unknown update option '{}'", arg);
                    print_usage();
                    std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                }
            }
            Commands::Update { write, path }
        }
        "index" => {
            let mut watch = false;
            let mut path = ".".to_string();
            let mut j = 0;
            while j < command_args.len() {
                let arg = &command_args[j];
                if arg == "--watch" {
                    watch = true;
                    j += 1;
                } else if !arg.starts_with('-') {
                    path = arg.clone();
                    j += 1;
                } else {
                    eprintln!("Error: Unknown index option '{}'", arg);
                    print_usage();
                    std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                }
            }
            Commands::Index { path, watch }
        }
        "clean" => {
            if !command_args.is_empty() {
                eprintln!("Warning: 'clean' takes no arguments; ignoring extra");
            }
            Commands::Clean
        }
        "lsp" => {
            if !command_args.is_empty() {
                eprintln!("Warning: 'lsp' takes no arguments; ignoring extra");
            }
            Commands::Lsp
        }
        "shell" => {
            if !command_args.is_empty() {
                eprintln!("Warning: 'shell' takes no arguments; ignoring extra");
            }
            Commands::Shell
        }
        _ => {
            eprintln!("Error: Unknown command '{}'", command_str);
            print_usage();
            std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
        }
    };

    Cli {
        debug,
        target,
        output_format,
        gpu,
        gpu_arch,
        no_cache,
        reuse_proofs,
        reuse_bitcode,
        offline,
        trust_modules,
        command,
    }
}

// -----------------------------------------------------------------------------
// Helper functions
// -----------------------------------------------------------------------------

pub fn get_output_dir(profile: &str) -> PathBuf {
    let mut dir = PathBuf::from("target");
    dir.push(profile);
    std::fs::create_dir_all(&dir).expect("Failed to create target directory");
    dir
}

pub fn get_cache_dir() -> PathBuf {
    let dir = get_output_dir("debug").join(".cache");
    std::fs::create_dir_all(&dir).expect("Failed to create cache directory");
    dir
}

fn has_kernel(node: &ASTNode, device_triple: &mut Option<String>) -> bool {
    match node {
        ASTNode::Program(stmts, _) => stmts.iter().any(|s| has_kernel(s, device_triple)),
        ASTNode::KernelFn {
            device_triple: dt, ..
        } => {
            if device_triple.is_none() {
                *device_triple = Some(dt.clone());
            }
            true
        }
        _ => false,
    }
}

#[allow(dead_code)]
fn detect_gpu_from_source(src_path: &Path) -> (bool, Option<String>) {
    let source_code = match std::fs::read_to_string(src_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: cannot read source for GPU detection: {}", e);
            return (false, None);
        }
    };
    let mut lexer = Lexer::new(&source_code);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(()) => {
            eprintln!("Warning: lexing failed during GPU detection");
            return (false, None);
        }
    };
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    let mut device_triple = None;
    let has_gpu = has_kernel(&ast, &mut device_triple);
    (has_gpu, device_triple)
}

// -----------------------------------------------------------------------------
// Compilation pipeline
// -----------------------------------------------------------------------------

pub struct CompilationResult {
    pub _ast: ASTNode,
    pub semantic_ok: bool,
    pub llvm_ir: String,
    pub has_gpu: bool,
    pub _device_triple: Option<String>,
}

pub fn compile_source(
    file_path: &str,
    debug: bool,
    target: &str,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
    config: &CacheConfig,
) -> Result<CompilationResult, String> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!(
            "Target source file '{}' does not exist.",
            file_path
        ));
    }
    if path.extension().and_then(|s| s.to_str()) != Some("vx") {
        return Err("Input file must have .vx extension.".to_string());
    }

    emit_phase_update("Loading source file", 5);
    let user_source = match module::read_source_file(path) {
        Ok(s) => s,
        Err(e) => return Err(format!("Failed to read file: {}", e)),
    };
    let full_source = user_source;

    if debug {
        eprintln!("[DEBUG] Source length: {} bytes", full_source.len());
        eprintln!("[DEBUG] ===== SOURCE BEGIN =====");
        eprintln!("{}", &full_source[..full_source.len().min(500)]);
        if full_source.len() > 500 {
            eprintln!("... (truncated)");
        }
        eprintln!("[DEBUG] ===== SOURCE END =====");
    }

    emit_phase_update("Lexical analysis", 10);
    let mut lexer = Lexer::new(&full_source);
    lexer.set_debug(debug);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(()) => return Err("Lexical analysis failed".to_string()),
    };

    emit_phase_update("Syntactic parsing", 25);
    if debug {
        eprintln!("\n=== TOKEN STREAM ===");
        for (i, t) in tokens.iter().enumerate() {
            eprintln!("  {:3}: {:?} at {:?}", i, t.kind, t.span);
        }
        eprintln!();
    }

    let mut parser = Parser::new(&tokens);
    parser.set_debug(debug);
    let ast = parser.parse();
    if parser.has_errors() {
        return Err("Parsing failed due to syntax errors".to_string());
    }

    eprintln!("\n[DEBUG] === AFTER PARSING ===");
    if let ASTNode::Program(stmts, _) = &ast {
        eprintln!("Parsed program has {} statements", stmts.len());
        for (idx, stmt) in stmts.iter().enumerate() {
            if let ASTNode::FunctionDef { name, .. } = stmt {
                eprintln!("  [{}] FunctionDef: {}", idx, name);
            } else {
                eprintln!("  [{}] {:?}", idx, stmt);
            }
        }
    } else {
        eprintln!("Parsed AST is not a Program node! {:?}", ast);
    }

    emit_phase_update("Desugaring syntactic sugar", 30);
    let desugared_ast = desugar(ast);

    eprintln!("\n[DEBUG] === AFTER DESUGARING ===");
    if let ASTNode::Program(stmts, _) = &desugared_ast {
        eprintln!("Desugared program has {} statements", stmts.len());
        for (idx, stmt) in stmts.iter().enumerate() {
            if let ASTNode::FunctionDef { name, .. } = stmt {
                eprintln!("  [{}] FunctionDef: {}", idx, name);
            } else {
                eprintln!("  [{}] {:?}", idx, stmt);
            }
        }
    } else {
        eprintln!("Desugared AST is not a Program node! {:?}", desugared_ast);
    }

    emit_phase_update("Import resolution & semantic analysis", 40);

    let mut resolver = ModuleResolver::new(path, config);
    let mut semantic = SemanticAnalyzer::with_resolver(&mut resolver);

    use bridge::ForeignFunction;
    let mut ffi_functions = vec![
        ForeignFunction {
            name: "puts".to_string(),
            param_types: vec!["i8*".to_string()],
            return_type: "i32".to_string(),
        },
        ForeignFunction {
            name: "exit".to_string(),
            param_types: vec!["i32".to_string()],
            return_type: "void".to_string(),
        },
        ForeignFunction {
            name: "vox_as_ptr".to_string(),
            param_types: vec!["*const u8".to_string(), "usize".to_string()],
            return_type: "*const u8".to_string(),
        },
        ForeignFunction {
            name: "vox_str_len".to_string(),
            param_types: vec!["*const u8".to_string(), "usize".to_string()],
            return_type: "i32".to_string(),
        },
        ForeignFunction {
            name: "vox_eprint_str".to_string(),
            param_types: vec!["*const u8".to_string(), "usize".to_string()],
            return_type: "i32".to_string(),
        },
        ForeignFunction {
            name: "vox_eprintln_str".to_string(),
            param_types: vec!["*const u8".to_string(), "usize".to_string()],
            return_type: "i32".to_string(),
        },
        ForeignFunction {
            name: "vox_print_str".to_string(),
            param_types: vec!["*const u8".to_string(), "usize".to_string()],
            return_type: "i32".to_string(),
        },
        ForeignFunction {
            name: "vox_println_str".to_string(),
            param_types: vec!["*const u8".to_string(), "usize".to_string()],
            return_type: "i32".to_string(),
        },
    ];
    if target.contains("windows") {
        ffi_functions.push(ForeignFunction {
            name: "MessageBoxA".to_string(),
            param_types: vec![
                "i8*".to_string(),
                "i8*".to_string(),
                "i8*".to_string(),
                "i32".to_string(),
            ],
            return_type: "i32".to_string(),
        });
    }
    semantic.register_ffi_signatures(ffi_functions);

    eprintln!("\n[DEBUG] === BEFORE SEMANTIC ANALYSIS ===");
    let semantic_ok = semantic.check(&desugared_ast);
    eprintln!("[DEBUG] Semantic analysis returned: {}", semantic_ok);

    if !semantic_ok {
        return Ok(CompilationResult {
            _ast: desugared_ast,
            semantic_ok: false,
            llvm_ir: String::new(),
            has_gpu: false,
            _device_triple: None,
        });
    }

    let resolved_types = semantic.resolved_variable_types.clone();
    let type_aliases = semantic.symbols.type_aliases.clone();
    let imported_modules = semantic.take_imported_modules();
    drop(semantic);

    let mut device_triple = None;
    let has_gpu = has_kernel(&desugared_ast, &mut device_triple);

    emit_phase_update("IR generation", 70);

    let forced_device_triple = gpu_backend.map(|backend| {
        let arch = gpu_arch.unwrap_or_else(|| match backend {
            "cuda" => "sm_70",
            "hip" => "gfx1200",
            _ => unreachable!(),
        });
        match backend {
            "cuda" => format!("nvptx64-nvidia-cuda--{}", arch),
            "hip" => format!("amdgcn-amd-amdhsa--{}", arch),
            _ => unreachable!(),
        }
    });

    let mut codegen = CodegenEngine::new(target);
    codegen.set_debug(debug);
    codegen.set_gpu_mode(gpu_backend);
    if let Some(triple) = forced_device_triple {
        codegen.set_device_triple_override(triple);
    }
    codegen.set_resolved_types(resolved_types);
    codegen.set_type_aliases(type_aliases);
    for (alias, module_ast) in imported_modules {
        codegen.add_imported_module_ast(alias, module_ast);
    }

    match discovery::find_llvm_tools() {
        Ok(llvm) => {
            codegen.set_llvm_paths(llvm.clang, llvm.llc, llvm.lld);
        }
        Err(e) => {
            eprintln!("Warning: LLVM tools auto-discovery failed: {}", e);
            eprintln!("Falling back to 'clang', 'llc', 'ld.lld' from PATH.");
        }
    }

    eprintln!("\n[DEBUG] === BEFORE CODEGEN ===");
    if let ASTNode::Program(stmts, _) = &desugared_ast {
        eprintln!("Codegen input program has {} statements", stmts.len());
        for (idx, stmt) in stmts.iter().enumerate() {
            if let ASTNode::FunctionDef { name, .. } = stmt {
                eprintln!("  [{}] FunctionDef: {}", idx, name);
            } else {
                eprintln!("  [{}] {:?}", idx, stmt);
            }
        }
    } else {
        eprintln!("Codegen input is not a Program node! {:?}", desugared_ast);
    }

    let llvm_ir = codegen.generate(&desugared_ast);
    if llvm_ir.is_empty() && codegen.has_error {
        return Err("IR generation failed".to_string());
    }

    emit_phase_update("Code generation complete", 100);

    Ok(CompilationResult {
        _ast: desugared_ast,
        semantic_ok,
        llvm_ir,
        has_gpu,
        _device_triple: device_triple,
    })
}

pub fn check_file(file_path: &str, debug: bool, target: &str, config: &CacheConfig) -> bool {
    match compile_source(file_path, debug, target, None, None, config) {
        Ok(result) => result.semantic_ok,
        Err(e) => {
            eprintln!("Fatal error: {}", e);
            false
        }
    }
}

// -----------------------------------------------------------------------------
// Command implementations
// -----------------------------------------------------------------------------

fn cmd_check(
    file: &str,
    debug: bool,
    target: &str,
    gpu: Option<&str>,
    gpu_arch: Option<&str>,
    config: &CacheConfig,
) -> i32 {
    emit_phase_update("Checking program", 0);
    match compile_source(file, debug, target, gpu, gpu_arch, config) {
        Ok(result) => {
            if result.semantic_ok {
                println!("Check passed.");
                diagnostic::exit_code::SUCCESS
            } else {
                diagnostic::exit_code::SEMANTIC_ERROR
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            diagnostic::exit_code::GENERIC_ERROR
        }
    }
}

// FIXED: test command runs example files from src/Examples (or fallback)
fn cmd_test(path_str: &str, config: &CacheConfig) -> i32 {
    let examples_dir = if Path::new(path_str).exists() {
        Path::new(path_str)
    } else if Path::new("src/Examples").exists() {
        Path::new("src/Examples")
    } else if Path::new("src/examples").exists() {
        Path::new("src/examples")
    } else if Path::new("examples").exists() {
        Path::new("examples")
    } else {
        eprintln!("Error: Examples directory '{}' not found.", path_str);
        return diagnostic::exit_code::IO_ERROR;
    };

    let current_exe =
        std::env::current_exe().expect("Failed to locate current compiler executable");

    // Recursively collect all .vx files, but skip those in a 'lib' subdirectory
    let mut test_files = Vec::new();
    for entry in WalkDir::new(examples_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            path.extension().and_then(|s| s.to_str()) == Some("vx")
                && !path.components().any(|c| c.as_os_str() == "lib")
        })
    {
        test_files.push(entry.path().to_path_buf());
    }

    if test_files.is_empty() {
        println!("No .vx files found in {}", examples_dir.display());
        return diagnostic::exit_code::SUCCESS;
    }

    let mut total_tests = 0;
    let mut passed_tests = 0;

    println!("==================================================");
    println!("       VOXLANG COMPILER CONFORMANCE SUITE        ");
    println!("==================================================\n");

    for path in test_files {
        let file_name = path.file_name().unwrap().to_string_lossy();
        if file_name.contains("gpu") {
            continue;
        }

        total_tests += 1;
        let rel_path = path.strip_prefix(examples_dir).unwrap_or(&path);
        print!("test {:<30} ... ", rel_path.display());
        std::io::Write::flush(&mut std::io::stdout()).unwrap();

        let mut link_cmd = Command::new(&current_exe);
        link_cmd.arg("run").arg(&path);
        if config.no_cache {
            link_cmd.arg("--no-cache");
        }

        let output = link_cmd
            .output()
            .expect("Failed to execute internal compiler run pipeline");

        let stderr_str = String::from_utf8_lossy(&output.stderr);
        let has_error = stderr_str.to_lowercase().contains("error:");
        let passed = output.status.success() || (!has_error);

        if passed {
            println!("✅ PASSED");
            passed_tests += 1;
        } else {
            println!("❌ FAILED");
            eprintln!("\n--- [STDERR OUTPUT: {}] ---", file_name);
            eprintln!("{}", stderr_str.trim_end());
            eprintln!("--------------------------------------------------\n");
        }
    }

    println!("\nResult: {}/{} tests passed.", passed_tests, total_tests);
    if passed_tests < total_tests {
        diagnostic::exit_code::GENERIC_ERROR
    } else {
        diagnostic::exit_code::SUCCESS
    }
}

fn cmd_build(
    file: &str,
    debug: bool,
    target: &str,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
    config: &CacheConfig,
) -> i32 {
    let compile_result = match compile_source(file, debug, target, gpu_backend, gpu_arch, config) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", e);
            return diagnostic::exit_code::GENERIC_ERROR;
        }
    };
    if !compile_result.semantic_ok {
        return diagnostic::exit_code::SEMANTIC_ERROR;
    }

    emit_phase_update("Writing IR and linking", 85);

    let path = Path::new(file);
    let file_name = path.file_stem().unwrap().to_str().unwrap();
    let out_dir = get_output_dir("debug");
    let debug_ir_path = out_dir.join(format!("{}.ll", file_name));
    if let Err(e) = fs::write(&debug_ir_path, compile_result.llvm_ir.as_bytes()) {
        eprintln!("Warning: failed to write .ll file: {}", e);
    }

    let exe_filename = if target.contains("windows") {
        format!("{}.exe", file_name)
    } else {
        file_name.to_string()
    };
    let exe_path = out_dir.join(&exe_filename);
    println!(
        "-> Invoking native toolchain backend for target {}...",
        target
    );

    let has_gpu = compile_result.has_gpu;
    let llvm_tools = discovery::find_llvm_tools().unwrap_or_else(|e| {
        eprintln!("Error: LLVM tools not found: {}", e);
        std::process::exit(diagnostic::exit_code::LINKER_ERROR);
    });

    let (linker, linker_is_hip) = if has_gpu && gpu_backend == Some("hip") {
        let hipcc = discovery::find_gpu_backend("hip")
            .and_then(|gpu| {
                gpu.hip_path.map(|p| {
                    p.join("bin")
                        .join("hipcc")
                        .with_extension(env::consts::EXE_EXTENSION)
                })
            })
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("hipcc"));
        (hipcc, true)
    } else {
        (llvm_tools.clang, false)
    };

    let actual_ll_path = debug_ir_path.clone();
    let mut link_cmd = Command::new(&linker);
    link_cmd.arg(&actual_ll_path).arg("-o").arg(&exe_path);

    let mut link_args = Vec::<String>::new();

    let target_triple = if target.contains("windows") && target.contains("msvc") {
        "x86_64-pc-windows-msvc"
    } else if target.contains("windows") && target.contains("gnu") {
        "x86_64-pc-windows-gnu"
    } else {
        target
    };

    let cache_dir = get_output_dir("debug").join(".vox_rt_cache");
    std::fs::create_dir_all(&cache_dir).expect("Failed to create runtime cache dir");

    let lib_name = "vox_rt";
    let lib_extension = if target_triple.contains("msvc") {
        ".lib"
    } else {
        ".a"
    };
    let static_lib = cache_dir.join(format!("{}{}", lib_name, lib_extension));

    // vox_rt.rs is now located at src/vox_rt.rs
    let need_rebuild = !static_lib.exists()
        || fs::metadata("src/vox_rt.rs")
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| {
                static_lib
                    .metadata()
                    .ok()
                    .map(|m| m.modified().unwrap() < t)
            })
            .unwrap_or(true);

    if need_rebuild {
        let mut rustc_cmd = Command::new("rustc");
        rustc_cmd
            .arg("--crate-type=staticlib")
            .arg("--target")
            .arg(target_triple)
            .arg("-C")
            .arg("panic=abort")
            .arg("-C")
            .arg("opt-level=3")
            .arg("-C")
            .arg("overflow-checks=off")
            .arg("--out-dir")
            .arg(&cache_dir)
            .arg("src/vox_rt.rs");
        if target_triple.contains("msvc") {
            rustc_cmd.arg("-C").arg("target-feature=+crt-static");
        }
        if gpu_backend == Some("hip") {
            rustc_cmd.arg("--cfg").arg("feature=\"vox_gpu_enabled\"");
        }
        let status = rustc_cmd.status().expect("failed to run rustc");
        if !status.success() {
            eprintln!("Compilation of vox_rt.rs failed");
            return diagnostic::exit_code::LINKER_ERROR;
        }
    }

    link_args.push(format!("-L{}", cache_dir.to_string_lossy()));
    link_args.push(format!("-l{}", lib_name));

    if let Some(sysroot) = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
    {
        let lib_path = format!("{}/lib/rustlib/{}/lib", sysroot, target_triple);
        if Path::new(&lib_path).exists() {
            link_args.push(format!("-L{}", lib_path));
        }
    }

    if target_triple.contains("msvc") {
        link_args.push("-lmsvcrt".to_string());
        link_args.push("-loldnames".to_string());
        link_args.push("-lkernel32".to_string());
        link_args.push("-lntdll".to_string());
        link_args.push("-lucrt".to_string());
        link_args.push("-lbcrypt".to_string());
        link_args.push("-lws2_32".to_string());
        link_args.push("-luserenv".to_string());
        link_args.push("-lsecur32".to_string());
        link_args.push("-liphlpapi".to_string());
    } else if target_triple.contains("gnu") {
        link_args.push("-lstdc++".to_string());
        link_args.push("-lpthread".to_string());
        link_args.push("-lmingw32".to_string());
        link_args.push("-lgcc_s".to_string());
        link_args.push("-lgcc".to_string());
    } else {
        link_args.push("-lstdc++".to_string());
        link_args.push("-lpthread".to_string());
        link_args.push("-lm".to_string());
    }

    if target_triple.contains("msvc") {
        link_cmd.arg("-Wl,/NODEFAULTLIB:libcmt");
    }

    if gpu_backend == Some("hip") {
        link_args.push("-lhip_hcc".to_string());
        link_args.push("-lamdhip64".to_string());
    }

    for arg in link_args {
        link_cmd.arg(arg);
    }

    if target.contains("windows") {
        link_cmd.arg("-luser32").arg("-Wl,-subsystem:console");
    } else if target.contains("linux") || target.contains("darwin") {
        link_cmd.arg("-lm");
    }

    if has_gpu {
        if let Some(backend) = gpu_backend {
            match backend {
                "hip" => {
                    if !linker_is_hip {
                        eprintln!("Error: HIP backend requires hipcc linker.");
                        return diagnostic::exit_code::LINKER_ERROR;
                    }
                    link_cmd.arg("-D__HIP_PLATFORM_AMD__");
                    link_cmd.arg("-DVOX_GPU_ENABLED");
                    let hip_path = discovery::find_gpu_backend("hip")
                        .and_then(|g| g.hip_path)
                        .map(|p| p.to_string_lossy().to_string())
                        .or_else(|| env::var("HIP_PATH").ok())
                        .unwrap_or_else(|| "C:\\Program Files\\AMD\\ROCm\\7.1".to_string());
                    let hip_include = format!("{}\\include", hip_path);
                    let hip_lib = format!("{}\\lib", hip_path);
                    link_cmd.arg(format!("-I\"{}\"", hip_include));
                    link_cmd.arg(format!("-L\"{}\"", hip_lib));
                }
                "cuda" => {
                    if target.contains("windows") {
                        link_cmd.arg("-lcuda").arg("-D__CUDACC__");
                        let cuda_path = discovery::find_gpu_backend("cuda")
                            .and_then(|g| g.cuda_path)
                            .map(|p| p.to_string_lossy().to_string())
                            .or_else(|| env::var("CUDA_PATH").ok())
                            .unwrap_or_else(|| {
                                "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.8"
                                    .to_string()
                            });
                        link_cmd.arg(&format!("-L{}\\lib\\x64", cuda_path));
                        link_cmd.arg(&format!("-I{}\\include", cuda_path));
                    } else if target.contains("linux") {
                        link_cmd.arg("-lcuda").arg("-lcudart").arg("-D__CUDACC__");
                        link_cmd.arg("-I/usr/local/cuda/include");
                        link_cmd.arg("-L/usr/local/cuda/lib64");
                    } else if target.contains("darwin") {
                        eprintln!("Warning: GPU support on macOS is experimental");
                        link_cmd.arg("-lcuda").arg("-D__CUDACC__");
                    }
                }
                _ => unreachable!(),
            }
        } else {
            eprintln!(
                "Info: @kernel present but no --gpu flag given. GPU code will run on CPU stub (no GPU libraries linked)."
            );
        }
    }

    if target.contains("nvptx") || target.contains("amdgcn") {
        println!(
            "GPU target {} does not produce a standalone host executable.",
            target
        );
        println!("The device IR has been written; use external tools (ptxas, llc) to assemble.");
        return diagnostic::exit_code::SUCCESS;
    }

    if debug {
        link_cmd.arg("-v");
        let output = match link_cmd.output() {
            Ok(out) => out,
            Err(e) => {
                eprintln!("Failed to spawn linker: {}", e);
                return diagnostic::exit_code::LINKER_ERROR;
            }
        };
        if output.status.success() {
            println!("SUCCESS: Native binary compiled -> {}", exe_path.display());
            diagnostic::exit_code::SUCCESS
        } else {
            eprintln!("Linker command: {:?}", link_cmd);
            eprintln!(
                "Linker stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            eprintln!(
                "Compilation failure: {:?} exited with status {}",
                linker, output.status
            );
            diagnostic::exit_code::LINKER_ERROR
        }
    } else {
        let compile_status = link_cmd.status();
        match compile_status {
            Ok(status) if status.success() => {
                println!("SUCCESS: Native binary compiled -> {}", exe_path.display());
                diagnostic::exit_code::SUCCESS
            }
            Ok(status) => {
                eprintln!(
                    "Compilation failure: {:?} exited with status {}",
                    linker, status
                );
                diagnostic::exit_code::LINKER_ERROR
            }
            Err(e) => {
                eprintln!("Compilation failure: could not invoke {:?}: {}", linker, e);
                diagnostic::exit_code::LINKER_ERROR
            }
        }
    }
}

fn cmd_run(
    file: &str,
    debug: bool,
    target: &str,
    gpu: Option<&str>,
    gpu_arch: Option<&str>,
    config: &CacheConfig,
) -> i32 {
    let build_code = cmd_build(file, debug, target, gpu, gpu_arch, config);
    if build_code != diagnostic::exit_code::SUCCESS {
        return build_code;
    }
    let path = Path::new(file);
    let file_name = path.file_stem().unwrap().to_str().unwrap();
    let out_dir = get_output_dir("debug");
    let exe_path = out_dir.join(if target.contains("windows") {
        format!("{}.exe", file_name)
    } else {
        file_name.to_string()
    });
    println!("\n=== EXECUTING ===");
    let execution_status = Command::new(&exe_path).status();
    match execution_status {
        Ok(status) => {
            println!("\nProcess exited with status: {}", status);
            if status.success() {
                diagnostic::exit_code::SUCCESS
            } else {
                diagnostic::exit_code::GENERIC_ERROR
            }
        }
        Err(e) => {
            eprintln!("Failed to execute binary: {}", e);
            diagnostic::exit_code::GENERIC_ERROR
        }
    }
}

fn cmd_update(path: &str, write: bool) -> i32 {
    let path = Path::new(path);
    let mut files = Vec::new();

    if path.is_file() {
        if path.extension().and_then(|s| s.to_str()) == Some("vx") {
            files.push(path.to_path_buf());
        } else {
            eprintln!("Error: {} is not a .vx file", path.display());
            return diagnostic::exit_code::IO_ERROR;
        }
    } else if path.is_dir() {
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("vx"))
        {
            files.push(entry.path().to_path_buf());
        }
    } else {
        eprintln!("Error: {} does not exist", path.display());
        return diagnostic::exit_code::IO_ERROR;
    }

    if files.is_empty() {
        println!("No .vx files found in {}", path.display());
        return diagnostic::exit_code::SUCCESS;
    }

    let mut updated_any = false;
    let mut error_occurred = false;

    for file in files {
        match update::process_file(&file, write) {
            Ok(updated) => {
                if updated {
                    updated_any = true;
                }
            }
            Err(e) => {
                eprintln!("{}", e);
                error_occurred = true;
            }
        }
    }

    if error_occurred {
        diagnostic::exit_code::GENERIC_ERROR
    } else if write && updated_any {
        println!("Updated hashes in source files.");
        diagnostic::exit_code::SUCCESS
    } else if !write && updated_any {
        println!("Mismatches found. Run with --write to update hashes.");
        diagnostic::exit_code::GENERIC_ERROR
    } else {
        println!("All remote imports are up-to-date.");
        diagnostic::exit_code::SUCCESS
    }
}

fn cmd_index(path: &str, watch: bool) -> i32 {
    let root = Path::new(path);
    if watch {
        if let Err(e) = watch::run_watcher(root, false) {
            eprintln!("Watch error: {}", e);
            diagnostic::exit_code::GENERIC_ERROR
        } else {
            diagnostic::exit_code::SUCCESS
        }
    } else {
        match index::index_project(root, false) {
            Ok(()) => {
                println!("Index generated successfully.");
                diagnostic::exit_code::SUCCESS
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                diagnostic::exit_code::GENERIC_ERROR
            }
        }
    }
}

fn cmd_clean() -> i32 {
    let target_dir = Path::new("target");
    if target_dir.exists() {
        println!("Removing target/ directory...");
        if let Err(e) = std::fs::remove_dir_all(target_dir) {
            eprintln!("Error: Failed to remove target/: {}", e);
            diagnostic::exit_code::IO_ERROR
        } else {
            println!("Clean completed.");
            diagnostic::exit_code::SUCCESS
        }
    } else {
        println!("No target/ directory found. Nothing to clean.");
        diagnostic::exit_code::SUCCESS
    }
}

fn cmd_lsp() -> i32 {
    if let Err(e) = lsp::run_server() {
        eprintln!("LSP server error: {}", e);
        diagnostic::exit_code::GENERIC_ERROR
    } else {
        diagnostic::exit_code::SUCCESS
    }
}

fn cmd_shell() -> i32 {
    if let Err(e) = shell::run() {
        eprintln!("Shell error: {}", e);
        diagnostic::exit_code::GENERIC_ERROR
    } else {
        diagnostic::exit_code::SUCCESS
    }
}

// -----------------------------------------------------------------------------
// main
// -----------------------------------------------------------------------------

fn main() {
    let cli = parse_args();

    set_global_debug(cli.debug);
    set_output_format(cli.output_format);

    let config = CacheConfig {
        no_cache: cli.no_cache,
        reuse_proofs: cli.reuse_proofs,
        reuse_bitcode: cli.reuse_bitcode,
        offline: cli.offline,
        trust_modules: cli.trust_modules,
    };

    refinement::set_proof_cache_enabled(!config.no_cache && config.reuse_proofs);

    let ui_handle = match cli.command {
        Commands::Check { .. } | Commands::Build { .. } | Commands::Run { .. } => {
            Some(spawn_ui_thread())
        }
        Commands::Test { .. } => None,
        Commands::Update { .. }
        | Commands::Index { .. }
        | Commands::Clean
        | Commands::Lsp
        | Commands::Shell => None,
    };

    let exit_code = match cli.command {
        Commands::Check { file } => cmd_check(
            &file,
            cli.debug,
            &cli.target,
            cli.gpu.as_deref(),
            cli.gpu_arch.as_deref(),
            &config,
        ),
        Commands::Build { file } => cmd_build(
            &file,
            cli.debug,
            &cli.target,
            cli.gpu.as_deref(),
            cli.gpu_arch.as_deref(),
            &config,
        ),
        Commands::Run { file } => cmd_run(
            &file,
            cli.debug,
            &cli.target,
            cli.gpu.as_deref(),
            cli.gpu_arch.as_deref(),
            &config,
        ),
        Commands::Test { path } => cmd_test(&path, &config),
        Commands::Update { write, path } => cmd_update(&path, write),
        Commands::Index { path, watch } => cmd_index(&path, watch),
        Commands::Clean => cmd_clean(),
        Commands::Lsp => cmd_lsp(),
        Commands::Shell => cmd_shell(),
    };

    let diag_code = get_exit_code();
    let final_code = if exit_code != diagnostic::exit_code::SUCCESS && diag_code != 0 {
        cmp::max(exit_code, diag_code)
    } else if exit_code != diagnostic::exit_code::SUCCESS {
        exit_code
    } else {
        diag_code
    };

    if let Err(e) = flush_logs(Some(get_output_dir("debug").join("output.log"))) {
        eprintln!("Warning: failed to flush logs: {}", e);
    }

    if let Some(handle) = ui_handle {
        stop_ui_thread(handle);
    }

    std::process::exit(final_code);
}
