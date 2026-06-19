# Voxlang – Verified Heterogeneous Systems Programming

[![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/rust-lang/rust)
[![LLVM](https://img.shields.io/badge/LLVM-%23414141.svg?style=for-the-badge&logo=llvm&logoColor=white)](https://github.com/llvm/llvm-project)
[![Z3](https://img.shields.io/badge/Z3-Prover-%23000000.svg?style=for-the-badge&logo=z3&logoColor=white)](https://github.com/Z3Prover/z3)

Voxlang is a modern statically typed systems programming language that combines ownership‑based memory safety, compile‑time refinement types (Z3), and first‑class heterogeneous compute (CPU + GPU).  
**Runs on Windows, macOS, and Linux (x86_64) with full GPU support:**
- **AMD ROCm/HIP** on **Windows** (tested on Radeon RX 9060 XT, ROCm 7.1+)
- **NVIDIA CUDA** on **Linux** (tested on CUDA 11.8/12.x)
- Cross‑platform combinations (CUDA on Windows, HIP on Linux) are expected but not yet officially verified.

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

## Example:

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
```

## Quick Examples

### Refinement types (preconditions / postconditions)

```vox
fn divide(a: i32, b: i32 where b != 0) -> i32 where result == a / b:
    return a / b
}

fn main():
    let result = divide(100, 5)   # verified at compile time
}
```

### Ownership and borrowing

```vox
fn take_ownership(s: String):
    # s is moved here
}

fn main():
    let s = String("hello")
    take_ownership(s)      # s moved, cannot use again
}
```

### GPU kernel (@kernel)

```vox

@kernel(block=(256,1,1)) fn vec_add(a: &[i32], b: &[i32], result: &mut [i32]):
    let i = get_global_id(0)
    if i < len(a):
        result[i] = a[i] + b[i]
    }
}

fn main():
    let a = [1, 2, 3]
    let b = [4, 5, 6]
    let mut out = [0, 0, 0]
    launch vec_add(1, 1, 1)(a, b, &mut out)
    # out == [5, 7, 9]
}
```

### Compile‑time evaluation (@comptime)

```vox
fn main():
    let a: i32 = @comptime {
        let x = 5
        let y = x * 2
        y + 3
    }
}
```

### Parallel loop (race‑free)

```vox
fn main():
    let mut sum = 0
    parallel for i in 0..10:
        sum = sum + i
    }
}
```
### ? operator – propagate errors

```vox
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
```

### Generic Vec<T> and HashMap<K,V>

```vox
fn main():
    let mut v: Vec<i32> = Vec::new()
    push(v, 10)
    push(v, 20)
    assert(len(v) == 2, "expected length 2")
    let last = pop(v)   # returns Option<i32>
}
```

```vox
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
```

### Pattern matching

``` vox
fn main():
    let opt: Option<i32> = Some(42)
    match opt:
        Some(x) -> println(x)
        None    -> println("none")
    }
}
```

## Getting Started

### Prerequisites

- Rust (stable) – install via rustup

- LLVM tools (clang, llc) – included with Xcode (macOS), MSVC (Windows), or sudo apt install clang lld (Linux)

- Z3 Prover – required for refinement verification

- Windows: download from Z3 releases and add to PATH

- macOS: brew install z3 or port install z3

- Linux: sudo apt install z3 (Ubuntu/Debian) or sudo dnf install z3 (Fedora)

- (Optional) GPU SDK – CUDA 11.8+ for NVIDIA GPUs, or ROCm 6.x+ for AMD GPUs

### Build from Source

```bash
git clone https://github.com/sufiytv-dev/Voxlang
cd Voxlang
cargo build --release
```

The binary will be at target/release/vox (or vox.exe on Windows). Copy it to a directory in your PATH or run cargo install --path.

### Prebuilt Binaries

Prebuilt binaries are available for Windows, macOS, and Linux (x86_64) on the releases page.
See Installation Guide for detailed instructions.

## Basic Commands

| Command | Description |
| :--- | :--- |
| `vox test` | Run all examples |
| `vox check examples/hello.vx` | Only verify |
| `vox build examples/hello.vx` | Compile to native binary |
| `vox run examples/hello.vx` | Build and run |
| `vox run --gpu cuda examples/kernel.vx` | Run GPU kernel with CUDA (Linux) |
| `vox run --gpu hip examples/kernel.vx` | Run GPU kernel with HIP/ROCm (Windows) |
| `vox update --write .` | Refresh remote import hashes |
| `vox index .` | Generate symbol index |
| `vox lsp` (experimental) | Start LSP server (basic diagnostics only) |
| `vox shell` (experimental) | Interactive REPL (basic evaluation) |
| `vox watch` (experimental) | File watcher (polling‑based) |
| `vox clean` | Remove target/ directory |

Note: LSP, Watch, and Shell are under active development and not yet feature‑complete. Use them for early experimentation only.

## C‑Bridge – Safe C Imports

Simply write import "mylib.h" in your .vx file. The compiler parses the header, generates safe Vox wrappers, and caches them. All pointers are checked for null, buffer lengths are verified, and ownership is tracked.
Status: experimental – see C Bridge documentation for details.

## Documentation

Full language reference – coming soon

GPU Kernels

Standard Library

Installation Guide

Change Log

Examples – see examples/ directory

Contributing

Security

## Driver & SDK Prerequisites

To build or run Voxlang with full heterogeneous GPU support, ensure your environment has the appropriate vendor toolkits installed:

* **NVIDIA CUDA (Linux/Windows)**: Install the [CUDA Toolkit Core](https://developer.nvidia.com/cuda-downloads) for native compute drivers and the nvcc backend.
* **AMD ROCm & HIP (Windows)**: Install [ROCm for Windows](https://www.amd.com/en/developer/rocm-hub/rocm-for-windows.html) to target runtime execution on supported Radeon hardware.
* **AMD ROCm (Linux)**: Follow the official [ROCm Linux Deployment Guide](https://rocm.docs.amd.com/) to configure package repositories and install the `amdgpu` stack.
* **Apple Metal (macOS)**: No driver installation required. Ensure Xcode or the Command Line Tools are active via the [Apple Metal Developer Hub](https://developer.apple.com/metal/).

## License

MIT License – see LICENSE for details.

## Acknowledgments

Rust – ownership and borrow checker inspiration

Z3 Prover – refinement verification

LLVM – code generation

ROCm / CUDA – GPU backends

Voxlang – Correctness you can prove.
