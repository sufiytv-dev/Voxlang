// main.rs - CLI driver for Voxlang compiler.

mod std;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::diagnostic::debug_log;
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

// Global flag to indicate if the program was launched with no arguments.
static NO_ARGS: AtomicBool = AtomicBool::new(false);

fn host_triple() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (arch, os) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc".to_string(),
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu".to_string(),
        ("x86_64", "macos") => "x86_64-apple-darwin".to_string(),
        ("aarch64", "macos") => "aarch64-apple-darwin".to_string(),
        (_, _) => {
            eprintln!("Unsupported host platform: {} {}", arch, os);
            std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
        }
    }
}

fn print_usage() {
    eprintln!(
        r#"Usage: vox <COMMAND> [OPTIONS] [FILE|PATH]

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

Options (may appear before or after the command):
  --debug                 Enable debug output (lexer, parser, codegen)
  --target <TRIPLE>       Target triple (default: host triple)
  --output-format <FORMAT> Specify terminal formatting ("pretty", "json", "auto")
  --gpu <BACKEND>         GPU backend: "cuda" or "hip" (auto-detected if omitted)
  --gpu-arch <ARCH>       GPU architecture (e.g., sm_70, sm_75, gfx1200) (auto-detected if omitted)
  --no-cache              Ignore all caches
  --reuse-proofs [true|false] Reuse cached Z3 proofs (default: true)
  --reuse-bitcode [true|false] Reuse cached LLVM bitcode (default: true)
  --offline               Do not download remote modules; fail if not cached
  --trust-modules         Allow @comptime execution in imported modules (security risk)
"#
    );
}

