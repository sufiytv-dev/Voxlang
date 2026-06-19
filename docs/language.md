# Language Syntax

Voxlang syntax is intentionally different from C. Read this guide carefully before writing your first program.

---

## Basic Syntax Rules

- **Comments** start with `#` and continue to end of line.
- **No opening brace `{`** – the compiler rejects it.
- **Blocks are terminated by `}`** on its own line (or after a statement). Indentation is ignored.
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

## Types

| Type | Description | LLVM representation |
| :--- | :--- | :--- |
| i8, i16, i32, i64 | Signed integers | i8…i64 |
| u8, u16, u32, u64 | Unsigned integers | i8…i64 (zero extended) |
| f32, f64 | Floating point | float, double |
| char | Unicode scalar (32‑bit) | i32 |
| bool | Actually i32 with 0/1 semantics | i32 |
| &str | String slice (fat pointer) | { i8*, i64 } |
| String | Heap‑allocated UTF‑8 string | { i8*, i64, i64 } |
| [T; N] | Fixed‑size array | [N x T] |
| []T | Dynamic array (runtime sized) | { i8*, i64, i64 } |
| `Vec<T>` | Growable array (opaque handle) | i8* |
| `HashMap<K,V>` | Hash map (opaque handle) | i8* |
| `Option<T>` | Optional value | { i32, T } |
| `Result<T,E>` | Success or error | { i32, T, E } |

## Variables and Mutability

let declares an immutable variable.

let mut declares a mutable variable.

Variables are moved by default (affine type system).

```vox
let x = 10          # immutable
let mut y = 20      # mutable
y = 30
```

## Functions

```vox
fn add(a: i32, b: i32) -> i32:
    return a + b
}
```

Return type is required (no inference for top‑level functions).

Early return with `return expr`.

Last expression is not implicitly returned – use `return`.

## Refinement Types (Preconditions / Postconditions)

```vox
fn divide(a: i32, b: i32 where b != 0) -> i32 where result == a / b:
    return a / b
}
```

Preconditions are checked at call sites; postconditions at function exit.

Both are verified by the Z3 prover at compile time.

## Ownership and Borrowing

Values are moved by default. Use & (immutable borrow) or &mut (mutable borrow) to avoid moving.

```vox
fn take_ownership(s: String):
    # s is moved here
}

fn main():
    let s = String("hello")
    take_ownership(s)      # s moved, cannot use again
}
```

### Borrowing:

```vox
fn peek(s: &String) -> i32:
    return len(s)          # OK, only reads
}

fn main():
    let s = String("hello")
    let len = peek(&s)     # borrow, not move
    # s still usable
}
```

## Control Flow

### if / else:

``` vox
if condition:
    # then block
} else:
    # else block
}
```
Condition is an integer: 0 = false, non‑zero = true.

### while:

```vox
while condition:
    # loop body
}
```

### match (pattern matching):

```vox
match value:
    pattern1 -> expr1
    pattern2 -> expr2
}
```

```vox
match opt:
    Some(x) -> println(x)
    None    -> println("none")
}
```

## parallel for:

Race‑free parallel iteration (atomics automatically handle updates):

```vox
let mut sum = 0
parallel for i in 0..10:
    sum = sum + i    # atomic add
}
```

## Compile‑time Evaluation (@comptime):

Run arbitrary Vox code during compilation. The result is embedded as a constant.

```vox
fn main():
    let a: i32 = @comptime {
        let x = 5
        let y = x * 2
        y + 3
    }
    # a == 13
}
```
## GPU Kernels (@kernel):

Mark a function as a GPU kernel. Same syntax, same verifier.

```vox
@kernel(block=(1,1,1)) fn add_kernel(a: i32, b: i32, result: &mut i32):
    *result = a + b
}

fn main() -> i32:
    let mut res: i32 = 0
    launch add_kernel(1, 1, 1)(5, 7, &mut res)
    if res != 12:
        return 1
    }
    return 0
}
```

## Error Handling – ? Operator:

`Result<T,E>` can be propagated with `?`. It returns early on Err.

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

## Generics:

Generic Functions
```vox
fn identity<T>(x: T) -> T:
    return x
}
```

Concrete instantiations (e.g., identity_i32) are generated on demand.

## Generic Structs:

```vox
struct Pair<A, B>:
    first: A
    second: B
}

fn main():
    let p = Pair(42, "hello")
}
```

## Built‑in Collections

### Vec\<T\>:

```vox
let mut v: Vec<i32> = Vec::new()
push(v, 10)
push(v, 20)
assert(len(v) == 2, "expected length 2")
let last = pop(v)   # returns Option<i32>
```

### HashMap\<K,V\>:

```vox
let mut m: HashMap<&str, i32> = HashMap::new()
insert(m, "a", 1)
insert(m, "b", 2)
let val = get(m, "a")   # returns Option<i32>
match val:
    Some(v) -> println(v)
    None    -> println("none")
}
```

## C Bridge (Experimental):

Import C headers and use them safely:

```vox
import "mylib.h"

fn main():
    mylib_function()
}
```

The compiler parses the header, generates safe Vox wrappers, and caches them.

Status: Minimal prototype – see C Bridge documentation for details.

## Next Steps

- Try the examples in the main repository.

- Read the Standard Library reference.

- Experiment with GPU kernels and refinements.
