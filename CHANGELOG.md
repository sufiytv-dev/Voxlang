# Changelog

All notable changes to the Voxlang compiler will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2-bootstrap]

### Added
- **Full macOS support** – All 31 conformance tests now pass on macOS (Apple Silicon and Intel). The same source code runs flawlessly on both macOS and Windows.
- **Cross‑platform verification** – Proven that no code changes are required to support Windows; the original macOS code works unchanged on Windows when the correct environment (Visual Studio Developer Command Prompt) is used.
- **Enhanced register allocation logging** – Debug builds now validate SSA register numbering and report gaps.

### Fixed
- **Windows linker environment** – Clarified in documentation that `vox test` requires a Visual Studio Developer Command Prompt (or an environment with `clang`/`lld` able to locate Windows SDK libraries). No code change was needed.

### Changed
- **Documentation** – Updated `README.md`, `SECURITY.md`, and `CONTRIBUTING.md` to reflect current project state.
- **Website** – Added live changelog loading from this file.

## [0.3-bootstrap] – Planned

### Planned
- **Full Linux support** – Expected to pass all 31 conformance tests on major distributions.
- **Cross‑platform installer** – Unified installation script for all three operating systems.

## [0.1-bootstrap]

### Added
- First public release
- Self‑hosted compiler written in Rust
- LLVM backend for native code generation
- Z3 refinement type verification engine
- Ownership and borrow checker
- GPU kernel support via `@kernel`
- C bridge with automatic safe wrapper generation
- Compile‑time evaluation (`@comptime`)
- Parallel loops (race‑free)
- `Vec<T>` and `HashMap<K,V>` generics
- Pattern matching
- `?` operator for `Result<T,E>`
- Windows and macOS binaries (prebuilt)
