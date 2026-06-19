// discovery.rs - Automatic toolchain discovery for Voxlang.

use crate::diagnostic::debug_log;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct LlvmPaths {
    pub clang: PathBuf,
    pub llc: PathBuf,
    pub lld: Option<PathBuf>,      // ld.lld for AMD GPU linking
    pub system_libs: Vec<PathBuf>, // Discovered SDK/CRT library paths for link resolution
}

#[derive(Debug, Clone)]
pub struct GpuPaths {
    pub hip_path: Option<PathBuf>,
    pub cuda_path: Option<PathBuf>,
}

/// SDK information for GPU backends (CUDA or HIP/ROCm).
#[derive(Debug, Clone)]
pub struct GpuSdk {
    pub backend: String, // "cuda" or "hip"
    pub bin_path: PathBuf,
    pub lib_path: PathBuf,
    pub include_path: Option<PathBuf>,
    pub version: String,
}

// ============================================================================
// Windows system library discovery (MSVC + Windows SDK)
// ============================================================================

/// Try to locate Visual Studio and Windows SDK library paths using `vswhere`.
#[cfg(target_os = "windows")]
fn find_vs_paths_via_vswhere() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Try to find vswhere in common locations
    let vswhere_candidates = [
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\vswhere.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\vswhere.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Professional\Common7\Tools\vswhere.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Enterprise\Common7\Tools\vswhere.exe",
    ];

    let vswhere_path = vswhere_candidates
        .iter()
        .find(|p| Path::new(p).exists())
        .map(PathBuf::from);

    let vswhere = match vswhere_path {
        Some(p) => p,
        None => {
            debug_log("[DISCOVERY] vswhere not found, falling back to static paths.");
            return paths;
        }
    };

    // Get the installation path of the latest VS
    let output = Command::new(&vswhere)
        .args(&[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output();

    let install_path = match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                return paths;
            }
            PathBuf::from(s)
        }
        _ => return paths,
    };

    // Find MSVC toolchain version directory
    let vc_tools_root = install_path.join("VC").join("Tools").join("MSVC");
    if let Ok(entries) = std::fs::read_dir(&vc_tools_root) {
        let mut versions: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                // Version directories are like "14.39.33519"
                if name.chars().any(|c| c.is_ascii_digit()) {
                    Some((name, e.path()))
                } else {
                    None
                }
            })
            .collect();

        // Sort by version descending (semver-like)
        versions.sort_by(|a, b| b.0.cmp(&a.0));
        if let Some((_, version_dir)) = versions.first() {
            let lib_x64 = version_dir.join("lib").join("x64");
            if lib_x64.exists() {
                debug_log(&format!(
                    "[DISCOVERY] Found MSVC libs in {}",
                    lib_x64.display()
                ));
                paths.push(lib_x64);
            }
        }
    }

    // Find Windows SDK paths
    let kits_root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\Lib");
    if let Ok(entries) = std::fs::read_dir(&kits_root) {
        let mut versions: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                // Version directories like "10.0.22621.0"
                if name.starts_with("10.") && name.contains('.') {
                    Some((name, e.path()))
                } else {
                    None
                }
            })
            .collect();

        // Sort by version descending (version tuple comparison)
        versions.sort_by(|a, b| {
            let a_parts: Vec<u32> = a.0.split('.').filter_map(|s| s.parse().ok()).collect();
            let b_parts: Vec<u32> = b.0.split('.').filter_map(|s| s.parse().ok()).collect();
            b_parts.cmp(&a_parts)
        });

        if let Some((_, version_dir)) = versions.first() {
            let ucrt_x64 = version_dir.join("ucrt").join("x64");
            let um_x64 = version_dir.join("um").join("x64");
            if ucrt_x64.exists() {
                debug_log(&format!(
                    "[DISCOVERY] Found UCRT libs in {}",
                    ucrt_x64.display()
                ));
                paths.push(ucrt_x64);
            }
            if um_x64.exists() {
                debug_log(&format!(
                    "[DISCOVERY] Found Windows SDK um libs in {}",
                    um_x64.display()
                ));
                paths.push(um_x64);
            }
        }
    }

    paths
}

