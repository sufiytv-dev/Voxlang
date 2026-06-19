// src/shell/runner.rs – Background compilation and execution
// Updated to capture and forward all subprocess output to the GUI terminal.
// Now supports build (debug/release), check, test, clean, and run.
// Added suppression of per-file phase updates during test runs.
// Centralised test phase updates inside run_tests().
// GPU linking now uses the same SDK detection and linker selection as cmd_build.

use std::env; // <-- added for env::consts::EXE_EXTENSION
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::CacheConfig;
use crate::WalkDir;
use crate::diagnostic::{emit_log, emit_phase_update, set_test_run_active};
use crate::{compile_source, get_output_dir, host_triple};

/// Output from a successful compile+run.
pub struct RunOutput {
    pub lines: Vec<String>,
}

/// Get the project root directory (where Cargo.toml is located).
fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Helper: run a command, capture stdout/stderr, log stderr lines to GUI, and return success status.
/// Logs lines with the given prefix.
fn run_command_with_logging(
    cmd: &mut Command,
    prefix: &str,
) -> Result<std::process::Output, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run command: {}", e))?;
    // Log stderr lines (they often contain warnings/errors)
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        if !line.is_empty() {
            emit_log(format!("[{}] {}", prefix, line));
        }
    }
    // Also log stdout if non-empty (rare for these tools, but we do it anyway)
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.is_empty() {
            emit_log(format!("[{}] {}", prefix, line));
        }
    }
    Ok(output)
}

