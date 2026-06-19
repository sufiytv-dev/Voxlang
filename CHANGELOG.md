# Changelog

All notable changes to the Voxlang compiler will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5-bootstrap] – 2026-06-19

### Added

- **Full Integrated Shell Environment (ISE)** – The `vox shell` command now launches a full Windows-native GUI with:
  - Syntax‑highlighting editor (Rich Edit) with unlimited file size support.
  - Coloured terminal output with ANSI escape code support.
  - Status bar with a progress bar for compilation, build, check, and test phases.
  - Auto‑scrolling terminal (stays at bottom unless the user scrolls up).
  - Right‑click context menu with Undo/Redo, Cut/Copy/Paste, Delete, Select All.
  - Accelerator shortcuts: `F5` to Run, `Ctrl+O`/`Ctrl+S`/`Ctrl+Q` for Open/Save/Quit.
  - Native clipboard support (`Ctrl+C/V/X/A`) for editor and terminal.
  - Drag‑and‑drop file loading (works even when running as Administrator).
  - Graceful LSP shutdown on exit.
  - File close confirmation with Save/Don't Save/Cancel.
  - All build actions (Build Debug, Build Release, Check, Test, Clean) available via menu and toolbar.

- **Improved diagnostics** – Compiler error and warning messages are now:
  - Colour‑coded in the terminal (red for errors, yellow for warnings, cyan for notes).
  - Properly flushed even on early compilation failures.
  - Fully captured and displayed in the GUI terminal.

- **Toolchain discovery** – The compiler now automatically finds LLVM tools (`clang`, `llc`, `lld`) and GPU SDKs (CUDA/HIP) on Windows, eliminating the need for a Visual Studio Developer Command Prompt.

- **Test suite discovery** – `vox test` now locates the examples directory relative to the executable, making it portable in self‑hosted installations. The GUI test command uses the same logic.

- **Context menu** – Right‑click in the editor now shows Undo/Redo, Cut/Copy/Paste, Delete, and Select All.

### Fixed

- **Undo/Redo shortcuts** – `Ctrl+Z` (Undo) and `Ctrl+Y`/`Ctrl+Shift+Z` (Redo) now work correctly without interfering with normal typing.
- **Accelerator table conflicts** – Removed the accelerator table entirely; all shortcuts are now handled via subclass, preventing interference with Rich Edit native shortcuts.
- **GPU kernel linking** – The compiler now correctly auto‑detects the installed GPU SDK (CUDA/HIP) and selects the appropriate linker (`hipcc` or `clang`), fixing runtime failures for `kernel.vx` in the ISE.
- **Import path resolution** – Module resolution now resolves imports relative to the current file’s directory, allowing `test_use.vx` to find `lib/math.vx`.
- **Imported struct field access** – Semantic analysis now correctly resolves qualified struct names and substitutes generic parameters, enabling field access on imported structs.
- **Terminal performance** – Terminal buffer now truncates to 5000 lines, preventing freezes on large output.
- **Drag‑and‑drop reliability** – UIPI bypass and OLE initialisation ensure file loading works even when running as Administrator.

### Changed

- **No‑arguments launch** – Double‑clicking `vox.exe` now launches the ISE with debug output enabled and the console fully hidden.
- **Updated documentation** – Overhauled README, installation guides, and GPU backend status tables.
- **Diagnostic colour mapping** – All diagnostic messages now use consistent colour codes in the GUI terminal.

### Planned for 0.6 (deferred)

- **Diagnostic tooltips on hover** – Hovering over underlined errors/warnings will show detailed tooltips.
- **Line numbers in editor** – A major UI upgrade.
- **Find/Replace (Ctrl+F)** – Implement find dialog.
- **DWM theming fallbacks** – Graceful degradation when DWM attributes fail.

## [0.4-bootstrap] – 2026-06-14

### Added

- **HIP (AMD) on Windows confirmed working** – GPU kernels now execute correctly on AMD GPUs under Windows with ROCm 7.1+. Tested on Radeon RX 9060 XT.
- **Address‑space fix for AMDGCN** – Mutable reference parameters (`&mut T`) are now correctly mapped to `ptr addrspace(1)` for global memory, resolving illegal memory accesses that caused kernel crashes.
- **Corrected HIP kernel launch API** – Replaced legacy config array with direct `kernelParams` pointer array, eliminating `hipErrorLaunchFailure`.
- **Unified GPU backend verification** – Both CUDA (Linux) and HIP (Windows) now pass the `add_kernel` conformance test (5 + 7 = 12).

