// device.rs - GPU‑specific IR assembly and binary finalization.
//
// Contains methods for emitting the device module header, compiling device IR
// to PTX (NVIDIA) or HSACO (AMD), and detecting kernel functions in the AST.

use crate::codegen::CodegenEngine;
use crate::codegen::helpers::create_temp_file;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
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
    }

    /// Finalize device code by compiling the accumulated device IR to a binary
    /// (PTX for NVIDIA, HSACO for AMD) and return the binary bytes.
    pub(crate) fn finalize_device_code(&mut self, triple: &str) -> Option<Vec<u8>> {
        if self.device_ir.is_empty() {
            return None;
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
            crate::diagnostic::debug_log(format!(
                "Saved device IR to {}",
                device_ll_path.display()
            ));
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

        if triple.contains("nvptx") {
            let gpu_arch = "sm_70";
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
            let status = match cmd.status() {
                Ok(s) => s,
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
            if !status.success() {
                emit_diagnostic(
                    &Diagnostic::error(&format!("llc failed to compile PTX (arch={})", gpu_arch))
                        .with_code("VX0417"),
                );
                let _ = fs::remove_file(&ir_path);
                let _ = fs::remove_file(&out_path);
                return None;
            }
            if let Ok(ptx_data) = fs::read(&out_path) {
                let device_ptx_path = debug_dir.join("device.ptx");
                if let Err(e) = fs::write(&device_ptx_path, &ptx_data) {
                    crate::diagnostic::debug_log(format!(
                        "Warning: could not write device.ptx: {}",
                        e
                    ));
                } else {
                    crate::diagnostic::debug_log(format!(
                        "Saved PTX to {}",
                        device_ptx_path.display()
                    ));
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
            // Clean up temp files
            let _ = fs::remove_file(&ir_path);
            let _ = fs::remove_file(&out_path);
            result
        } else if triple.contains("amdgcn") {
            let gpu_arch = "gfx1200";
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
            let status = match llc_cmd.status() {
                Ok(s) => s,
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
            if !status.success() {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "llc failed to compile device IR to object file (arch={})",
                        gpu_arch
                    ))
                    .with_code("VX0417"),
                );
                let _ = fs::remove_file(&ir_path);
                let _ = fs::remove_file(&obj_path);
                return None;
            }

            let ld_path = self
                .lld_path
                .as_ref()
                .map(|p| p.as_os_str())
                .unwrap_or_else(|| "ld.lld".as_ref());
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
            let mut link_cmd = Command::new(ld_path);
            link_cmd
                .arg("-shared")
                .arg("-export-dynamic")
                .arg("-o")
                .arg(&hsaco_path)
                .arg(&obj_path);
            let status = match link_cmd.status() {
                Ok(s) => s,
                Err(e) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Failed to execute ld.lld: {}", e))
                            .with_code("VX0416"),
                    );
                    let _ = fs::remove_file(&ir_path);
                    let _ = fs::remove_file(&obj_path);
                    let _ = fs::remove_file(&hsaco_path);
                    return None;
                }
            };
            if !status.success() {
                emit_diagnostic(
                    &Diagnostic::error("ld.lld failed to link object into HSACO")
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
                    crate::diagnostic::debug_log(format!(
                        "Warning: could not write device.hsaco: {}",
                        e
                    ));
                } else {
                    crate::diagnostic::debug_log(format!(
                        "Saved HSACO to {}",
                        device_hsaco_path.display()
                    ));
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
            // Clean up temp files
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
