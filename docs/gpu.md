# GPU Kernels

Voxlang supports first‑class heterogeneous computing with `@kernel` functions. The same ownership, borrowing, and refinement type verification applies to GPU code.

> **✅ Stable feature** – GPU kernels are fully supported on:
> - **Windows** with **AMD ROCm/HIP** (tested on Radeon RX 9060 XT, ROCm 7.1+)
> - **Linux** with **NVIDIA CUDA** (tested on CUDA 11.8/12.x)
> - **Mac** with **AIR** (coming soon)
> - Cross‑platform combinations (CUDA on Windows, HIP on Linux) are expected to work but not yet officially verified.

---

## Writing a Kernel

A kernel is a function annotated with `@kernel`. It must return `void` and can take `&mut` pointers to return results.

```vox
@kernel fn add(a: i32, b: i32, result: &mut i32):
    *result = a + b
}
```
## Refinement Types

Kernel functions support the same refinement type system as host functions. Preconditions (`where` clauses) are checked at every call site, and postconditions verify the return value (though kernels return `void`, so postconditions are less common).

```vox
@kernel fn divide(a: i32, b: i32, result: &mut i32) where b != 0:
    *result = a / b
}
```
## Launching Kernels

Kernels are launched using the launch keyword, followed by the grid size in parentheses, and then the arguments in parentheses:

```vox

fn main():
    let mut out = 0
    launch add(1, 1, 1)(5, 7, &mut out)
    # out == 12
}

    The first three numbers (gx, gy, gz) are the grid dimensions (number of blocks).

    Block dimensions are defined in the @kernel attribute (e.g., @kernel(block=(256,1,1))). If omitted, (1,1,1) is used.
```

## Supported Backends
Backend	Status	Tested on	Minimum Driver / SDK
CUDA	✅ Working	NVIDIA GPUs (Linux)	CUDA 11.8+ / 12.x
ROCm/HIP	✅ Working	AMD RX 6000/7000 (Linux)	ROCm 6.x+
CPU fallback	✅ Always	Any machine	–

If no GPU is available or the --gpu flag is omitted, kernels run on the CPU (slower, but useful for testing). The verification still applies.

### Metal Backend

The Metal backend is fully supported on macOS, both on Intel and Apple Silicon. To use it, pass `--gpu metal` to the compiler. The architecture can be specified with `--gpu-arch apple2` (Apple Silicon) or `--gpu-arch mac2` (Intel); if omitted, the compiler auto‑detects the architecture based on the host target triple.

Metal kernels are compiled from MSL (Metal Shading Language) source at runtime using the Metal API (`newLibraryWithSource:options:error:`). Scalars are automatically packed into a struct and passed as a buffer, as required by MSL. `match` expressions on `Option`, `Result`, and custom enums are fully supported inside Metal kernels. No separate device code compilation step is required; the MSL source is embedded in the host binary and compiled just‑in‑time.

The Metal backend requires macOS 10.13+ and Xcode 10+ (or the command‑line tools) to be installed. It has been tested on Intel HD 5000 and Apple Silicon GPUs.

## Verification Guarantees

Preconditions and postconditions are checked by Z3 at compile time – same as host code.

Ownership rules apply: a &mut argument cannot be aliased.

No runtime bounds checks are inserted for verified accesses.

## Limitations (Current)

Kernel launch configuration (block dimensions) is hard‑coded in the @kernel attribute; grid dimensions are given at launch.

No automatic device memory management – you must manually allocate GPU buffers using vox_gpu_malloc / vox_gpu_free.

The host‑side runtime (vox_rt) must be compiled with GPU support (enabled by default when --gpu is used).

CUDA on Windows and HIP on Linux are not yet officially verified.

## Example: Vector Addition

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

Note: get_global_id(0) is a built‑in function that returns the global thread index in the first dimension.

## Future Work (Roadmap)

- 0.6 – Automatic device memory management (RAII), texture and shared memory support.

- 1.0 – All three backends (CUDA, HIP, Metal) fully tested across all major OS combinations.

For now, use the verified backends and report issues via GitHub.
