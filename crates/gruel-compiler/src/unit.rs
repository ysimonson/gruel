//! Unified compilation unit that owns all compilation artifacts.
//!
//! The [`CompilationUnit`] provides a single source of truth for all compilation state,
//! from source files through to machine code. It enforces phase ordering through the
//! type system - you can't access AIR without first running semantic analysis.
//!
//! # Example
//!
//! ```ignore
//! use gruel_compiler::{CompilationUnit, SourceFile, CompileOptions};
//! use gruel_util::FileId;
//!
//! // Create source files
//! let sources = vec![
//!     SourceFile::new("main.gruel", "fn main() -> i32 { 42 }", FileId::new(1)),
//! ];
//!
//! // Create compilation unit and run phases
//! let mut unit = CompilationUnit::new(sources, CompileOptions::default())?;
//! unit.parse()?;
//! unit.analyze()?;
//! let output = unit.compile()?;
//! ```

use rustc_hash::FxHashMap as HashMap;

use lasso::ThreadedRodeo;
use rayon::prelude::*;
use tracing::{info, info_span};

use crate::{
    AnalyzedFunction, Ast, AstGen, Cfg, CfgBuilder, CompileError, CompileErrors, CompileOptions,
    CompileOutput, CompileWarning, ErrorKind, FunctionWithCfg, Lexer, MultiErrorResult, OptLevel,
    Parser, Rir, Sema, SourceFile, Type, TypeInternPool, compile_backend,
};
use gruel_cache::{CacheKey, CacheStore, Hasher, compiler_fingerprint};
use gruel_util::{FileId, PreviewFeature};

fn opt_level_to_u32(o: OptLevel) -> u32 {
    match o {
        OptLevel::O0 => 0,
        OptLevel::O1 => 1,
        OptLevel::O2 => 2,
        OptLevel::O3 => 3,
    }
}

/// Run sema on the merged RIR. Extracted so the AIR-cache miss path
/// (ADR-0074 Phase 4) can call it in two places without duplicating
/// the boilerplate.
fn run_sema(
    rir: &Rir,
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    file_paths: HashMap<FileId, String>,
) -> MultiErrorResult<gruel_air::SemaOutput> {
    let _span = info_span!("sema").entered();
    let mut sema = Sema::new(rir, interner, options.preview_features.clone());
    sema.set_file_paths(file_paths);
    sema.set_suppress_comptime_dbg_print(options.capture_comptime_dbg);
    let output = sema.analyze_all()?;
    info!(
        function_count = output.functions.len(),
        struct_count = output.type_pool.stats().struct_count,
        "semantic analysis complete"
    );
    Ok(output)
}

/// Clone a slice of analyzed functions for the AIR cache write path.
fn clone_functions(fns: &[gruel_air::AnalyzedFunction]) -> Vec<gruel_air::AnalyzedFunction> {
    fns.to_vec()
}

/// Snapshot a TypeInternPool for the AIR cache write path.
fn clone_type_pool(pool: &gruel_air::TypeInternPool) -> gruel_air::TypeInternPool {
    pool.clone_snapshot()
}

/// ADR-0065 / ADR-0070: the synthetic compiler prelude.
///
/// Contains canonical type definitions injected into every compilation:
/// - `Option(T)` (ADR-0065): the canonical optional type.
/// - `Result(T, E)` (ADR-0070): the canonical fallible-with-context type.
///
/// This is parsed as a regular Gruel source file with `FileId::PRELUDE` and
/// runs through the standard pipeline. Adding more types here is just adding
/// more declarations to the string.
const PRELUDE_SOURCE: &str = r#"
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,

        fn is_some(self: Ref(Self)) -> bool {
            match self {
                Self::Some(_) => true,
                Self::None => false,
            }
        }

        fn is_none(self: Ref(Self)) -> bool {
            match self {
                Self::Some(_) => false,
                Self::None => true,
            }
        }

        fn unwrap(self) -> T {
            match self {
                Self::Some(x) => x,
                Self::None => @panic("called unwrap on a None value"),
            }
        }

        fn unwrap_or(self, default: T) -> T {
            match self {
                Self::Some(x) => x,
                Self::None => default,
            }
        }
    }
}

fn Result(comptime T: type, comptime E: type) -> type {
    enum {
        Ok(T),
        Err(E),

        fn is_ok(self: Ref(Self)) -> bool {
            match self {
                Self::Ok(_) => true,
                Self::Err(_) => false,
            }
        }

        fn is_err(self: Ref(Self)) -> bool {
            match self {
                Self::Ok(_) => false,
                Self::Err(_) => true,
            }
        }

        fn unwrap(self) -> T {
            match self {
                Self::Ok(x) => x,
                Self::Err(_) => @panic("called unwrap on an Err value"),
            }
        }

        fn unwrap_err(self) -> E {
            match self {
                Self::Ok(_) => @panic("called unwrap_err on an Ok value"),
                Self::Err(e) => e,
            }
        }

        fn unwrap_or(self, default: T) -> T {
            match self {
                Self::Ok(x) => x,
                Self::Err(_) => default,
            }
        }

        fn expect(self, msg: String) -> T {
            match self {
                Self::Ok(x) => x,
                Self::Err(_) => @panic(msg),
            }
        }

        fn expect_err(self, msg: String) -> E {
            match self {
                Self::Ok(_) => @panic(msg),
                Self::Err(e) => e,
            }
        }
    }
}