/// Dynamic discovery for Windows MSVC and Windows SDK lib targets.
#[cfg(target_os = "windows")]
fn find_windows_system_libs() -> Vec<PathBuf> {
    // First attempt with vswhere
    let mut paths = find_vs_paths_via_vswhere();

    // If we found some paths, return them (they should be sufficient)
    if !paths.is_empty() {
        return paths;
    }

    // Fallback: static hardcoded paths for common VS 2022 editions and Windows SDK
    debug_log("[DISCOVERY] vswhere failed or no paths found, falling back to static paths.");

    // MSVC Toolchain Libs (VS 2022 Editions)
    let vs_editions = ["Community", "BuildTools", "Professional", "Enterprise"];
    for edition in &vs_editions {
        let base_msvc = format!(
            r"C:\Program Files\Microsoft Visual Studio\2022\{}\VC\Tools\MSVC",
            edition
        );
        if let Ok(entries) = std::fs::read_dir(&base_msvc) {
            let mut versions: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();

            versions.sort(); // alphabetical, but version numbers are lexicographically similar
            if let Some(highest_version) = versions.last() {
                let lib_x64 = highest_version.join(r"lib\x64");
                if lib_x64.exists() {
                    debug_log(&format!(
                        "[DISCOVERY] Found MSVC libs in {}",
                        lib_x64.display()
                    ));
                    paths.push(lib_x64);
                    break;
                }
            }
        }
    }

    // Windows SDK Libs (User Mode and Universal CRT)
    let sdk_base = r"C:\Program Files (x86)\Windows Kits\10\Lib";
    if let Ok(entries) = std::fs::read_dir(sdk_base) {
        let mut versions: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();

        versions.sort();
        if let Some(highest_version) = versions.last() {
            let ucrt_x64 = highest_version.join(r"ucrt\x64");
            let um_x64 = highest_version.join(r"um\x64");
            if ucrt_x64.exists() {
                debug_log(&format!(
                    "[DISCOVERY] Found UCRT libs in {}",
                    ucrt_x64.display()
                ));
                paths.push(ucrt_x64);
            }
            if um_x64.exists() {
                debug_log(&format!(
                    "[DISCOVERY] Found Windows SDK um libs in {}",
                    um_x64.display()
                ));
                paths.push(um_x64);
            }
        }
    }

    // FINAL FALLBACK: If still no paths found, attempt to parse the LIB environment variable
    // This is common in CI environments where the VS Developer Command Prompt has set LIB.
    if paths.is_empty() {
        if let Ok(lib_var) = env::var("LIB") {
            debug_log("[DISCOVERY] No system libs found; using LIB environment variable paths.");
            for entry in lib_var.split(';') {
                let p = PathBuf::from(entry);
                if p.exists() && p.is_dir() {
                    paths.push(p);
                }
            }
        }
    }

    paths
}

// ============================================================================
// LLVM tool detection
// ============================================================================