### Changed

- **GPU documentation** – Updated status tables: HIP (Windows) and CUDA (Linux) marked as stable; cross‑platform combinations noted as expected but not yet officially tested.
- **CPU fallback** – Remains default when `--gpu` is omitted; works on all platforms.

### Fixed

- **Device IR generation for AMD** – All loads/stores of pointer arguments now use the correct address space (`ptr addrspace(1)`), derived from the symbol table.
- **Dereference expression compilation on device** – `*ptr` now correctly loads from the pointer’s stored address space.
- **Register numbering validation** – Debug builds report gaps without affecting production builds.

## [0.3-bootstrap] – 2026-06-13

### Added

- **Full Linux support (x86_64)** – All 31 conformance tests now pass on Ubuntu 22.04/24.04, Debian 12, and Fedora 38+.
- **CUDA backend for NVIDIA GPUs** – GPU kernels can now be compiled and executed on NVIDIA GPUs via `--gpu cuda`. Minimum driver version: CUDA 11.8+ or 12.x.
- **ROCm/HIP backend** – Continued support for AMD GPUs (ROCm 6.x+), tested on RX 6000/7000 series.
- **CPU fallback for GPU kernels** – When `--gpu` is omitted or no compatible GPU is found, kernels run on CPU (useful for testing on any machine).
- **Unified kernel launch syntax** – `launch kernel(grid)(args)` works identically for both CUDA and HIP.
- **Built‑in `get_global_id(dim)` function** for kernels to obtain thread indices.

### Changed

- **Installation documentation** – Added comprehensive Linux setup instructions, including dependencies (`z3`, `clang`, `lld`) and optional GPU SDKs.
- **README** – Updated to reflect Linux as a fully supported platform, added `--gpu cuda` examples.
- **GPU documentation** – Marked CUDA as stable, updated backend status table, corrected launch syntax examples.

### Fixed

- **Register numbering validation** – Debug builds now correctly detect and report SSA register gaps (non‑fatal, but aids development).
- **Monomorphised function declarations** – Forward declarations are now emitted at module scope, fixing “expected instruction opcode” errors when compiling generics + GPU code.

### Planned for 0.4 (subsequently released)

- ~~Apple/Metal AIR support~~ → moved to 0.5
- ~~Windows GPU support (CUDA + HIP)~~ → HIP completed; CUDA on Windows pending
- ~~Full self‑hosting (standard library rewritten in Voxlang)~~ → moved to 0.6
- ~~Automatic device memory management for GPU buffers~~ → moved to 0.6

## [0.2-bootstrap] – 2026-06-01

### Added

- **Full macOS support** – All 31 conformance tests now pass on macOS (Apple Silicon and Intel). The same source code runs flawlessly on both macOS and Windows.
- **Cross‑platform verification** – Proven that no code changes are required to support Windows; the original macOS code works unchanged on Windows when the correct environment (Visual Studio Developer Command Prompt) is used.
- **Enhanced register allocation logging** – Debug builds now validate SSA register numbering and report gaps.

### Fixed

- **Windows linker environment** – Clarified in documentation that `vox test` requires a Visual Studio Developer Command Prompt (or an environment with `clang`/`lld` able to locate Windows SDK libraries). No code change was needed.

### Changed

- **Documentation** – Updated `README.md`, `SECURITY.md`, and `CONTRIBUTING.md` to reflect current project state.
- **Website** – Added live changelog loading from this file.

## [0.1-bootstrap] – 2026-05-15

### Added

- First public release
- Self‑hosted compiler written in Rust
- LLVM backend for native code generation
- Z3 refinement type verification engine
- Ownership and borrow checker
- GPU kernel support via `@kernel` (initial AMD/HIP only)
- C bridge with automatic safe wrapper generation
- Compile‑time evaluation (`@comptime`)
- Parallel loops (race‑free)
- `Vec<T>` and `HashMap<K,V>` generics
- Pattern matching
- `?` operator for `Result<T,E>`
- Windows and macOS binaries (prebuilt)