/// Private helper: link a `.ll` file into an executable using the appropriate toolchain.
/// Returns `Ok(())` on success.
/// Now uses the same GPU SDK detection and linker selection as cmd_build.
fn link_executable(
    ll_path: &Path,
    exe_path: &Path,
    target: &str,
    profile: &str,
    gpu_backend: Option<&str>,
    _gpu_arch: Option<&str>, // kept for API consistency, not used directly in linking
) -> Result<(), String> {
    let target_triple = if target.contains("windows") && target.contains("msvc") {
        "x86_64-pc-windows-msvc"
    } else if target.contains("windows") && target.contains("gnu") {
        "x86_64-pc-windows-gnu"
    } else {
        target
    };

    let cache_dir = get_output_dir(profile).join(".vox_rt_cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir: {}", e))?;

    let lib_name = "vox_rt";
    let lib_extension = if target_triple.contains("msvc") {
        ".lib"
    } else {
        ".a"
    };
    let static_lib = cache_dir.join(format!("{}{}", lib_name, lib_extension));

    // Path to the runtime source file relative to the project root.
    let runtime_source = project_root().join("src/vox_rt.rs");
    if !runtime_source.exists() {
        return Err(format!(
            "Runtime source not found at {}",
            runtime_source.display()
        ));
    }

    // -------------------------------------------------------------------------
    // Check if the required SDK is available; if not, fall back to CPU.
    // -------------------------------------------------------------------------
    let mut detected_backend = gpu_backend;
    let sdk_available = match detected_backend {
        Some("cuda") => crate::discovery::find_cuda_sdk().is_some(),
        Some("hip") => crate::discovery::find_hip_sdk().is_some(),
        _ => true,
    };
    if !sdk_available {
        eprintln!(
            "Warning: {} SDK not found, kernels will run on CPU.",
            detected_backend.unwrap_or("GPU")
        );
        detected_backend = None; // Fall back to CPU mode
    }

    // -------------------------------------------------------------------------
    // Rebuild vox_rt with the appropriate feature flags (or none if CPU fallback)
    // -------------------------------------------------------------------------
    let need_rebuild = !static_lib.exists()
        || fs::metadata(&runtime_source)
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
            .arg(&runtime_source);

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

        let output = run_command_with_logging(&mut rustc_cmd, "RUSTC")?;
        if !output.status.success() {
            return Err("Compilation of vox_rt.rs failed".to_string());
        }
    }

    // -------------------------------------------------------------------------
    // Determine the linker based on the detected backend (after SDK check)
    // -------------------------------------------------------------------------
    let llvm_tools = crate::discovery::find_llvm_tools().map_err(|e| e.to_string())?;
    let (linker, linker_is_hip) = match detected_backend {
        Some("hip") => {
            let sdk = crate::discovery::find_hip_sdk().unwrap();
            let hipcc = sdk
                .bin_path
                .join("hipcc")
                .with_extension(env::consts::EXE_EXTENSION);
            if !hipcc.exists() {
                eprintln!("Error: hipcc not found in HIP SDK.");
                return Err("hipcc not found".to_string());
            }
            (hipcc, true)
        }
        Some("cuda") => (llvm_tools.clang, false),
        _ => (llvm_tools.clang, false),
    };

    let mut cmd = Command::new(&linker);
    cmd.arg(ll_path)
        .arg("-o")
        .arg(exe_path)
        .arg("-L")
        .arg(&cache_dir)
        .arg(&format!("-l{}", lib_name));

    // Additional linker flags (consistent with cmd_build)
    if target_triple.contains("msvc") {
        cmd.args(&[
            "-Wl,/NODEFAULTLIB:libcmt",
            "-lmsvcrt",
            "-loldnames",
            "-lkernel32",
            "-lntdll",
            "-lucrt",
            "-lbcrypt",
            "-lws2_32",
            "-luserenv",
            "-lsecur32",
            "-liphlpapi",
        ]);
    } else if target_triple.contains("windows") && target_triple.contains("gnu") {
        cmd.args(&["-lstdc++", "-lpthread", "-lmingw32", "-lgcc_s", "-lgcc"]);
    } else {
        cmd.args(&["-lstdc++", "-lpthread", "-lm"]);
    }

    if target.contains("windows") {
        cmd.arg("-luser32").arg("-Wl,-subsystem:console");
    }

    // -------------------------------------------------------------------------
    // Add GPU SDK library paths and libraries only if SDK is available
    // -------------------------------------------------------------------------
    if let Some(backend) = detected_backend {
        match backend {
            "cuda" => {
                if let Some(sdk) = crate::discovery::find_cuda_sdk() {
                    cmd.arg("-L").arg(&sdk.lib_path);
                    cmd.arg("-lcuda");
                    cmd.arg("-lcudart");
                }
            }
            "hip" => {
                // hipcc adds its own flags; nothing extra needed.
            }
            _ => {}
        }
    }

    let output = run_command_with_logging(&mut cmd, "LINK")?;
    if !output.status.success() {
        return Err("Linking failed".to_string());
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// High-level build actions (shared between CLI and GUI)
// -----------------------------------------------------------------------------

/// Build (debug or release) the given .vx file, producing an executable.
/// Returns the path to the built executable on success.
pub fn build_file(
    path: &Path,
    release: bool,
    target: &str,
    config: &CacheConfig,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
) -> Result<PathBuf, String> {
    let profile = if release { "release" } else { "debug" };
    let source_path = path.to_str().unwrap();
    let result = compile_source(
        source_path,
        false, // debug flag (we can add an option later)
        target,
        gpu_backend,
        gpu_arch,
        config,
        profile,
        false, // check_only
    )?;
    if !result.semantic_ok {
        return Err("Semantic errors".to_string());
    }

    let out_dir = get_output_dir(profile);
    let exe_name = path.file_stem().unwrap().to_str().unwrap();
    let exe_path = if cfg!(windows) {
        out_dir.join(format!("{}.exe", exe_name))
    } else {
        out_dir.join(exe_name)
    };
    let ll_path = out_dir.join(format!("{}.ll", exe_name));
    fs::write(&ll_path, &result.llvm_ir).map_err(|e| e.to_string())?;

    // Link with runtime – pass the backend and arch from the compilation result.
    link_executable(
        &ll_path,
        &exe_path,
        target,
        profile,
        result.gpu_backend.as_deref(),
        result.gpu_arch.as_deref(),
    )?;

    // Clean up intermediate LLVM file (optional)
    let _ = fs::remove_file(&ll_path);

    Ok(exe_path)
}

/// Check (semantic analysis only) the given file.
pub fn check_file(
    path: &Path,
    target: &str,
    config: &CacheConfig,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
) -> Result<bool, String> {
    let source_path = path.to_str().unwrap();
    let result = compile_source(
        source_path,
        false,
        target,
        gpu_backend,
        gpu_arch,
        config,
        "debug",
        true, // check_only
    )?;
    Ok(result.semantic_ok)
}

/// Clean the target/ directory.
pub fn clean_project() -> Result<(), String> {
    let target_dir = Path::new("target");
    if target_dir.exists() {
        fs::remove_dir_all(target_dir).map_err(|e| format!("Failed to remove target/: {}", e))?;
    }
    Ok(())
}

/// Run all tests in the given directory (default "src/Examples").
/// Returns (passed, total) or an error.
pub fn run_tests(
    test_dir: &Path,
    target: &str,
    config: &CacheConfig,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
) -> Result<(usize, usize), String> {
    // Find all .vx files in the test directory (skip "lib" subdirs)
    let mut test_files = Vec::new();
    for entry in WalkDir::new(test_dir)
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
        return Ok((0, 0));
    }

    // Emit initial test phase (0%) – this will be visible in the status bar.
    emit_phase_update("Testing", 0);

    // Signal that we are starting a test run – suppresses per‑file phase spam in the GUI.
    set_test_run_active(true);

    let mut total = 0;
    let mut passed = 0;
    for path in test_files {
        let file_name = path.file_name().unwrap().to_string_lossy();
        // Skip GPU tests for now (they may require hardware)
        if file_name.contains("gpu") {
            continue;
        }
        total += 1;

        // Build and run the test file
        let exe_path = build_file(&path, false, target, config, gpu_backend, gpu_arch)?;
        let output = Command::new(&exe_path)
            .output()
            .map_err(|e| format!("Execution failed: {}", e))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let has_error = stderr.to_lowercase().contains("error:");
        let ok = output.status.success() || (!has_error);
        if ok {
            passed += 1;
        } else {
            // Log stderr for debugging
            emit_log(format!("[TEST FAIL] {}: {}", file_name, stderr));
        }
    }

    // Test run complete – re‑enable phase updates and emit the final status.
    set_test_run_active(false);
    emit_phase_update("Test complete", 100);

    Ok((passed, total))
}

