// module.rs - Content‑addressed module resolution with hash verification.
// Provides module resolution, caching, symbol extraction, and C‑bridge integration.

use crate::CacheConfig;
use crate::bridge;
use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::frontend::lexer::Lexer;
use crate::frontend::span::Span;
use crate::parser::{
    ASTNode, EnumVariant, HashAlgorithm, HashValue, ImportSource, Parser, StructField,
};

use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

// -----------------------------------------------------------------------------
// Symbol table for a single module
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ModuleSymbolTable {
    pub functions: HashMap<
        String,
        (
            Vec<String>,               // param types
            Vec<Option<Box<ASTNode>>>, // param refinements
            String,                    // return type
            Option<Box<ASTNode>>,      // return refinement
            Vec<String>,               // generic_params
        ),
    >,
    pub structs: HashMap<String, Vec<StructField>>,
    pub enums: HashMap<String, Vec<EnumVariant>>,
}

impl ModuleSymbolTable {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Extract top‑level functions, structs, and enums from an AST.
pub fn extract_module_symbols(ast: &ASTNode) -> ModuleSymbolTable {
    let mut symbols = ModuleSymbolTable::new();
    match ast {
        ASTNode::Program(stmts, _) => {
            for stmt in stmts {
                match stmt {
                    ASTNode::FunctionDef {
                        name,
                        params,
                        return_type,
                        return_refinement,
                        ..
                    } => {
                        let param_types = params.iter().map(|p| p.ty.clone()).collect();
                        let param_refinements =
                            params.iter().map(|p| p.refinement.clone()).collect();
                        symbols.functions.insert(
                            name.clone(),
                            (
                                param_types,
                                param_refinements,
                                return_type.clone(),
                                return_refinement.clone(),
                                Vec::new(),
                            ),
                        );
                    }
                    ASTNode::KernelFn {
                        name,
                        params,
                        device_triple: _,
                        ..
                    } => {
                        let param_types = params.iter().map(|p| p.ty.clone()).collect();
                        let param_refinements =
                            params.iter().map(|p| p.refinement.clone()).collect();
                        symbols.functions.insert(
                            name.clone(),
                            (
                                param_types,
                                param_refinements,
                                "void".to_string(),
                                None,
                                Vec::new(),
                            ),
                        );
                    }
                    ASTNode::StructDef { name, fields, .. } => {
                        symbols.structs.insert(name.clone(), fields.clone());
                    }
                    ASTNode::EnumDef { name, variants, .. } => {
                        symbols.enums.insert(name.clone(), variants.clone());
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    symbols
}

// -----------------------------------------------------------------------------
// Cache directories
// -----------------------------------------------------------------------------
fn cache_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap_or_else(|| PathBuf::from(".").into_os_string());
    PathBuf::from(home).join(".vox/cache")
}

fn source_cache_dir() -> PathBuf {
    cache_dir().join("source")
}

pub fn proof_cache_dir() -> PathBuf {
    cache_dir().join("proofs")
}

// -----------------------------------------------------------------------------
// File reading
// -----------------------------------------------------------------------------
fn read_source_mmap(path: &Path) -> io::Result<String> {
    let data = fs::read(path)?;
    if data.is_empty() {
        return Ok(String::new());
    }
    let content =
        std::str::from_utf8(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(content.to_string())
}

pub fn read_source_file(path: &Path) -> io::Result<String> {
    read_source_mmap(path)
}

// -----------------------------------------------------------------------------
// Hash computation
// -----------------------------------------------------------------------------
pub fn compute_hash(content: &str, _algo: &HashAlgorithm) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let hash = hasher.finish();
    format!("{:x}", hash)
}

// -----------------------------------------------------------------------------
// Download helpers (curl/wget)
// -----------------------------------------------------------------------------
fn download_with_curl(url: &str) -> Result<String, String> {
    debug_log(format!("Downloading {} via curl", url));
    let curl_status = Command::new("curl")
        .args(["-L", "-s", "-S", "--fail", url])
        .output();

    if let Ok(output) = curl_status {
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .map_err(|e| format!("Invalid UTF-8 in downloaded content: {}", e));
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "curl failed with exit code {:?}: {}",
                output.status.code(),
                stderr
            ));
        }
    }

    debug_log("curl failed, trying wget");
    let wget_status = Command::new("wget").args(["-q", "-O-", url]).output();

    match wget_status {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .map_err(|e| format!("Invalid UTF-8 in downloaded content: {}", e)),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "wget failed with exit code {:?}: {}",
                output.status.code(),
                stderr
            ))
        }
        Err(e) => Err(format!(
            "Neither curl nor wget is available or both failed to download '{}'. \
             Please install curl or wget, or use --offline mode with a cached module. \
             (Error: {})",
            url, e
        )),
    }
}