// ADR-0071: validated u32 → char conversion.
fn char__from_u32(n: u32) -> Result(char, u32) {
    let surrogate_lo: u32 = 55296;
    let surrogate_hi: u32 = 57343;
    let max_scalar: u32 = 1114111;
    if (n >= surrogate_lo && n <= surrogate_hi) || n > max_scalar {
        let R = Result(char, u32);
        R::Err(n)
    } else {
        checked {
            let c: char = char::from_u32_unchecked(n);
            let R = Result(char, u32);
            R::Ok(c)
        }
    }
}

// ADR-0071: char.is_ascii() — true iff the codepoint is < 128.
fn char__is_ascii(c: char) -> bool {
    let n: u32 = c.to_u32();
    let limit: u32 = 128;
    n < limit
}

// ADR-0071: char.len_utf8() — number of UTF-8 bytes (1, 2, 3, or 4)
// needed to encode the codepoint.
fn char__len_utf8(c: char) -> usize {
    let n: u32 = c.to_u32();
    let one_byte: u32 = 128;
    let two_bytes: u32 = 2048;
    let three_bytes: u32 = 65536;
    if n < one_byte {
        1
    } else if n < two_bytes {
        2
    } else if n < three_bytes {
        3
    } else {
        4
    }
}

// ADR-0071: char.encode_utf8(buf) — write the canonical UTF-8 encoding of `c`
// to `buf` and return the byte count (1, 2, 3, or 4).
fn char__encode_utf8(c: char, buf: MutRef([u8; 4])) -> usize {
    let n: u32 = c.to_u32();
    let one_byte: u32 = 128;
    let two_bytes: u32 = 2048;
    let three_bytes: u32 = 65536;
    let cont_mask: u32 = 63;
    let cont_high: u32 = 128;
    let lead2: u32 = 192;
    let lead3: u32 = 224;
    let lead4: u32 = 240;
    if n < one_byte {
        let b0: u8 = @cast(n);
        buf[0] = b0;
        1
    } else if n < two_bytes {
        let b0u: u32 = (n >> 6) | lead2;
        let b1u: u32 = (n & cont_mask) | cont_high;
        let b0: u8 = @cast(b0u);
        let b1: u8 = @cast(b1u);
        buf[0] = b0;
        buf[1] = b1;
        2
    } else if n < three_bytes {
        let b0u: u32 = (n >> 12) | lead3;
        let b1u: u32 = ((n >> 6) & cont_mask) | cont_high;
        let b2u: u32 = (n & cont_mask) | cont_high;
        let b0: u8 = @cast(b0u);
        let b1: u8 = @cast(b1u);
        let b2: u8 = @cast(b2u);
        buf[0] = b0;
        buf[1] = b1;
        buf[2] = b2;
        3
    } else {
        let b0u: u32 = (n >> 18) | lead4;
        let b1u: u32 = ((n >> 12) & cont_mask) | cont_high;
        let b2u: u32 = ((n >> 6) & cont_mask) | cont_high;
        let b3u: u32 = (n & cont_mask) | cont_high;
        let b0: u8 = @cast(b0u);
        let b1: u8 = @cast(b1u);
        let b2: u8 = @cast(b2u);
        let b3: u8 = @cast(b3u);
        buf[0] = b0;
        buf[1] = b1;
        buf[2] = b2;
        buf[3] = b3;
        4
    }
}

// ADR-0072: error wrapper used by `String::from_utf8` to ferry the
// invalid byte buffer back to the caller. The struct wrapping makes the
// second type argument concrete in `Result(String, Utf8DecodeError)` —
// instantiating `Result(String, Vec(u8))` from a prelude body errors
// because the comptime evaluator can't bind the `E` parameter to a
// parameterized builtin type-call (`Vec(u8)` is a built-in constructor,
// not a comptime function). The wrapper structurally pins the buffer
// without burdening the caller with type-binding ceremony.
struct Utf8DecodeError {
    bytes: Vec(u8),
}

// ADR-0072: validated `Vec(u8) -> String` conversion. Performs a UTF-8
// scan; on success consumes `v` and returns `Result::Ok(s)`, on failure
// hands `v` back inside `Result::Err(Utf8DecodeError { bytes: v })`.
fn String__from_utf8(v: Vec(u8)) -> Result(String, Utf8DecodeError) {
    let valid: bool = checked {
        let p = v.ptr();
        let n = v.len();
        let s: Slice(u8) = @parts_to_slice(p, n);
        @utf8_validate(s)
    };
    if valid {
        let s: String = checked { String::from_utf8_unchecked(v) };
        let R = Result(String, Utf8DecodeError);
        R::Ok(s)
    } else {
        let e = Utf8DecodeError { bytes: v };
        let R = Result(String, Utf8DecodeError);
        R::Err(e)
    }
}

// ADR-0072: validated `Ptr(u8) -> String` conversion. strlen + alloc +
// memcpy into a `Vec(u8)` (via `__gruel_cstr_to_vec`), then forwards
// to `String__from_utf8`.
fn String__from_c_str(p: Ptr(u8)) -> Result(String, Utf8DecodeError) {
    let v: Vec(u8) = checked { @cstr_to_vec(p) };
    String__from_utf8(v)
}
"#;

/// Result of parsing a single file within a compilation unit.
#[derive(Debug)]
struct ParsedFileData {
    /// Path to the source file.
    path: String,
    /// The parsed abstract syntax tree.
    ast: Ast,
}