// -----------------------------------------------------------------------------
// Argument parsing (flags anywhere, command first)
// -----------------------------------------------------------------------------
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
    let mut update_write = false;
    let mut index_watch = false;

    let mut i = 0;
    let mut non_flag_tokens = Vec::new();

    while i < args.len() {
        let arg = &args[i];
        if arg.starts_with("--") {
            match arg.as_str() {
                "--debug" => debug = true,
                "--target" => {
                    if i + 1 >= args.len() {
                        eprintln!("Error: --target requires a value");
                        print_usage();
                        std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                    }
                    target = args[i + 1].clone();
                    i += 1;
                }
                "--output-format" => {
                    if i + 1 >= args.len() {
                        eprintln!(
                            "Error: --output-format requires a value (pretty, json, or auto)"
                        );
                        print_usage();
                        std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                    }
                    let val = args[i + 1].as_str();
                    output_format = match val {
                        "pretty" => diagnostic::OutputFormat::Pretty,
                        "json" => diagnostic::OutputFormat::Json,
                        "auto" => diagnostic::OutputFormat::Auto,
                        _ => {
                            eprintln!("Error: Invalid output format '{}'", val);
                            print_usage();
                            std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                        }
                    };
                    i += 1;
                }
                "--gpu" => {
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
                    i += 1;
                }
                "--gpu-arch" => {
                    if i + 1 >= args.len() {
                        eprintln!("Error: --gpu-arch requires a value");
                        print_usage();
                        std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                    }
                    gpu_arch = Some(args[i + 1].clone());
                    i += 1;
                }
                "--no-cache" => no_cache = true,
                "--reuse-proofs" => {
                    if i + 1 < args.len() && args[i + 1] == "false" {
                        reuse_proofs = false;
                        i += 1;
                    }
                }
                "--reuse-bitcode" => {
                    if i + 1 < args.len() && args[i + 1] == "false" {
                        reuse_bitcode = false;
                        i += 1;
                    }
                }
                "--offline" => offline = true,
                "--trust-modules" => trust_modules = true,
                "--write" => update_write = true,
                "--watch" => index_watch = true,
                _ => {
                    eprintln!("Error: Unknown option '{}'", arg);
                    print_usage();
                    std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
                }
            }
            i += 1;
        } else {
            non_flag_tokens.push(arg.clone());
            i += 1;
        }
    }

    // If no command is provided, launch the shell (GUI) with debug enabled.
    if non_flag_tokens.is_empty() {
        NO_ARGS.store(true, Ordering::Relaxed);
        return Cli {
            debug: true,
            target: host_triple(),
            output_format: diagnostic::OutputFormat::Auto,
            gpu: None,
            gpu_arch: None,
            no_cache: false,
            reuse_proofs: true,
            reuse_bitcode: true,
            offline: false,
            trust_modules: false,
            command: Commands::Shell,
        };
    }

    let command_str = non_flag_tokens[0].clone();
    let cmd_args = &non_flag_tokens[1..];

    let command = match command_str.as_str() {
        "check" => {
            if cmd_args.is_empty() {
                eprintln!("Error: 'check' requires a file argument");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            Commands::Check {
                file: cmd_args[0].clone(),
            }
        }
        "build" => {
            if cmd_args.is_empty() {
                eprintln!("Error: 'build' requires a file argument");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            Commands::Build {
                file: cmd_args[0].clone(),
            }
        }
        "run" => {
            if cmd_args.is_empty() {
                eprintln!("Error: 'run' requires a file argument");
                print_usage();
                std::process::exit(diagnostic::exit_code::GENERIC_ERROR);
            }
            Commands::Run {
                file: cmd_args[0].clone(),
            }
        }
        "test" => {
            let path = cmd_args
                .first()
                .cloned()
                .unwrap_or_else(|| "src/Examples".to_string());
            Commands::Test { path }
        }
        "update" => {
            let path = cmd_args.first().cloned().unwrap_or_else(|| ".".to_string());
            Commands::Update {
                write: update_write,
                path,
            }
        }
        "index" => {
            let path = cmd_args.first().cloned().unwrap_or_else(|| ".".to_string());
            Commands::Index {
                watch: index_watch,
                path,
            }
        }
        "clean" => Commands::Clean,
        "lsp" => Commands::Lsp,
        "shell" => Commands::Shell,
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

// -----------------------------------------------------------------------------
// Compilation pipeline
// -----------------------------------------------------------------------------

pub struct CompilationResult {
    pub _ast: ASTNode,
    pub semantic_ok: bool,
    pub llvm_ir: String,
    pub has_gpu: bool,
    pub _device_triple: Option<String>,
    // Auto-detected GPU backend and architecture (if any)
    pub gpu_backend: Option<String>,
    pub gpu_arch: Option<String>,
}

pub fn compile_source(
    file_path: &str,
    debug: bool,
    target: &str,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
    config: &CacheConfig,
    profile: &str,    // "debug" or "release"
    check_only: bool, // if true, stop after semantic analysis (no IR generation)
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
    let full_source =
        module::read_source_file(path).map_err(|e| format!("Failed to read file: {}", e))?;

    if debug {
        eprintln!("[DEBUG] Source length: {} bytes", full_source.len());
    }

    emit_phase_update("Lexical analysis", 10);
    let mut lexer = Lexer::new(&full_source);
    let tokens = lexer
        .tokenize()
        .map_err(|_| "Lexical analysis failed".to_string())?;

    emit_phase_update("Syntactic parsing", 25);
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    if parser.has_errors() {
        return Err("Parsing failed due to syntax errors".to_string());
    }

    emit_phase_update("Desugaring syntactic sugar", 30);
    let desugared_ast = desugar(ast);

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

    let semantic_ok = semantic.check(&desugared_ast);
    if !semantic_ok {
        return Ok(CompilationResult {
            _ast: desugared_ast,
            semantic_ok: false,
            llvm_ir: String::new(),
            has_gpu: false,
            _device_triple: None,
            gpu_backend: None,
            gpu_arch: None,
        });
    }

    // If check_only, we stop here and return success.
    if check_only {
        let mut device_triple = None;
        let has_gpu = has_kernel(&desugared_ast, &mut device_triple);
        return Ok(CompilationResult {
            _ast: desugared_ast,
            semantic_ok: true,
            llvm_ir: String::new(),
            has_gpu,
            _device_triple: device_triple,
            gpu_backend: None,
            gpu_arch: None,
        });
    }

    // Otherwise, continue to IR generation.
    let resolved_types = semantic.resolved_variable_types.clone();
    let type_aliases = semantic.symbols.type_aliases.clone();
    let imported_modules = semantic.take_imported_modules();
    drop(semantic);

    let mut device_triple = None;
    let has_gpu = has_kernel(&desugared_ast, &mut device_triple);

    // -------------------------------------------------------------------------
    // Auto-detect GPU backend and architecture (prioritise installed SDK)
    // -------------------------------------------------------------------------
    let (final_backend, final_arch): (Option<String>, Option<String>);

    // 1. User override via --gpu / --gpu-arch
    if let Some(backend) = gpu_backend {
        final_backend = Some(backend.to_string());
        final_arch = gpu_arch.map(|s| s.to_string());
    } else if let Some(sdk) = discovery::find_gpu_sdk() {
        // 2. If a GPU SDK is installed, use that
        final_backend = Some(sdk.backend.clone());
        let default_arch = match sdk.backend.as_str() {
            "cuda" => "sm_75",
            "hip" => "gfx1200",
            _ => "sm_75",
        };
        final_arch = gpu_arch
            .map(|s| s.to_string())
            .or(Some(default_arch.to_string()));
        eprintln!(
            "[INFO] Using installed GPU backend: {} (version {})",
            sdk.backend, sdk.version
        );
    } else if let Some(triple) = &device_triple {
        // 3. Fallback to kernel device triple (if no SDK is installed)
        if triple.contains("cuda") {
            final_backend = Some("cuda".to_string());
            final_arch = gpu_arch
                .map(|s| s.to_string())
                .or(Some("sm_75".to_string()));
        } else if triple.contains("amdhsa") || triple.contains("hip") {
            final_backend = Some("hip".to_string());
            final_arch = gpu_arch
                .map(|s| s.to_string())
                .or(Some("gfx1200".to_string()));
        } else {
            final_backend = None;
            final_arch = None;
        }
    } else {
        final_backend = None;
        final_arch = None;
    }

    emit_phase_update("IR generation", 70);

    // Determine GPU architecture default (sm_75 for RTX 2070 Super)
    let default_gpu_arch = match final_backend.as_deref() {
        Some("cuda") => "sm_75",
        Some("hip") => "gfx1200",
        _ => "sm_75",
    };
    let effective_gpu_arch = final_arch.as_deref().unwrap_or(default_gpu_arch);

    // Standard device triples WITHOUT architecture suffix (architecture passed via -mcpu to llc)
    let forced_device_triple = final_backend.as_deref().map(|backend| match backend {
        "cuda" => "nvptx64-nvidia-cuda".to_string(),
        "hip" => "amdgcn-amd-amdhsa".to_string(),
        _ => unreachable!(),
    });

    let mut codegen = CodegenEngine::new(target);
    codegen.set_gpu_mode(final_backend.as_deref());
    if let Some(triple) = forced_device_triple {
        codegen.set_device_triple_override(triple);
    }
    codegen.set_resolved_types(resolved_types);
    codegen.set_type_aliases(type_aliases);
    for (alias, module_ast) in imported_modules {
        codegen.add_imported_module_ast(alias, module_ast);
    }
    // Pass the GPU architecture (e.g., "sm_75") to the code generator
    codegen.set_gpu_arch(Some(effective_gpu_arch.to_string()));

    // LLVM tool discovery with fallback
    let llvm_paths = match discovery::find_llvm_tools() {
        Ok(tools) => tools,
        Err(e) => {
            eprintln!("Warning: LLVM auto-discovery failed: {}", e);
            find_llvm_fallback().unwrap_or_else(|| {
                eprintln!("Error: Could not find clang/llc. Please install LLVM tools.");
                std::process::exit(diagnostic::exit_code::LINKER_ERROR);
            })
        }
    };
    codegen.set_llvm_paths(llvm_paths.clang, llvm_paths.llc, llvm_paths.lld);

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
        gpu_backend: final_backend,
        gpu_arch: final_arch,
    })
}

// Public function used by watch.rs and GUI
pub fn check_file(file_path: &str, debug: bool, target: &str, config: &CacheConfig) -> bool {
    match compile_source(file_path, debug, target, None, None, config, "debug", true) {
        Ok(result) => result.semantic_ok,
        Err(e) => {
            eprintln!("Fatal error: {}", e);
            false
        }
    }
}

// -----------------------------------------------------------------------------
// LLVM tool fallback (search many common paths)
// -----------------------------------------------------------------------------
fn find_llvm_fallback() -> Option<discovery::LlvmPaths> {
    let mut clang_paths = Vec::new();
    let mut llc_paths = Vec::new();

    // 1. Inject Windows Scoop paths dynamically if the environment matches
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let local_scoop_clang = format!(r"{}\scoop\apps\llvm\current\bin\clang.exe", user_profile);
        let local_scoop_llc = format!(r"{}\scoop\apps\llvm\current\bin\llc.exe", user_profile);
        clang_paths.push(local_scoop_clang);
        llc_paths.push(local_scoop_llc);
    }

    // Global Scoop location fallback
    clang_paths.push(r"C:\ProgramData\scoop\apps\llvm\current\bin\clang.exe".to_string());
    llc_paths.push(r"C:\ProgramData\scoop\apps\llvm\current\bin\llc.exe".to_string());

    // 2. Base Linux / Unix standard paths
    clang_paths.push("/usr/bin/clang".to_string());
    clang_paths.push("/usr/local/bin/clang".to_string());
    llc_paths.push("/usr/bin/llc".to_string());
    llc_paths.push("/usr/local/bin/llc".to_string());

    // 3. Versioned Linux paths (10..=19)
    for v in 10..=19 {
        clang_paths.push(format!("/usr/lib/llvm-{}/bin/clang", v));
        clang_paths.push(format!("/usr/bin/clang-{}", v));
        llc_paths.push(format!("/usr/lib/llvm-{}/bin/llc", v));
        llc_paths.push(format!("/usr/bin/llc-{}", v));
    }

    // 4. Resolve exact binaries from the path matrix
    let mut clang = None;
    for p in &clang_paths {
        let path = PathBuf::from(p);
        if path.exists() {
            clang = Some(path);
            break;
        }
    }

    let mut llc = None;
    for p in &llc_paths {
        let path = PathBuf::from(p);
        if path.exists() {
            llc = Some(path);
            break;
        }
    }

    // 5. Package verified paths back into the toolchain asset locator
    match (clang, llc) {
        (Some(c), Some(l)) => Some(discovery::LlvmPaths {
            clang: c.clone(),
            llc: l,
            lld: Some(c), // Preserves your original assignment logic mapping lld to the clang path
            system_libs: Vec::new(),
        }),
        _ => None,
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
    match compile_source(file, debug, target, gpu, gpu_arch, config, "debug", true) {
        Ok(result) if result.semantic_ok => {
            println!("Check passed.");
            diagnostic::exit_code::SUCCESS
        }
        Ok(_) => diagnostic::exit_code::SEMANTIC_ERROR,
        Err(e) => {
            eprintln!("{}", e);
            diagnostic::exit_code::GENERIC_ERROR
        }
    }
}

/// Find the Vox installation root by searching upward from the executable path.
/// Looks for `src/Examples` or `examples` as a marker.
pub fn find_vox_root() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = exe.parent().unwrap_or(&PathBuf::from(".")).to_path_buf();

    loop {
        if dir.join("src/Examples").exists() {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }

    // Fallback: current working directory
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn cmd_test(path_str: &str, config: &CacheConfig) -> i32 {
    // If user provided an explicit path, use it; otherwise auto-discover
    let user_path = Path::new(path_str);
    let test_dir = if user_path.exists() {
        user_path.to_path_buf()
    } else {
        let root = find_vox_root();
        let default = root.join("src/Examples");
        if default.exists() {
            default
        } else {
            root.join("examples")
        }
    };

    if !test_dir.exists() {
        eprintln!(
            "Error: Examples directory not found at '{}'",
            test_dir.display()
        );
        return diagnostic::exit_code::IO_ERROR;
    }

    let current_exe =
        std::env::current_exe().expect("Failed to locate current compiler executable");
    let mut test_files = Vec::new();
    for entry in WalkDir::new(&test_dir)
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
        println!("No .vx files found in {}", test_dir.display());
        return diagnostic::exit_code::SUCCESS;
    }

    let mut total = 0;
    let mut passed = 0;
    println!("==================================================");
    println!("       VOXLANG COMPILER CONFORMANCE SUITE        ");
    println!("==================================================\n");

    for path in test_files {
        let file_name = path.file_name().unwrap().to_string_lossy();
        if file_name.contains("gpu") {
            continue;
        }
        total += 1;
        let rel_path = path.strip_prefix(&test_dir).unwrap_or(&path);
        print!("test {:<30} ... ", rel_path.display());
        std::io::Write::flush(&mut std::io::stdout()).unwrap();

        let mut cmd = Command::new(&current_exe);
        cmd.arg("run").arg(&path);
        if config.no_cache {
            cmd.arg("--no-cache");
        }
        let output = cmd
            .output()
            .expect("Failed to execute internal compiler run pipeline");
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        let has_error = stderr_str.to_lowercase().contains("error:");
        let ok = output.status.success() || (!has_error);
        if ok {
            println!("✅ PASSED");
            passed += 1;
        } else {
            println!("❌ FAILED");
            eprintln!("\n--- [STDERR OUTPUT: {}] ---", file_name);
            eprintln!("{}", stderr_str.trim_end());
            eprintln!("--------------------------------------------------\n");
        }
    }

    println!("\nResult: {}/{} tests passed.", passed, total);
    if passed < total {
        diagnostic::exit_code::GENERIC_ERROR
    } else {
        diagnostic::exit_code::SUCCESS
    }
}

// ============================================================================
// UPDATED cmd_build with robust library generation and absolute paths
// ============================================================================
fn cmd_build(
    file: &str,
    debug: bool,
    target: &str,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
    config: &CacheConfig,
) -> i32 {
    let profile = "debug";

    let compile_result = match compile_source(
        file,
        debug,
        target,
        gpu_backend,
        gpu_arch,
        config,
        profile,
        false,
    ) {
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
    let out_dir = get_output_dir(profile);
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

    let mut detected_backend = compile_result.gpu_backend.as_deref();

    // -------------------------------------------------------------------------
    // Check if the required SDK is available; if not, fall back to CPU.
    // -------------------------------------------------------------------------
    let sdk_available = match detected_backend {
        Some("cuda") => discovery::find_cuda_sdk().is_some(),
        Some("hip") => discovery::find_hip_sdk().is_some(),
        _ => true,
    };
    if !sdk_available {
        eprintln!(
            "Warning: {} SDK not found, kernels will run on CPU.",
            detected_backend.unwrap_or("GPU")
        );
        detected_backend = None; // Fall back to CPU mode
    }

    let llvm_tools = match discovery::find_llvm_tools() {
        Ok(tools) => tools,
        Err(e) => {
            eprintln!("Warning: LLVM auto-discovery failed: {}", e);
            find_llvm_fallback().unwrap_or_else(|| {
                eprintln!("Error: Could not find clang/llc. Please install LLVM tools.");
                std::process::exit(diagnostic::exit_code::LINKER_ERROR);
            })
        }
    };

    // -------------------------------------------------------------------------
    // Determine the linker based on the detected backend (after SDK check)
    // -------------------------------------------------------------------------
    let (linker, linker_is_hip) = match detected_backend {
        Some("hip") => {
            let sdk = discovery::find_hip_sdk().unwrap();
            let hipcc = sdk
                .bin_path
                .join("hipcc")
                .with_extension(env::consts::EXE_EXTENSION);
            if !hipcc.exists() {
                eprintln!("Error: hipcc not found in HIP SDK.");
                return diagnostic::exit_code::LINKER_ERROR;
            }
            (hipcc, true)
        }
        Some("cuda") => (llvm_tools.clang, false),
        _ => (llvm_tools.clang, false),
    };

    let target_triple = if target.contains("windows") && target.contains("msvc") {
        "x86_64-pc-windows-msvc"
    } else if target.contains("windows") && target.contains("gnu") {
        "x86_64-pc-windows-gnu"
    } else {
        target
    };

    let cache_dir = get_output_dir(profile).join(".vox_rt_cache");
    std::fs::create_dir_all(&cache_dir).expect("Failed to create runtime cache dir");

    let lib_name = "vox_rt";
    let lib_extension = if target_triple.contains("msvc") {
        ".lib"
    } else {
        ".a"
    };
    let static_lib = cache_dir.join(format!("{}{}", lib_name, lib_extension));

    // -------------------------------------------------------------------------
    // Rebuild vox_rt with the appropriate feature flags (or none if CPU fallback)
    // -------------------------------------------------------------------------
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
        // Only add feature flag if SDK is available and we have a valid backend
        if let Some(backend) = detected_backend {
            if backend == "cuda" {
                rustc_cmd.arg("--cfg").arg("feature=\"vox_gpu_cuda\"");
            } else if backend == "hip" {
                rustc_cmd.arg("--cfg").arg("feature=\"vox_gpu_enabled\"");
            }
        }
        let status = rustc_cmd.status().expect("failed to run rustc");
        if !status.success() {
            eprintln!("Compilation of vox_rt.rs failed");
            return diagnostic::exit_code::LINKER_ERROR;
        }
    }

    // -------------------------------------------------------------------------
    // Verify that the library exists; if not, abort with clear error.
    // -------------------------------------------------------------------------
    if !static_lib.exists() {
        eprintln!(
            "Error: Static library '{}' was not generated.",
            static_lib.display()
        );
        return diagnostic::exit_code::LINKER_ERROR;
    }

    // -------------------------------------------------------------------------
    // Build the linker command
    // -------------------------------------------------------------------------
    let mut link_cmd = Command::new(&linker);

    // Link directly from .ll (as in 0.4)
    link_cmd.arg(&debug_ir_path).arg("-o").arg(&exe_path);

    // Set the target triple explicitly (important for Windows)
    link_cmd.arg("-target").arg(target);

    // Use absolute paths for -L to avoid path resolution issues
    let cache_dir_abs = cache_dir.canonicalize().unwrap_or(cache_dir);
    link_cmd.arg(&format!("-L{}", cache_dir_abs.display()));

    // Add system library paths to LIB environment variable instead of -L (to avoid quoting issues)
    let mut lib_paths = Vec::new();
    if let Ok(current_lib) = env::var("LIB") {
        if !current_lib.is_empty() {
            lib_paths.push(current_lib);
        }
    }

    // Add discovered system library paths
    for p in &llvm_tools.system_libs {
        if let Some(s) = p.to_str() {
            lib_paths.push(s.to_string());
        }
    }

    // Add cache dir absolute path to LIB
    if let Some(s) = cache_dir_abs.to_str() {
        lib_paths.push(s.to_string());
    }

    // Add rustlib path if available
    if let Some(sysroot) = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
    {
        let lib_path = format!("{}/lib/rustlib/{}/lib", sysroot, target);
        let lib_path_abs = Path::new(&lib_path);
        if lib_path_abs.exists() {
            if let Some(s) = lib_path_abs.to_str() {
                lib_paths.push(s.to_string());
            }
            link_cmd.arg(&format!("-L{}", lib_path_abs.display()));
        }
    }

    // Set LIB environment variable for the linker
    unsafe {
        env::set_var("LIB", lib_paths.join(";"));
    }
    debug_log("[DISCOVERY] Set LIB environment variable for linking.");

    // Link against vox_rt
    link_cmd.arg("-lvox_rt");

    // Standard system libraries – exactly as in 0.4
    if target_triple.contains("msvc") {
        link_cmd.arg("-Wl,/NODEFAULTLIB:libcmt");
        link_cmd.arg("-lmsvcrt");
        link_cmd.arg("-loldnames");
        link_cmd.arg("-lkernel32");
        link_cmd.arg("-lntdll");
        link_cmd.arg("-lucrt");
        link_cmd.arg("-lbcrypt");
        link_cmd.arg("-lws2_32");
        link_cmd.arg("-luserenv");
        link_cmd.arg("-lsecur32");
        link_cmd.arg("-liphlpapi");
    } else if target_triple.contains("windows") && target_triple.contains("gnu") {
        link_cmd.arg("-lstdc++");
        link_cmd.arg("-lpthread");
        link_cmd.arg("-lmingw32");
        link_cmd.arg("-lgcc_s");
        link_cmd.arg("-lgcc");
    } else {
        link_cmd.arg("-lstdc++");
        link_cmd.arg("-lpthread");
        link_cmd.arg("-lm");
    }

    // -------------------------------------------------------------------------
    // Add GPU SDK library paths and libraries only if SDK is available
    // -------------------------------------------------------------------------
    if let Some(backend) = detected_backend {
        match backend {
            "cuda" => {
                if let Some(sdk) = discovery::find_cuda_sdk() {
                    // Add CUDA lib to LIB and -L
                    if let Some(s) = sdk.lib_path.to_str() {
                        unsafe {
                            let current = env::var("LIB").unwrap_or_default();
                            env::set_var("LIB", format!("{};{}", current, s));
                        }
                        link_cmd.arg(&format!("-L{}", sdk.lib_path.display()));
                    }
                    link_cmd.arg("-lcuda");
                    link_cmd.arg("-lcudart");
                }
            }
            "hip" => {
                // hipcc adds its own flags; we just need to link the HIP runtime
                link_cmd.arg("-lamdhip64");
            }
            _ => {}
        }
    }

    if target.contains("windows") {
        link_cmd.arg("-luser32");
        link_cmd.arg("-Wl,-subsystem:console");
    } else if target.contains("linux") || target.contains("darwin") {
        link_cmd.arg("-lm");
    }

    // Ensure HIP uses hipcc
    if compile_result.has_gpu && detected_backend == Some("hip") && !linker_is_hip {
        eprintln!("Error: HIP backend requires hipcc linker.");
        return diagnostic::exit_code::LINKER_ERROR;
    }

    match link_cmd.status() {
        Ok(status) if status.success() => {
            println!("SUCCESS: Native binary compiled -> {}", exe_path.display());
            let _ = fs::remove_file(&debug_ir_path);
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

// -----------------------------------------------------------------------------
// The rest of the commands (unchanged)
// -----------------------------------------------------------------------------

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
    match Command::new(&exe_path).status() {
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
    match lsp::run_server() {
        Ok(()) => diagnostic::exit_code::SUCCESS,
        Err(e) => {
            eprintln!("LSP server error: {}", e);
            diagnostic::exit_code::GENERIC_ERROR
        }
    }
}

fn cmd_shell() -> i32 {
    let hide_console = NO_ARGS.load(Ordering::Relaxed);
    match shell::run(hide_console) {
        Ok(()) => diagnostic::exit_code::SUCCESS,
        Err(e) => {
            eprintln!("Shell error: {}", e);
            diagnostic::exit_code::GENERIC_ERROR
        }
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
        _ => None,
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