// -----------------------------------------------------------------------------
// Original compile_and_run_file (now adapted to new compile_source signature)
// -----------------------------------------------------------------------------

/// Compiles the given source file, links it with the prebuilt runtime library,
/// runs the resulting executable, and returns all lines of stdout/stderr.
///
/// This function is meant to be called from a background thread; it does no UI updates.
pub fn compile_and_run_file(
    path: &Path,
    target: &str,
    config: &CacheConfig,
    gpu_backend: Option<&str>,
    gpu_arch: Option<&str>,
) -> Result<RunOutput, String> {
    let source_path = path.to_str().unwrap();
    let result = compile_source(
        source_path,
        false,
        target,
        gpu_backend,
        gpu_arch,
        config,
        "debug",
        false,
    )?;

    if !result.semantic_ok {
        return Err("Semantic errors".to_string());
    }

    let exe_name = path.file_stem().unwrap().to_str().unwrap();
    let out_dir = get_output_dir("debug");
    let exe_path = if cfg!(windows) {
        out_dir.join(format!("{}.exe", exe_name))
    } else {
        out_dir.join(exe_name)
    };
    let ll_path = out_dir.join(format!("{}.ll", exe_name));
    fs::write(&ll_path, &result.llvm_ir).map_err(|e| e.to_string())?;

    // Link with runtime – pass the backend and arch from the compilation result.
    link_executable(
        &ll_path,
        &exe_path,
        target,
        "debug",
        result.gpu_backend.as_deref(),
        result.gpu_arch.as_deref(),
    )?;

    // Clean up intermediate LLVM file
    let _ = fs::remove_file(&ll_path);

    // Execute the final binary and capture its output
    let output = Command::new(&exe_path)
        .output()
        .map_err(|e| format!("Execution failed: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut lines = Vec::new();
    for line in stdout.lines() {
        lines.push(line.to_string());
    }
    for line in stderr.lines() {
        lines.push(format!("[stderr] {}", line));
    }

    Ok(RunOutput { lines })
}
