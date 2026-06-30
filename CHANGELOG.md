# Changelog

All notable changes to the Voxlang compiler will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [0.6-bootstrap] – 2026-06-29

### Added

- **Complete Metal GPU backend for macOS** – Kernel compilation and execution via Metal Shading Language (MSL) and the Metal API, supporting both Apple Silicon and Intel (tested on Intel HD 5000).
  - **MSL generation** – Scalars are packed into a `KernelArgs_*` struct; all kernel signatures use buffers only, satisfying MSL requirements.
  - **Metal runtime initialisation** – `MTLCopyAllDevices` fallback works reliably; device discovery and initialisation complete.
  - **On‑the‑fly MSL compilation** – Uses `newLibraryWithSource:options:error:` to compile MSL source at runtime.
  - **Kernel dispatch** – Buffers are created, bound, kernels dispatched, and results copied back to the host.
  - **IR argument passing** – Host passes host addresses; the runtime manages all device memory automatically.
  - **Argument sizes** – Sizes are now passed from the compiler (as an `i64*` array), eliminating hardcoded sizes.
  - **Match expressions in MSL** – Supports `match` on `Option`, `Result`, and custom enums, generating `if/else if` chains; binding patterns extract payloads into temporary variables for use in arm bodies.
  - **Register numbering validation** – Fixed validator to correctly track definitions (LHS of `=`), ignoring uses; debug builds now report zero false positives.

- **macOS Native GUI (ISE) improvements** – Further polished the Cocoa application launched by `vox shell`:
  - **Full menu bar** – All menus (`File`, `Edit`, `Build`, `Debug`) with working keyboard equivalents.
  - **All key equivalents** – `Cmd+O/S/Q`, `Cmd+Z/Shift+Z/Y`, `Cmd+X/C/V/A`, `Cmd+R`, `Cmd+B/Shift+B`, `Cmd+Shift+C`, `Cmd+T`, `Cmd+L` now reliably respond.
  - **Coloured terminal output** – ANSI escape sequences are parsed and rendered with full colour support (errors in red, warnings in yellow, info in cyan, etc.) while preserving the Menlo font.
  - **Auto‑scrolling terminal** – Automatically scrolls to the bottom when new output arrives, unless the user has manually scrolled up.
  - **Native progress bar** – Hardware‑accelerated `NSProgressIndicator` animates smoothly during compilations, builds, checks, tests, and clean operations.
  - **Drag‑and‑drop** – Dropping a `.vx` file onto the editor or terminal pane loads it instantly, handling `NSFilenamesPboardType`, `NSURL`, and `NSString` pasteboard types.
  - **Save‑on‑close** – Window delegate prompts to save modified files with proper error handling and user feedback.
  - **LSP integration** – Editor notifies the language server on file open/change; diagnostics appear in the terminal.
  - **Dark appearance** – Window uses the system dark theme and rounded corners.
  - **Resize‑aware layout** – Editor and terminal panes resize proportionally; status bar and progress bar maintain consistent width.
  - **Context menu** – Right‑click in the editor shows Undo/Redo, Cut/Copy/Paste, Delete, and Select All.

- **Windows Native GUI (ISE) – full feature parity with macOS**
  - **Complete menu bar** – All menus (`File`, `Edit`, `Build`) with working keyboard shortcuts (`Ctrl+O/S/Q`, `Ctrl+Z/Y/X/C/V/A`, `Ctrl+B/Shift+B`, `Ctrl+Shift+C`, `Ctrl+T`, `Ctrl+L`, `F5`).
  - **Rich Edit editor & terminal** – Full support for large files; terminal uses ANSI colour parsing with `CHARFORMAT2W`.
  - **Native progress bar** – `msctls_progress32` with smooth updates during all build actions.
  - **Drag‑and‑drop** – Drag a `.vx` file onto the window to open it, with UIPI bypass for Administrator‑level execution.
  - **Auto‑scroll** – Terminal stays at the bottom unless the user manually scrolls up.
  - **Dark mode** – Fully dark theme using:
    - DWM attributes (`DWMWA_USE_IMMERSIVE_DARK_MODE`, `DWMWA_CAPTION_COLOR`).
    - Undocumented UAH messages (`0x0091`/`0x0092`) to paint the horizontal menu bar dark.
    - `SetPreferredAppMode` (ordinal 135) for dark dropdown menus.
    - `DarkMode_Explorer` theme for scrollbars and child controls.
    - Custom 1‑px border using the "Gap Trick" – RichEdit controls are inset into the client area, revealing a dark border.
  - **Save‑on‑close** – Window procedure prompts to save modified files with `MB_YESNOCANCEL`.
  - **LSP integration** – Editor notifies the language server; diagnostics appear in the terminal with underlined errors/warnings.
  - **Build actions** – All actions (`Run`, `Build Debug`, `Build Release`, `Check`, `Test`, `Clean`) are fully implemented and threaded.
  - **Context menu** – Right‑click in the editor shows Undo/Redo, Cut/Copy/Paste, Delete, and Select All.
  - **Live test streaming** – Test output now streams line‑by‑line in real time, matching macOS behaviour.
  - **Custom title bar** – Uses `DWMWA_CAPTION_COLOR` to set a slightly lighter shade for the title bar.