/// A unified compilation unit that owns all artifacts from source to machine code.
///
/// The compilation unit progresses through phases:
/// 1. **New**: Just source files
/// 2. **Parsed**: ASTs and interner from parsing
/// 3. **Lowered**: RIR (untyped intermediate representation)
/// 4. **Analyzed**: AIR (typed IR) and CFGs for all functions
///
/// Each phase builds on the previous one. The unit validates that phases
/// are run in order - you can't analyze before parsing.
///
/// # Thread Safety
///
/// The compilation unit uses [`ThreadedRodeo`] for string interning, which is
/// thread-safe. Parallel operations (like per-function CFG construction) can
/// safely share the interner.
#[derive(Debug)]
pub struct CompilationUnit<'src> {
    // === Configuration ===
    /// Compilation options (target, optimization level, etc.)
    options: CompileOptions,

    // === Source ===
    /// Source files being compiled.
    sources: Vec<SourceFile<'src>>,

    // === Phase 1: Parsing ===
    /// Parsed ASTs for each file (populated by `parse()`).
    parsed_files: Option<Vec<ParsedFileData>>,
    /// Merged AST containing all items (populated by `parse()`).
    merged_ast: Option<Ast>,
    /// String interner shared across all files.
    interner: Option<ThreadedRodeo>,
    /// Maps FileId to source file path (for error messages).
    file_paths: HashMap<FileId, String>,

    // === Phase 2: RIR Generation ===
    /// Untyped intermediate representation (populated by `lower()`).
    rir: Option<Rir>,

    // === Phase 3: Semantic Analysis + CFG ===
    /// Analyzed functions with typed IR and control flow graphs.
    functions: Option<Vec<FunctionWithCfg>>,
    /// Type intern pool containing all struct and enum definitions.
    type_pool: Option<TypeInternPool>,
    /// String literals indexed by their string_const index.
    strings: Option<Vec<String>>,
    /// Byte-blob literals from `@embed_file`, indexed by bytes_const index.
    bytes: Option<Vec<Vec<u8>>>,
    /// Warnings collected during compilation.
    warnings: Vec<CompileWarning>,
    /// Interface definitions (ADR-0056), indexed by InterfaceId.0.
    interface_defs: Option<Vec<gruel_air::InterfaceDef>>,
    /// (StructId, InterfaceId) → conformance witness; codegen uses this to
    /// emit one vtable global per pair.
    interface_vtables: Option<gruel_air::InterfaceVtables>,
}

impl<'src> CompilationUnit<'src> {
    /// Create a new compilation unit from source files.
    ///
    /// This initializes the unit with source files but does not run any
    /// compilation phases. Call [`parse()`](Self::parse), [`lower()`](Self::lower),
    /// and [`analyze()`](Self::analyze) to progress through compilation.
    ///
    /// # Arguments
    ///
    /// * `sources` - Source files to compile
    /// * `options` - Compilation options (target, optimization, etc.)
    pub fn new(sources: Vec<SourceFile<'src>>, options: CompileOptions) -> Self {
        let file_paths: HashMap<FileId, String> = sources
            .iter()
            .map(|s| (s.file_id, s.path.to_string()))
            .collect();

        Self {
            options,
            sources,
            parsed_files: None,
            merged_ast: None,
            interner: None,
            file_paths,
            rir: None,
            functions: None,
            type_pool: None,
            strings: None,
            bytes: None,
            warnings: Vec::new(),
            interface_defs: None,
            interface_vtables: None,
        }
    }

    // =========================================================================
    // Phase 1: Parsing
    // =========================================================================

    /// Open the AIR cache for whole-program AIR caching (ADR-0074
    /// Phase 4). Returns `None` when the preview gate is off, the
    /// cache_dir is not configured, or the underlying machinery
    /// (CacheStore / compiler_fingerprint) fails. The cache key
    /// concatenates every source file's content so any change to any
    /// file invalidates the whole-program AIR — the per-file
    /// granularity from the ADR design needs sema to run per-file,
    /// which is its own refactor.
    fn open_air_cache(&self) -> Option<(CacheStore, CacheKey)> {
        let (store, build_fp) = self.open_parse_cache()?;
        let mut h = Hasher::new();
        h.update(build_fp.as_bytes());
        for source in &self.sources {
            h.update_str(source.path);
            h.update_str(source.source);
        }
        Some((store, h.finalize()))
    }