// -----------------------------------------------------------------------------
// C header bridging
// -----------------------------------------------------------------------------
fn resolve_c_header(header_path: &Path, config: &CacheConfig, span: Span) -> Option<ASTNode> {
    let header_content = match read_source_file(header_path) {
        Ok(c) => c,
        Err(e) => {
            emit_diagnostic(
                &Diagnostic::error(format!("Failed to read C header: {}", e))
                    .with_code("VX0303")
                    .with_span(span),
            );
            return None;
        }
    };
    let header_hash = compute_hash(&header_content, &HashAlgorithm::Sha256);
    let cache_dir = crate::get_cache_dir();
    let bridge_cache_dir = cache_dir.join("bridge");
    let cached_vx_path = bridge_cache_dir.join(format!("{}.vx", header_hash));

    if !config.no_cache && cached_vx_path.exists() {
        debug_log(format!(
            "Using cached bridge output: {}",
            cached_vx_path.display()
        ));
        if let Ok(content) = read_source_file(&cached_vx_path) {
            return parse_vx_source(&content, &cached_vx_path, span);
        }
    }

    debug_log(format!("Bridging C header: {}", header_path.display()));
    let generated_vx = match bridge::bridge_header(header_path) {
        Ok(code) => code,
        Err(e) => {
            emit_diagnostic(
                &Diagnostic::error(format!("Bridge failed: {}", e))
                    .with_code("VX0304")
                    .with_span(span),
            );
            return None;
        }
    };

    if !config.no_cache {
        if let Err(e) = fs::create_dir_all(&bridge_cache_dir) {
            emit_diagnostic(
                &Diagnostic::warning(format!("Failed to create bridge cache dir: {}", e))
                    .with_code("VX0305")
                    .with_span(span),
            );
        } else if let Err(e) = fs::write(&cached_vx_path, &generated_vx) {
            emit_diagnostic(
                &Diagnostic::warning(format!("Failed to write bridge cache: {}", e))
                    .with_code("VX0306")
                    .with_span(span),
            );
        }
    }

    parse_vx_source(&generated_vx, header_path, span)
}

fn parse_vx_source(source: &str, origin: &Path, span: Span) -> Option<ASTNode> {
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(()) => {
            emit_diagnostic(
                &Diagnostic::error(format!(
                    "Lexical errors in generated Vox code from {:?}",
                    origin
                ))
                .with_code("VX0307")
                .with_span(span),
            );
            return None;
        }
    };
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    if matches!(ast, ASTNode::Error) {
        emit_diagnostic(
            &Diagnostic::error(format!(
                "Parse errors in generated Vox code from {:?}",
                origin
            ))
            .with_code("VX0308")
            .with_span(span),
        );
        return None;
    }
    Some(ast)
}

// -----------------------------------------------------------------------------
// Module resolver
// -----------------------------------------------------------------------------
pub struct ModuleResolver {
    current_file: PathBuf,
    loaded_modules: HashMap<String, ASTNode>, // canonical → AST
    loaded_symbols: HashMap<String, ModuleSymbolTable>,
    config: CacheConfig,
}

