# Contributing to Voxlang

Contributions to Voxlang are welcome! As this is a high-integrity compiler project, I have four strict rules for all contributions:

1. **Zero Dependencies:** Voxlang is built to be a standalone, high-integrity system. Do not add external dependencies. Everything must be implemented from the ground up.
2. **Safety First:** Avoid `unsafe` code at all costs. This compiler is built on the promise of memory safety; violations of this are non-negotiable.
3. **Robust Diagnostics:** Every new feature must include proper error handling and diagnostic reporting integrated with `diagnostic.rs`. Do not use `panic!` or `unwrap()` in production code.
4. **The Simplicity Mandate:** Please keep things incredibly simple and modular. I prioritize "Explainable Code" over "Clever Code." If the logic is too complex for me to grasp quickly, it is too complex for the project.

## Environment Requirements

To build and work on Voxlang, your environment must be configured with the following tools:

* **Compiler Toolchain**: Windows users must have MSVC installed. The recommended way to manage this environment is via [Scoop](https://scoop.sh/).
* **Z3 Prover**: The Z3 SMT solver is required for the refinement type verification engine.
* **GPU Development (Optional)**: If you intend to contribute to the kernel or parallel compute modules, a device-ready GPU driver (ROCm/HIP or CUDA) is recommended.


## How to contribute

- Please open an issue to discuss major changes *before* submitting a Pull Request.
- Ensure all tests pass before submitting.
- Follow the existing project structure (`src/codegen`, `src/bridge`, etc.).

Thank you for helping keep Voxlang clean, safe, and maintainable.
