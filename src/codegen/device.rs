// device.rs - GPU‑specific IR assembly and binary finalization.
//
// Contains methods for emitting the device module header, compiling device IR
// to PTX (NVIDIA), HSACO (AMD), or embedding MSL source (Apple Metal), and detecting
// kernel functions in the AST.
//
// UPDATED (2026-06-29):
// - Metal backend: now returns the MSL source as a byte string (instead of compiling
//   to AIR/metallib). The runtime will compile the source on-device using
//   newLibraryWithSource:options:error: for maximum compatibility.
// - Added extreme debug logging for MSL generation and source preview.
// - Removed all invocations of `metal` and `metallib` – no longer needed.

use crate::CodegenEngine;
use crate::codegen::helpers::create_temp_file;
use crate::codegen::msl;
use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::parser::ASTNode;
use std::fs::{self};
use std::path::PathBuf;
use std::process::Command;

impl CodegenEngine {
    /// Emit the LLVM header for the device module (target triple and datalayout).
    /// For Metal, this is a no‑op because we generate MSL directly.
    pub(crate) fn emit_global_device_header(&mut self, triple: &str) {
        if let Some(ref mode) = self.gpu_mode {
            if mode == "metal" {
                debug_log("[DEVICE] Skipping LLVM header for Metal backend");
                return;
            }
        }

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
    // (Only used for NVPTX; for Metal we generate MSL directly)
    // ------------------------------------------------------------------------
    fn add_kernel_attributes(&mut self) {
        if let Some(ref mode) = self.gpu_mode {
            if mode == "metal" {
                return;
            }
        }

        let mut ir = std::mem::take(&mut self.device_ir);
        let mut metadata_nodes = Vec::new();
        let mut idx = 0;

        for kernel_name in &self.kernel_names {
            let pattern = format!("define void @{}(", kernel_name);
            if let Some(pos) = ir.find(&pattern) {
                let line_end = ir[pos..].find('\n').unwrap_or(0) + pos;
                let line = &ir[pos..line_end];
                if !line.contains('#') {
                    if let Some(brace_pos) = line.find('{') {
                        let new_line = format!("{} #0 {{", &line[..brace_pos]);
                        ir.replace_range(pos..line_end, &new_line);
                    }
                }
            }

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
            let args_start = def_line.find('(').unwrap() + 1;
            let args_end = def_line.rfind(')').unwrap();
            let args_part = &def_line[args_start..args_end];
            let arg_tys: Vec<String> = args_part
                .split(',')
                .map(|s| s.trim().split_whitespace().next().unwrap().to_string())
                .collect();
            let type_str = format!("void ({})", arg_tys.join(", "));

            let metadata = format!(
                "!{} = !{{ {}* @{}, !\"kernel\", i32 1 }}",
                idx, type_str, kernel_name
            );
            metadata_nodes.push(metadata);
            idx += 1;
        }

        if !self.kernel_names.is_empty() {
            ir.push_str("\nattributes #0 = { \"kernel\" }\n");
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
        if let Some(ref path) = self.lld_path {
            if path.exists() {
                return path.clone();
            }
            debug_log(&format!(
                "[DEVICE] stored lld_path '{}' does not exist, falling back to search",
                path.display()
            ));
        }

        let candidates = vec![
            PathBuf::from("C:\\Program Files\\AMD\\ROCm\\7.1\\bin\\ld.lld.exe"),
            PathBuf::from("C:\\Program Files\\AMD\\ROCm\\7.0\\bin\\ld.lld.exe"),
            PathBuf::from("C:\\Users\\Sufiy\\scoop\\apps\\llvm\\current\\bin\\ld.lld.exe"),
            PathBuf::from("C:\\Program Files\\LLVM\\bin\\ld.lld.exe"),
            PathBuf::from("ld.lld"),
            PathBuf::from("ld.lld.exe"),
        ];

        for candidate in candidates {
            if candidate.exists() {
                debug_log(&format!("[DEVICE] Found ld.lld at {}", candidate.display()));
                return candidate;
            }
        }

        debug_log("[DEVICE] No explicit ld.lld found, will rely on PATH");
        PathBuf::from("ld.lld")
    }

    /// Finalize device code by compiling the accumulated device IR to a binary
    /// (PTX for NVIDIA, HSACO for AMD) OR returning the MSL source as bytes (Metal).
    ///
    /// # Parameters
    /// - `triple`: device target triple (e.g., "nvptx64-nvidia-cuda", "amdgcn-amd-amdhsa", "metal-apple-macos")
    /// - `ast`: optional AST node (required for Metal backend to generate MSL).
    pub(crate) fn finalize_device_code(
        &mut self,
        triple: &str,
        ast: Option<&ASTNode>,
    ) -> Option<Vec<u8>> {
        // ----------------------------------------------
        // Metal backend: generate MSL source and return it as bytes.
        // The runtime will compile it on-device using newLibraryWithSource:options:error:.
        // ----------------------------------------------
        if self.gpu_mode.as_deref() == Some("metal") || triple.contains("metal") {
            debug_log("[DEVICE] ========== METAL BACKEND ==========");
            debug_log("[DEVICE] Generating MSL source (will be JIT‑compiled at runtime)");

            let program_ast = match ast {
                Some(a) => a,
                None => {
                    emit_diagnostic(
                        &Diagnostic::error("No program AST provided for Metal code generation")
                            .with_code("VX9002"),
                    );
                    return None;
                }
            };

            let msl_source = msl::generate_msl(self, program_ast);
            if msl_source.is_empty() {
                emit_diagnostic(
                    &Diagnostic::error("Generated MSL source is empty; no kernels found?")
                        .with_code("VX9003"),
                );
                return None;
            }

            // Log the first 256 chars of MSL for debugging
            let msl_preview = if msl_source.len() > 256 {
                format!("{}...", &msl_source[..256])
            } else {
                msl_source.clone()
            };
            debug_log(&format!("[DEVICE] MSL source preview: {}", msl_preview));
            debug_log(&format!(
                "[DEVICE] MSL source length: {} bytes",
                msl_source.len()
            ));

            // Save MSL source for debugging.
            let debug_dir = PathBuf::from("target").join("debug");
            let _ = std::fs::create_dir_all(&debug_dir);
            let device_metal_path = debug_dir.join("device.metal");
            if let Err(e) = fs::write(&device_metal_path, &msl_source) {
                debug_log(&format!(
                    "[DEVICE] Warning: could not write device.metal: {}",
                    e
                ));
            } else {
                debug_log(&format!(
                    "[DEVICE] Saved MSL to {}",
                    device_metal_path.display()
                ));
            }

            // Return the MSL source as bytes – the runtime will compile it.
            debug_log("[DEVICE] ========== METAL BACKEND SUCCESS (MSL source embedded) ==========");
            return Some(msl_source.into_bytes());
        }

        // ----------------------------------------------
        // Existing NVIDIA/AMD paths (unchanged below)
        // ----------------------------------------------
        if self.device_ir.is_empty() {
            debug_log("[DEVICE] No device IR to finalize");
            return None;
        }

        self.add_kernel_attributes();

        if triple.contains("nvptx") {
            debug_log("[DEVICE] Converting explicit pointer types to opaque pointers for NVPTX");
            let mut ir = std::mem::take(&mut self.device_ir);
            ir = ir.replace("i32 addrspace(5)*", "ptr addrspace(5)");
            ir = ir.replace("i32 addrspace(1)*", "ptr addrspace(1)");
            ir = ir.replace("i32*", "ptr addrspace(5)");
            ir = ir.replace("i32**", "ptr addrspace(5)");
            ir = ir.replace("i8*", "ptr");
            self.device_ir = ir;
            debug_log("[DEVICE] Pointer conversion complete");
        }

        let debug_dir = PathBuf::from("target").join("debug");
        let _ = std::fs::create_dir_all(&debug_dir);

        let device_ll_path = debug_dir.join("device.ll");
        if let Err(e) = fs::write(&device_ll_path, &self.device_ir) {
            emit_diagnostic(&Diagnostic::error(&format!(
                "Warning: could not write device.ll: {}",
                e
            )));
        } else {
            debug_log(format!("Saved device IR to {}", device_ll_path.display()));
        }

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