impl ModuleResolver {
    pub fn new(current_file: &Path, config: &CacheConfig) -> Self {
        Self {
            current_file: current_file.to_path_buf(),
            loaded_modules: HashMap::new(),
            loaded_symbols: HashMap::new(),
            config: *config,
        }
    }

    /// Resolve an import node, returning the AST of the imported module.
    pub fn resolve_import(&mut self, import: &ASTNode, span: Span) -> Option<ASTNode> {
        match import {
            ASTNode::Import {
                source,
                alias: _,
                hash,
                span: _,
            } => {
                let canonical = self.canonicalize_source(source, span)?;

                if let Some(ast) = self.loaded_modules.get(&canonical) {
                    debug_log(format!("Module already loaded: {}", canonical));
                    return Some(ast.clone());
                }

                // C header handling
                if let ImportSource::LocalPath(path) = source {
                    if path.ends_with(".h") || path.ends_with(".hpp") {
                        let base = self.current_file.parent().unwrap_or(Path::new("."));
                        let resolved = base.join(path);
                        let ast = resolve_c_header(&resolved, &self.config, span)?;
                        let symbols = extract_module_symbols(&ast);
                        self.loaded_modules.insert(canonical.clone(), ast.clone());
                        self.loaded_symbols.insert(canonical, symbols);
                        return Some(ast);
                    }
                }

                // Normal Vox module
                let content = self.load_source(&canonical, source, hash.as_ref(), span)?;
                let hash_algo = hash.as_ref().map(|h| h.algorithm.clone());
                let expected_digest = hash.as_ref().map(|h| h.digest.clone());

                if let Some((algo, expected)) = hash_algo.zip(expected_digest) {
                    let actual = compute_hash(&content, &algo);
                    if actual != expected {
                        emit_diagnostic(
                            &Diagnostic::error(format!(
                                "Hash mismatch for import '{}': expected {}, got {}",
                                self.format_source(source),
                                expected,
                                actual
                            ))
                            .with_code("VX0807")
                            .with_span(span),
                        );
                        return None;
                    }
                }

                let ast = self.parse_module(&content, &canonical, span)?;
                let symbols = extract_module_symbols(&ast);
                self.loaded_modules.insert(canonical.clone(), ast.clone());
                self.loaded_symbols.insert(canonical, symbols);
                Some(ast)
            }
            _ => {
                emit_diagnostic(
                    &Diagnostic::error("Expected import node")
                        .with_code("VX0808")
                        .with_span(span),
                );
                None
            }
        }
    }

    pub fn get_module_symbols(&self, canonical: &str) -> Option<&ModuleSymbolTable> {
        self.loaded_symbols.get(canonical)
    }

    pub fn get_loaded_ast(&self, canonical: &str) -> Option<&ASTNode> {
        self.loaded_modules.get(canonical)
    }

    pub fn get_loaded_modules(&self) -> &HashMap<String, ASTNode> {
        &self.loaded_modules
    }

    pub fn canonicalize_source(&self, source: &ImportSource, span: Span) -> Option<String> {
        match source {
            ImportSource::LocalPath(path) => {
                let base = self.current_file.parent().unwrap_or(Path::new("."));
                let resolved = base.join(path);
                if !resolved.exists() {
                    emit_diagnostic(
                        &Diagnostic::error(format!("Local import not found: {}", path))
                            .with_code("VX0809")
                            .with_span(span),
                    );
                    return None;
                }
                Some(
                    resolved
                        .canonicalize()
                        .unwrap_or(resolved)
                        .to_string_lossy()
                        .to_string(),
                )
            }
            ImportSource::RemoteUrl(url) => Some(url.clone()),
        }
    }

