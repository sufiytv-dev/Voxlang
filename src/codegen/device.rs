// device.rs - GPU‑specific IR assembly and binary finalization.
//
// Contains methods for emitting the device module header, compiling device IR
// to PTX (NVIDIA) or HSACO (AMD), and detecting kernel functions in the AST.
//
// UPDATED (2026-06-14):
// - Dynamically generates kernel metadata with correct function types.
// - Captures stderr from llc for detailed error reporting.
// - Added extensive debug logging.
// - POST‑PROCESSING: converts explicit pointer types to opaque `ptr` with address spaces.
// - FIXED NVVM: use function attribute "kernel" placed correctly before '{'.
// - FIXED NVVM: emit !nvvm.annotations metadata with correct syntax (!0 = !{...}).
// - IMPROVED: Linker path resolution – searches known locations if self.lld_path is None.
// - BETTER: Error messages when llc or ld.lld fail.

use crate::codegen::CodegenEngine;
use crate::codegen::helpers::create_temp_file;
use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::parser::ASTNode;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

impl CodegenEngine {
    /// Emit the LLVM header for the device module (target triple and datalayout).
    pub(crate) fn emit_global_device_header(&mut self, triple: &str) {
        self.device_ir
            .push_str(&format!("; Device module for {}\n", triple));
        self.device_ir
            .push_str(&format!("target triple = \"{}\"\n", triple));
        let datalayout = if triple.contains("nvptx") {
            "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v32:32:32-v64:64:64-v128:128:128-n16:32:64"
        } else if triple.contains("amdgcn") {
            "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v32:32:32-v64:64:64-v128:128:128-n16:32:64"
        } else {
            "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v32:32:32-v64:64:64-v128:128:128-n16:32:64"
        };
        self.device_ir
            .push_str(&format!("target datalayout = \"{}\"\n\n", datalayout));
        debug_log(format!(
            "[DEVICE] Emitted global header for triple: {}",
            triple
        ));
    }

    // ------------------------------------------------------------------------
    // Helper: add "kernel" attribute AND NVVM annotations to each kernel function
    // ------------------------------------------------------------------------
    fn add_kernel_attributes(&mut self) {
        let mut ir = std::mem::take(&mut self.device_ir);
        let mut metadata_nodes = Vec::new();
        let mut idx = 0;

        for kernel_name in &self.kernel_names {
            // Find the function definition line: define void @kernel_name(
            let pattern = format!("define void @{}(", kernel_name);
            if let Some(pos) = ir.find(&pattern) {
                // Find the end of the line (newline)
                let line_end = ir[pos..].find('\n').unwrap_or(0) + pos;
                let line = &ir[pos..line_end];
                // If the line doesn't already have an attribute group
                if !line.contains('#') {
                    // Replace the opening brace with " #0 {"
                    if let Some(brace_pos) = line.find('{') {
                        let new_line = format!("{} #0 {{", &line[..brace_pos]);
                        ir.replace_range(pos..line_end, &new_line);
                    }
                }
            }

            // Extract the function type from the IR to build correct metadata.
            // Find the function definition line again to parse argument types.
            let def_start = if let Some(p) = ir.find(&pattern) {
                p
            } else {
                continue;
            };
            let def_end = ir[def_start..]
                .find('{')
                .map(|p| def_start + p)
                .unwrap_or(def_start);
            let def_line = &ir[def_start..def_end];
            // def_line example: "define void @add_kernel(i32 %a, i32 %b, ptr addrspace(1) %result)"
            // Extract argument types.
            let args_start = def_line.find('(').unwrap() + 1;
            let args_end = def_line.rfind(')').unwrap();
            let args_part = &def_line[args_start..args_end];
            let arg_tys: Vec<String> = args_part
                .split(',')
                .map(|s| s.trim().split_whitespace().next().unwrap().to_string())
                .collect();
            let type_str = format!("void ({})", arg_tys.join(", "));

            // Correct metadata syntax: !0 = !{ void (i32, i32, ptr addrspace(1))* @add_kernel, !"kernel", i32 1 }
            let metadata = format!(
                "!{} = !{{ {}* @{}, !\"kernel\", i32 1 }}",
                idx, type_str, kernel_name
            );
            metadata_nodes.push(metadata);
            idx += 1;
        }

        // Append the attribute declaration at the end of the module
        if !self.kernel_names.is_empty() {
            ir.push_str("\nattributes #0 = { \"kernel\" }\n");

            // Emit NVVM annotations metadata
            ir.push_str("\n!nvvm.annotations = !{");
            for i in 0..metadata_nodes.len() {
                if i > 0 {
                    ir.push_str(", ");
                }
                ir.push_str(&format!("!{}", i));
            }
            ir.push_str("}\n");
            for node in metadata_nodes {
                ir.push_str(&node);
                ir.push('\n');
            }
        }

        self.device_ir = ir;
        debug_log("[DEVICE] Added kernel attributes and NVVM annotations");
    }