/// Auto‑detect LLVM tools (clang, llc, lld).
pub fn find_llvm_tools() -> Result<LlvmPaths, String> {
    // Proactively discover system libraries on Windows to backfill environment definitions
    let system_libs = {
        #[cfg(target_os = "windows")]
        {
            let libs = find_windows_system_libs();
            if !libs.is_empty() {
                let current_lib = env::var("LIB").unwrap_or_default();
                let mut paths_str = Vec::new();
                if !current_lib.is_empty() {
                    paths_str.push(current_lib);
                }
                for p in &libs {
                    if let Some(s) = p.to_str() {
                        paths_str.push(s.to_string());
                    }
                }

                // SAFETY: Toolchain discovery is invoked early within a single-threaded
                // execution context prior to spawning any concurrent background or UI threads,
                // guaranteeing that environment modifications are safe and free from data races.
                unsafe {
                    env::set_var("LIB", paths_str.join(";"));
                }
                debug_log(
                    "[DISCOVERY] Seeded %LIB% environment variable with MSVC and Windows SDK paths",
                );
            }
            libs
        }
        #[cfg(not(target_os = "windows"))]
        {
            Vec::new()
        }
    };

    if let Ok(llvm_path) = env::var("LLVM_PATH") {
        debug_log(format!("[DISCOVERY] Checking LLVM_PATH: {}", llvm_path));
        let base = PathBuf::from(&llvm_path);
        let clang = base
            .join("bin")
            .join("clang")
            .with_extension(env::consts::EXE_EXTENSION);
        let llc = base
            .join("bin")
            .join("llc")
            .with_extension(env::consts::EXE_EXTENSION);
        let lld = base
            .join("bin")
            .join("ld.lld")
            .with_extension(env::consts::EXE_EXTENSION);
        if clang.exists() {
            debug_log("[DISCOVERY] Found LLVM tools via LLVM_PATH");
            let verified_llc = if llc.exists() { llc } else { clang.clone() };
            return Ok(LlvmPaths {
                clang,
                llc: verified_llc,
                lld: if lld.exists() { Some(lld) } else { None },
                system_libs,
            });
        }
    }

    #[cfg(target_os = "windows")]
    let common_paths = {
        let mut paths = Vec::new();

        if let Ok(user_profile) = env::var("USERPROFILE") {
            paths.push(PathBuf::from(user_profile).join(r"scoop\apps\llvm\current\bin"));
        }

        paths.push(PathBuf::from(r"C:\ProgramData\scoop\apps\llvm\current\bin"));
        paths.push(PathBuf::from(r"C:\Program Files\LLVM\bin"));
        paths.push(PathBuf::from(r"C:\LLVM\bin"));
        paths
    };

    #[cfg(not(target_os = "windows"))]
    let common_paths = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")];

    for bin_dir in common_paths {
        debug_log(format!(
            "[DISCOVERY] Checking common path: {}",
            bin_dir.display()
        ));
        let clang = bin_dir
            .join("clang")
            .with_extension(env::consts::EXE_EXTENSION);
        let llc = bin_dir
            .join("llc")
            .with_extension(env::consts::EXE_EXTENSION);
        let lld = bin_dir
            .join("ld.lld")
            .with_extension(env::consts::EXE_EXTENSION);

        if clang.exists() {
            debug_log(format!("[DISCOVERY] Found clang in {}", bin_dir.display()));

            let verified_llc = if llc.exists() {
                llc
            } else {
                debug_log(
                    "[DISCOVERY] llc.exe not found in this distribution; using clang as backend fallback",
                );
                clang.clone()
            };

            return Ok(LlvmPaths {
                clang,
                llc: verified_llc,
                lld: if lld.exists() { Some(lld) } else { None },
                system_libs,
            });
        }
    }

    debug_log("[DISCOVERY] No LLVM installation found; falling back to PATH names");
    Ok(LlvmPaths {
        clang: PathBuf::from("clang"),
        llc: PathBuf::from("llc"),
        lld: Some(PathBuf::from("ld.lld")),
        system_libs,
    })
}

// -----------------------------------------------------------------------------
// GPU SDK Detection (dynamic version scanning with environment priority)
// -----------------------------------------------------------------------------

/// Detect the installed GPU SDK (CUDA or ROCm) by scanning common paths.
/// Returns `Some(GpuSdk)` with the highest version found, or `None` if none installed.
pub fn find_gpu_sdk() -> Option<GpuSdk> {
    // 1. Check environment variables first (user override)
    if let Some(cuda) = find_cuda_sdk_from_env() {
        debug_log(format!(
            "[DISCOVERY] Using CUDA from CUDA_PATH: {}",
            cuda.bin_path.display()
        ));
        return Some(cuda);
    }
    if let Some(hip) = find_hip_sdk_from_env() {
        debug_log(format!(
            "[DISCOVERY] Using HIP from HIP_PATH: {}",
            hip.bin_path.display()
        ));
        return Some(hip);
    }

    // 2. Fallback to scanning common directories
    if let Some(cuda) = find_cuda_sdk_by_scanning() {
        debug_log(format!(
            "[DISCOVERY] Found CUDA version {} at {}",
            cuda.version,
            cuda.bin_path.display()
        ));
        return Some(cuda);
    }
    if let Some(hip) = find_hip_sdk_by_scanning() {
        debug_log(format!(
            "[DISCOVERY] Found HIP version {} at {}",
            hip.version,
            hip.bin_path.display()
        ));
        return Some(hip);
    }

    debug_log("[DISCOVERY] No GPU SDK detected.");
    None
}

