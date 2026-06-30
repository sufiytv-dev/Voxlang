// ir_builder.rs - Low‑level IR emission and register/block management.
//
// Extracted from the original utils.rs.
// Contains methods for emitting IR lines, managing registers and blocks,
// and tracking block termination.

use crate::codegen::CodegenEngine;

impl CodegenEngine {
    /// Log a message if global debug mode is enabled.
    pub fn debug_log(&self, msg: &str) {
        if crate::diagnostic::global_debug() {
            crate::diagnostic::debug_log(format!("[CODEGEN] {}", msg));
        }
    }

    /// Emit a line to the host IR, with clean logging.
    pub fn debug_emit(&mut self, line: &str) {
        // Log the IR line with a simple [IR] prefix
        if crate::diagnostic::global_debug() {
            crate::diagnostic::debug_log(format!("[IR] {}", line));
        }

        // Special handling for closing braces – log separately
        if line.trim() == "}" {
            let func_stack = self
                .current_function_stack
                .last()
                .map(|s| s.as_str())
                .unwrap_or("<none>");
            let msg = format!("[CODEGEN] Closing brace for function '{}'", func_stack);
            crate::diagnostic::debug_log(msg);
        }

        // Reset block_terminated when starting a new block or closing a function
        if line.ends_with(':') && !line.trim_start().starts_with("define") {
            self.block_terminated = false;
        }
        if line.trim() == "}" {
            self.block_terminated = false;
        }

        // Append the line to IR
        self.ir.push_str(line);
        self.ir.push('\n');
    }

    /// Emit a line to the device IR, with clean logging.
    pub fn debug_emit_device(&mut self, line: &str) {
        if crate::diagnostic::global_debug() {
            crate::diagnostic::debug_log(format!("[IR:device] {}", line));
        }
        self.device_ir.push_str(line);
        self.device_ir.push('\n');
    }

    /// Allocate a fresh SSA numbered register name (e.g., `%0`, `%1`, `%2`).
    /// These are guaranteed to be sequential starting from 0 per function.
    pub fn next_register(&mut self) -> String {
        let reg = format!("%{}", self.register_counter);
        self.debug_log(&format!("allocated new register {}", reg));
        self.register_counter += 1;
        reg
    }

    /// Allocate a unique named alloca (e.g., `%x.addr_0`).
    /// Uses a separate counter so named allocas don't interfere with numbered
    /// SSA register numbering.
    pub fn fresh_alloca_name(&mut self, base: &str) -> String {
        let name = format!("%{}.addr_{}", base, self.alloca_counter);
        self.alloca_counter += 1;
        name
    }

    /// Generate a fresh block label (e.g., `block0`).
    pub fn next_block(&mut self) -> String {
        let lbl = format!("block{}", self.block_counter);
        self.block_counter += 1;
        lbl
    }

    /// Generate a fresh worker function name for parallel loops.
    pub fn next_worker_name(&mut self) -> String {
        let name = format!("__vox_parallel_worker_{}", self.worker_counter);
        self.worker_counter += 1;
        name
    }

    /// Reset register and block counters for a new function.
    /// Register counter starts at 0 so the first numbered register is %0.
    /// Alloca counter starts at 0 for named allocas.
    /// Block counter starts at 0 for block labels.
    pub fn reset_for_new_function(&mut self) {
        self.register_counter = 0;
        self.alloca_counter = 0;
        self.block_counter = 0;
    }

    /// Check whether the device IR currently ends with a terminator instruction.
    /// Scans backwards from the end of the device IR string.
    pub fn is_device_block_terminated(&self) -> bool {
        let lines: Vec<&str> = self.device_ir.lines().collect();
        for line in lines.iter().rev() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }
            return trimmed.starts_with("ret ")
                || trimmed.starts_with("br ")
                || trimmed.starts_with("unreachable");
        }
        false
    }
}