    /// Helper to locate `ld.lld` if `self.lld_path` is not usable.
    fn find_lld(&self) -> PathBuf {
        // If we already have a path and it exists, use it.
        if let Some(ref path) = self.lld_path {
            if path.exists() {
                return path.clone();
            }
            debug_log(&format!(
                "[DEVICE] stored lld_path '{}' does not exist, falling back to search",
                path.display()
            ));
        }

        // Common locations to search for ld.lld on Windows.
        let candidates = vec![
            // ROCm bin directory (most important for HIP)
            PathBuf::from("C:\\Program Files\\AMD\\ROCm\\7.1\\bin\\ld.lld.exe"),
            PathBuf::from("C:\\Program Files\\AMD\\ROCm\\7.0\\bin\\ld.lld.exe"),
            // LLVM from scoop
            PathBuf::from("C:\\Users\\Sufiy\\scoop\\apps\\llvm\\current\\bin\\ld.lld.exe"),
            // Standard LLVM installation
            PathBuf::from("C:\\Program Files\\LLVM\\bin\\ld.lld.exe"),
            // Just the name (relies on PATH)
            PathBuf::from("ld.lld"),
            PathBuf::from("ld.lld.exe"),
        ];

        for candidate in candidates {
            if candidate.exists() {
                debug_log(&format!("[DEVICE] Found ld.lld at {}", candidate.display()));
                return candidate;
            }
        }

        // Final fallback – we will try to run "ld.lld" and let the OS search PATH.
        debug_log("[DEVICE] No explicit ld.lld found, will rely on PATH");
        PathBuf::from("ld.lld")
    }

