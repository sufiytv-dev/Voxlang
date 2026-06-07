// runtime.rs - Runtime declarations and module header emission.
//
// Contains methods for emitting the LLVM module header, declaring external
// runtime functions (panic, arithmetic, arrays, strings, Vec, HashMap, GPU, etc.).

use crate::codegen::CodegenEngine;
use crate::diagnostic::{Diagnostic, emit_diagnostic};

impl CodegenEngine {
    /// Emit the LLVM module header (target triple, datalayout, and declarations
    /// for all runtime functions provided by vox_rt.lib).
    pub(crate) fn emit_module_header(&mut self) {
        let (triple, datalayout) = match self.target_triple.as_str() {
            "x86_64-pc-windows-msvc" => (
                "x86_64-pc-windows-msvc",
                "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
            ),
            "x86_64-unknown-linux-gnu" => (
                "x86_64-unknown-linux-gnu",
                "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
            ),
            "x86_64-apple-darwin" => (
                "x86_64-apple-darwin",
                "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
            ),
            _ => {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Unsupported target triple: {}",
                        self.target_triple
                    ))
                    .with_code("VX0400"),
                );
                self.has_error = true;
                return;
            }
        };

        self.debug_emit("; --- Raw IR Module Emitted by Voxlang ---");
        self.debug_emit(&format!("target datalayout = \"{}\"", datalayout));
        self.debug_emit(&format!("target triple = \"{}\"\n", triple));

        // FFI Bridge Targets (platform‑specific)
        if self.target_triple.contains("windows") {
            self.debug_emit("; External Linkage Call Sites (FFI Bridge Targets)");
            self.debug_emit("declare i32 @MessageBoxA(i8*, i8*, i8*, i32)");
        } else {
            self.debug_emit("; External Linkage Call Sites (Unix)");
        }
        // Common C library functions used by generated code
        self.debug_emit("declare i32 @puts(i8*)");
        self.debug_emit("declare void @exit(i32)");
        self.debug_emit("");

        // Panic handlers (provided by vox_rt.lib)
        self.debug_emit("; Panic handlers (provided by vox_rt.lib)");
        self.debug_emit("declare void @vox_panic()");
        self.debug_emit("declare void @vox_overflow_panic(i8*, i32, i32)");
        self.debug_emit("declare void @vox_divide_by_zero_panic()");
        self.debug_emit("");

        // Checked arithmetic support (from vox_rt.rs)
        self.debug_emit("; Checked arithmetic support (from vox_rt.rs)");
        self.debug_emit("declare i32 @vox_add_i32(i32, i32)");
        self.debug_emit("declare i32 @vox_sub_i32(i32, i32)");
        self.debug_emit("declare i32 @vox_mul_i32(i32, i32)");
        self.debug_emit("declare i32 @vox_div_i32(i32, i32)");
        self.debug_emit("declare i32 @vox_rem_i32(i32, i32)");
        self.debug_emit("declare i64 @vox_add_i64(i64, i64)");
        self.debug_emit("declare i64 @vox_sub_i64(i64, i64)");
        self.debug_emit("declare i64 @vox_mul_i64(i64, i64)");
        self.debug_emit("declare i64 @vox_div_i64(i64, i64)");
        self.debug_emit("declare i64 @vox_rem_i64(i64, i64)");
        self.debug_emit("");

        // Sovereign dispatcher (provided by vox_rt.lib)
        self.debug_emit("; Sovereign dispatcher (provided by vox_rt.lib)");
        self.debug_emit("declare void @vox_dispatch_parallel(i8*, i8*, i64, i64)");
        self.debug_emit("");

        // Dynamic array runtime support
        self.debug_emit("; Dynamic array runtime support");
        self.debug_emit("declare i8* @vox_array_alloc(i64, i64)");
        self.debug_emit("declare void @vox_array_free(i8*)");
        self.debug_emit("declare void @vox_array_push(i8*, i8*, i64)");
        self.debug_emit("declare void @vox_array_pop(i8*, i8*, i64)");
        self.debug_emit("declare i64 @vox_array_len(i8*)");
        self.debug_emit("");

        // String runtime support
        self.debug_emit("; String runtime support");
        self.debug_emit("declare i8* @vox_string_alloc(i64)");
        self.debug_emit("declare i8* @vox_string_realloc(i8*, i64, i64)");
        self.debug_emit("declare void @vox_string_append_bytes(i8*, i8*, i64)");
        self.debug_emit("declare i32 @vox_string_compare(i8*, i64, i8*, i64)");
        self.debug_emit("declare i8* @vox_string_new()");
        self.debug_emit("declare i8* @vox_string_from(i8*, i64)");
        self.debug_emit("");

        // Vec<T> runtime support
        self.debug_emit("; Vec<T> runtime support");
        self.debug_emit("declare i8* @vox_vec_new(i64)");
        self.debug_emit("declare void @vox_vec_push(i8*, i8*)");
        self.debug_emit("declare i32 @vox_vec_pop(i8*, i8*)");
        self.debug_emit("declare i64 @vox_vec_len(i8*)");
        self.debug_emit("declare i32 @vox_vec_get(i8*, i64, i8*)");
        self.debug_emit("declare void @vox_vec_drop(i8*)");
        self.debug_emit("");

        // HashMap runtime support
        self.debug_emit("; HashMap runtime support");
        self.debug_emit("declare i8* @vox_hashmap_new(i64, i64)");
        self.debug_emit("declare void @vox_hashmap_insert(i8*, i8*, i8*)");
        self.debug_emit("declare i32 @vox_hashmap_get(i8*, i8*, i8*)");
        self.debug_emit("declare i32 @vox_hashmap_contains_key(i8*, i8*)");
        self.debug_emit("declare i32 @vox_hashmap_remove(i8*, i8*, i8*)");
        self.debug_emit("declare i64 @vox_hashmap_len(i8*)");
        self.debug_emit("declare void @vox_hashmap_drop(i8*)");
        self.debug_emit("");

        // memcpy intrinsic
        self.debug_emit("; memcpy intrinsic");
        self.debug_emit("declare void @llvm.memcpy.p0i8.p0i8.i64(i8*, i8*, i64, i1)");
        self.debug_emit("");

        // -----------------------------------------------------------------
        // NEW: Declarations for epintf / printf support (vox_eprint_str, etc.)
        // -----------------------------------------------------------------
        self.debug_emit("; epintf / printf support (stderr/stdout)");
        self.debug_emit("declare i32 @vox_eprint_str(i8*, i32)");
        self.debug_emit("declare i32 @vox_eprintln_str(i8*, i32)");
        self.debug_emit("declare i32 @vox_print_str(i8*, i32)");
        self.debug_emit("declare i32 @vox_println_str(i8*, i32)");
        self.debug_emit("");
    }

    /// Emit GPU runtime declarations (HIP / CUDA) into the host IR.
    /// This is idempotent; it only emits once.
    pub(crate) fn emit_gpu_runtime_declarations(&mut self) {
        if self.gpu_decls_emitted {
            return;
        }
        self.debug_emit("; GPU runtime support (HIP / CUDA)");
        self.debug_emit("declare void @vox_load_device_module(i8*, i64)");
        self.debug_emit("declare i32 @vox_launch_kernel_1d(i8*, i8**, i32, i32, i32)");
        self.debug_emit("declare i8* @vox_gpu_malloc(i64)");
        self.debug_emit("declare void @vox_gpu_free(i8*)");
        self.debug_emit("declare void @vox_gpu_memcpy_host_to_device(i8*, i8*, i64)");
        self.debug_emit("declare void @vox_gpu_memcpy_device_to_host(i8*, i8*, i64)");
        self.debug_emit("");
        self.gpu_decls_emitted = true;
    }
}