/// Public wrapper: find CUDA SDK (environment first, then scanning).
pub fn find_cuda_sdk() -> Option<GpuSdk> {
    find_cuda_sdk_from_env().or_else(find_cuda_sdk_by_scanning)
}

/// Public wrapper: find HIP SDK (environment first, then scanning).
pub fn find_hip_sdk() -> Option<GpuSdk> {
    find_hip_sdk_from_env().or_else(find_hip_sdk_by_scanning)
}

/// Check CUDA_PATH environment variable.
fn find_cuda_sdk_from_env() -> Option<GpuSdk> {
    let path = env::var("CUDA_PATH").ok()?;
    let base = PathBuf::from(&path);
    if !base.exists() {
        return None;
    }
    let bin_path = base.join("bin");
    let lib_path = base.join("lib").join("x64");
    let include_path = Some(base.join("include"));
    // Try to extract version from the path (e.g., v12.0) or from the environment
    let version = base
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|s| {
            if s.starts_with('v') {
                s[1..].parse::<f64>().ok()
            } else {
                None
            }
        })
        .map(|v| format!("{:.1}", v))
        .unwrap_or_else(|| "unknown".to_string());
    Some(GpuSdk {
        backend: "cuda".to_string(),
        bin_path,
        lib_path,
        include_path,
        version,
    })
}

/// Check HIP_PATH environment variable.
fn find_hip_sdk_from_env() -> Option<GpuSdk> {
    let path = env::var("HIP_PATH").ok()?;
    let base = PathBuf::from(&path);
    if !base.exists() {
        return None;
    }
    let bin_path = base.join("bin");
    let lib_path = base.join("lib");
    let include_path = Some(base.join("include"));
    // Try to extract version from the path (e.g., 7.1)
    let version = base
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| format!("{:.1}", v))
        .unwrap_or_else(|| "unknown".to_string());
    Some(GpuSdk {
        backend: "hip".to_string(),
        bin_path,
        lib_path,
        include_path,
        version,
    })
}

// -----------------------------------------------------------------------------
// Scanning functions (environment‑fallback)
// -----------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn find_cuda_sdk_by_scanning() -> Option<GpuSdk> {
    let base = PathBuf::from("C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA");
    if !base.exists() {
        return None;
    }
    scan_cuda_versions(&base)
}

#[cfg(not(target_os = "windows"))]
fn find_cuda_sdk_by_scanning() -> Option<GpuSdk> {
    // Linux: check /usr/local/cuda-* and /usr/local/cuda
    let base = PathBuf::from("/usr/local");
    if !base.exists() {
        return None;
    }
    let mut versions = Vec::new();

    // Check for versioned directories like cuda-12.0, cuda-12.1, etc.
    for entry in std::fs::read_dir(&base).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("cuda-") {
            if let Ok(ver) = name_str[5..].parse::<f64>() {
                versions.push((ver, entry.path()));
            }
        }
    }
    // Also check /usr/local/cuda symlink
    let symlink = base.join("cuda");
    if symlink.exists() {
        versions.push((999.0, symlink));
    }
    if versions.is_empty() {
        return None;
    }
    versions.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let (_, path) = versions.first()?;
    let bin_path = path.join("bin");
    let lib_path = path.join("lib64");
    let include_path = Some(path.join("include"));
    Some(GpuSdk {
        backend: "cuda".to_string(),
        bin_path,
        lib_path,
        include_path,
        version: format!("{:.1}", versions.first()?.0),
    })
}

#[cfg(target_os = "windows")]
fn find_hip_sdk_by_scanning() -> Option<GpuSdk> {
    let base = PathBuf::from("C:\\Program Files\\AMD\\ROCm");
    if !base.exists() {
        return None;
    }
    scan_hip_versions(&base)
}

