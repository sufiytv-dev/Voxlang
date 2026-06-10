# Standard Library

> **⚠️ Work in progress** – The Voxlang standard library is being rewritten from scratch in Voxlang itself (self‑hosting). Many modules are currently stubs or minimal implementations. The core functionality required for the conformance test suite is complete, but the library is not yet ready for production.

## Built‑in Types (Compiler Primitives)

These types are baked into the compiler and do not require an import.

| Type        | Description                                          |
|-------------|------------------------------------------------------|
| `i8`…`i64`  | Signed integers (2’s complement)                     |
| `u8`…`u64`  | Unsigned integers (zero‑extended to LLVM signed)     |
| `f32`, `f64`| IEEE 754 floating point                             |
| `char`      | Unicode scalar value (32‑bit)                        |
| `bool`      | `i32` with `0` = false, non‑zero = true              |
| `&str`      | Immutable string slice `{ i8*, i64 }`                |
| `String`    | Owned heap string `{ i8*, i64, i64 }`                |
| `[T; N]`    | Fixed‑size array                                     |
| `[]T`       | Dynamic array (runtime length)                       |
| `Vec<T>`    | Growable array (opaque handle)                       |
| `HashMap<K,V>` | Hash map (opaque handle)                          |
| `Option<T>` | `{ i32, T }` – discriminant: 0=None, 1=Some          |
| `Result<T,E>`| `{ i32, T, E }` – discriminant: 0=Ok, 1=Err         |

## Core Functions (Always Available)

The following functions are implemented in the runtime library (`vox_rt`) and can be used without an explicit import. Most are minimal but sufficient for the test suite.

### Arithmetic (Checked)

- `vox_add_i32(a: i32, b: i32) -> i32` – panics on overflow
- `vox_sub_i32(a: i32, b: i32) -> i32`
- `vox_mul_i32(a: i32, b: i32) -> i32`
- `vox_div_i32(a: i32, b: i32) -> i32` – panics on division by zero
- `vox_rem_i32(a: i32, b: i32) -> i32`
- Likewise for `i64` variants.

### String & Memory

- `vox_string_new() -> i8*` – new empty `String`
- `vox_string_from(ptr: i8*, len: i64) -> i8*` – `String` from `&str`
- `vox_string_append_bytes(handle: i8*, data: i8*, len: i64)`
- `vox_string_compare(...) -> i32` – lexicographic comparison
- `len(v: Vec<T>) -> i64`, `len(v: &str) -> i64`, `len(arr: [T; N]) -> i64`
- `push(v: &mut Vec<T>, val: T)`
- `pop(v: &mut Vec<T>) -> Option<T>`
- `insert(m: &mut HashMap<K,V>, key: K, val: V)`
- `get(m: &HashMap<K,V>, key: K) -> Option<V>`
- `contains_key(m: &HashMap<K,V>, key: K) -> i32` (0/1)
- `remove(m: &mut HashMap<K,V>, key: K) -> Option<V>`

### Assertions & Panics

- `assert(cond: i32, msg: &str)` – panics with message if `cond == 0`
- `vox_panic()` – aborts the program

## Modules (Planned / Stubs)

The following modules are placeholders for the self‑hosted standard library. Most currently contain only type definitions and stubs.

| Module      | Status      | Description                                |
|-------------|-------------|--------------------------------------------|
| `std::mem`  | 🟡 Minimal   | `size_of`, `align_of`, `offset_of`         |
| `std::ptr`  | 🟡 Minimal   | `read`, `write`, `copy_nonoverlapping`     |
| `std::slice`| 🟡 Minimal   | `from_raw_parts`, `iter` (stub)            |
| `std::iter` | 🔴 Stub      | `Iterator` trait (not yet used)            |
| `std::vec`  | 🟢 Complete  | `Vec<T>` operations (runtime)              |
| `std::string`| 🟢 Complete | `String` concatenation, comparison         |
| `std::hash` | 🟡 Minimal   | `HashMap` (runtime)                        |
| `std::io`   | 🔴 Stub      | No I/O except `println` via runtime        |
| `std::fmt`  | 🔴 Stub      | Placeholder for formatting                 |
| `std::option`| 🟢 Complete | `Option<T>` constructors and combinators   |
| `std::result`| 🟢 Complete | `Result<T,E>` with `?` operator            |

## Self‑Hosting Roadmap

The compiler currently depends on a Rust‑written runtime (`vox_rt`). The goal is to rewrite **every** standard library component in Voxlang itself, making the compiler fully self‑hosted.

**Current status:**
- ✅ Core runtime functions (checked arithmetic, `Vec`, `HashMap`, `String`) – implemented in Rust, exposed as foreign functions.
- 🟡 Wrappers for those functions – being transcribed to Voxlang.
- 🔴 Pure Voxlang modules – mostly stubs.

**Next steps:**
1. Finish the `std::mem` and `std::ptr` modules.
2. Implement `std::iter` and adapt `Vec` to use it.
3. Move all `vox_rt` functionality into `.vx` source files.
4. Compile the compiler with itself.

Once the standard library is complete, Voxlang will be **self‑hosting** – the compiler written in Voxlang will compile itself. This is the single largest remaining milestone before v1.0.

## Contributing

Standard library development is the highest priority. If you are interested in systems programming, memory safety, or language implementation, please join the effort. See `CONTRIBUTING.md` for guidelines.

---

> For now, rely on the built‑in functions and the runtime. The standard library is minimal but sufficient to pass the conformance test suite. More functionality will arrive as we approach self‑hosting.
