# C Bridge – Safe C Imports (Experimental / Minimal)

The C Bridge is the **most important feature** of Voxlang. It allows you to import C headers directly and generates safe, verified Vox wrappers automatically. However, the current implementation is **just beginning** – it is minimal, incomplete, and not yet ready for production use.

> **⚠️ Status:** Minimal prototype. Works for very simple headers (no macros, no unions, limited pointer analysis). Active development is ongoing – expect rapid changes.

---

## Current Limitations

- Only a subset of C syntax is supported (no bitfields, no complex macros, no varargs).
- Buffer length inference is primitive; many `where` clauses must be written manually.
- Ownership tracking for C pointers is not yet implemented (you must use `&mut` explicitly).
- The generated wrappers are not yet cached across builds (regeneration on every compile).
- No support for C++ or inline assembly.

## Vision (What It Will Become)

The goal is a **zero‑overhead, provably safe** bridge:

1. **Parse any C header** – including system headers like `SDL.h`, `pthread.h`, `OpenGL`.
2. **Infer safe preconditions** – null checks, buffer sizes, initialization state.
3. **Generate Vox wrappers** – with `where` clauses that Z3 can prove.
4. **Cache generated bindings** – so headers are only processed once.
5. **Ownership tracking** – automatically add `&mut` where the C API expects mutations.

## How to Use (Experimental)

Place an `import` directive at the top of your `.vx` file:

```vox
import "mylib.h"

fn main():
    # The imported function is available after generation
    mylib_function()
}
```

When you run vox build, the compiler will:

Locate mylib.h (using the system include path or relative paths).

Parse it with a minimal C parser.

Generate a .vx wrapper in target/bridge/mylib.vx.

Compile the wrapper together with your code.

Example (Very Simple)
Given math_helpers.h:

c
int add(int a, int b);
The bridge generates:

```vox
# Automatically generated
fn add(a: i32, b: i32) -> i32:
    return C_add(a, b)   # Foreign function interface
}
```

No safety annotations are needed for this trivial case. More complex APIs will require where clauses (future).

## Contributing

The C Bridge is the highest priority area for contributions. If you are interested in C parsing, type inference, or compiler plugin systems, please reach out via GitHub issues.

## Future Roadmap

### Version	Feature

- v0.2	Basic header parsing + simple wrappers
- v0.3	Macro expansion + constant propagation
- v0.4	Ownership inference for pointers
- v0.5	Full system header support (POSIX, WinAPI)
- v1.0	Production‑ready C interop
For now, use the bridge only for experimentation. The rest of the language is stable, but the C bridge will evolve significantly.