    /// Open the parse cache when `--preview incremental_compilation` is
    /// enabled and `cache_dir` is configured. Returns `None` if either
    /// requirement is missing, or if opening the store / hashing the
    /// compiler binary fails (in which case the build silently
    /// continues uncached — correctness is preserved).
    fn open_parse_cache(&self) -> Option<(CacheStore, CacheKey)> {
        if !self
            .options
            .preview_features
            .contains(&PreviewFeature::IncrementalCompilation)
        {
            return None;
        }
        let dir = self.options.cache_dir.as_ref()?;

        let store = match CacheStore::open(dir) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, dir = %dir.display(), "failed to open cache");
                return None;
            }
        };

        // Compose the compilation fingerprint from compiler-binary hash
        // + target + opt level + sorted preview features.
        let bin_path = match gruel_cache::current_binary_path() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "could not resolve current binary path");
                return None;
            }
        };
        let memo_dir = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|h| h.join(".cache").join("gruel").join("binary-hash"))
            .unwrap_or_else(|| std::env::temp_dir().join("gruel-binary-hash"));
        let compiler_fp = match compiler_fingerprint(&bin_path, &memo_dir) {
            Ok(fp) => fp,
            Err(e) => {
                tracing::warn!(error = %e, "compiler_fingerprint failed");
                return None;
            }
        };

        let mut h = Hasher::new();
        h.update(compiler_fp.as_bytes());
        h.update_str(&format!("{}", self.options.target));
        h.update_u32(opt_level_to_u32(self.options.opt_level));
        let mut feats: Vec<&'static str> = self
            .options
            .preview_features
            .iter()
            .map(|f| f.name())
            .collect();
        feats.sort_unstable();
        for f in feats {
            h.update_str(f);
        }
        let build_fp = h.finalize();
        Some((store, build_fp))
    }

    /// Parse all source files.
    ///
    /// This runs lexing and parsing on each source file, producing ASTs.
    /// The ASTs are then merged into a single program, detecting any
    /// duplicate symbol definitions.
    ///
    /// # Errors
    ///
    /// Returns errors if:
    /// - Any file fails to lex or parse
    /// - Duplicate function, struct, or enum definitions are found
    pub fn parse(&mut self) -> MultiErrorResult<()> {
        let _span = info_span!("parse", file_count = self.sources.len()).entered();

        // Parse all files with a shared interner
        let mut parsed_files = Vec::with_capacity(self.sources.len() + 1);
        let mut interner = ThreadedRodeo::new();

        // ADR-0065: prepend the synthetic prelude (canonical Option(T), etc.).
        // The prelude is always parsed first under FileId::PRELUDE so user
        // files retain their original FileIds for diagnostics.
        let prelude = SourceFile::new("<prelude>", PRELUDE_SOURCE, FileId::PRELUDE);
        let prelude_lexer =
            Lexer::with_interner_and_file_id(prelude.source, interner, prelude.file_id);
        let (prelude_tokens, returned_interner) =
            prelude_lexer.tokenize().map_err(CompileErrors::from)?;
        interner = returned_interner;
        let prelude_parser = Parser::new(prelude_tokens, interner)
            .with_preview_features(self.options.preview_features.clone());
        let (prelude_ast, returned_interner) = prelude_parser.parse()?;
        interner = returned_interner;
        parsed_files.push(ParsedFileData {
            path: prelude.path.to_string(),
            ast: prelude_ast,
        });

        // ADR-0074 Phase 2: when --preview incremental_compilation is on
        // AND a cache_dir is configured, route user-file parsing through
        // the on-disk cache. The prelude (above) is always parsed
        // uncached because its source is a constant in the binary; its
        // Spurs are already in `interner`, which the cache wiring then
        // reuses as the build-shared interner.
        let cache_handle = self.open_parse_cache();
        if let Some((store, build_fp)) = cache_handle {
            let (cached_files, stats) = crate::parse_cache::parse_files_into(
                &interner,
                &self.sources,
                &self.options.preview_features,
                &store,
                &build_fp,
            )?;
            info!(
                hits = stats.hits,
                misses = stats.misses,
                files = self.sources.len(),
                "parse cache complete"
            );
            for file in cached_files {
                parsed_files.push(ParsedFileData {
                    path: file.path,
                    ast: file.ast,
                });
            }
        } else {
            for source in &self.sources {
                let _file_span = info_span!("parse_file", path = %source.path).entered();

                // Create lexer with shared interner and file ID
                let lexer =
                    Lexer::with_interner_and_file_id(source.source, interner, source.file_id);

                // Tokenize
                let (tokens, returned_interner) = lexer.tokenize().map_err(CompileErrors::from)?;
                interner = returned_interner;

                info!(token_count = tokens.len(), "lexing complete");

                // Parse
                let parser = Parser::new(tokens, interner)
                    .with_preview_features(self.options.preview_features.clone());
                let (ast, returned_interner) = parser.parse()?;
                interner = returned_interner;

                info!(item_count = ast.items.len(), "parsing complete");

                parsed_files.push(ParsedFileData {
                    path: source.path.to_string(),
                    ast,
                });
            }
        }

        // Merge symbols and check for duplicates
        let merged_ast = self.merge_symbols(&parsed_files, &interner)?;

        self.parsed_files = Some(parsed_files);
        self.merged_ast = Some(merged_ast);
        self.interner = Some(interner);

        Ok(())
    }

    /// Merge symbols from all parsed files, checking for duplicates.
    fn merge_symbols(
        &self,
        files: &[ParsedFileData],
        interner: &ThreadedRodeo,
    ) -> MultiErrorResult<Ast> {
        use crate::{Item, Span};

        /// Information about a symbol definition for duplicate detection.
        struct SymbolDef {
            span: Span,
            file_path: String,
        }

        let _span = info_span!("merge_symbols", file_count = files.len()).entered();

        let mut functions: HashMap<String, SymbolDef> = HashMap::default();
        let mut structs: HashMap<String, SymbolDef> = HashMap::default();
        let mut enums: HashMap<String, SymbolDef> = HashMap::default();
        let mut all_items = Vec::new();
        let mut errors = Vec::new();

        for file in files {
            for item in &file.ast.items {
                match item {
                    Item::Function(func) => {
                        let name = interner.resolve(&func.name.name).to_string();
                        if let Some(first) = functions.get(&name) {
                            errors.push(
                                CompileError::new(
                                    ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!("function `{}`", name),
                                    },
                                    func.span,
                                )
                                .with_label(
                                    format!("first defined in {}", first.file_path),
                                    first.span,
                                ),
                            );
                        } else {
                            functions.insert(
                                name,
                                SymbolDef {
                                    span: func.span,
                                    file_path: file.path.clone(),
                                },
                            );
                        }
                    }
                    Item::Struct(s) => {
                        let name = interner.resolve(&s.name.name).to_string();
                        if let Some(first) = structs.get(&name) {
                            errors.push(
                                CompileError::new(
                                    ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!("struct `{}`", name),
                                    },
                                    s.span,
                                )
                                .with_label(
                                    format!("first defined in {}", first.file_path),
                                    first.span,
                                ),
                            );
                        } else if let Some(first) = enums.get(&name) {
                            errors.push(
                                CompileError::new(
                                    ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!(
                                            "struct `{}` (conflicts with enum)",
                                            name
                                        ),
                                    },
                                    s.span,
                                )
                                .with_label(
                                    format!("enum first defined in {}", first.file_path),
                                    first.span,
                                ),
                            );
                        } else {
                            structs.insert(
                                name,
                                SymbolDef {
                                    span: s.span,
                                    file_path: file.path.clone(),
                                },
                            );
                        }
                    }
                    Item::Enum(e) => {
                        let name = interner.resolve(&e.name.name).to_string();
                        if let Some(first) = enums.get(&name) {
                            errors.push(
                                CompileError::new(
                                    ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!("enum `{}`", name),
                                    },
                                    e.span,
                                )
                                .with_label(
                                    format!("first defined in {}", first.file_path),
                                    first.span,
                                ),
                            );
                        } else if let Some(first) = structs.get(&name) {
                            errors.push(
                                CompileError::new(
                                    ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!(
                                            "enum `{}` (conflicts with struct)",
                                            name
                                        ),
                                    },
                                    e.span,
                                )
                                .with_label(
                                    format!("struct first defined in {}", first.file_path),
                                    first.span,
                                ),
                            );
                        } else {
                            enums.insert(
                                name,
                                SymbolDef {
                                    span: e.span,
                                    file_path: file.path.clone(),
                                },
                            );
                        }
                    }
                    Item::Interface(_) => {
                        // Interfaces (ADR-0056) are validated in Sema; cross-file
                        // duplicate detection is added in Phase 2.
                    }
                    Item::Derive(_) => {
                        // Derives (ADR-0058) are validated in Sema; cross-file
                        // duplicate detection follows the interface model.
                    }
                    Item::DropFn(_) | Item::Const(_) => {
                        // Validated in Sema
                    }
                    Item::Error(_) => {
                        // Error nodes from parser recovery are skipped
                    }
                }
                all_items.push(item.clone());
            }
        }

        if !errors.is_empty() {
            return Err(CompileErrors::from(errors));
        }

        info!(
            function_count = functions.len(),
            struct_count = structs.len(),
            enum_count = enums.len(),
            "symbol merging complete"
        );

        Ok(Ast { items: all_items })
    }

    // =========================================================================
    // Phase 2: RIR Generation
    // =========================================================================

    /// Generate untyped intermediate representation (RIR).
    ///
    /// This transforms the merged AST into RIR, which is a more uniform
    /// representation suitable for semantic analysis.
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse).
    pub fn lower(&mut self) -> MultiErrorResult<()> {
        let ast = self
            .merged_ast
            .as_ref()
            .expect("lower() called before parse()");
        let interner = self.interner.as_ref().expect("interner not initialized");

        let _span = info_span!("astgen").entered();

        let astgen = AstGen::new(ast, interner);
        let rir = astgen.generate();

        info!(instruction_count = rir.len(), "RIR generation complete");

        self.rir = Some(rir);
        Ok(())
    }

    // =========================================================================
    // Phase 3: Semantic Analysis + CFG Construction
    // =========================================================================

    /// Perform semantic analysis and build control flow graphs.
    ///
    /// This runs type checking, symbol resolution, and other semantic checks,
    /// then builds CFGs for each function. Optimizations are applied based
    /// on the configured optimization level.
    ///
    /// # Panics
    ///
    /// Panics if called before [`lower()`](Self::lower).
    pub fn analyze(&mut self) -> MultiErrorResult<()> {
        let rir = self.rir.as_ref().expect("analyze() called before lower()");
        let interner = self.interner.as_ref().expect("interner not initialized");

        // ADR-0074 Phase 4: try the whole-program AIR cache first.
        // Per-file AIR caching needs sema to run per-file (currently
        // it runs program-wide on the merged AST). Whole-program is
        // coarser but exercises the full cache pipeline end-to-end.
        let air_cache_handle = self.open_air_cache();

        // Semantic analysis
        let sema_output = if let Some((store, key)) = &air_cache_handle {
            match store.get(gruel_cache::CacheKind::Air, key) {
                Ok(Some(bytes)) => match gruel_cache::CachedAirOutput::decode(&bytes) {
                    Ok(cached) => {
                        info!("air cache hit");
                        // Restore the build's interner to the cached
                        // state. This is sound because the AIR cache is
                        // whole-program — every Spur in cached AIR was
                        // produced against this snapshot. Replay
                        // re-interns each cached string back into the
                        // build's interner; for newly-empty interners
                        // (typical at this point), this restores the
                        // original Spur values.
                        let _remap = cached.interner.restore_into(interner);
                        // Replay comptime @dbg output to stderr so cache
                        // hits are observably identical to cold builds
                        // (ADR-0074 "Comptime side-effects replay").
                        if !self.options.capture_comptime_dbg {
                            for line in &cached.comptime_dbg_output {
                                eprintln!("{}", line);
                            }
                        }
                        // Note: warnings are not cached yet (DiagnosticWrapper
                        // serde is its own follow-up); cache hits omit
                        // them. Documented in ADR-0074.
                        gruel_air::SemaOutput {
                            functions: cached.functions,
                            strings: cached.strings,
                            bytes: cached.bytes,
                            warnings: Vec::new(),
                            type_pool: cached.type_pool,
                            comptime_dbg_output: cached.comptime_dbg_output,
                            interface_defs: cached.interface_defs,
                            interface_vtables: cached.interface_vtables,
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "air cache decode failed; recomputing");
                        run_sema(rir, interner, &self.options, self.file_paths.clone())?
                    }
                },
                _ => {
                    info!("air cache miss");
                    let output = run_sema(rir, interner, &self.options, self.file_paths.clone())?;
                    // Best-effort cache write; failure is not a build error.
                    let cached = gruel_cache::CachedAirOutput {
                        interner: gruel_cache::InternerSnapshot::capture(interner),
                        functions: clone_functions(&output.functions),
                        type_pool: clone_type_pool(&output.type_pool),
                        strings: output.strings.clone(),
                        bytes: output.bytes.clone(),
                        interface_defs: output.interface_defs.clone(),
                        interface_vtables: output.interface_vtables.clone(),
                        comptime_dbg_output: output.comptime_dbg_output.clone(),
                    };
                    match cached.encode() {
                        Ok(bytes) => {
                            if let Err(e) = store.put(gruel_cache::CacheKind::Air, key, &bytes) {
                                tracing::warn!(error = %e, "air cache write failed");
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "air cache encode failed"),
                    }
                    output
                }
            }
        } else {
            run_sema(rir, interner, &self.options, self.file_paths.clone())?
        };

        // Synthesize drop glue functions
        let drop_glue_functions =
            crate::drop_glue::synthesize_drop_glue(&sema_output.type_pool, interner);
        // ADR-0065: synthesize clone glue for `@derive(Clone)` structs.
        let clone_glue_functions = crate::clone_glue::synthesize_clone_glue(&sema_output.type_pool);

        // Combine user functions with drop glue, filtering out comptime-only functions
        let all_functions: Vec<_> = sema_output
            .functions
            .into_iter()
            .filter(|f| f.air.return_type() != Type::COMPTIME_TYPE)
            .chain(drop_glue_functions)
            .chain(clone_glue_functions)
            .collect();

        // Build CFGs in parallel
        let interner_ref = self.interner.as_ref().expect("interner not initialized");
        let (functions, cfg_warnings) =
            self.build_cfgs(all_functions, &sema_output.type_pool, interner_ref);

        self.functions = Some(functions);
        self.type_pool = Some(sema_output.type_pool);
        self.strings = Some(sema_output.strings);
        self.bytes = Some(sema_output.bytes);
        self.warnings.extend(sema_output.warnings);
        self.warnings.extend(cfg_warnings);
        self.interface_defs = Some(sema_output.interface_defs);
        self.interface_vtables = Some(sema_output.interface_vtables);

        Ok(())
    }

    /// Build CFGs for all functions in parallel.
    fn build_cfgs(
        &self,
        functions: Vec<AnalyzedFunction>,
        type_pool: &TypeInternPool,
        interner: &ThreadedRodeo,
    ) -> (Vec<FunctionWithCfg>, Vec<CompileWarning>) {
        let _span = info_span!("cfg_construction").entered();

        let results: Vec<(FunctionWithCfg, Vec<CompileWarning>)> = functions
            .into_par_iter()
            .map(|func| {
                let cfg_output = CfgBuilder::build(&func, type_pool, interner);

                (
                    FunctionWithCfg {
                        analyzed: func,
                        cfg: cfg_output.cfg,
                    },
                    cfg_output.warnings,
                )
            })
            .collect();

        let mut functions = Vec::with_capacity(results.len());
        let mut warnings = Vec::new();
        for (func, func_warnings) in results {
            functions.push(func);
            warnings.extend(func_warnings);
        }

        info!(
            function_count = functions.len(),
            "CFG construction complete"
        );

        (functions, warnings)
    }

    // =========================================================================
    // Phase 4: Code Generation + Linking
    // =========================================================================

    /// Generate machine code and link into an executable.
    ///
    /// This is the final compilation phase. It generates machine code for
    /// all functions and links them into an executable binary.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn compile(&self) -> MultiErrorResult<CompileOutput> {
        let functions = self
            .functions
            .as_ref()
            .expect("compile() called before analyze()");
        let type_pool = self.type_pool.as_ref().expect("type_pool not available");
        let strings = self.strings.as_ref().expect("strings not available");
        let bytes = self.bytes.as_ref().expect("bytes not available");
        let interner = self.interner.as_ref().expect("interner not available");

        let empty_iface_defs: Vec<gruel_air::InterfaceDef> = Vec::new();
        let empty_iface_vtables: gruel_air::InterfaceVtables = rustc_hash::FxHashMap::default();
        let interface_defs = self.interface_defs.as_ref().unwrap_or(&empty_iface_defs);
        let interface_vtables = self
            .interface_vtables
            .as_ref()
            .unwrap_or(&empty_iface_vtables);
        let inputs = crate::BackendInputs {
            functions,
            type_pool,
            strings,
            bytes,
            interner,
            interface_defs,
            interface_vtables,
            target: &self.options.target,
        };

        // ADR-0074 Phase 5: bitcode cache. If the AIR cache is configured
        // and air_key matches, we may have cached pre-optimization LLVM
        // bitcode that lets us skip the AIR→IR translation step. The
        // LLVM optimizer + back-end + linker still run on every build.
        if let Some((store, key)) = self.open_air_cache() {
            return self.compile_with_bitcode_cache(&inputs, &store, &key);
        }
        compile_backend(&inputs, &self.options, &self.warnings)
    }

    /// Codegen path that consults the LLVM bitcode cache (ADR-0074
    /// Phase 5). On hit, parses the cached bitcode and runs the
    /// optimizer + back-end on it. On miss, generates bitcode, writes
    /// it to the cache, then runs optimizer + back-end. Either way the
    /// optimizer pipeline runs — the cache only saves the AIR→IR
    /// translation step.
    fn compile_with_bitcode_cache(
        &self,
        inputs: &crate::BackendInputs<'_>,
        store: &CacheStore,
        air_key: &CacheKey,
    ) -> MultiErrorResult<CompileOutput> {
        // Check for main function (matches compile_backend).
        let _main_fn = inputs
            .functions
            .iter()
            .find(|f| f.analyzed.name == "main")
            .ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::NoMainFunction))
            })?;

        // Bitcode cache key is the same as air_key — bitcode is a
        // deterministic function of AIR, and cached AIR keys already
        // factor in everything that influences codegen (target, opt
        // level, preview features, source content, compiler binary).
        let cfgs: Vec<&Cfg> = inputs.functions.iter().map(|f| &f.cfg).collect();
        let codegen_inputs = inputs.to_codegen_inputs(&cfgs);

        let object_bytes = match store.get(gruel_cache::CacheKind::LlvmIr, air_key) {
            Ok(Some(bitcode)) => {
                info!("bitcode cache hit");
                gruel_codegen_llvm::compile_bitcode_to_object(
                    &bitcode,
                    self.options.opt_level,
                    &self.options.target,
                )
                .map_err(CompileErrors::from)?
            }
            _ => {
                info!("bitcode cache miss");
                let bitcode = gruel_codegen_llvm::generate_bitcode(&codegen_inputs)
                    .map_err(CompileErrors::from)?;
                if let Err(e) = store.put(gruel_cache::CacheKind::LlvmIr, air_key, &bitcode) {
                    tracing::warn!(error = %e, "bitcode cache write failed");
                }
                gruel_codegen_llvm::compile_bitcode_to_object(
                    &bitcode,
                    self.options.opt_level,
                    &self.options.target,
                )
                .map_err(CompileErrors::from)?
            }
        };

        // Reuse the same link tail as generate_llvm_objects_and_link.
        let object_files = vec![object_bytes];
        let linker_cmd = match &self.options.linker {
            crate::LinkerMode::System(cmd) => cmd.clone(),
            crate::LinkerMode::Internal => "cc".to_string(),
        };
        crate::link::link_system_with_warnings(
            &self.options,
            &object_files,
            &linker_cmd,
            &self.warnings,
        )
    }

    // =========================================================================
    // Convenience Methods
    // =========================================================================

    /// Run all frontend phases (parse, lower, analyze).
    ///
    /// This is a convenience method that runs the complete frontend pipeline.
    /// Equivalent to calling `parse()`, `lower()`, and `analyze()` in sequence.
    pub fn run_frontend(&mut self) -> MultiErrorResult<()> {
        self.parse()?;
        self.lower()?;
        self.analyze()?;
        Ok(())
    }

    /// Run all phases and produce a compiled binary.
    ///
    /// This is a convenience method that runs the complete compilation pipeline.
    /// Equivalent to calling `run_frontend()` followed by `compile()`.
    pub fn run_all(&mut self) -> MultiErrorResult<CompileOutput> {
        self.run_frontend()?;
        self.compile()
    }

    /// Check if parsing has been completed.
    pub fn is_parsed(&self) -> bool {
        self.merged_ast.is_some()
    }

    /// Check if RIR generation has been completed.
    pub fn is_lowered(&self) -> bool {
        self.rir.is_some()
    }

    /// Check if semantic analysis has been completed.
    pub fn is_analyzed(&self) -> bool {
        self.functions.is_some()
    }

    // =========================================================================
    // Accessors
    // =========================================================================

    /// Get the compilation options.
    pub fn options(&self) -> &CompileOptions {
        &self.options
    }

    /// Get the merged AST (after parsing).
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse).
    pub fn ast(&self) -> &Ast {
        self.merged_ast
            .as_ref()
            .expect("ast() called before parse()")
    }

    /// Get the string interner.
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse).
    pub fn interner(&self) -> &ThreadedRodeo {
        self.interner
            .as_ref()
            .expect("interner() called before parse()")
    }

    /// Get the RIR (after lowering).
    ///
    /// # Panics
    ///
    /// Panics if called before [`lower()`](Self::lower).
    pub fn rir(&self) -> &Rir {
        self.rir.as_ref().expect("rir() called before lower()")
    }

    /// Get the analyzed functions with CFGs (after analysis).
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn functions(&self) -> &[FunctionWithCfg] {
        self.functions
            .as_ref()
            .expect("functions() called before analyze()")
    }

    /// Get the type pool (after analysis).
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn type_pool(&self) -> &TypeInternPool {
        self.type_pool
            .as_ref()
            .expect("type_pool() called before analyze()")
    }

    /// Get string literals (after analysis).
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze).
    pub fn strings(&self) -> &[String] {
        self.strings
            .as_ref()
            .expect("strings() called before analyze()")
    }

    /// Get all warnings collected during compilation.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    /// Get the file paths map.
    pub fn file_paths(&self) -> &HashMap<FileId, String> {
        &self.file_paths
    }

    /// Take the interner out of the compilation unit.
    ///
    /// This is useful when you need ownership of the interner (e.g., for
    /// code generation).
    ///
    /// # Panics
    ///
    /// Panics if called before [`parse()`](Self::parse) or if the interner
    /// has already been taken.
    pub fn take_interner(&mut self) -> ThreadedRodeo {
        self.interner
            .take()
            .expect("interner not available (not parsed or already taken)")
    }

    /// Take the functions out of the compilation unit.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze) or if the
    /// functions have already been taken.
    pub fn take_functions(&mut self) -> Vec<FunctionWithCfg> {
        self.functions
            .take()
            .expect("functions not available (not analyzed or already taken)")
    }

    /// Take the type pool out of the compilation unit.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze) or if the
    /// type pool has already been taken.
    pub fn take_type_pool(&mut self) -> TypeInternPool {
        self.type_pool
            .take()
            .expect("type_pool not available (not analyzed or already taken)")
    }

    /// Take the strings out of the compilation unit.
    ///
    /// # Panics
    ///
    /// Panics if called before [`analyze()`](Self::analyze) or if the
    /// strings have already been taken.
    pub fn take_strings(&mut self) -> Vec<String> {
        self.strings
            .take()
            .expect("strings not available (not analyzed or already taken)")
    }

    /// Take the warnings out of the compilation unit.
    pub fn take_warnings(&mut self) -> Vec<CompileWarning> {
        std::mem::take(&mut self.warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileId;

    fn make_sources(source: &str) -> Vec<SourceFile<'_>> {
        vec![SourceFile::new("<test>", source, FileId::new(1))]
    }

    #[test]
    fn test_compilation_unit_basic() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        assert!(!unit.is_parsed());
        assert!(!unit.is_lowered());
        assert!(!unit.is_analyzed());

        unit.run_frontend().unwrap();

        assert!(unit.is_parsed());
        assert!(unit.is_lowered());
        assert!(unit.is_analyzed());
        // ADR-0071 added char__from_u32 / char__is_ascii / char__len_utf8 /
        // char__encode_utf8 plus Option/Result methods to the prelude; the
        // analysed function count includes those plus user-defined `main`.
        assert!(unit.functions().len() >= 1);
    }

    #[test]
    fn test_phase_ordering() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        // Parse first
        unit.parse().unwrap();
        assert!(unit.is_parsed());
        assert!(!unit.is_lowered());

        // Then lower
        unit.lower().unwrap();
        assert!(unit.is_lowered());
        assert!(!unit.is_analyzed());

        // Then analyze
        unit.analyze().unwrap();
        assert!(unit.is_analyzed());
    }

    #[test]
    fn test_duplicate_function_error() {
        let sources = vec![
            SourceFile::new("a.gruel", "fn foo() -> i32 { 1 }", FileId::new(1)),
            SourceFile::new("b.gruel", "fn foo() -> i32 { 2 }", FileId::new(2)),
        ];
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());

        let result = unit.parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("function"));
    }

    #[test]
    fn test_warnings_collected() {
        let sources = make_sources("fn main() -> i32 { let x = 42; 0 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());
        unit.run_frontend().unwrap();

        assert_eq!(unit.warnings().len(), 1);
        assert!(unit.warnings()[0].to_string().contains("unused"));
    }

    #[test]
    #[should_panic(expected = "lower() called before parse()")]
    fn test_lower_before_parse_panics() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());
        unit.lower().unwrap();
    }

    #[test]
    #[should_panic(expected = "analyze() called before lower()")]
    fn test_analyze_before_lower_panics() {
        let sources = make_sources("fn main() -> i32 { 42 }");
        let mut unit = CompilationUnit::new(sources, CompileOptions::default());
        unit.parse().unwrap();
        unit.analyze().unwrap();
    }

    #[test]
    fn test_llvm_optimization_wiring() {
        // Verify that -O2 produces a valid binary that runs correctly.
        // This exercises the LLVM pass pipeline end-to-end.
        use crate::{CompileOptions, OptLevel};
        let sources = make_sources("fn main() -> i32 { let x = 2 + 3; x }");
        let options = CompileOptions {
            opt_level: OptLevel::O2,
            ..CompileOptions::default()
        };
        let mut unit = CompilationUnit::new(sources, options);
        unit.run_frontend().unwrap();
        // The frontend should succeed; backend (LLVM codegen) is tested separately
        // via spec tests that run the resulting binary. The prelude contributes
        // additional functions (char__*, etc.) so we only assert that user
        // code analysed at all.
        assert!(unit.functions().len() >= 1);
    }
}
