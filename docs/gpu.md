# GPU Kernels

Voxlang supports first‑class heterogeneous computing with `@kernel` functions. The same ownership, borrowing, and refinement type verification applies to GPU code.

> **✅ Stable feature** – GPU kernels are fully supported on **Linux** for both **NVIDIA CUDA** and **AMD ROCm/HIP**. Windows GPU support is not yet verified (may work with appropriate drivers but is not officially supported).

---

## Writing a Kernel

A kernel is a function annotated with `@kernel`. It must return `void` and can take `&mut` pointers to return results.

```vox
@kernel fn add(a: i32, b: i32, result: &mut i32):
    *result = a + b
}

Refinement types work as usual:
vox

@kernel fn safe_add(a: i32, b: i32, result: &mut i32) where a + b < 2_147_483_647:
    *result = a + b
}

Launching Kernels

Kernels are launched using the launch keyword, followed by the grid size in parentheses, and then the arguments in parentheses:
vox

fn main():
    let mut out = 0
    launch add(1, 1, 1)(5, 7, &mut out)
    # out == 12
}

    The first three numbers (gx, gy, gz) are the grid dimensions (number of blocks).

    Block dimensions are defined in the @kernel attribute (e.g., @kernel(block=(256,1,1))). If omitted, (1,1,1) is used.

Supported Backends
Backend	Status	Tested on	Minimum Driver / SDK
CUDA	✅ Working	NVIDIA GPUs (Linux)	CUDA 11.8+ / 12.x
ROCm/HIP	✅ Working	AMD RX 6000/7000 (Linux)	ROCm 6.x+
CPU fallback	✅ Always	Any machine	–

If no GPU is available or the --gpu flag is omitted, kernels run on the CPU (slower, but useful for testing). The verification still applies.
Verification Guarantees

    Preconditions and postconditions are checked by Z3 at compile time – same as host code.

    Ownership rules apply: a &mut argument cannot be aliased.

    No runtime bounds checks are inserted for verified accesses.

Limitations

    Kernel launch configuration (grid/block size) is currently hard‑coded in the attribute.

    No automatic device memory management – you must manually allocate GPU buffers using the runtime API (see examples).

    Kernel launch stubs are generated but the host‑side runtime (vox_rt) must be compiled with GPU support.

    Windows GPU support is not yet verified.

Example: Vector Addition
vox

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

    Note: get_global_id(0) is a built‑in function that returns the global thread index in the first dimension.

Future Work

    Automatic buffer management

    Unified launch API across backends

    Texture and shared memory support

    Windows GPU support (CUDA + HIP)

For now, use the Linux backends and report issues via GitHub.