    /// Finalize device code by compiling the accumulated device IR to a binary
    /// (PTX for NVIDIA, HSACO for AMD) and return the binary bytes.
    pub(crate) fn finalize_device_code(&mut self, triple: &str) -> Option<Vec<u8>> {
        if self.device_ir.is_empty() {
            debug_log("[DEVICE] No device IR to finalize");
            return None;
        }

        // -----------------------------------------------------------------
        // Add "kernel" attributes to all kernel functions
        // -----------------------------------------------------------------
        self.add_kernel_attributes();

        // -----------------------------------------------------------------
        // POST‑PROCESSING: Convert explicit pointer types to opaque `ptr` with address spaces
        // This makes the IR compatible with the NVPTX backend.
        // -----------------------------------------------------------------
        if triple.contains("nvptx") {
            debug_log("[DEVICE] Converting explicit pointer types to opaque pointers for NVPTX");
            let mut ir = std::mem::take(&mut self.device_ir);
            ir = ir.replace("i32 addrspace(5)*", "ptr addrspace(5)");
            ir = ir.replace("i32 addrspace(1)*", "ptr addrspace(1)");
            ir = ir.replace("i32*", "ptr addrspace(5)");
            ir = ir.replace("i32**", "ptr addrspace(5)");
            // Also handle any remaining `i8*` that might appear (e.g., from runtime calls)
            ir = ir.replace("i8*", "ptr");
            self.device_ir = ir;
            debug_log("[DEVICE] Pointer conversion complete");
        }

        let debug_dir = PathBuf::from("target").join("debug");
        if let Err(e) = std::fs::create_dir_all(&debug_dir) {
            emit_diagnostic(
                &Diagnostic::error(&format!("Failed to create target/debug directory: {}", e))
                    .with_code("VX0423"),
            );
            return None;
        }

        let device_ll_path = debug_dir.join("device.ll");
        if let Err(e) = fs::write(&device_ll_path, &self.device_ir) {
            emit_diagnostic(&Diagnostic::error(&format!(
                "Warning: could not write device.ll: {}",
                e
            )));
        } else {
            debug_log(format!("Saved device IR to {}", device_ll_path.display()));
        }

        // Create a temporary file for the IR
        let (ir_path, _ir_file) = match create_temp_file("vox_ir", ".ll") {
            Ok(pair) => pair,
            Err(e) => {
                emit_diagnostic(
                    &Diagnostic::error(&format!("Failed to create temp file for device IR: {}", e))
                        .with_code("VX0412"),
                );
                return None;
            }
        };
        if let Err(e) = fs::write(&ir_path, &self.device_ir) {
            emit_diagnostic(
                &Diagnostic::error(&format!("Failed to write device IR: {}", e))
                    .with_code("VX0413"),
            );
            let _ = fs::remove_file(&ir_path);
            return None;
        }
        debug_log(format!(
            "Wrote device IR to temporary file: {}",
            ir_path.display()
        ));

        if triple.contains("nvptx") {
            let default_arch = "sm_70".to_string();
            let gpu_arch = self.gpu_arch.as_ref().unwrap_or(&default_arch);
            let (out_path, _out_file) = match create_temp_file("vox_ptx", ".ptx") {
                Ok(pair) => pair,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to create temp output file: {}", e))
                            .with_code("VX0415"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    return None;
                }
            };
            let mut cmd = Command::new(&self.llc_path);
            cmd.args(&["-march=nvptx64", "-mcpu", gpu_arch, "-o"])
                .arg(&out_path)
                .arg(&ir_path);
            debug_log(format!("[DEVICE] Running llc: {:?}", cmd));

            let output = match cmd.output() {
                Ok(o) => o,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to execute llc: {}", e))
                            .with_code("VX0416"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    let _ = fs::remove_file(&out_path);
                    return None;
                }
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug_log(format!("[DEVICE] llc stderr: {}", stderr));
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "llc failed to compile PTX (arch={}): {}",
                        gpu_arch, stderr
                    ))
                    .with_code("VX0417"),
                );
                let _ = fs::remove_file(&ir_path);
                let _ = fs::remove_file(&out_path);
                return None;
            }
            debug_log("[DEVICE] llc succeeded");

            if let Ok(ptx_data) = fs::read(&out_path) {
                let device_ptx_path = debug_dir.join("device.ptx");
                if let Err(e) = fs::write(&device_ptx_path, &ptx_data) {
                    debug_log(format!("Warning: could not write device.ptx: {}", e));
                } else {
                    debug_log(format!("Saved PTX to {}", device_ptx_path.display()));
                }
            } else {
                debug_log("[DEVICE] Could not read PTX binary for debugging");
            }
            let result = match fs::read(&out_path) {
                Ok(b) => Some(b),
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to read PTX binary: {}", e))
                            .with_code("VX0418"),
                    );
                    None
                }
            };
            let _ = fs::remove_file(&ir_path);
            let _ = fs::remove_file(&out_path);
            result
        } else if triple.contains("amdgcn") {
            let default_arch = "gfx1200".to_string();
            let gpu_arch = self.gpu_arch.as_ref().unwrap_or(&default_arch);
            let (obj_path, _obj_file) = match create_temp_file("vox_obj", ".o") {
                Ok(pair) => pair,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to create temp object file: {}", e))
                            .with_code("VX0415"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    return None;
                }
            };
            let mut llc_cmd = Command::new(&self.llc_path);
            llc_cmd
                .args(&[
                    "-march=amdgcn",
                    "-mcpu",
                    gpu_arch,
                    "--amdhsa-code-object-version=5",
                    "-filetype=obj",
                    "-o",
                ])
                .arg(&obj_path)
                .arg(&ir_path);
            debug_log(format!("[DEVICE] Running llc for AMD: {:?}", llc_cmd));
            let output = match llc_cmd.output() {
                Ok(o) => o,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to execute llc: {}", e))
                            .with_code("VX0416"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    let _ = fs::remove_file(&obj_path);
                    return None;
                }
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug_log(format!("[DEVICE] llc stderr: {}", stderr));
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "llc failed to compile device IR to object file (arch={}): {}",
                        gpu_arch, stderr
                    ))
                    .with_code("VX0417"),
                );
                let _ = fs::remove_file(&ir_path);
                let _ = fs::remove_file(&obj_path);
                return None;
            }

            // Determine the linker executable
            let linker = self.find_lld();
            debug_log(&format!("[DEVICE] Using linker: {}", linker.display()));

            let (hsaco_path, _hsaco_file) = match create_temp_file("vox_hsaco", ".hsaco") {
                Ok(pair) => pair,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to create temp output file: {}", e))
                            .with_code("VX0415"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    let _ = fs::remove_file(&obj_path);
                    return None;
                }
            };
            let mut link_cmd = Command::new(&linker);
            link_cmd
                .arg("-shared")
                .arg("-export-dynamic")
                .arg("-o")
                .arg(&hsaco_path)
                .arg(&obj_path);
            debug_log(format!("[DEVICE] Running linker: {:?}", link_cmd));
            let output = match link_cmd.output() {
                Ok(o) => o,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Failed to execute linker '{}': {}",
                            linker.display(),
                            e
                        ))
                        .with_code("VX0416"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    let _ = fs::remove_file(&obj_path);
                    let _ = fs::remove_file(&hsaco_path);
                    return None;
                }
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug_log(format!("[DEVICE] Linker stderr: {}", stderr));
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Linker '{}' failed to link object into HSACO: {}",
                        linker.display(),
                        stderr
                    ))
                    .with_code("VX0417"),
                );
                let _ = fs::remove_file(&ir_path);
                let _ = fs::remove_file(&obj_path);
                let _ = fs::remove_file(&hsaco_path);
                return None;
            }

            if let Ok(hsaco_data) = fs::read(&hsaco_path) {
                let device_hsaco_path = debug_dir.join("device.hsaco");
                if let Err(e) = fs::write(&device_hsaco_path, &hsaco_data) {
                    debug_log(format!("Warning: could not write device.hsaco: {}", e));
                } else {
                    debug_log(format!("Saved HSACO to {}", device_hsaco_path.display()));
                }
            }

            let result = match fs::read(&hsaco_path) {
                Ok(b) => Some(b),
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to read HSACO binary: {}", e))
                            .with_code("VX0418"),
                    );
                    None
                }
            };
            let _ = fs::remove_file(&ir_path);
            let _ = fs::remove_file(&obj_path);
            let _ = fs::remove_file(&hsaco_path);
            result
        } else {
            emit_diagnostic(
                &Diagnostic::error(&format!("Unsupported device triple: {}", triple))
                    .with_code("VX0414"),
            );
            None
        }
    }

    /// Check whether the AST contains any kernel function definition.
    pub(crate) fn contains_kernel(&self, node: &ASTNode) -> bool {
        match node {
            ASTNode::Program(stmts, _) => stmts.iter().any(|s| self.contains_kernel(s)),
            ASTNode::KernelFn { .. } => true,
            _ => false,
        }
    }
}
