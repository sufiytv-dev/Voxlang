// vox_rt.rs - Runtime support for dynamic arrays, strings, parallel loops, GPU (CUDA & HIP), Vec<T>, and HashMap<K,V>.
// Uses std for logging/threading, direct FFI for C library functions.
//
// FEATURES:
// - Comprehensive debug logging (always enabled) for every operation
// - Checked arithmetic with overflow protection (i32, i64, etc.)
// - Debug print helpers for values
// - Distinguishable panic messages: assertion failures vs. arithmetic overflows
// - eprintln! and print! support (write to stderr/stdout)

use std::ffi::{c_char, c_void};
use std::mem;
use std::ptr;

// Direct FFI declarations for C standard library functions (no libc crate)
extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
    fn memcmp(s1: *const c_void, s2: *const c_void, n: usize) -> i32;
}

// ------------------------------------------------------------------
// Comprehensive debug logging (always enabled)
// ------------------------------------------------------------------
#[inline(always)]
fn vox_rt_log(level: &str, message: &str) {
    eprintln!("[VOX_RT][{}] {}", level, message);
}

// Helper to log a hex dump of a memory region (for debugging)
#[allow(dead_code)]
fn log_hex_dump(ptr: *const c_void, len: usize, label: &str) {
    if ptr.is_null() || len == 0 {
        vox_rt_log("debug", &format!("{}: (null or empty)", label));
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    vox_rt_log("debug", &format!("{} ({} bytes): {}", label, len, hex));
}

// ------------------------------------------------------------------
// Panic functions – now clearly separated
// ------------------------------------------------------------------

/// Called by the compiler for assertion failures (no additional info).
#[no_mangle]
pub extern "C" fn vox_panic() -> ! {
    eprintln!("VOX PANIC: assertion failed (no details)");
    eprintln!("Hint: Use vox_print_int / vox_debug_print_str before assertions to track values.");
    std::process::exit(1);
}

/// Called for assertion failures that include a message string (future extension).
#[no_mangle]
pub extern "C" fn vox_panic_str(msg: *const c_char) -> ! {
    let msg_str = unsafe { std::ffi::CStr::from_ptr(msg) };
    eprintln!(
        "VOX PANIC: assertion failed – {}",
        msg_str.to_string_lossy()
    );
    std::process::exit(1);
}

/// Called when integer overflow is detected (provides operation and operands).
#[no_mangle]
pub extern "C" fn vox_overflow_panic(op: *const c_char, a: i32, b: i32) -> ! {
    let op_str = unsafe { std::ffi::CStr::from_ptr(op as *const i8) };
    eprintln!(
        "VOX PANIC: integer overflow during {} ({}, {})",
        op_str.to_string_lossy(),
        a,
        b
    );
    std::process::exit(1);
}

/// Called for division or modulo by zero.
#[no_mangle]
pub extern "C" fn vox_divide_by_zero_panic() -> ! {
    eprintln!("VOX PANIC: division or modulo by zero");
    std::process::exit(1);
}

// ------------------------------------------------------------------
// Checked arithmetic (i32)
// ------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn vox_add_i32(a: i32, b: i32) -> i32 {
    vox_rt_log("debug", &format!("vox_add_i32({}, {})", a, b));
    match a.checked_add(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"add\0".as_ptr() as *const c_char, a, b);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_sub_i32(a: i32, b: i32) -> i32 {
    vox_rt_log("debug", &format!("vox_sub_i32({}, {})", a, b));
    match a.checked_sub(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"sub\0".as_ptr() as *const c_char, a, b);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_mul_i32(a: i32, b: i32) -> i32 {
    vox_rt_log("debug", &format!("vox_mul_i32({}, {})", a, b));
    match a.checked_mul(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"mul\0".as_ptr() as *const c_char, a, b);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_div_i32(a: i32, b: i32) -> i32 {
    vox_rt_log("debug", &format!("vox_div_i32({}, {})", a, b));
    if b == 0 {
        vox_rt_log("error", "division by zero, panicking");
        vox_divide_by_zero_panic();
    }
    match a.checked_div(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"div\0".as_ptr() as *const c_char, a, b);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_rem_i32(a: i32, b: i32) -> i32 {
    vox_rt_log("debug", &format!("vox_rem_i32({}, {})", a, b));
    if b == 0 {
        vox_rt_log("error", "modulo by zero, panicking");
        vox_divide_by_zero_panic();
    }
    match a.checked_rem(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"rem\0".as_ptr() as *const c_char, a, b);
        }
    }
}

// ------------------------------------------------------------------
// Checked arithmetic (i64)
// ------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn vox_add_i64(a: i64, b: i64) -> i64 {
    vox_rt_log("debug", &format!("vox_add_i64({}, {})", a, b));
    match a.checked_add(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"add_i64\0".as_ptr() as *const c_char, a as i32, b as i32);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_sub_i64(a: i64, b: i64) -> i64 {
    vox_rt_log("debug", &format!("vox_sub_i64({}, {})", a, b));
    match a.checked_sub(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"sub_i64\0".as_ptr() as *const c_char, a as i32, b as i32);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_mul_i64(a: i64, b: i64) -> i64 {
    vox_rt_log("debug", &format!("vox_mul_i64({}, {})", a, b));
    match a.checked_mul(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"mul_i64\0".as_ptr() as *const c_char, a as i32, b as i32);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_div_i64(a: i64, b: i64) -> i64 {
    vox_rt_log("debug", &format!("vox_div_i64({}, {})", a, b));
    if b == 0 {
        vox_rt_log("error", "division by zero, panicking");
        vox_divide_by_zero_panic();
    }
    match a.checked_div(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"div_i64\0".as_ptr() as *const c_char, a as i32, b as i32);
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_rem_i64(a: i64, b: i64) -> i64 {
    vox_rt_log("debug", &format!("vox_rem_i64({}, {})", a, b));
    if b == 0 {
        vox_rt_log("error", "modulo by zero, panicking");
        vox_divide_by_zero_panic();
    }
    match a.checked_rem(b) {
        Some(r) => {
            vox_rt_log("debug", &format!("  -> {}", r));
            r
        }
        None => {
            vox_rt_log("error", "overflow detected, panicking");
            vox_overflow_panic(b"rem_i64\0".as_ptr() as *const c_char, a as i32, b as i32);
        }
    }
}

// ------------------------------------------------------------------
// Debug helpers
// ------------------------------------------------------------------
#[no_mangle]
pub extern "C" fn vox_print_int(value: i32) {
    eprintln!("[DEBUG] i32 = {}", value);
}

#[no_mangle]
pub extern "C" fn vox_print_ptr(ptr: *mut c_void) {
    eprintln!("[DEBUG] ptr = {:p}", ptr);
}

#[no_mangle]
pub extern "C" fn vox_debug_print_str(ptr: *const c_char) {
    if ptr.is_null() {
        eprintln!("[DEBUG] (null string)");
    } else {
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) };
        eprintln!("[DEBUG] str = {}", s.to_string_lossy());
    }
}

// ------------------------------------------------------------------
// eprintf / printf support (write to stderr / stdout)
// ------------------------------------------------------------------
/// Write a string to stderr (raw bytes, no newline). Returns 0 on success, non‑zero on error.
#[no_mangle]
pub extern "C" fn vox_eprint_str(ptr: *const u8, len: usize) -> i32 {
    if len == 0 || ptr.is_null() {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    use std::io::Write;
    let mut stderr = std::io::stderr();
    match stderr.write_all(slice) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// Write a string to stderr followed by a newline. Returns 0 on success, non‑zero on error.
#[no_mangle]
pub extern "C" fn vox_eprintln_str(ptr: *const u8, len: usize) -> i32 {
    let ret = vox_eprint_str(ptr, len);
    if ret == 0 {
        let newline = b"\n";
        use std::io::Write;
        let mut stderr = std::io::stderr();
        match stderr.write_all(newline) {
            Ok(_) => 0,
            Err(_) => 1,
        }
    } else {
        ret
    }
}

/// Write a string to stdout (raw bytes, no newline). Returns 0 on success, non‑zero on error.
#[no_mangle]
pub extern "C" fn vox_print_str(ptr: *const u8, len: usize) -> i32 {
    if len == 0 || ptr.is_null() {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    use std::io::Write;
    let mut stdout = std::io::stdout();
    match stdout.write_all(slice) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// Write a string to stdout followed by a newline. Returns 0 on success, non‑zero on error.
#[no_mangle]
pub extern "C" fn vox_println_str(ptr: *const u8, len: usize) -> i32 {
    let ret = vox_print_str(ptr, len);
    if ret == 0 {
        let newline = b"\n";
        use std::io::Write;
        let mut stdout = std::io::stdout();
        match stdout.write_all(newline) {
            Ok(_) => 0,
            Err(_) => 1,
        }
    } else {
        ret
    }
}

// ------------------------------------------------------------------
// String helper FFI (used by prelude)
// ------------------------------------------------------------------
/// Return the raw pointer from a &str (ignores the length argument).
#[no_mangle]
pub extern "C" fn vox_as_ptr(ptr: *const u8, _len: usize) -> *const u8 {
    ptr
}

/// Return the length of a &str as an i32.
#[no_mangle]
pub extern "C" fn vox_str_len(_ptr: *const u8, len: usize) -> i32 {
    len as i32
}

// ------------------------------------------------------------------
// Dynamic Array Runtime Support
// ------------------------------------------------------------------
#[repr(C)]
struct VoxArray {
    data: *mut c_void,
    len: usize,
    cap: usize,
}

#[no_mangle]
pub extern "C" fn vox_array_alloc(elem_size: usize, capacity: usize) -> *mut c_void {
    vox_rt_log(
        "debug",
        &format!(
            "vox_array_alloc(elem_size={}, capacity={})",
            elem_size, capacity
        ),
    );
    let cap = if capacity == 0 { 1 } else { capacity };
    let data = unsafe { malloc(elem_size * cap) };
    if data.is_null() {
        vox_rt_log("error", "vox_array_alloc: malloc failed");
        return ptr::null_mut();
    }
    let arr = unsafe { malloc(mem::size_of::<VoxArray>()) as *mut VoxArray };
    if arr.is_null() {
        unsafe { free(data) };
        vox_rt_log(
            "error",
            "vox_array_alloc: failed to allocate VoxArray struct",
        );
        return ptr::null_mut();
    }
    unsafe {
        (*arr).data = data;
        (*arr).len = 0;
        (*arr).cap = cap;
    }
    vox_rt_log(
        "debug",
        &format!("  -> allocated array at {:p}, cap={}", arr, cap),
    );
    arr as *mut c_void
}

#[no_mangle]
pub extern "C" fn vox_array_free(arr_ptr: *mut c_void) {
    vox_rt_log("debug", &format!("vox_array_free({:p})", arr_ptr));
    if arr_ptr.is_null() {
        return;
    }
    let arr = arr_ptr as *mut VoxArray;
    unsafe {
        if !(*arr).data.is_null() {
            free((*arr).data);
        }
        free(arr as *mut c_void);
    }
    vox_rt_log("debug", "  -> freed");
}

#[no_mangle]
pub extern "C" fn vox_array_push(arr_ptr: *mut c_void, elem: *mut c_void, elem_size: usize) {
    vox_rt_log(
        "debug",
        &format!("vox_array_push({:p}, {:p}, {})", arr_ptr, elem, elem_size),
    );
    if arr_ptr.is_null() || elem.is_null() {
        vox_rt_log("error", "  -> null pointer, ignoring");
        return;
    }
    let arr = arr_ptr as *mut VoxArray;
    unsafe {
        if (*arr).len == (*arr).cap {
            let new_cap = if (*arr).cap == 0 { 1 } else { (*arr).cap * 2 };
            let new_data = realloc((*arr).data, elem_size * new_cap);
            if new_data.is_null() {
                vox_rt_log("error", "vox_array_push: realloc failed");
                return;
            }
            (*arr).data = new_data;
            (*arr).cap = new_cap;
            vox_rt_log("debug", &format!("  -> resized cap={}", new_cap));
        }
        let dest = ((*arr).data as *mut u8).add((*arr).len * elem_size);
        ptr::copy(elem, dest as *mut c_void, elem_size);
        (*arr).len += 1;
        vox_rt_log("debug", &format!("  -> new len={}", (*arr).len));
    }
}

#[no_mangle]
pub extern "C" fn vox_array_pop(arr_ptr: *mut c_void, out_elem: *mut c_void, elem_size: usize) {
    vox_rt_log(
        "debug",
        &format!(
            "vox_array_pop({:p}, {:p}, {})",
            arr_ptr, out_elem, elem_size
        ),
    );
    if arr_ptr.is_null() || out_elem.is_null() {
        vox_rt_log("error", "  -> null pointer, ignoring");
        return;
    }
    let arr = arr_ptr as *mut VoxArray;
    unsafe {
        if (*arr).len == 0 {
            vox_rt_log("debug", "  -> empty array, zeroing output");
            ptr::write_bytes(out_elem, 0, elem_size);
            return;
        }
        (*arr).len -= 1;
        let src = ((*arr).data as *mut u8).add((*arr).len * elem_size);
        ptr::copy(src as *mut c_void, out_elem, elem_size);
        vox_rt_log("debug", &format!("  -> new len={}", (*arr).len));
    }
}

#[no_mangle]
pub extern "C" fn vox_array_len(arr_ptr: *mut c_void) -> usize {
    if arr_ptr.is_null() {
        vox_rt_log("debug", "vox_array_len(null) -> 0");
        return 0;
    }
    let len = unsafe { (*(arr_ptr as *mut VoxArray)).len };
    vox_rt_log("debug", &format!("vox_array_len({:p}) -> {}", arr_ptr, len));
    len
}

// ------------------------------------------------------------------
// String Runtime Support
// ------------------------------------------------------------------
#[repr(C)]
struct VoxString {
    data: *mut u8,
    len: usize,
    cap: usize,
}

#[no_mangle]
pub extern "C" fn vox_string_alloc(cap: i64) -> *mut c_void {
    vox_rt_log("debug", &format!("vox_string_alloc(cap={})", cap));
    if cap < 0 {
        vox_rt_log("error", "  -> negative capacity, returning null");
        return ptr::null_mut();
    }
    let cap_usize = cap as usize;
    let s = unsafe { malloc(mem::size_of::<VoxString>()) as *mut VoxString };
    if s.is_null() {
        vox_rt_log("error", "  -> malloc failed for VoxString");
        return ptr::null_mut();
    }
    let data = if cap_usize > 0 {
        unsafe { malloc(cap_usize) as *mut u8 }
    } else {
        ptr::null_mut()
    };
    if cap_usize > 0 && data.is_null() {
        unsafe { free(s as *mut c_void) };
        vox_rt_log("error", "  -> malloc failed for data");
        return ptr::null_mut();
    }
    unsafe {
        (*s).data = data;
        (*s).len = 0;
        (*s).cap = cap_usize;
    }
    vox_rt_log(
        "debug",
        &format!("  -> allocated string at {:p}, cap={}", s, cap_usize),
    );
    s as *mut c_void
}

#[no_mangle]
pub extern "C" fn vox_string_realloc(ptr: *mut c_void, new_cap: i64, _old_len: i64) -> *mut c_void {
    vox_rt_log(
        "debug",
        &format!("vox_string_realloc({:p}, new_cap={}, ...)", ptr, new_cap),
    );
    if new_cap < 0 {
        vox_rt_log("error", "  -> negative new_cap, returning null");
        return ptr::null_mut();
    }
    let new_ptr = unsafe { realloc(ptr, new_cap as usize) };
    if new_ptr.is_null() {
        vox_rt_log("error", "  -> realloc failed");
    } else {
        vox_rt_log("debug", &format!("  -> new ptr = {:p}", new_ptr));
    }
    new_ptr
}

#[no_mangle]
pub extern "C" fn vox_string_append_bytes(str_ptr: *mut c_void, bytes: *const c_void, len: i64) {
    vox_rt_log(
        "debug",
        &format!(
            "vox_string_append_bytes({:p}, {:p}, len={})",
            str_ptr, bytes, len
        ),
    );
    if len <= 0 || str_ptr.is_null() {
        vox_rt_log("debug", "  -> nothing to append or null pointer");
        return;
    }
    let s = str_ptr as *mut VoxString;
    let append_len = len as usize;
    unsafe {
        let new_len = (*s).len + append_len;
        if new_len > (*s).cap {
            let new_cap = if (*s).cap * 2 > new_len {
                (*s).cap * 2
            } else {
                new_len
            };
            let new_data = realloc((*s).data as *mut c_void, new_cap) as *mut u8;
            if new_data.is_null() {
                vox_rt_log("error", "string_append_bytes: realloc failed");
                return;
            }
            (*s).data = new_data;
            (*s).cap = new_cap;
            vox_rt_log("debug", &format!("  -> resized cap={}", new_cap));
        }
        if !bytes.is_null() {
            ptr::copy_nonoverlapping(bytes as *const u8, (*s).data.add((*s).len), append_len);
        } else {
            ptr::write_bytes((*s).data.add((*s).len), 0, append_len);
        }
        (*s).len = new_len;
        vox_rt_log("debug", &format!("  -> new len={}", new_len));
    }
}

#[no_mangle]
pub extern "C" fn vox_string_compare(
    left: *const c_void,
    left_len: i64,
    right: *const c_void,
    right_len: i64,
) -> i32 {
    vox_rt_log(
        "debug",
        &format!(
            "vox_string_compare(left={:p}, left_len={}, right={:p}, right_len={})",
            left, left_len, right, right_len
        ),
    );
    let l_len = if left_len < 0 { 0 } else { left_len as usize };
    let r_len = if right_len < 0 { 0 } else { right_len as usize };
    let min_len = if l_len < r_len { l_len } else { r_len };
    let cmp = unsafe { memcmp(left, right, min_len) };
    let result = if cmp != 0 {
        cmp as i32
    } else if l_len < r_len {
        -1
    } else if l_len > r_len {
        1
    } else {
        0
    };
    vox_rt_log("debug", &format!("  -> {}", result));
    result
}

#[no_mangle]
pub extern "C" fn vox_string_new() -> *mut c_void {
    vox_rt_log("debug", "vox_string_new()");
    let s = vox_string_alloc(0);
    vox_rt_log("debug", &format!("  -> {:p}", s));
    s
}

#[no_mangle]
pub extern "C" fn vox_string_from(data: *const c_void, len: i64) -> *mut c_void {
    vox_rt_log(
        "debug",
        &format!("vox_string_from({:p}, len={})", data, len),
    );
    if len < 0 {
        vox_rt_log("error", "  -> negative len, returning null");
        return ptr::null_mut();
    }
    let s = vox_string_alloc(len);
    if !s.is_null() && len > 0 {
        vox_string_append_bytes(s, data, len);
    }
    vox_rt_log("debug", &format!("  -> {:p}", s));
    s
}

// ------------------------------------------------------------------
// Vec<T> Runtime Support (generic, type‑erased, byte‑oriented)
// ------------------------------------------------------------------
#[repr(C)]
struct VoxVec {
    data: *mut u8,
    len: usize,
    cap: usize,
    elem_size: usize,
}

#[no_mangle]
pub extern "C" fn vox_vec_new(elem_size: usize) -> *mut c_void {
    vox_rt_log("debug", &format!("vox_vec_new(elem_size={})", elem_size));
    let vec = unsafe { malloc(mem::size_of::<VoxVec>()) as *mut VoxVec };
    if vec.is_null() {
        vox_rt_log("error", "vox_vec_new: failed to allocate VoxVec struct");
        return ptr::null_mut();
    }
    unsafe {
        (*vec).data = ptr::null_mut();
        (*vec).len = 0;
        (*vec).cap = 0;
        (*vec).elem_size = elem_size;
    }
    vox_rt_log("debug", &format!("  -> new Vec at {:p}", vec));
    vec as *mut c_void
}

#[no_mangle]
pub extern "C" fn vox_vec_push(vec_ptr: *mut c_void, elem_ptr: *mut c_void) {
    vox_rt_log(
        "debug",
        &format!("vox_vec_push({:p}, {:p})", vec_ptr, elem_ptr),
    );
    if vec_ptr.is_null() || elem_ptr.is_null() {
        vox_rt_log("error", "  -> null pointer, ignoring");
        return;
    }
    let vec = vec_ptr as *mut VoxVec;
    unsafe {
        let elem_size = (*vec).elem_size;
        if (*vec).len == (*vec).cap {
            let new_cap = if (*vec).cap == 0 { 4 } else { (*vec).cap * 2 };
            let new_data = realloc((*vec).data as *mut c_void, new_cap * elem_size);
            if new_data.is_null() {
                vox_rt_log("error", "vox_vec_push: realloc failed");
                return;
            }
            (*vec).data = new_data as *mut u8;
            (*vec).cap = new_cap;
            vox_rt_log("debug", &format!("  -> resized cap={}", new_cap));
        }
        let dest = (*vec).data.add((*vec).len * elem_size);
        ptr::copy(elem_ptr, dest as *mut c_void, elem_size);
        (*vec).len += 1;
        vox_rt_log("debug", &format!("  -> new len={}", (*vec).len));
    }
}

#[no_mangle]
pub extern "C" fn vox_vec_pop(vec_ptr: *mut c_void, out_elem: *mut c_void) -> i32 {
    vox_rt_log(
        "debug",
        &format!("vox_vec_pop({:p}, {:p})", vec_ptr, out_elem),
    );
    if vec_ptr.is_null() || out_elem.is_null() {
        vox_rt_log("error", "  -> null pointer, returning 0");
        return 0;
    }
    let vec = vec_ptr as *mut VoxVec;
    unsafe {
        if (*vec).len == 0 {
            vox_rt_log("debug", "  -> empty Vec, returning 0");
            return 0;
        }
        (*vec).len -= 1;
        let elem_size = (*vec).elem_size;
        let src = (*vec).data.add((*vec).len * elem_size);
        ptr::copy(src as *mut c_void, out_elem, elem_size);
        vox_rt_log("debug", &format!("  -> success, new len={}", (*vec).len));
        1
    }
}

#[no_mangle]
pub extern "C" fn vox_vec_len(vec_ptr: *mut c_void) -> usize {
    if vec_ptr.is_null() {
        vox_rt_log("debug", "vox_vec_len(null) -> 0");
        return 0;
    }
    let len = unsafe { (*(vec_ptr as *mut VoxVec)).len };
    vox_rt_log("debug", &format!("vox_vec_len({:p}) -> {}", vec_ptr, len));
    len
}

#[no_mangle]
pub extern "C" fn vox_vec_get(vec_ptr: *mut c_void, idx: usize, out_elem: *mut c_void) -> i32 {
    vox_rt_log(
        "debug",
        &format!("vox_vec_get({:p}, idx={}, {:p})", vec_ptr, idx, out_elem),
    );
    if vec_ptr.is_null() || out_elem.is_null() {
        vox_rt_log("error", "  -> null pointer, returning 0");
        return 0;
    }
    let vec = vec_ptr as *mut VoxVec;
    unsafe {
        if idx >= (*vec).len {
            vox_rt_log(
                "debug",
                &format!("  -> index out of bounds (len={}), returning 0", (*vec).len),
            );
            return 0;
        }
        let elem_size = (*vec).elem_size;
        let src = (*vec).data.add(idx * elem_size);
        ptr::copy(src as *mut c_void, out_elem, elem_size);
        vox_rt_log("debug", "  -> success");
        1
    }
}

#[no_mangle]
pub extern "C" fn vox_vec_drop(vec_ptr: *mut c_void) {
    vox_rt_log("debug", &format!("vox_vec_drop({:p})", vec_ptr));
    if vec_ptr.is_null() {
        return;
    }
    let vec = vec_ptr as *mut VoxVec;
    unsafe {
        if !(*vec).data.is_null() {
            free((*vec).data as *mut c_void);
        }
        free(vec as *mut c_void);
    }
    vox_rt_log("debug", "  -> dropped");
}

// ------------------------------------------------------------------
// HashMap<K,V> Runtime Support
// ------------------------------------------------------------------
#[repr(C)]
struct VoxHashMapEntry {
    key: *mut u8,
    value: *mut u8,
    occupied: bool,
}

struct VoxHashMap {
    entries: *mut VoxHashMapEntry,
    capacity: usize,
    len: usize,
    key_size: usize,
    value_size: usize,
}

fn fnv1a_hash(data: *const u8, len: usize) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    unsafe {
        for i in 0..len {
            hash ^= *data.add(i) as u64;
            hash = hash.wrapping_mul(0x100000001b3u64);
        }
    }
    hash
}

fn byte_eq(a: *const u8, b: *const u8, len: usize) -> bool {
    unsafe { memcmp(a as *const c_void, b as *const c_void, len) == 0 }
}

impl VoxHashMap {
    unsafe fn find_index(&self, key_ptr: *const u8) -> (usize, bool) {
        let hash = fnv1a_hash(key_ptr, self.key_size);
        let mask = self.capacity - 1;
        let mut idx = (hash & mask as u64) as usize;
        let start = idx;
        loop {
            let entry = &*self.entries.add(idx);
            if !entry.occupied {
                return (idx, false);
            }
            if entry.occupied && byte_eq(key_ptr, entry.key, self.key_size) {
                return (idx, true);
            }
            idx = (idx + 1) & mask;
            if idx == start {
                break;
            }
        }
        (idx, false)
    }

    unsafe fn grow(&mut self, new_min_cap: usize) {
        let new_cap = (new_min_cap.next_power_of_two()).max(8);
        let old_entries = self.entries;
        let old_cap = self.capacity;
        let new_entries =
            calloc(new_cap, mem::size_of::<VoxHashMapEntry>()) as *mut VoxHashMapEntry;
        if new_entries.is_null() {
            vox_rt_log("error", "HashMap grow: calloc failed");
            return;
        }
        self.entries = new_entries;
        self.capacity = new_cap;
        self.len = 0;

        for i in 0..old_cap {
            let entry = &mut *old_entries.add(i);
            if entry.occupied {
                let (new_idx, _) = self.find_index(entry.key);
                let new_entry = &mut *self.entries.add(new_idx);
                *new_entry = VoxHashMapEntry {
                    key: entry.key,
                    value: entry.value,
                    occupied: true,
                };
                self.len += 1;
            }
        }
        free(old_entries as *mut c_void);
        vox_rt_log(
            "debug",
            &format!("hashmap_grow: {} -> {}", old_cap, new_cap),
        );
    }
}

#[no_mangle]
pub extern "C" fn vox_hashmap_new(key_size: usize, value_size: usize) -> *mut c_void {
    vox_rt_log(
        "debug",
        &format!(
            "vox_hashmap_new(key_size={}, value_size={})",
            key_size, value_size
        ),
    );
    let map = unsafe { malloc(mem::size_of::<VoxHashMap>()) as *mut VoxHashMap };
    if map.is_null() {
        vox_rt_log("error", "vox_hashmap_new: failed to allocate VoxHashMap");
        return ptr::null_mut();
    }
    let capacity = 8;
    let entries =
        unsafe { calloc(capacity, mem::size_of::<VoxHashMapEntry>()) as *mut VoxHashMapEntry };
    if entries.is_null() {
        unsafe { free(map as *mut c_void) };
        vox_rt_log("error", "vox_hashmap_new: failed to allocate entries");
        return ptr::null_mut();
    }
    unsafe {
        (*map).entries = entries;
        (*map).capacity = capacity;
        (*map).len = 0;
        (*map).key_size = key_size;
        (*map).value_size = value_size;
    }
    vox_rt_log("debug", &format!("  -> new HashMap at {:p}", map));
    map as *mut c_void
}

#[no_mangle]
pub extern "C" fn vox_hashmap_insert(
    map_ptr: *mut c_void,
    key_ptr: *mut c_void,
    value_ptr: *mut c_void,
) {
    vox_rt_log(
        "debug",
        &format!(
            "vox_hashmap_insert({:p}, {:p}, {:p})",
            map_ptr, key_ptr, value_ptr
        ),
    );
    if map_ptr.is_null() || key_ptr.is_null() || value_ptr.is_null() {
        vox_rt_log("error", "  -> null pointer, ignoring");
        return;
    }
    let map = map_ptr as *mut VoxHashMap;
    unsafe {
        let load_factor = ((*map).len + 1) * 10 / (*map).capacity;
        if load_factor > 7 {
            let new_cap = (*map).capacity * 2;
            (*map).grow(new_cap);
        }
        let (idx, exists) = (*map).find_index(key_ptr as *const u8);
        let entry = &mut *(*map).entries.add(idx);
        if !exists {
            let key_copy = malloc((*map).key_size);
            if key_copy.is_null() {
                vox_rt_log("error", "hashmap_insert: key malloc failed");
                return;
            }
            let value_copy = malloc((*map).value_size);
            if value_copy.is_null() {
                free(key_copy);
                vox_rt_log("error", "hashmap_insert: value malloc failed");
                return;
            }
            ptr::copy_nonoverlapping(key_ptr as *const u8, key_copy as *mut u8, (*map).key_size);
            ptr::copy_nonoverlapping(
                value_ptr as *const u8,
                value_copy as *mut u8,
                (*map).value_size,
            );
            entry.key = key_copy as *mut u8;
            entry.value = value_copy as *mut u8;
            entry.occupied = true;
            (*map).len += 1;
            vox_rt_log(
                "debug",
                &format!("  -> inserted new entry, len={}", (*map).len),
            );
        } else {
            ptr::copy_nonoverlapping(value_ptr as *const u8, entry.value, (*map).value_size);
            vox_rt_log("debug", "  -> overwrote existing entry");
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_hashmap_get(
    map_ptr: *mut c_void,
    key_ptr: *mut c_void,
    out_value: *mut c_void,
) -> i32 {
    vox_rt_log(
        "debug",
        &format!(
            "vox_hashmap_get({:p}, {:p}, {:p})",
            map_ptr, key_ptr, out_value
        ),
    );
    if map_ptr.is_null() || key_ptr.is_null() || out_value.is_null() {
        vox_rt_log("error", "  -> null pointer, returning 0");
        return 0;
    }
    let map = map_ptr as *mut VoxHashMap;
    unsafe {
        let (idx, found) = (*map).find_index(key_ptr as *const u8);
        if found {
            let entry = &*((*map).entries.add(idx));
            ptr::copy_nonoverlapping(entry.value, out_value as *mut u8, (*map).value_size);
            vox_rt_log("debug", "  -> found, copied value");
            1
        } else {
            vox_rt_log("debug", "  -> not found");
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_hashmap_contains_key(map_ptr: *mut c_void, key_ptr: *mut c_void) -> i32 {
    vox_rt_log(
        "debug",
        &format!("vox_hashmap_contains_key({:p}, {:p})", map_ptr, key_ptr),
    );
    if map_ptr.is_null() || key_ptr.is_null() {
        vox_rt_log("error", "  -> null pointer, returning 0");
        return 0;
    }
    let map = map_ptr as *mut VoxHashMap;
    unsafe {
        let (_, found) = (*map).find_index(key_ptr as *const u8);
        vox_rt_log("debug", &format!("  -> {}", found));
        found as i32
    }
}

#[no_mangle]
pub extern "C" fn vox_hashmap_remove(
    map_ptr: *mut c_void,
    key_ptr: *mut c_void,
    out_value: *mut c_void,
) -> i32 {
    vox_rt_log(
        "debug",
        &format!(
            "vox_hashmap_remove({:p}, {:p}, {:p})",
            map_ptr, key_ptr, out_value
        ),
    );
    if map_ptr.is_null() || key_ptr.is_null() || out_value.is_null() {
        vox_rt_log("error", "  -> null pointer, returning 0");
        return 0;
    }
    let map = map_ptr as *mut VoxHashMap;
    unsafe {
        let (idx, found) = (*map).find_index(key_ptr as *const u8);
        if found {
            let entry = &mut *((*map).entries.add(idx));
            ptr::copy_nonoverlapping(entry.value, out_value as *mut u8, (*map).value_size);
            free(entry.key as *mut c_void);
            free(entry.value as *mut c_void);
            entry.occupied = false;
            (*map).len -= 1;
            vox_rt_log("debug", &format!("  -> removed, new len={}", (*map).len));
            1
        } else {
            vox_rt_log("debug", "  -> not found");
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn vox_hashmap_len(map_ptr: *mut c_void) -> usize {
    if map_ptr.is_null() {
        vox_rt_log("debug", "vox_hashmap_len(null) -> 0");
        return 0;
    }
    let len = unsafe { (*(map_ptr as *mut VoxHashMap)).len };
    vox_rt_log(
        "debug",
        &format!("vox_hashmap_len({:p}) -> {}", map_ptr, len),
    );
    len
}

#[no_mangle]
pub extern "C" fn vox_hashmap_drop(map_ptr: *mut c_void) {
    vox_rt_log("debug", &format!("vox_hashmap_drop({:p})", map_ptr));
    if map_ptr.is_null() {
        return;
    }
    let map = map_ptr as *mut VoxHashMap;
    unsafe {
        let entries = (*map).entries;
        let cap = (*map).capacity;
        for i in 0..cap {
            let entry = &mut *entries.add(i);
            if entry.occupied {
                free(entry.key as *mut c_void);
                free(entry.value as *mut c_void);
            }
        }
        free(entries as *mut c_void);
        free(map as *mut c_void);
    }
    vox_rt_log("debug", "  -> dropped");
}

// ------------------------------------------------------------------
// CPU parallel loop dispatcher – using std::thread
// ------------------------------------------------------------------
type WorkerFn = extern "C" fn(index: i64, context: *mut c_void);

#[no_mangle]
pub extern "C" fn vox_dispatch_parallel(
    func: *mut c_void,
    context: *mut c_void,
    start: i64,
    end: i64,
) {
    vox_rt_log(
        "debug",
        &format!(
            "vox_dispatch_parallel(func={:p}, context={:p}, start={}, end={})",
            func, context, start, end
        ),
    );
    if start >= end {
        vox_rt_log("debug", "  -> empty range, returning");
        return;
    }
    let work: WorkerFn = unsafe { mem::transmute(func) };
    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let total = end - start;
    let chunk = (total + num_threads as i64 - 1) / num_threads as i64;
    vox_rt_log(
        "debug",
        &format!("  -> num_threads={}, chunk={}", num_threads, chunk),
    );
    let mut handles = Vec::new();
    let ctx = context as usize;
    for t in 0..num_threads {
        let chunk_start = start + t as i64 * chunk;
        let chunk_end = std::cmp::min(chunk_start + chunk, end);
        if chunk_start >= chunk_end {
            continue;
        }
        vox_rt_log(
            "debug",
            &format!(
                "  -> spawning thread {}: range [{}, {})",
                t, chunk_start, chunk_end
            ),
        );
        handles.push(std::thread::spawn(move || {
            let ctx_ptr = ctx as *mut c_void;
            for i in chunk_start..chunk_end {
                work(i, ctx_ptr);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    vox_rt_log("debug", "  -> all threads finished");
}

// ==================================================================
// GPU Runtime Support
// ==================================================================
//
// The following modules implement the GPU backend functions.
// Exactly one of the features `vox_gpu_cuda` or `vox_gpu_enabled` can be active.
// If neither is enabled, a fallback CPU implementation is used.

// ------------------------------------------------------------------
// CUDA backend (Driver API, enabled with `vox_gpu_cuda`)
// ------------------------------------------------------------------
#[cfg(feature = "vox_gpu_cuda")]
mod gpu_cuda {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use std::sync::Once;

    // CUDA Driver API types (opaque)
    type CUdevice = i32;
    type CUcontext = *mut c_void;
    type CUmodule = *mut c_void;
    type CUfunction = *mut c_void;
    type CUdeviceptr = u64;
    type CUresult = i32;

    const CUDA_SUCCESS: CUresult = 0;
    const CU_CTX_SCHED_AUTO: u32 = 0;

    extern "C" {
        fn cuInit(flags: u32) -> CUresult;
        fn cuDeviceGet(device: *mut CUdevice, ordinal: i32) -> CUresult;
        fn cuCtxCreate(ctx: *mut CUcontext, flags: u32, dev: CUdevice) -> CUresult;
        fn cuCtxSetCurrent(ctx: CUcontext) -> CUresult;
        fn cuModuleLoadData(module: *mut CUmodule, image: *const c_void) -> CUresult;
        fn cuModuleGetFunction(func: *mut CUfunction, module: CUmodule, name: *const c_char) -> CUresult;
        fn cuLaunchKernel(
            f: CUfunction,
            gridX: u32, gridY: u32, gridZ: u32,
            blockX: u32, blockY: u32, blockZ: u32,
            sharedMemBytes: u32, hStream: *mut c_void,
            kernelParams: *mut *mut c_void, extra: *mut *mut c_void,
        ) -> CUresult;
        fn cuMemAlloc(dptr: *mut CUdeviceptr, bytesize: usize) -> CUresult;
        fn cuMemFree(dptr: CUdeviceptr) -> CUresult;
        fn cuMemcpyHtoD(dstDevice: CUdeviceptr, srcHost: *const c_void, ByteCount: usize) -> CUresult;
        fn cuMemcpyDtoH(dstHost: *mut c_void, srcDevice: CUdeviceptr, ByteCount: usize) -> CUresult;
        fn cuCtxSynchronize() -> CUresult;
        fn cuGetErrorString(error: CUresult, pStr: *mut *const c_char) -> CUresult;
    }

    static CUDA_CONTEXT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static CUDA_MODULE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static CUDA_FAILED: AtomicBool = AtomicBool::new(false);
    static CUDA_INIT_ONCE: Once = Once::new();

    fn get_error_string(err: CUresult) -> String {
        unsafe {
            let mut s: *const c_char = ptr::null();
            if cuGetErrorString(err, &mut s) == CUDA_SUCCESS && !s.is_null() {
                std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned()
            } else {
                format!("CUDA error {}", err)
            }
        }
    }

    // Ensure CUDA is initialised and context is set for the calling thread.
    fn cuda_ensure_init() -> bool {
        if CUDA_FAILED.load(Ordering::SeqCst) {
            return false;
        }

        CUDA_INIT_ONCE.call_once(|| {
            unsafe {
                vox_rt_log("info", "Initializing CUDA Driver API...");
                let err = cuInit(0);
                if err != CUDA_SUCCESS {
                    vox_rt_log("error", &format!("cuInit failed: {}", get_error_string(err)));
                    CUDA_FAILED.store(true, Ordering::SeqCst);
                    return;
                }
                let mut device: CUdevice = 0;
                let err = cuDeviceGet(&mut device, 0);
                if err != CUDA_SUCCESS {
                    vox_rt_log("error", &format!("cuDeviceGet failed: {}", get_error_string(err)));
                    CUDA_FAILED.store(true, Ordering::SeqCst);
                    return;
                }
                let mut ctx: CUcontext = ptr::null_mut();
                let err = cuCtxCreate(&mut ctx, CU_CTX_SCHED_AUTO, device);
                if err != CUDA_SUCCESS {
                    vox_rt_log("error", &format!("cuCtxCreate failed: {}", get_error_string(err)));
                    CUDA_FAILED.store(true, Ordering::SeqCst);
                    return;
                }
                CUDA_CONTEXT.store(ctx as *mut c_void, Ordering::SeqCst);
                vox_rt_log("info", "CUDA context created successfully");
            }
        });

        if CUDA_FAILED.load(Ordering::SeqCst) {
            return false;
        }

        let ctx = CUDA_CONTEXT.load(Ordering::SeqCst);
        if ctx.is_null() {
            vox_rt_log("error", "CUDA context is null");
            CUDA_FAILED.store(true, Ordering::SeqCst);
            return false;
        }

        unsafe {
            let err = cuCtxSetCurrent(ctx as CUcontext);
            if err != CUDA_SUCCESS {
                vox_rt_log("error", &format!("cuCtxSetCurrent failed: {}", get_error_string(err)));
                CUDA_FAILED.store(true, Ordering::SeqCst);
                return false;
            }
        }
        true
    }

    #[no_mangle]
    pub extern "C" fn vox_load_device_module(ptx_data: *mut c_void, ptx_size: usize) {
        vox_rt_log(
            "info",
            &format!(
                "vox_load_device_module(ptx_data={:p}, size={})",
                ptx_data, ptx_size
            ),
        );
        log_hex_dump(ptx_data, std::cmp::min(ptx_size, 128), "PTX first 128 bytes");
        if CUDA_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "CUDA previously failed, ignoring load");
            return;
        }
        if !cuda_ensure_init() {
            return;
        }
        if ptx_data.is_null() {
            vox_rt_log("error", "null PTX data");
            CUDA_FAILED.store(true, Ordering::SeqCst);
            return;
        }
        unsafe {
            if !CUDA_MODULE.load(Ordering::SeqCst).is_null() {
                vox_rt_log("debug", "Module already loaded");
                return;
            }

            // Create a null-terminated copy of the PTX string.
            let mut ptx_string = Vec::with_capacity(ptx_size + 1);
            let src = std::slice::from_raw_parts(ptx_data as *const u8, ptx_size);
            ptx_string.extend_from_slice(src);
            ptx_string.push(0); // null terminator

            let mut module: CUmodule = ptr::null_mut();
            let err = cuModuleLoadData(&mut module, ptx_string.as_ptr() as *const c_void);
            if err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuModuleLoadData failed: {}", get_error_string(err)),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
            } else {
                CUDA_MODULE.store(module as *mut c_void, Ordering::SeqCst);
                vox_rt_log("info", "CUDA module loaded successfully");
            }
        }
    }

    // Legacy 1D launch – kept for compatibility
    #[no_mangle]
    pub extern "C" fn vox_launch_kernel_1d(
        kernel_name: *mut c_void,
        arg_ptrs: *mut *mut c_void,
        num_args: i32,
        grid_x: i32,
        block_x: i32,
    ) -> i32 {
        vox_launch_kernel_3d(kernel_name, grid_x, 1, 1, block_x, 1, 1, arg_ptrs, num_args)
    }

    // New 3D launch function with maximum debugging
    #[no_mangle]
    pub extern "C" fn vox_launch_kernel_3d(
        kernel_name: *mut c_void,
        grid_x: i32, grid_y: i32, grid_z: i32,
        block_x: i32, block_y: i32, block_z: i32,
        arg_ptrs: *mut *mut c_void,
        num_args: i32,
    ) -> i32 {
        vox_rt_log(
            "info",
            &format!(
                "vox_launch_kernel_3d(kernel_name={:p}, grid=({},{},{}), block=({},{},{}), num_args={})",
                kernel_name, grid_x, grid_y, grid_z, block_x, block_y, block_z, num_args
            ),
        );
        if CUDA_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "CUDA previously failed, skipping kernel launch");
            return -1;
        }
        let name_cstr = unsafe { std::ffi::CStr::from_ptr(kernel_name as *const c_char) };
        let name = name_cstr.to_string_lossy();
        vox_rt_log("info", &format!("  kernel='{}'", name));

        if !cuda_ensure_init() {
            CUDA_FAILED.store(true, Ordering::SeqCst);
            return -1;
        }
        let module = CUDA_MODULE.load(Ordering::SeqCst);
        if module.is_null() {
            vox_rt_log("error", "No device module loaded");
            CUDA_FAILED.store(true, Ordering::SeqCst);
            return -1;
        }

        unsafe {
            let mut kernel: CUfunction = ptr::null_mut();
            let err = cuModuleGetFunction(&mut kernel, module as CUmodule, kernel_name as *const c_char);
            if err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuModuleGetFunction failed: {}", get_error_string(err)),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            vox_rt_log("info", "Kernel function retrieved");

            // Debug: print each argument pointer and its first 4/8 bytes
            vox_rt_log("debug", "Inspecting kernel arguments:");
            for i in 0..num_args as usize {
                let arg_ptr = *arg_ptrs.add(i);
                if arg_ptr.is_null() {
                    vox_rt_log("error", &format!("Argument {} pointer is null", i));
                    CUDA_FAILED.store(true, Ordering::SeqCst);
                    return -1;
                }
                let first_word = *(arg_ptr as *const u32);
                vox_rt_log("debug", &format!("  Arg[{}]: ptr={:p}, first 4 bytes = 0x{:08x}", i, arg_ptr, first_word));
                // Also attempt to read as pointer if it looks like one (for debugging)
                if first_word & 0xFFFF0000 != 0 {
                    let as_ptr = *(arg_ptr as *const *const c_void);
                    vox_rt_log("debug", &format!("         as pointer: {:p}", as_ptr));
                }
            }

            // Launch kernel with full 3D grid and block dimensions
            let launch_err = cuLaunchKernel(
                kernel,
                grid_x as u32, grid_y as u32, grid_z as u32,
                block_x as u32, block_y as u32, block_z as u32,
                0,
                ptr::null_mut(),
                arg_ptrs,
                ptr::null_mut(),
            );
            if launch_err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuLaunchKernel failed: {}", get_error_string(launch_err)),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            vox_rt_log("info", "Kernel launched, synchronising...");

            // Synchronise and capture any kernel execution error
            let sync_err = cuCtxSynchronize();
            if sync_err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuCtxSynchronize failed: {}", get_error_string(sync_err)),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }

            vox_rt_log("info", "Kernel execution completed successfully");
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_malloc(size: usize) -> *mut c_void {
        vox_rt_log("debug", &format!("vox_gpu_malloc(size={})", size));
        if CUDA_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "CUDA failed, returning host memory");
            let ptr = unsafe { calloc(1, size) };
            vox_rt_log("debug", &format!("  -> {:p} (host fallback)", ptr));
            return ptr;
        }
        if !cuda_ensure_init() {
            CUDA_FAILED.store(true, Ordering::SeqCst);
            let ptr = unsafe { calloc(1, size) };
            vox_rt_log(
                "debug",
                &format!("  -> {:p} (host fallback after init fail)", ptr),
            );
            return ptr;
        }
        unsafe {
            let mut dptr: CUdeviceptr = 0;
            let err = cuMemAlloc(&mut dptr, size);
            if err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuMemAlloc(size={}) failed: {}", size, get_error_string(err)),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
                let ptr = calloc(1, size);
                vox_rt_log("debug", &format!("  -> {:p} (host fallback)", ptr));
                return ptr;
            } else {
                let ptr = dptr as *mut c_void;
                vox_rt_log("debug", &format!("  -> device ptr {:p}", ptr));
                ptr
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_free(ptr: *mut c_void) {
        vox_rt_log("debug", &format!("vox_gpu_free({:p})", ptr));
        if ptr.is_null() {
            return;
        }
        if CUDA_FAILED.load(Ordering::SeqCst) {
            unsafe {
                free(ptr);
                vox_rt_log("debug", "  -> freed host memory");
            }
            return;
        }
        unsafe {
            let dptr = ptr as CUdeviceptr;
            let err = cuMemFree(dptr);
            if err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuMemFree failed: {}", get_error_string(err)),
                );
            } else {
                vox_rt_log("debug", "  -> freed device memory");
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_memcpy_host_to_device(
        dst: *mut c_void,
        src: *mut c_void,
        size: usize,
    ) {
        vox_rt_log(
            "debug",
            &format!(
                "vox_gpu_memcpy_host_to_device(dst={:p}, src={:p}, size={})",
                dst, src, size
            ),
        );
        if CUDA_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "CUDA failed, skipping copy");
            return;
        }
        if size == 0 {
            vox_rt_log("debug", "  -> size zero, nothing to copy");
            return;
        }
        unsafe {
            let err = cuMemcpyHtoD(dst as CUdeviceptr, src, size);
            if err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuMemcpyHtoD failed: {} (dst={:p}, src={:p}, size={})", get_error_string(err), dst, src, size),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
            } else {
                vox_rt_log("info", "H2D copy succeeded");
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_memcpy_device_to_host(
        dst: *mut c_void,
        src: *mut c_void,
        size: usize,
    ) {
        vox_rt_log(
            "debug",
            &format!(
                "vox_gpu_memcpy_device_to_host(dst={:p}, src={:p}, size={})",
                dst, src, size
            ),
        );
        if CUDA_FAILED.load(Ordering::SeqCst) {
            unsafe {
                ptr::write_bytes(dst, 0, size);
            }
            vox_rt_log("warning", "CUDA failed, zeroing destination memory");
            return;
        }
        if size == 0 {
            vox_rt_log("debug", "  -> size zero, nothing to copy");
            return;
        }
        unsafe {
            let err = cuMemcpyDtoH(dst, src as CUdeviceptr, size);
            if err != CUDA_SUCCESS {
                vox_rt_log(
                    "error",
                    &format!("cuMemcpyDtoH failed: {} (dst={:p}, src={:p}, size={})", get_error_string(err), dst, src, size),
                );
                CUDA_FAILED.store(true, Ordering::SeqCst);
                ptr::write_bytes(dst, 0, size);
                vox_rt_log("warning", "CUDA copy failed, zeroed destination memory");
            } else {
                vox_rt_log("info", "D2H copy succeeded");
                // Log first few bytes of copied data for debugging
                if size > 0 {
                    let mut buf = vec![0u8; std::cmp::min(size, 32)];
                    std::ptr::copy_nonoverlapping(dst, buf.as_mut_ptr() as *mut c_void, buf.len());
                    log_hex_dump(buf.as_ptr() as *mut c_void, buf.len(), "Copied data (host)");
                }
            }
        }
    }
}

// ------------------------------------------------------------------
// HIP backend (original, enabled with `vox_gpu_enabled`)
// ------------------------------------------------------------------
#[cfg(feature = "vox_gpu_enabled")]
mod gpu_hip {
    use super::*;
    use std::ffi::{c_char, c_int};
    use std::sync::atomic::{AtomicBool, Ordering};

    extern "C" {
        fn hipInit(flags: u32) -> i32;
        fn hipGetDeviceCount(count: *mut i32) -> i32;
        fn hipSetDevice(device: i32) -> i32;
        fn hipMalloc(ptr: *mut *mut c_void, size: usize) -> i32;
        fn hipFree(ptr: *mut c_void) -> i32;
        fn hipMemcpy(dst: *mut c_void, src: *mut c_void, size: usize, kind: i32) -> i32;
        fn hipModuleLoadData(module: *mut *mut c_void, image: *mut c_void) -> i32;
        fn hipModuleGetFunction(
            function: *mut *mut c_void,
            module: *mut c_void,
            name: *const c_char,
        ) -> i32;
        fn hipModuleLaunchKernel(
            function: *mut c_void,
            grid_x: u32,
            grid_y: u32,
            grid_z: u32,
            block_x: u32,
            block_y: u32,
            block_z: u32,
            shared_mem: u32,
            stream: *mut c_void,
            kernel_params: *mut *mut c_void,
            extra: *mut *mut c_void,
        ) -> i32;
        fn hipDeviceSynchronize() -> i32;
        fn hipGetErrorString(error: i32) -> *const c_char;
    }

    const hipMemcpyHostToDevice: i32 = 1;
    const hipMemcpyDeviceToHost: i32 = 2;

    static GPU_FAILED: AtomicBool = AtomicBool::new(false);
    static HIP_INITIALIZED: AtomicBool = AtomicBool::new(false);
    static mut HIP_MODULE: *mut c_void = ptr::null_mut();

    fn get_error_string(err: i32) -> String {
        unsafe {
            let s = hipGetErrorString(err);
            if s.is_null() {
                "unknown".into()
            } else {
                std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned()
            }
        }
    }

    fn hip_ensure_init() -> i32 {
        if GPU_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("debug", "hip_ensure_init: GPU_FAILED = true");
            return -1;
        }
        if HIP_INITIALIZED.load(Ordering::SeqCst) {
            return 0;
        }
        unsafe {
            vox_rt_log("info", "Initializing HIP...");
            let err = hipInit(0);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipInit failed: {}", get_error_string(err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            let mut count: i32 = 0;
            let err = hipGetDeviceCount(&mut count);
            if err != 0 || count == 0 {
                vox_rt_log(
                    "error",
                    &format!(
                        "hipGetDeviceCount failed: {} devices={}",
                        get_error_string(err),
                        count
                    ),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            let err = hipSetDevice(0);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipSetDevice failed: {}", get_error_string(err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            HIP_INITIALIZED.store(true, Ordering::SeqCst);
            vox_rt_log("info", "HIP initialized successfully");
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn vox_load_device_module(hsaco_data: *mut c_void, hsaco_size: usize) {
        vox_rt_log(
            "info",
            &format!(
                "vox_load_device_module(hsaco_data={:p}, size={})",
                hsaco_data, hsaco_size
            ),
        );
        if GPU_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "GPU previously failed, ignoring load");
            return;
        }
        if hip_ensure_init() != 0 {
            return;
        }
        if hsaco_data.is_null() {
            vox_rt_log("error", "null hsaco_data");
            GPU_FAILED.store(true, Ordering::SeqCst);
            return;
        }
        unsafe {
            if !HIP_MODULE.is_null() {
                vox_rt_log("debug", "Module already loaded");
                return;
            }
            let copy = malloc(hsaco_size);
            if copy.is_null() {
                vox_rt_log("error", "malloc failed for HSACO copy");
                GPU_FAILED.store(true, Ordering::SeqCst);
                return;
            }
            ptr::copy(hsaco_data, copy, hsaco_size);
            let err = hipModuleLoadData(&mut HIP_MODULE, copy);
            free(copy);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipModuleLoadData failed: {}", get_error_string(err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
            } else {
                vox_rt_log("info", "HIP module loaded successfully");
            }
        }
    }

    // Legacy 1D launch – kept for compatibility
    #[no_mangle]
    pub extern "C" fn vox_launch_kernel_1d(
        kernel_name: *mut c_void,
        arg_ptrs: *mut *mut c_void,
        num_args: i32,
        grid_x: i32,
        block_x: i32,
    ) -> i32 {
        vox_launch_kernel_3d(kernel_name, grid_x, 1, 1, block_x, 1, 1, arg_ptrs, num_args)
    }

    // New 3D launch function
    #[no_mangle]
    pub extern "C" fn vox_launch_kernel_3d(
        kernel_name: *mut c_void,
        grid_x: i32, grid_y: i32, grid_z: i32,
        block_x: i32, block_y: i32, block_z: i32,
        arg_ptrs: *mut *mut c_void,
        num_args: i32,
    ) -> i32 {
        vox_rt_log(
            "info",
            &format!(
                "vox_launch_kernel_3d(kernel_name={:p}, grid=({},{},{}), block=({},{},{}), num_args={})",
                kernel_name, grid_x, grid_y, grid_z, block_x, block_y, block_z, num_args
            ),
        );
        if GPU_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "GPU previously failed, skipping kernel launch");
            return -1;
        }
        let name_cstr = unsafe { std::ffi::CStr::from_ptr(kernel_name as *const c_char) };
        let name = name_cstr.to_string_lossy();
        vox_rt_log("info", &format!("  kernel='{}'", name));

        if hip_ensure_init() != 0 {
            GPU_FAILED.store(true, Ordering::SeqCst);
            return -1;
        }
        unsafe {
            if HIP_MODULE.is_null() {
                vox_rt_log("error", "No device module loaded");
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            let mut kernel: *mut c_void = ptr::null_mut();
            let err = hipModuleGetFunction(&mut kernel, HIP_MODULE, kernel_name as *const c_char);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipModuleGetFunction failed: {}", get_error_string(err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            vox_rt_log("info", "Kernel function retrieved");

            // Debug: print each argument pointer
            vox_rt_log("debug", "Inspecting kernel arguments:");
            for i in 0..num_args as usize {
                let arg_ptr = *arg_ptrs.add(i);
                if arg_ptr.is_null() {
                    vox_rt_log("error", &format!("Argument {} pointer is null", i));
                    GPU_FAILED.store(true, Ordering::SeqCst);
                    return -1;
                }
                let first_word = *(arg_ptr as *const u32);
                vox_rt_log("debug", &format!("  Arg[{}]: ptr={:p}, first 4 bytes = 0x{:08x}", i, arg_ptr, first_word));
            }

            // Launch kernel with full 3D grid and block dimensions
            const HIP_LAUNCH_PARAM_BUFFER_POINTER: usize = 0x01;
            const HIP_LAUNCH_PARAM_BUFFER_SIZE: usize = 0x02;
            const HIP_LAUNCH_PARAM_END: usize = 0x00;
            let config: [*mut c_void; 6] = [
                &HIP_LAUNCH_PARAM_BUFFER_POINTER as *const _ as *mut c_void,
                arg_ptrs as *mut c_void,
                &HIP_LAUNCH_PARAM_BUFFER_SIZE as *const _ as *mut c_void,
                &(num_args as usize) as *const _ as *mut c_void,
                &HIP_LAUNCH_PARAM_END as *const _ as *mut c_void,
                ptr::null_mut(),
            ];
            let launch_err = hipModuleLaunchKernel(
                kernel,
                grid_x as u32, grid_y as u32, grid_z as u32,
                block_x as u32, block_y as u32, block_z as u32,
                0,
                ptr::null_mut(),
                config.as_ptr() as *mut *mut c_void,
                ptr::null_mut(),
            );
            if launch_err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipModuleLaunchKernel failed: {}", get_error_string(launch_err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            let sync_err = hipDeviceSynchronize();
            if sync_err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipDeviceSynchronize failed: {}", get_error_string(sync_err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                return -1;
            }
            vox_rt_log("info", "Kernel executed successfully");
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_malloc(size: usize) -> *mut c_void {
        vox_rt_log("debug", &format!("vox_gpu_malloc(size={})", size));
        if GPU_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "GPU failed, returning host memory");
            let ptr = unsafe { calloc(1, size) };
            vox_rt_log("debug", &format!("  -> {:p} (host fallback)", ptr));
            return ptr;
        } else {
            if hip_ensure_init() != 0 {
                GPU_FAILED.store(true, Ordering::SeqCst);
                let ptr = unsafe { calloc(1, size) };
                vox_rt_log(
                    "debug",
                    &format!("  -> {:p} (host fallback after init fail)", ptr),
                );
                return ptr;
            }
            unsafe {
                let mut ptr: *mut c_void = ptr::null_mut();
                let err = hipMalloc(&mut ptr, size);
                if err != 0 {
                    vox_rt_log(
                        "error",
                        &format!("hipMalloc failed: {}", get_error_string(err)),
                    );
                    GPU_FAILED.store(true, Ordering::SeqCst);
                    let ptr = calloc(1, size);
                    vox_rt_log("debug", &format!("  -> {:p} (host fallback)", ptr));
                    return ptr;
                } else {
                    vox_rt_log("debug", &format!("  -> device ptr {:p}", ptr));
                    ptr
                }
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_free(ptr: *mut c_void) {
        vox_rt_log("debug", &format!("vox_gpu_free({:p})", ptr));
        if ptr.is_null() {
            return;
        }
        if GPU_FAILED.load(Ordering::SeqCst) {
            unsafe {
                free(ptr);
                vox_rt_log("debug", "  -> freed host memory");
            }
            return;
        }
        unsafe {
            let err = hipFree(ptr);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipFree failed: {}", get_error_string(err)),
                );
            } else {
                vox_rt_log("debug", "  -> freed device memory");
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_memcpy_host_to_device(
        dst: *mut c_void,
        src: *mut c_void,
        size: usize,
    ) {
        vox_rt_log(
            "debug",
            &format!(
                "vox_gpu_memcpy_host_to_device(dst={:p}, src={:p}, size={})",
                dst, src, size
            ),
        );
        if GPU_FAILED.load(Ordering::SeqCst) {
            vox_rt_log("warning", "GPU failed, skipping copy");
            return;
        }
        if size == 0 {
            vox_rt_log("debug", "  -> size zero, nothing to copy");
            return;
        }
        unsafe {
            let err = hipMemcpy(dst, src, size, hipMemcpyHostToDevice);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipMemcpy H2D failed: {}", get_error_string(err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
            } else {
                vox_rt_log("info", "H2D copy succeeded");
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_memcpy_device_to_host(
        dst: *mut c_void,
        src: *mut c_void,
        size: usize,
    ) {
        vox_rt_log(
            "debug",
            &format!(
                "vox_gpu_memcpy_device_to_host(dst={:p}, src={:p}, size={})",
                dst, src, size
            ),
        );
        if GPU_FAILED.load(Ordering::SeqCst) {
            unsafe {
                ptr::write_bytes(dst, 0, size);
            }
            vox_rt_log("warning", "GPU failed, zeroing destination memory");
            return;
        }
        if size == 0 {
            vox_rt_log("debug", "  -> size zero, nothing to copy");
            return;
        }
        unsafe {
            let err = hipMemcpy(dst, src, size, hipMemcpyDeviceToHost);
            if err != 0 {
                vox_rt_log(
                    "error",
                    &format!("hipMemcpy D2H failed: {}", get_error_string(err)),
                );
                GPU_FAILED.store(true, Ordering::SeqCst);
                ptr::write_bytes(dst, 0, size);
                vox_rt_log("warning", "GPU copy failed, zeroed destination memory");
            } else {
                vox_rt_log("info", "D2H copy succeeded");
                if size > 0 {
                    let mut buf = vec![0u8; std::cmp::min(size, 32)];
                    std::ptr::copy_nonoverlapping(dst, buf.as_mut_ptr() as *mut c_void, buf.len());
                    log_hex_dump(buf.as_ptr() as *mut c_void, buf.len(), "Copied data (host)");
                }
            }
        }
    }
}

// ------------------------------------------------------------------
// Fallback when no GPU feature is enabled
// ------------------------------------------------------------------
#[cfg(not(any(feature = "vox_gpu_cuda", feature = "vox_gpu_enabled")))]
mod gpu_fallback {
    use super::*;

    #[no_mangle]
    pub extern "C" fn vox_load_device_module(_data: *mut c_void, size: usize) {
        vox_rt_log(
            "info",
            &format!("Loaded CPU fallback module ({} bytes).", size),
        );
    }

    // Legacy 1D launch – kept for compatibility
    #[no_mangle]
    pub extern "C" fn vox_launch_kernel_1d(
        kernel_name: *mut c_void,
        _arg_ptrs: *mut *mut c_void,
        _num_args: i32,
        _grid_x: i32,
        _block_x: i32,
    ) -> i32 {
        vox_launch_kernel_3d(kernel_name, _grid_x, 1, 1, _block_x, 1, 1, _arg_ptrs, _num_args)
    }

    // New 3D launch function (fallback – CPU stub)
    #[no_mangle]
    pub extern "C" fn vox_launch_kernel_3d(
        kernel_name: *mut c_void,
        grid_x: i32, grid_y: i32, grid_z: i32,
        block_x: i32, block_y: i32, block_z: i32,
        _arg_ptrs: *mut *mut c_void,
        _num_args: i32,
    ) -> i32 {
        let name = unsafe { std::ffi::CStr::from_ptr(kernel_name as *const c_char) };
        vox_rt_log(
            "info",
            &format!(
                "CPU execution stub for '{}' (grid={},{},{}, block={},{},{})",
                name.to_string_lossy(),
                grid_x, grid_y, grid_z,
                block_x, block_y, block_z
            ),
        );
        0
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_malloc(size: usize) -> *mut c_void {
        vox_rt_log("debug", &format!("vox_gpu_malloc (fallback) size={}", size));
        let ptr = unsafe { malloc(size) };
        vox_rt_log("debug", &format!("  -> {:p}", ptr));
        ptr
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_free(ptr: *mut c_void) {
        vox_rt_log("debug", &format!("vox_gpu_free (fallback) {:p}", ptr));
        unsafe {
            free(ptr);
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_memcpy_host_to_device(
        dst: *mut c_void,
        src: *mut c_void,
        size: usize,
    ) {
        vox_rt_log("debug", "vox_gpu_memcpy_host_to_device (fallback)");
        unsafe {
            ptr::copy(src, dst, size);
        }
    }

    #[no_mangle]
    pub extern "C" fn vox_gpu_memcpy_device_to_host(
        dst: *mut c_void,
        src: *mut c_void,
        size: usize,
    ) {
        vox_rt_log("debug", "vox_gpu_memcpy_device_to_host (fallback)");
        unsafe {
            ptr::copy(src, dst, size);
        }
    }
}

// Re-export the appropriate GPU functions based on the active feature.
#[cfg(feature = "vox_gpu_cuda")]
pub use gpu_cuda::*;
#[cfg(feature = "vox_gpu_enabled")]
pub use gpu_hip::*;
#[cfg(not(any(feature = "vox_gpu_cuda", feature = "vox_gpu_enabled")))]
pub use gpu_fallback::*;