#[cfg(not(target_os = "windows"))]
fn find_hip_sdk_by_scanning() -> Option<GpuSdk> {
    // Linux: check /opt/rocm-* and /opt/rocm
    let base = PathBuf::from("/opt/rocm");
    if !base.exists() {
        return None;
    }
    let mut versions = Vec::new();

    // Check for versioned directories like rocm-7.0, rocm-7.1, etc.
    for entry in std::fs::read_dir(&base).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("rocm-") {
            if let Ok(ver) = name_str[5..].parse::<f64>() {
                versions.push((ver, entry.path()));
            }
        }
    }
    // Also check /opt/rocm symlink
    let symlink = base;
    if symlink.exists() {
        versions.push((999.0, symlink));
    }
    if versions.is_empty() {
        return None;
    }
    versions.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let (_, path) = versions.first()?;
    let bin_path = path.join("bin");
    let lib_path = path.join("lib");
    let include_path = Some(path.join("include"));
    Some(GpuSdk {
        backend: "hip".to_string(),
        bin_path,
        lib_path,
        include_path,
        version: format!("{:.1}", versions.first()?.0),
    })
}

// Shared helpers for Windows scanning
#[cfg(target_os = "windows")]
fn scan_cuda_versions(base: &Path) -> Option<GpuSdk> {
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(base).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("v") {
            if let Ok(ver) = name_str[1..].parse::<f64>() {
                versions.push((ver, entry.path()));
            }
        }
    }
    if versions.is_empty() {
        return None;
    }
    versions.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let (_, path) = versions.first()?;
    let bin_path = path.join("bin");
    let lib_path = path.join("lib").join("x64");
    let include_path = Some(path.join("include"));
    Some(GpuSdk {
        backend: "cuda".to_string(),
        bin_path,
        lib_path,
        include_path,
        version: format!("{:.1}", versions.first()?.0),
    })
}

#[cfg(target_os = "windows")]
fn scan_hip_versions(base: &Path) -> Option<GpuSdk> {
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(base).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Ok(ver) = name_str.parse::<f64>() {
            versions.push((ver, entry.path()));
        }
    }
    if versions.is_empty() {
        return None;
    }
    versions.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let (_, path) = versions.first()?;
    let bin_path = path.join("bin");
    let lib_path = path.join("lib");
    let include_path = Some(path.join("include"));
    Some(GpuSdk {
        backend: "hip".to_string(),
        bin_path,
        lib_path,
        include_path,
        version: format!("{:.1}", versions.first()?.0),
    })
}

// -----------------------------------------------------------------------------
// Legacy GPU backend finder (now uses the new detection)
// -----------------------------------------------------------------------------

/// Auto‑detect GPU backends (legacy wrapper, kept for compatibility).
pub fn find_gpu_backend(backend: &str) -> Option<GpuPaths> {
    let sdk = find_gpu_sdk();
    match (backend, sdk) {
        ("hip", Some(sdk)) if sdk.backend == "hip" => Some(GpuPaths {
            hip_path: Some(sdk.bin_path.parent().unwrap().to_path_buf()),
            cuda_path: None,
        }),
        ("cuda", Some(sdk)) if sdk.backend == "cuda" => Some(GpuPaths {
            hip_path: None,
            cuda_path: Some(sdk.bin_path.parent().unwrap().to_path_buf()),
        }),
        _ => {
            debug_log(format!("[DISCOVERY] GPU backend '{}' not found.", backend));
            None
        }
    }
}

/// Find Z3 library path (optional, used for refinement proofs).
pub fn find_z3() -> Option<PathBuf> {
    let result = env::var("Z3_PATH").ok().map(PathBuf::from).or_else(|| {
        #[cfg(target_os = "windows")]
        let candidates = vec!["C:\\Program Files\\Z3\\bin\\z3.dll", "C:\\z3\\bin\\z3.dll"];
        #[cfg(target_os = "linux")]
        let candidates = vec!["/usr/lib/libz3.so", "/usr/local/lib/libz3.so"];
        #[cfg(target_os = "macos")]
        let candidates = vec!["/usr/local/lib/libz3.dylib"];
        candidates
            .into_iter()
            .find(|p| Path::new(p).exists())
            .map(PathBuf::from)
    });
    if let Some(ref path) = result {
        debug_log(format!("[DISCOVERY] Found Z3 at {}", path.display()));
    } else {
        debug_log("[DISCOVERY] Z3 not found");
    }
    result
}
