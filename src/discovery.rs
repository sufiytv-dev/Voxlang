// discovery.rs - Automatic toolchain discovery for Voxlang.

use crate::diagnostic::debug_log;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LlvmPaths {
    pub clang: PathBuf,
    pub llc: PathBuf,
    pub lld: Option<PathBuf>, // ld.lld for AMD GPU linking
}

#[derive(Debug, Clone)]
pub struct GpuPaths {
    pub hip_path: Option<PathBuf>,
    pub cuda_path: Option<PathBuf>,
}

/// Auto‑detect LLVM tools (clang, llc, lld).
pub fn find_llvm_tools() -> Result<LlvmPaths, String> {
    if let Ok(llvm_path) = env::var("LLVM_PATH") {
        debug_log(format!("Checking LLVM_PATH: {}", llvm_path));
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
        if clang.exists() && llc.exists() {
            debug_log("Found LLVM tools via LLVM_PATH");
            return Ok(LlvmPaths {
                clang,
                llc,
                lld: if lld.exists() { Some(lld) } else { None },
            });
        }
    }

    #[cfg(target_os = "windows")]
    let common_paths = vec![
        PathBuf::from("C:\\Program Files\\LLVM\\bin"),
        PathBuf::from("C:\\LLVM\\bin"),
    ];
    #[cfg(not(target_os = "windows"))]
    let common_paths = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")];

    for bin_dir in common_paths {
        debug_log(format!("Checking common path: {}", bin_dir.display()));
        let clang = bin_dir
            .join("clang")
            .with_extension(env::consts::EXE_EXTENSION);
        let llc = bin_dir
            .join("llc")
            .with_extension(env::consts::EXE_EXTENSION);
        let lld = bin_dir
            .join("ld.lld")
            .with_extension(env::consts::EXE_EXTENSION);
        if clang.exists() && llc.exists() {
            debug_log(format!("Found clang/llc in {}", bin_dir.display()));
            return Ok(LlvmPaths {
                clang,
                llc,
                lld: if lld.exists() { Some(lld) } else { None },
            });
        }
    }

    debug_log("No LLVM installation found; falling back to PATH names");
    Ok(LlvmPaths {
        clang: PathBuf::from("clang"),
        llc: PathBuf::from("llc"),
        lld: Some(PathBuf::from("ld.lld")),
    })
}

/// Auto‑detect GPU backends.
pub fn find_gpu_backend(backend: &str) -> Option<GpuPaths> {
    match backend {
        "hip" => {
            let hip_path = env::var("HIP_PATH").ok().or_else(|| {
                let candidates = vec![
                    "C:\\Program Files\\AMD\\ROCm\\5.7",
                    "C:\\Program Files\\AMD\\ROCm\\6.0",
                    "C:\\Program Files\\AMD\\ROCm\\7.1",
                    "/opt/rocm",
                ];
                candidates
                    .into_iter()
                    .find(|p| Path::new(p).exists())
                    .map(|s| s.to_string())
            });
            if let Some(ref path) = hip_path {
                debug_log(format!("Found HIP at {}", path));
            } else {
                debug_log("HIP not found");
            }
            Some(GpuPaths {
                hip_path: hip_path.map(PathBuf::from),
                cuda_path: None,
            })
        }
        "cuda" => {
            let cuda_path = env::var("CUDA_PATH").ok().or_else(|| {
                let candidates = vec![
                    "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.8",
                    "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.6",
                    "/usr/local/cuda",
                ];
                candidates
                    .into_iter()
                    .find(|p| Path::new(p).exists())
                    .map(|s| s.to_string())
            });
            if let Some(ref path) = cuda_path {
                debug_log(format!("Found CUDA at {}", path));
            } else {
                debug_log("CUDA not found");
            }
            Some(GpuPaths {
                hip_path: None,
                cuda_path: cuda_path.map(PathBuf::from),
            })
        }
        _ => {
            debug_log(format!("Unrecognized GPU backend: {}", backend));
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
        debug_log(format!("Found Z3 at {}", path.display()));
    } else {
        debug_log("Z3 not found");
    }
    result
}
