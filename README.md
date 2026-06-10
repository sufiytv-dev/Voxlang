# Voxlang – Verified Heterogeneous Systems Programming

Voxlang is a modern statically typed systems programming language that combines ownership‑based memory safety, compile‑time refinement types (Z3), and first‑class heterogeneous compute (CPU + GPU).

> **Syntax is different from C.** Please read the [Syntax Rules](#syntax-rules) before writing your first program.

## Philosophy

- **Correctness by default** – every `where` clause is statically proven.
- **Memory safety without GC** – ownership, moves, borrow checker.
- **Heterogeneous computing as a core feature** – GPU kernels in the same language.
- **Zero‑cost C integration** – import any C header; the C‑Bridge generates safe wrappers.
- **Excellent diagnostics** – clear error messages with source spans.

## Syntax Rules (Important)

Voxlang deliberately avoids C‑family braces and semicolons.

- **Comments** start with `#` and continue to end of line.
- **No opening brace `{`** is allowed – the compiler rejects it.
- **Blocks are terminated by `}`** on its own line (or after a statement).  
  Indentation is ignored; `}` is mandatory.
- **No semicolons** – newlines terminate statements.
- **Boolean conditions** are integers: `0` = false, any other integer = true.
- **Logical operators**: `and`, `or`, `not` (not `&&`, `||`, `!`).

Example:

```vox
fn main() -> i32:
    # This is a comment
    let x = 42
    if x > 0:
        return x
    } else:
        return 0
    }
}
Quick Examples
Refinement types (preconditions / postconditions)
vox
fn divide(a: i32, b: i32 where b != 0) -> i32 where result == a / b:
    return a / b
}

fn main():
    let result = divide(100, 5)   # verified at compile time
}
Ownership and borrowing
vox
fn take_ownership(s: String):
    # s is moved here
}

fn main():
    let s = String("hello")
    take_ownership(s)      # s moved, cannot use again
}
GPU kernel (@kernel)
vox
@kernel fn add(a: i32, b: i32, result: &mut i32):
    *result = a + b
}
Compile‑time evaluation (@comptime)
vox
fn main():
    let a: i32 = @comptime {
        let x = 5
        let y = x * 2
        y + 3
    }
}
Parallel loop (race‑free)
vox
fn main():
    let mut sum = 0
    parallel for i in 0..10:
        sum = sum + i
    }
}
? operator – propagate errors (fully supported)
vox
fn may_fail(flag: i32) -> Result<i32, &str>:
    if flag == 0:
        return Result::Ok(42)
    } else:
        return Result::Err("error")
    }
}

fn use_question(flag: i32) -> Result<i32, &str>:
    let x = may_fail(flag)?
    return Result::Ok(x)
}
Generic Vec<T> and HashMap<K,V>
vox
fn main():
    let mut v: Vec<i32> = Vec::new()
    push(v, 10)
    push(v, 20)
    assert(len(v) == 2, "expected length 2")
    let last = pop(v)   # returns Option<i32>
}
vox
fn main():
    let mut m: HashMap<&str, i32> = HashMap::new()
    insert(m, "a", 1)
    insert(m, "b", 2)
    let val = get(m, "a")   # returns Option<i32>
    match val:
        Some(v) -> println(v)
        None    -> println("none")
    }
}
Pattern matching
vox
fn main():
    let opt: Option<i32> = Some(42)
    match opt:
        Some(x) -> println(x)
        None    -> println("none")
    }
}
Getting Started
Prerequisites
Rust (stable)

LLVM tools (clang, llc)

(Optional) ROCm/HIP or CUDA for GPU support

(Optional) Z3 library for refinement verification

Build
bash
cargo build --release
Basic Commands
Command	Description
vox test	Run all examples
vox check examples/hello.vx	Only verify
vox build examples/hello.vx	Compile to native binary
vox run examples/hello.vx	Build and run
vox run --gpu hip examples/gpu_add.vx	GPU example (HIP)
vox update --write .	Refresh remote import hashes
vox index .	Generate symbol index
vox lsp	Experimental – Start LSP server (basic diagnostics only)
vox shell	Experimental – Interactive REPL (basic evaluation)
vox watch	Experimental – File watcher (polling‑based)
vox clean	Remove target/
Note: LSP, Watch, and Shell are under active development and not yet feature‑complete. Use them for early experimentation only.


IMPORTANT: we do plan to add a full cross-OS installer, and making the remaining pieces feature complete.

C‑Bridge – Safe C Imports
Simply write import "mylib.h" in your .vx file. The compiler parses the header, generates safe Vox wrappers, and caches them. All pointers are checked for null, buffer lengths are verified, and ownership is tracked.

Documentation
Full language reference – coming soon

Examples – see examples/ directory

Contributing – CONTRIBUTING.md

Security – SECURITY.md

License
MIT License – see LICENSE for details.

Acknowledgments
Rust – ownership and borrow checker inspiration

Z3 Prover – refinement verification

LLVM – code generation

ROCm / CUDA – GPU backends

Voxlang – Correctness you can prove.