- **Windows manifest embedding** – `vox.exe.manifest` is now embedded at link time via `build.rs`, enabling:
  - DPI awareness (`permonitorv2`).
  - Dark mode support (manifest flag).
  - `asInvoker` execution level.

- **Automated macOS bundle creation** – Added `bundle.sh` script and `cargo bundle` alias for one‑command `.app` generation.

- **Hotkey fixes for macOS** – Command key modifier mask now correctly set to `1 << 20`, fixing all menu key equivalents and custom key‑down handling.

- **Attributed‑string terminal rendering** – Uses `NSTextStorage` and `NSAttributedString` to preserve colours and fonts, with a full ANSI SGR parser.

- **Main‑thread routing for progress updates** – Phase‑update notifications from background threads are dispatched to the main thread, ensuring the status bar updates reliably.

- **Objective‑C ABI fixes** – Added explicit function bindings (`objc_msgSend_f64` and `objc_msgSend_perform_delay`) to correctly pass `f64` values, eliminating crashes and throttle‑loop spam.

- **Throttle debouncing** – The terminal refresh cancels any previously scheduled delayed refreshes, preventing overflow of queued operations.

- **Cross‑platform test streaming** – `run_tests` now uses `Stdio::piped()` and `BufReader` to stream output line‑by‑line, providing live test progress on both macOS and Windows.

- **Dark mode menu bar for Windows** – Undocumented UAH message handling (`WM_UAHDRAWMENU`/`WM_UAHDRAWMENUITEM`) paints the horizontal menu bar dark, matching the rest of the UI.

### Changed

- **MacOS GUI activation** – Replaced the fragile three‑stage delayed activation workaround with a single, reliable sequence, eliminating flicker and ensuring the menu bar appears immediately.
- **Layout polish** – Status text now has a fixed width, allowing the progress bar to expand and fill the remaining space.
- **Internal roadmap** – Core Metal backend phases (0–5) are now marked complete; optional Phase 6 (integration testing & documentation) is deferred but recommended for completeness.
- **Windows GUI refresh logic** – During test runs, the 200ms cooldown is bypassed, allowing instant terminal updates and live test output streaming.
- **Windows GUI border handling** – Native RichEdit borders removed and replaced with a custom 1‑px dark border using the "Gap Trick", eliminating the legacy 3D bevel.

### Fixed

- **Hotkeys not responding** – Corrected the Command key mask and ensured menu items have the correct action (`handleMenuCommand:`).
- **Terminal auto‑scroll** – Replaced manual scroll‑offset calculations with Cocoa’s native `scrollToEndOfDocument:`.
- **Missing terminal colours** – Enabled `setRichText:YES` and switched to attributed‑string appending; colours now display correctly.
- **Progress bar never filling** – Used the correct `percent` value and explicit `objc_msgSend_f64` binding to update the bar.
- **Segfault on `clear_output`** – Replaced `replaceCharactersInRange:withString:` with direct `setString:` on the terminal to avoid `NSRange` ABI mismatches.
- **`performSelector:afterDelay:` infinite loop** – Added explicit `objc_msgSend_perform_delay` binding with the correct `f64` register.
- **Drag‑and‑drop failure** – Restored `NSFilenamesPboardType` fallback alongside `NSURL` and `NSString` reading, ensuring file drops from Finder work.
- **Compilation thread crashes** – Moved all UI updates to the main thread before spawning the compilation thread, avoiding race conditions.
- **Register numbering validation** – Fixed false positives in debug builds by correctly tracking definitions only.
- **Duplicate FFI definitions** – Removed duplicate `SetTextColor` and `SetBkMode` declarations in `windows_gui.rs` that caused linker errors.
- **RECT Copy trait** – Added `#[derive(Copy, Clone)]` to `RECT` and `POINT` structs, fixing the "cannot move out of `dis.rcItem`" compile error.
- **Windows test output not streaming** – Fixed `request_refresh` and `process_output_refresh` to bypass the 200ms throttle during test runs, enabling live output streaming.

### Planned for future (beyond 0.6)

- **Diagnostic tooltips on hover** – Hovering over underlined errors/warnings will show detailed tooltips.
- **Line numbers in editor** – A gutter with line numbers for both macOS and Windows.
- **Find/Replace (Cmd+F)** – Implement a find dialog.
- **DWM theming fallbacks** – Graceful degradation when DWM attributes fail on Windows.
- **Phase 6 integration tests & documentation** – Comprehensive Metal test suite and user documentation (currently optional but recommended).
- **Metal optimisations** – Use of `threadgroup` memory, further performance tuning.
- **Windows GPU support (CUDA)** – Currently HIP (AMD) works; CUDA on Windows is planned.
- **Full self‑hosting** – Standard library rewritten in Voxlang (moved to 0.7).
- **Automatic device memory management** – For GPU buffers (moved to 0.7).


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