    fn load_source(
        &self,
        canonical: &str,
        source: &ImportSource,
        hash: Option<&HashValue>,
        span: Span,
    ) -> Option<String> {
        match source {
            ImportSource::LocalPath(_) => {
                let path = Path::new(canonical);
                match read_source_mmap(path) {
                    Ok(content) => Some(content),
                    Err(_) => match fs::read_to_string(path) {
                        Ok(content) => Some(content),
                        Err(_) => {
                            emit_diagnostic(
                                &Diagnostic::error(format!(
                                    "Failed to read local module: {}",
                                    canonical
                                ))
                                .with_code("VX0810")
                                .with_span(span),
                            );
                            None
                        }
                    },
                }
            }
            ImportSource::RemoteUrl(url) => {
                let hash = match hash {
                    Some(h) => h,
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error("Remote import missing hash")
                                .with_code("VX0806")
                                .with_span(span),
                        );
                        return None;
                    }
                };
                let cache_path = source_cache_dir()
                    .join(format!("{:?}", hash.algorithm))
                    .join(&hash.digest[0..2])
                    .join(&hash.digest)
                    .with_extension("vx");

                if self.config.offline && !cache_path.exists() {
                    emit_diagnostic(
                        &Diagnostic::error(format!(
                            "Offline mode: module {} not found in cache. Run without --offline to download.",
                            url
                        ))
                        .with_code("VX0819")
                        .with_span(span),
                    );
                    return None;
                }

                if !self.config.no_cache && cache_path.exists() {
                    debug_log(format!("Cache hit for {}", url));
                    if let Ok(content) = read_source_mmap(&cache_path) {
                        return Some(content);
                    }
                    if let Ok(content) = fs::read_to_string(&cache_path) {
                        return Some(content);
                    }
                }

                debug_log(format!("Downloading remote module: {}", url));
                let content = match download_with_curl(url) {
                    Ok(data) => data,
                    Err(e) => {
                        emit_diagnostic(
                            &Diagnostic::error(format!("Failed to download {}: {}", url, e))
                                .with_code("VX0811")
                                .with_span(span),
                        );
                        return None;
                    }
                };

                let actual = compute_hash(&content, &hash.algorithm);
                if actual != hash.digest {
                    emit_diagnostic(
                        &Diagnostic::error(format!(
                            "Hash mismatch for downloaded module: expected {}, got {}",
                            hash.digest, actual
                        ))
                        .with_code("VX0807")
                        .with_span(span),
                    );
                    return None;
                }

                if !self.config.no_cache {
                    if let Err(e) = fs::create_dir_all(cache_path.parent().unwrap()) {
                        emit_diagnostic(
                            &Diagnostic::warning(format!("Failed to create cache dir: {}", e))
                                .with_code("VX0814")
                                .with_span(span),
                        );
                    } else if let Err(e) = fs::write(&cache_path, &content) {
                        emit_diagnostic(
                            &Diagnostic::warning(format!("Failed to write cache: {}", e))
                                .with_code("VX0815")
                                .with_span(span),
                        );
                    }
                }
                Some(content)
            }
        }
    }

    fn parse_module(&self, source: &str, path: &str, span: Span) -> Option<ASTNode> {
        let mut lexer = Lexer::new(source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(()) => {
                emit_diagnostic(
                    &Diagnostic::error(format!("Lexical errors in imported module: {}", path))
                        .with_code("VX0816")
                        .with_span(span),
                );
                return None;
            }
        };
        let mut parser = Parser::new(&tokens);
        let ast = parser.parse();
        if matches!(ast, ASTNode::Error) {
            emit_diagnostic(
                &Diagnostic::error(format!("Parse errors in imported module: {}", path))
                    .with_code("VX0817")
                    .with_span(span),
            );
            return None;
        }
        Some(ast)
    }

    fn format_source(&self, source: &ImportSource) -> String {
        match source {
            ImportSource::LocalPath(p) => p.clone(),
            ImportSource::RemoteUrl(u) => u.clone(),
        }
    }
}
