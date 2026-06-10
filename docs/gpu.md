# GPU Kernels

Voxlang supports first‑class heterogeneous computing with `@kernel` functions. The same ownership, borrowing, and refinement type verification applies to GPU code.

> **⚠️ Experimental feature** – GPU kernels have been tested on **AMD GPUs (ROCm/HIP)**. NVIDIA (CUDA) and Intel are not yet verified. Full support across all major GPU vendors is planned for a future release. The feature is usable but may contain bugs.

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
Kernels are launched via the runtime API. The exact syntax depends on the target backend. Example for HIP:

vox
fn main():
    let mut out = 0
    launch_kernel(add, 1, 1, 5, 7, &mut out)
    # out == 12
}
Note: The launch API is subject to change as GPU support matures.

Supported Backends
Backend	Status	Tested on
ROCm/HIP	✅ Working	AMD RX 6000/7000
CUDA	❌ Not tested	Planned for v0.3
CPU fallback	✅ Always	Any machine
If no GPU is available or the --gpu flag is omitted, kernels run on the CPU (slower, but useful for testing). The verification still applies.

Verification Guarantees
Preconditions and postconditions are checked by Z3 at compile time – same as host code.

Ownership rules apply: a &mut argument cannot be aliased.

No runtime bounds checks are inserted for verified accesses.

Limitations (Experimental)
Only AMD GPUs with ROCm 6.x+ have been tested.

Kernel launch configuration (grid/block size) is currently hard‑coded.

No automatic device memory management – you must manually allocate GPU buffers using the runtime API (see examples).

Kernel launch stubs are generated but the host‑side runtime (vox_rt) must be compiled with GPU support.

Example: Vector Addition
vox
@kernel fn vec_add(a: &[i32], b: &[i32], result: &mut [i32]):
    let i = get_global_id(0)
    if i < len(a):
        result[i] = a[i] + b[i]
    }
}

fn main():
    let a = [1, 2, 3]
    let b = [4, 5, 6]
    let mut out = [0, 0, 0]
    launch_kernel(vec_add, len(a), 1, a, b, &mut out)
    # out == [5, 7, 9]
}
Future Work
Full CUDA support

Automatic buffer management

Unified launch API across backends

Texture and shared memory support

For now, use the HIP backend and report issues via GitHub.
