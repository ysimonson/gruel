//! Gruel compiler driver.
//!
//! This crate orchestrates the compilation pipeline:
//! Source -> Lexer -> Parser -> AstGen -> Sema -> CodeGen -> ELF
//!
//! It re-exports types from the component crates for convenience.
//!
//! # Diagnostic Formatting
//!
//! The [`MultiFileFormatter`] provides a clean API for formatting errors and warnings:
//!
//! ```ignore
//! use gruel_compiler::{MultiFileFormatter, SourceInfo, FileId};
//!
//! let sources = vec![(FileId::new(1), SourceInfo::new(&source, "example.gruel"))];
//! let formatter = MultiFileFormatter::new(sources);
//!
//! // Format an error
//! let error_output = formatter.format_error(&error);
//! eprintln!("{}", error_output);
//! ```
//!
//! # Tracing
//!
//! This crate is instrumented with `tracing` spans for performance analysis.
//! Use `--log-level info` or `--time-passes` to see timing information.

mod diagnostic;
mod drop_glue;
mod unit;

pub use unit::CompilationUnit;

use rayon::prelude::*;
use tracing::{info, info_span};

pub use diagnostic::{
    ColorChoice, JsonDiagnostic, JsonSpan, JsonSuggestion, MultiFileFormatter,
    MultiFileJsonFormatter, SourceInfo,
};

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// Error Helper Functions
// ============================================================================

/// Convert an I/O result into a `CompileResult` with a contextual message.
///
/// This helper wraps `std::io::Error` with a descriptive message explaining
/// what operation failed.
///
/// # Example
/// ```ignore
/// std::fs::create_dir_all(&path).map_err(|e| io_link_error("failed to create temp directory", e))?;
/// ```
fn io_link_error(context: &str, err: std::io::Error) -> CompileError {
    CompileError::without_span(ErrorKind::LinkError(format!("{}: {}", context, err)))
}

/// Counter for generating unique temp directory names.
static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory for linking that automatically cleans up on drop.
///
/// This struct manages the creation of a unique temporary directory for the
/// linking process and automatically removes it when dropped (whether via
/// normal completion or early error return).
struct TempLinkDir {
    /// Path to the temporary directory.
    path: PathBuf,
    /// Paths to the object files written to the directory.
    obj_paths: Vec<PathBuf>,
    /// Path to the runtime archive in the directory.
    runtime_path: PathBuf,
    /// Path where the linked executable will be written.
    output_path: PathBuf,
}

impl TempLinkDir {
    /// Create a new temporary directory for linking.
    ///
    /// Creates a unique directory in the system temp directory with the
    /// format `gruel-<pid>-<counter>` to ensure uniqueness even in parallel
    /// test execution.
    fn new() -> CompileResult<Self> {
        let unique_id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("gruel-{}-{}", std::process::id(), unique_id));
        std::fs::create_dir_all(&path)
            .map_err(|e| io_link_error("failed to create temp directory", e))?;

        let runtime_path = path.join("libgruel_runtime.a");
        let output_path = path.join("output");

        Ok(Self {
            path,
            obj_paths: Vec::new(),
            runtime_path,
            output_path,
        })
    }

    /// Write object files to the temporary directory.
    ///
    /// Each object file is written to a file named `obj{N}.o` where N is
    /// the index. The paths are stored in `self.obj_paths`.
    fn write_object_files(&mut self, object_files: &[Vec<u8>]) -> CompileResult<()> {
        for (i, obj_bytes) in object_files.iter().enumerate() {
            let obj_path = self.path.join(format!("obj{}.o", i));
            let mut file = std::fs::File::create(&obj_path)
                .map_err(|e| io_link_error("failed to create temp object file", e))?;
            file.write_all(obj_bytes)
                .map_err(|e| io_link_error("failed to write temp object file", e))?;
            self.obj_paths.push(obj_path);
        }
        Ok(())
    }

    /// Write the runtime archive to the temporary directory.
    fn write_runtime(&self, runtime_bytes: &[u8]) -> CompileResult<()> {
        std::fs::write(&self.runtime_path, runtime_bytes)
            .map_err(|e| io_link_error("failed to write runtime archive", e))
    }

    /// Read the linked executable from the output path.
    fn read_output(&self) -> CompileResult<Vec<u8>> {
        std::fs::read(&self.output_path)
            .map_err(|e| io_link_error("failed to read linked executable", e))
    }
}

impl Drop for TempLinkDir {
    fn drop(&mut self) {
        // Best-effort cleanup; ignore errors
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The gruel-runtime staticlib archive bytes, embedded at compile time.
/// This is linked into every Gruel executable.
static RUNTIME_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libgruel_runtime.a"));

// Re-export commonly used types
pub use gruel_air::{Air, AnalyzedFunction, Sema, SemaOutput, StructDef, Type, TypeInternPool};
pub use gruel_cfg::{Cfg, CfgBuilder, CfgOutput, OptLevel};
pub use gruel_error::{
    Applicability, CompileError, CompileErrors, CompileResult, CompileWarning, Diagnostic,
    ErrorCode, ErrorKind, MultiErrorResult, PreviewFeature, PreviewFeatures, Suggestion,
    WarningKind,
};
pub use gruel_lexer::{Lexer, Token, TokenKind};
pub use gruel_parser::{Ast, Expr, Function, Item, Parser};
pub use gruel_rir::{AstGen, Rir, RirPrinter};
pub use gruel_span::{FileId, Span};
pub use gruel_target::{Arch, Target};
pub use lasso::{Spur, ThreadedRodeo};

// ============================================================================
// Multi-file Compilation Types
// ============================================================================

/// A source file with its path and content.
///
/// Used for multi-file compilation to associate source content with file paths.
#[derive(Debug, Clone)]
pub struct SourceFile<'a> {
    /// Path to the source file (used for error messages).
    pub path: &'a str,
    /// Source code content.
    pub source: &'a str,
    /// Unique identifier for this file.
    pub file_id: FileId,
}

impl<'a> SourceFile<'a> {
    /// Create a new source file.
    pub fn new(path: &'a str, source: &'a str, file_id: FileId) -> Self {
        Self {
            path,
            source,
            file_id,
        }
    }
}

/// Result of parsing a single file.
///
/// Contains the AST and interner from parsing. The interner contains all
/// string literals and identifiers interned during lexing.
#[derive(Debug)]
pub struct ParsedFile {
    /// Path to the source file.
    pub path: String,
    /// File identifier for error reporting.
    pub file_id: FileId,
    /// The parsed abstract syntax tree.
    pub ast: Ast,
    /// String interner from lexing.
    pub interner: ThreadedRodeo,
}

/// Result of parsing all source files.
///
/// Contains all parsed files and a merged interner for use in later phases.
#[derive(Debug)]
pub struct ParsedProgram {
    /// Parsed files with their ASTs.
    pub files: Vec<ParsedFile>,
    /// Merged interner containing all symbols from all files.
    pub interner: ThreadedRodeo,
}

/// Parse multiple source files with a shared interner.
///
/// Each file is lexed and parsed sequentially with a single shared interner.
/// This ensures Spur values are consistent across all files, enabling cross-file
/// symbol resolution during semantic analysis.
///
/// Note: This uses sequential parsing rather than parallel to share the interner.
/// A future optimization could add parallel parsing with interner merging and
/// AST Spur remapping.
///
/// # Arguments
///
/// * `sources` - Slice of source files to parse
///
/// # Returns
///
/// A `ParsedProgram` containing all parsed files and the shared interner,
/// or errors from any file that failed to parse.
///
/// # Example
///
/// ```ignore
/// use gruel_compiler::{SourceFile, parse_all_files};
/// use gruel_span::FileId;
///
/// let sources = vec![
///     SourceFile::new("main.gruel", "fn main() -> i32 { 0 }", FileId::new(1)),
///     SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
/// ];
/// let program = parse_all_files(&sources)?;
/// ```
pub fn parse_all_files(sources: &[SourceFile<'_>]) -> MultiErrorResult<ParsedProgram> {
    parse_all_files_with_preview(sources, &PreviewFeatures::new())
}

/// Parse all files with an explicit set of preview features for the parser (ADR-0049).
pub fn parse_all_files_with_preview(
    sources: &[SourceFile<'_>],
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<ParsedProgram> {
    // Parse all files sequentially with a shared interner
    // This ensures Spur values are consistent across files for cross-file symbol resolution
    let mut parsed_files = Vec::with_capacity(sources.len());
    let mut interner = ThreadedRodeo::new();

    for source in sources {
        // Create lexer with shared interner and file ID for proper error reporting
        let lexer = Lexer::with_interner_and_file_id(source.source, interner, source.file_id);

        // Tokenize - propagate error immediately (interner is consumed)
        let (tokens, returned_interner) = lexer.tokenize().map_err(CompileErrors::from)?;
        interner = returned_interner;

        // Parse the tokens - propagate error immediately (interner is consumed)
        let parser = Parser::new(tokens, interner).with_preview_features(preview_features.clone());
        let (ast, returned_interner) = parser.parse()?;
        interner = returned_interner;

        parsed_files.push(ParsedFile {
            path: source.path.to_string(),
            file_id: source.file_id,
            ast,
            // Note: interner is shared, but we store a dummy here for API compatibility
            // The real interner is in the returned ParsedProgram
            interner: ThreadedRodeo::new(),
        });
    }

    Ok(ParsedProgram {
        files: parsed_files,
        interner,
    })
}

/// Result of merging symbols from multiple parsed files.
///
/// Contains a merged AST with all items from all files and the merged interner.
/// Used as input to RIR generation for multi-file compilation.
#[derive(Debug)]
pub struct MergedProgram {
    /// The merged AST containing items from all files.
    pub ast: Ast,
    /// Merged interner containing all symbols from all files.
    pub interner: ThreadedRodeo,
}

/// Result of validating and generating RIR from multiple parsed files.
///
/// This is the parallel-optimized path: RIR is generated per-file in parallel,
/// then merged. Used by `compile_multi_file_with_options`.
pub struct ValidatedProgram {
    /// The merged RIR from all files.
    pub rir: Rir,
    /// Merged interner containing all symbols from all files.
    pub interner: ThreadedRodeo,
    /// Maps FileId to source file path (for module resolution).
    pub file_paths: std::collections::HashMap<FileId, String>,
}

/// Information about a symbol definition for duplicate detection.
#[derive(Debug, Clone)]
struct SymbolDef {
    /// Span of the first definition.
    span: Span,
    /// File path where the first definition was found.
    file_path: String,
}

/// Merge symbols from all parsed files into a unified program.
///
/// This function:
/// 1. Combines all items from all files into a single merged AST
/// 2. Detects duplicate function, struct, and enum definitions
/// 3. Reports errors with both locations for any duplicates found
///
/// # Arguments
///
/// * `program` - The parsed program containing all files
///
/// # Returns
///
/// A `MergedProgram` ready for RIR generation, or errors if duplicates are found.
///
/// # Example
///
/// ```ignore
/// use gruel_compiler::{parse_all_files, merge_symbols, SourceFile};
/// use gruel_span::FileId;
///
/// let sources = vec![
///     SourceFile::new("main.gruel", "fn main() -> i32 { helper() }", FileId::new(1)),
///     SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
/// ];
/// let parsed = parse_all_files(&sources)?;
/// let merged = merge_symbols(parsed)?;
/// // merged.ast now contains both functions
/// ```
pub fn merge_symbols(program: ParsedProgram) -> MultiErrorResult<MergedProgram> {
    use std::collections::HashMap;

    let _span = info_span!("merge_symbols", file_count = program.files.len()).entered();

    // Track seen symbols for duplicate detection.
    // Key: symbol name (resolved string), Value: first definition info
    let mut functions: HashMap<String, SymbolDef> = HashMap::new();
    let mut structs: HashMap<String, SymbolDef> = HashMap::new();
    let mut enums: HashMap<String, SymbolDef> = HashMap::new();

    // Collect all items and detect duplicates
    let mut all_items = Vec::new();
    let mut errors: Vec<CompileError> = Vec::new();

    // Use the shared interner for resolving all symbol names
    let interner = &program.interner;

    for file in &program.files {
        for item in &file.ast.items {
            match item {
                Item::Function(func) => {
                    // Use shared interner for consistent Spur resolution
                    let name = interner.resolve(&func.name.name).to_string();
                    if let Some(first) = functions.get(&name) {
                        // Duplicate function definition
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("function `{}`", name),
                            },
                            func.span,
                        )
                        .with_label(format!("first defined in {}", first.file_path), first.span);
                        errors.push(err);
                    } else {
                        functions.insert(
                            name.clone(),
                            SymbolDef {
                                span: func.span,
                                file_path: file.path.clone(),
                            },
                        );
                    }
                }
                Item::Struct(s) => {
                    // Use shared interner for consistent Spur resolution
                    let name = interner.resolve(&s.name.name).to_string();
                    if let Some(first) = structs.get(&name) {
                        // Duplicate struct definition
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("struct `{}`", name),
                            },
                            s.span,
                        )
                        .with_label(format!("first defined in {}", first.file_path), first.span);
                        errors.push(err);
                    } else if let Some(first) = enums.get(&name) {
                        // Struct name conflicts with enum
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("struct `{}` (conflicts with enum)", name),
                            },
                            s.span,
                        )
                        .with_label(
                            format!("enum first defined in {}", first.file_path),
                            first.span,
                        );
                        errors.push(err);
                    } else {
                        structs.insert(
                            name.clone(),
                            SymbolDef {
                                span: s.span,
                                file_path: file.path.clone(),
                            },
                        );
                    }
                }
                Item::Enum(e) => {
                    // Use shared interner for consistent Spur resolution
                    let name = interner.resolve(&e.name.name).to_string();
                    if let Some(first) = enums.get(&name) {
                        // Duplicate enum definition
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("enum `{}`", name),
                            },
                            e.span,
                        )
                        .with_label(format!("first defined in {}", first.file_path), first.span);
                        errors.push(err);
                    } else if let Some(first) = structs.get(&name) {
                        // Enum name conflicts with struct
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("enum `{}` (conflicts with struct)", name),
                            },
                            e.span,
                        )
                        .with_label(
                            format!("struct first defined in {}", first.file_path),
                            first.span,
                        );
                        errors.push(err);
                    } else {
                        enums.insert(
                            name.clone(),
                            SymbolDef {
                                span: e.span,
                                file_path: file.path.clone(),
                            },
                        );
                    }
                }
                Item::Interface(_) => {
                    // Interfaces (ADR-0056) are validated in Sema. Cross-file
                    // duplicate detection is added when interfaces become a
                    // namespace alongside structs/enums in Phase 2.
                }
                Item::Derive(_) => {
                    // Derives (ADR-0058) are validated in Sema; cross-file
                    // duplicate detection follows the interface model.
                }
                Item::DropFn(_) | Item::Const(_) => {
                    // Drop fns and const declarations are validated in Sema, not here.
                    // Const declarations are checked for duplicates in the declarations phase.
                }
                Item::Error(_) => {
                    // Error nodes from parser recovery are skipped - errors were already reported
                }
            }
            all_items.push(item.clone());
        }
    }

    // If there are any duplicate definitions, return all errors
    if !errors.is_empty() {
        return Err(CompileErrors::from(errors));
    }

    info!(
        function_count = functions.len(),
        struct_count = structs.len(),
        enum_count = enums.len(),
        "symbol merging complete"
    );

    Ok(MergedProgram {
        ast: Ast { items: all_items },
        interner: program.interner,
    })
}

/// Validate symbols and generate RIR in parallel for multi-file compilation.
///
/// This is the optimized path for multi-file compilation:
/// 1. Validates that there are no duplicate symbol definitions across files
/// 2. Generates RIR for each file in parallel using Rayon
/// 3. Merges the per-file RIRs into a single RIR with renumbered references
///
/// This is more efficient than the sequential path for projects with many files,
/// as RIR generation is embarrassingly parallel (no cross-file dependencies
/// at the RIR level).
///
/// # Arguments
///
/// * `program` - The parsed program containing all files and shared interner
///
/// # Returns
///
/// A `ValidatedProgram` containing the merged RIR, or errors if duplicates are found.
pub fn validate_and_generate_rir_parallel(
    program: ParsedProgram,
) -> MultiErrorResult<ValidatedProgram> {
    use std::collections::HashMap;

    let _span = info_span!(
        "validate_and_generate_rir",
        file_count = program.files.len()
    )
    .entered();

    // Step 0: Build file_id -> path mapping for module resolution
    let file_paths: HashMap<FileId, String> = program
        .files
        .iter()
        .map(|f| (f.file_id, f.path.clone()))
        .collect();

    // Step 1: Validate symbols for duplicates (same logic as merge_symbols)
    let mut functions: HashMap<String, SymbolDef> = HashMap::new();
    let mut structs: HashMap<String, SymbolDef> = HashMap::new();
    let mut enums: HashMap<String, SymbolDef> = HashMap::new();
    let mut errors: Vec<CompileError> = Vec::new();

    let interner = &program.interner;

    for file in &program.files {
        for item in &file.ast.items {
            match item {
                Item::Function(func) => {
                    let name = interner.resolve(&func.name.name).to_string();
                    if let Some(first) = functions.get(&name) {
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("function `{}`", name),
                            },
                            func.span,
                        )
                        .with_label(format!("first defined in {}", first.file_path), first.span);
                        errors.push(err);
                    } else {
                        functions.insert(
                            name.clone(),
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
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("struct `{}`", name),
                            },
                            s.span,
                        )
                        .with_label(format!("first defined in {}", first.file_path), first.span);
                        errors.push(err);
                    } else if let Some(first) = enums.get(&name) {
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("struct `{}` (conflicts with enum)", name),
                            },
                            s.span,
                        )
                        .with_label(
                            format!("enum first defined in {}", first.file_path),
                            first.span,
                        );
                        errors.push(err);
                    } else {
                        structs.insert(
                            name.clone(),
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
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("enum `{}`", name),
                            },
                            e.span,
                        )
                        .with_label(format!("first defined in {}", first.file_path), first.span);
                        errors.push(err);
                    } else if let Some(first) = structs.get(&name) {
                        let err = CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: format!("enum `{}` (conflicts with struct)", name),
                            },
                            e.span,
                        )
                        .with_label(
                            format!("struct first defined in {}", first.file_path),
                            first.span,
                        );
                        errors.push(err);
                    } else {
                        enums.insert(
                            name.clone(),
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
        }
    }

    if !errors.is_empty() {
        return Err(CompileErrors::from(errors));
    }

    info!(
        function_count = functions.len(),
        struct_count = structs.len(),
        enum_count = enums.len(),
        "symbol validation complete"
    );

    // Step 2: Generate RIR per-file in parallel
    let interner = program.interner;
    let rirs: Vec<Rir> = {
        let _span = info_span!("parallel_astgen").entered();

        program
            .files
            .par_iter()
            .map(|file| {
                let astgen = AstGen::new(&file.ast, &interner);
                astgen.generate()
            })
            .collect()
    };

    // Step 3: Merge RIRs
    let rir = {
        let _span = info_span!("merge_rirs", rir_count = rirs.len()).entered();
        Rir::merge(&rirs)
    };

    info!(instruction_count = rir.len(), "RIR generation complete");

    Ok(ValidatedProgram {
        rir,
        interner,
        file_paths,
    })
}

/// Which linker to use for the final linking phase.
///
/// The Gruel compiler can either use its built-in ELF linker or delegate to
/// an external system linker like `clang`, `gcc`, or `ld`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinkerMode {
    /// Use the internal linker (default).
    #[default]
    Internal,
    /// Use an external system linker (e.g., "clang", "ld", "gcc").
    System(String),
}

/// Configuration options for compilation.
///
/// Controls target architecture, linker selection, optimization level, and feature flags.
///
/// # Example
///
/// ```ignore
/// let options = CompileOptions {
///     target: Target::host(),
///     linker: LinkerMode::System("cc".to_string()),
///     opt_level: OptLevel::O1,
///     preview_features: PreviewFeatures::new(),
///     jobs: 0, // 0 = auto-detect
/// };
/// let output = compile_with_options(source, &options)?;
/// ```
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// The target architecture and OS.
    pub target: Target,
    /// Which linker to use.
    pub linker: LinkerMode,
    /// Optimization level.
    pub opt_level: OptLevel,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// Number of parallel jobs (0 = auto-detect, use all cores).
    pub jobs: usize,
    /// When true, suppress the on-the-fly stderr print of comptime `@dbg`
    /// output. The output is still collected on `SemaOutput.comptime_dbg_output`
    /// and a warning is still emitted for each call. Used by fuzz harnesses.
    pub capture_comptime_dbg: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host(),
            linker: LinkerMode::Internal,
            opt_level: OptLevel::default(),
            preview_features: PreviewFeatures::new(),
            jobs: 0,
            capture_comptime_dbg: false,
        }
    }
}

/// A function with its typed IR (AIR) and control flow graph (CFG).
///
/// This combines the output of semantic analysis with CFG construction.
#[derive(Debug)]
pub struct FunctionWithCfg {
    /// The analyzed function from semantic analysis.
    pub analyzed: AnalyzedFunction,
    /// The control flow graph built from the AIR.
    pub cfg: Cfg,
}

/// Intermediate compilation state after frontend processing.
///
/// This allows inspection of the IR at each stage, useful for
/// debugging and the `--emit` CLI flags.
pub struct CompileState {
    /// The abstract syntax tree.
    pub ast: Ast,
    /// String interner used during compilation.
    pub interner: ThreadedRodeo,
    /// The untyped IR (RIR).
    pub rir: Rir,
    /// Analyzed functions with typed IR and control flow graphs.
    pub functions: Vec<FunctionWithCfg>,
    /// Type intern pool containing all struct and enum definitions.
    pub type_pool: TypeInternPool,
    /// String literals indexed by their string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during compilation.
    pub warnings: Vec<CompileWarning>,
    /// Lines of `@dbg` output collected during comptime evaluation.
    pub comptime_dbg_output: Vec<String>,
    /// Interface definitions (ADR-0056), indexed by InterfaceId.0.
    pub interface_defs: Vec<gruel_air::InterfaceDef>,
    /// (StructId, InterfaceId) → conformance witness; codegen uses this to
    /// emit one vtable global per pair.
    pub interface_vtables: std::collections::HashMap<
        (gruel_air::StructId, gruel_air::InterfaceId),
        Vec<(gruel_air::StructId, lasso::Spur)>,
    >,
}

/// Output from successful compilation.
///
/// Contains the compiled executable binary and any warnings generated
/// during compilation. The binary format depends on the target platform
/// (ELF for Linux, Mach-O for macOS).
#[derive(Debug)]
pub struct CompileOutput {
    /// The compiled ELF binary.
    pub elf: Vec<u8>,
    /// Warnings generated during compilation.
    pub warnings: Vec<CompileWarning>,
}

/// Compile source code through all frontend phases (up to but not including codegen).
///
/// This runs: lexing → parsing → AST to RIR → semantic analysis → CFG construction.
/// Returns the compile state which can be inspected for debugging.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
///
/// Uses default optimization level (O0) and no preview features. For custom options,
/// use [`compile_frontend_with_options`].
pub fn compile_frontend(source: &str) -> MultiErrorResult<CompileState> {
    compile_frontend_with_options(source, &PreviewFeatures::new())
}

/// Compile source code through all frontend phases.
///
/// This runs: lexing → parsing → AST to RIR → semantic analysis → CFG construction.
/// Returns the compile state which can be inspected for debugging.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile_frontend_with_options(
    source: &str,
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<CompileState> {
    compile_frontend_with_options_full(source, preview_features, false)
}

/// Like [`compile_frontend_with_options`], but additionally lets the caller
/// suppress the on-the-fly stderr print of comptime `@dbg` output. When
/// `suppress_comptime_dbg_print` is true, output is only collected in the
/// `SemaOutput.comptime_dbg_output` buffer. Used by fuzz harnesses that
/// consume the buffer directly and don't want noise on stderr.
pub fn compile_frontend_with_options_full(
    source: &str,
    preview_features: &PreviewFeatures,
    suppress_comptime_dbg_print: bool,
) -> MultiErrorResult<CompileState> {
    let _span = info_span!("frontend", source_bytes = source.len()).entered();

    // Lexing - errors here are fatal (can't continue without tokens)
    let (tokens, interner) = {
        let _span = info_span!("lexer").entered();
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from)?;
        info!(token_count = tokens.len(), "lexing complete");
        (tokens, interner)
    };

    // Parsing - errors here are fatal (can't continue without AST)
    let (ast, interner) = {
        let _span = info_span!("parser").entered();
        let parser = Parser::new(tokens, interner).with_preview_features(preview_features.clone());
        let (ast, interner) = parser.parse()?;
        info!(item_count = ast.items.len(), "parsing complete");
        (ast, interner)
    };

    compile_frontend_from_ast_with_options_full(
        ast,
        interner,
        preview_features,
        suppress_comptime_dbg_print,
    )
}

/// Compile from an already-parsed AST through all remaining frontend phases.
///
/// This runs: AST to RIR → semantic analysis → CFG construction.
/// Use this when you already have a parsed AST (e.g., for `--emit` modes that
/// need both AST output and later stage output without double-parsing).
///
/// Uses no preview features. For custom options, use [`compile_frontend_from_ast_with_options`].
pub fn compile_frontend_from_ast(
    ast: Ast,
    interner: ThreadedRodeo,
) -> MultiErrorResult<CompileState> {
    compile_frontend_from_ast_with_options(ast, interner, &PreviewFeatures::new())
}

/// Compile from an already-parsed AST through all remaining frontend phases.
///
/// This runs: AST to RIR → semantic analysis → CFG construction.
/// Use this when you already have a parsed AST (e.g., for `--emit` modes that
/// need both AST output and later stage output without double-parsing).
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile_frontend_from_ast_with_options(
    ast: Ast,
    interner: ThreadedRodeo,
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<CompileState> {
    compile_frontend_from_ast_with_options_full(ast, interner, preview_features, false)
}

/// Like [`compile_frontend_from_ast_with_options`], but additionally lets the
/// caller suppress the on-the-fly stderr print of comptime `@dbg` output.
pub fn compile_frontend_from_ast_with_options_full(
    ast: Ast,
    interner: ThreadedRodeo,
    preview_features: &PreviewFeatures,
    suppress_comptime_dbg_print: bool,
) -> MultiErrorResult<CompileState> {
    // AST to RIR (untyped IR)
    let (rir, interner) = {
        let _span = info_span!("astgen").entered();
        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();
        info!(instruction_count = rir.len(), "AST generation complete");
        (rir, interner)
    };

    // Semantic analysis (RIR to AIR) - this now collects multiple errors
    let sema_output = {
        let _span = info_span!("sema").entered();
        let mut sema = Sema::new(&rir, &interner, preview_features.clone());
        sema.set_suppress_comptime_dbg_print(suppress_comptime_dbg_print);
        let output = sema.analyze_all()?;
        info!(
            function_count = output.functions.len(),
            struct_count = output.type_pool.stats().struct_count,
            "semantic analysis complete"
        );
        output
    };

    // Synthesize drop glue functions for structs that need them
    let drop_glue_functions = drop_glue::synthesize_drop_glue(&sema_output.type_pool, &interner);

    // Combine user functions with synthesized drop glue functions
    // Filter out comptime-only functions (those returning `type`) as they don't generate runtime code
    let all_functions: Vec<_> = sema_output
        .functions
        .into_iter()
        .filter(|f| f.air.return_type() != Type::COMPTIME_TYPE)
        .chain(drop_glue_functions)
        .collect();

    // Build CFGs from AIR (one per function) in parallel, collecting warnings
    let (functions, warnings) = {
        let _span = info_span!("cfg_construction").entered();

        // Build CFGs in parallel - each function is independent
        let results: Vec<(FunctionWithCfg, Vec<CompileWarning>)> = all_functions
            .into_par_iter()
            .map(|func| {
                let cfg_output = CfgBuilder::build(&func, &sema_output.type_pool);

                (
                    FunctionWithCfg {
                        analyzed: func,
                        cfg: cfg_output.cfg,
                    },
                    cfg_output.warnings,
                )
            })
            .collect();

        // Unzip the results and collect all warnings
        let mut functions = Vec::with_capacity(results.len());
        let mut warnings = sema_output.warnings;
        for (func, func_warnings) in results {
            functions.push(func);
            warnings.extend(func_warnings);
        }

        info!(
            function_count = functions.len(),
            "CFG construction complete"
        );
        (functions, warnings)
    };

    Ok(CompileState {
        ast,
        interner,
        rir,
        functions,
        type_pool: sema_output.type_pool,
        strings: sema_output.strings,
        warnings,
        comptime_dbg_output: sema_output.comptime_dbg_output,
        interface_defs: sema_output.interface_defs,
        interface_vtables: sema_output.interface_vtables,
    })
}

/// Compile from already-generated RIR through remaining frontend phases.
///
/// This runs: semantic analysis → CFG construction → optimization.
/// This is the optimized path used by parallel multi-file compilation, where
/// RIR has already been generated per-file in parallel and merged.
///
/// # Arguments
///
/// * `rir` - The RIR (already merged if from multiple files)
/// * `interner` - The shared string interner
/// * `opt_level` - Optimization level
/// * `preview_features` - Enabled preview features
///
/// # Returns
///
/// A `CompileStateFromRir` containing the compilation state.
pub fn compile_frontend_from_rir_with_options(
    rir: Rir,
    interner: ThreadedRodeo,
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<CompileStateFromRir> {
    compile_frontend_from_rir_with_file_paths(
        rir,
        interner,
        preview_features,
        std::collections::HashMap::new(),
    )
}

/// Compile frontend from RIR with file paths for module resolution.
///
/// This is the full version that accepts file_id -> path mapping for
/// multi-file compilation with module imports.
pub fn compile_frontend_from_rir_with_file_paths(
    rir: Rir,
    interner: ThreadedRodeo,
    preview_features: &PreviewFeatures,
    file_paths: std::collections::HashMap<FileId, String>,
) -> MultiErrorResult<CompileStateFromRir> {
    // Semantic analysis (RIR to AIR)
    let sema_output = {
        let _span = info_span!("sema").entered();
        let mut sema = Sema::new(&rir, &interner, preview_features.clone());
        sema.set_file_paths(file_paths);
        let output = sema.analyze_all()?;
        info!(
            function_count = output.functions.len(),
            struct_count = output.type_pool.stats().struct_count,
            "semantic analysis complete"
        );
        output
    };

    // Synthesize drop glue functions for structs that need them
    let drop_glue_functions = drop_glue::synthesize_drop_glue(&sema_output.type_pool, &interner);

    // Combine user functions with synthesized drop glue functions
    // Filter out comptime-only functions (those returning `type`) as they don't generate runtime code
    let all_functions: Vec<_> = sema_output
        .functions
        .into_iter()
        .filter(|f| f.air.return_type() != Type::COMPTIME_TYPE)
        .chain(drop_glue_functions)
        .collect();

    // Build CFGs from AIR (one per function) in parallel, collecting warnings
    let (functions, warnings) = {
        let _span = info_span!("cfg_construction").entered();

        // Build CFGs in parallel - each function is independent
        let results: Vec<(FunctionWithCfg, Vec<CompileWarning>)> = all_functions
            .into_par_iter()
            .map(|func| {
                let cfg_output = CfgBuilder::build(&func, &sema_output.type_pool);

                (
                    FunctionWithCfg {
                        analyzed: func,
                        cfg: cfg_output.cfg,
                    },
                    cfg_output.warnings,
                )
            })
            .collect();

        // Unzip the results and collect all warnings
        let mut functions = Vec::with_capacity(results.len());
        let mut warnings = sema_output.warnings;
        for (func, func_warnings) in results {
            functions.push(func);
            warnings.extend(func_warnings);
        }

        info!(
            function_count = functions.len(),
            "CFG construction complete"
        );
        (functions, warnings)
    };

    Ok(CompileStateFromRir {
        interner,
        rir,
        functions,
        type_pool: sema_output.type_pool,
        strings: sema_output.strings,
        warnings,
        comptime_dbg_output: sema_output.comptime_dbg_output,
        interface_defs: sema_output.interface_defs,
        interface_vtables: sema_output.interface_vtables,
    })
}

/// Intermediate compilation state after frontend processing from RIR.
///
/// Similar to `CompileState` but without the AST (since we started from RIR directly
/// in the parallel compilation path).
pub struct CompileStateFromRir {
    /// String interner used during compilation.
    pub interner: ThreadedRodeo,
    /// The untyped IR (RIR).
    pub rir: Rir,
    /// Analyzed functions with typed IR and control flow graphs.
    pub functions: Vec<FunctionWithCfg>,
    /// Type intern pool containing all struct and enum definitions.
    pub type_pool: TypeInternPool,
    /// String literals indexed by their string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during compilation.
    pub warnings: Vec<CompileWarning>,
    /// Lines of `@dbg` output collected during comptime evaluation.
    pub comptime_dbg_output: Vec<String>,
    /// Interface definitions (ADR-0056), indexed by InterfaceId.0.
    pub interface_defs: Vec<gruel_air::InterfaceDef>,
    /// (StructId, InterfaceId) → conformance witness; codegen uses this to
    /// emit one vtable global per pair.
    pub interface_vtables: std::collections::HashMap<
        (gruel_air::StructId, gruel_air::InterfaceId),
        Vec<(gruel_air::StructId, lasso::Spur)>,
    >,
}

/// Compile source code to an ELF binary.
///
/// This is the main entry point for compilation.
/// Returns the ELF binary along with any warnings generated during compilation.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile(source: &str) -> MultiErrorResult<CompileOutput> {
    compile_with_options(source, &CompileOptions::default())
}

/// Compile source code to an ELF binary with the given options.
///
/// This allows specifying the target architecture, optimization level, and other compilation options.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile_with_options(
    source: &str,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    // Delegate to multi-file compilation with a single source file
    let sources = vec![SourceFile::new("<source>", source, FileId::new(1))];
    compile_multi_file_with_options(&sources, options)
}

/// Compile multiple source files to an ELF binary.
///
/// This is the main entry point for multi-file compilation. It:
/// 1. Parses all files in parallel
/// 2. Merges symbols into a unified program
/// 3. Performs semantic analysis across all files
/// 4. Generates code for the combined program
///
/// Cross-file references (function calls, struct/enum usage) are resolved during
/// semantic analysis since all symbols are visible in the merged program.
///
/// # Arguments
///
/// * `sources` - Slice of source files to compile
/// * `options` - Compilation options (target, linker, optimization level, etc.)
///
/// # Returns
///
/// A `CompileOutput` containing the ELF binary and any warnings,
/// or errors if compilation fails.
///
/// # Example
///
/// ```ignore
/// use gruel_compiler::{SourceFile, CompileOptions, compile_multi_file_with_options};
/// use gruel_span::FileId;
///
/// let sources = vec![
///     SourceFile::new("main.gruel", "fn main() -> i32 { helper() }", FileId::new(1)),
///     SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
/// ];
/// let options = CompileOptions::default();
/// let output = compile_multi_file_with_options(&sources, &options)?;
/// ```
pub fn compile_multi_file_with_options(
    sources: &[SourceFile<'_>],
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    // Configure Rayon's global thread pool based on the jobs setting.
    // This must happen before any parallel operations.
    // 0 means auto-detect (use all cores), which is Rayon's default.
    if options.jobs > 0 {
        // Ignore the error if the pool has already been initialized (e.g., in tests).
        // This is safe because we're just trying to set the thread count.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(options.jobs)
            .build_global();
    }

    let total_source_bytes: usize = sources.iter().map(|s| s.source.len()).sum();
    let _span = info_span!(
        "compile",
        target = %options.target,
        file_count = sources.len(),
        source_bytes = total_source_bytes
    )
    .entered();

    // Use CompilationUnit for the entire pipeline
    let mut unit = CompilationUnit::new(sources.to_vec(), options.clone());
    unit.run_all()
}

/// Link using an external system linker.
fn link_system_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "system", command = linker_cmd).entered();

    // Set up temporary directory with object files and runtime
    let mut temp_dir = TempLinkDir::new().map_err(CompileErrors::from)?;
    temp_dir
        .write_object_files(object_files)
        .map_err(CompileErrors::from)?;
    temp_dir
        .write_runtime(RUNTIME_BYTES)
        .map_err(CompileErrors::from)?;

    // Build the linker command
    let mut cmd = Command::new(linker_cmd);

    // Add target-specific linker flags.
    // We use -nostartfiles (not -nostdlib) because the runtime provides its own
    // _start/_main entry points but relies on libc for syscalls.
    if options.target.is_macho() {
        // macOS-specific flags
        cmd.arg("-nostartfiles");
        cmd.arg("-arch").arg("arm64");
        cmd.arg("-e").arg("__main");
    } else {
        // Linux/ELF-specific flags
        // Dynamic linking lets ld.so initialize libc (TLS, malloc, stdio)
        // before jumping to our _start. We only skip the C startup files
        // since the runtime provides its own _start entry point.
        cmd.arg("-nostartfiles");
    }

    cmd.arg("-o");
    cmd.arg(&temp_dir.output_path);

    // Add object files
    for path in &temp_dir.obj_paths {
        cmd.arg(path);
    }

    // Add the runtime library
    cmd.arg(&temp_dir.runtime_path);

    // macOS requires libSystem for syscalls
    if options.target.is_macho() {
        cmd.arg("-lSystem");
    }

    // Run the linker
    let output = cmd.output().map_err(|e| {
        CompileErrors::from(CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker '{}': {}",
            linker_cmd, e
        ))))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // temp_dir is dropped here, cleaning up automatically
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::LinkError(format!("linker '{}' failed: {}", linker_cmd, stderr)),
        )));
    }

    // Read the resulting executable
    let elf = temp_dir.read_output().map_err(CompileErrors::from)?;
    info!(
        object_count = object_files.len(),
        output_bytes = elf.len(),
        "linking complete"
    );

    // temp_dir is dropped here, cleaning up automatically
    Ok(CompileOutput {
        elf,
        warnings: warnings.to_vec(),
    })
}

// ============================================================================
// Unified Backend (Codegen + Linking)
// ============================================================================

/// Compile analyzed functions to a binary.
///
/// This is the unified backend that handles both architectures. It:
/// 1. Generates machine code for each function in parallel
/// 2. Creates object files with relocations
/// 3. Links them into an executable
///
/// This function is used by `CompilationUnit::compile()` and the legacy
/// compile functions.
#[allow(clippy::too_many_arguments)]
pub fn compile_backend(
    functions: &[FunctionWithCfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    interface_defs: &[gruel_air::InterfaceDef],
    interface_vtables: &std::collections::HashMap<
        (gruel_air::StructId, gruel_air::InterfaceId),
        Vec<(gruel_air::StructId, lasso::Spur)>,
    >,
    options: &CompileOptions,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    // Check for main function
    let _main_fn = functions
        .iter()
        .find(|f| f.analyzed.name == "main")
        .ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::NoMainFunction))
        })?;

    generate_llvm_objects_and_link(
        functions,
        type_pool,
        strings,
        interner,
        interface_defs,
        interface_vtables,
        options,
        warnings,
    )
}

/// Generate a single LLVM object file from all functions and link it.
///
/// Unlike the native backends, which produce one object file per function,
/// the LLVM backend compiles all functions into a single LLVM module and emits
/// one object file. Linking always uses the system linker because LLVM emits
/// platform-native object formats (ELF on Linux, Mach-O on macOS).
#[allow(clippy::too_many_arguments)]
fn generate_llvm_objects_and_link(
    functions: &[FunctionWithCfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    interface_defs: &[gruel_air::InterfaceDef],
    interface_vtables: &std::collections::HashMap<
        (gruel_air::StructId, gruel_air::InterfaceId),
        Vec<(gruel_air::StructId, lasso::Spur)>,
    >,
    options: &CompileOptions,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("codegen", backend = "llvm").entered();

    let cfgs: Vec<&Cfg> = functions.iter().map(|f| &f.cfg).collect();
    let object_bytes = gruel_codegen_llvm::generate(
        &cfgs,
        type_pool,
        strings,
        interner,
        interface_defs,
        interface_vtables,
        options.opt_level,
    )
    .map_err(CompileErrors::from)?;

    // LLVM produces a single object file; wrap it as a one-element slice.
    let object_files = vec![object_bytes];

    // The LLVM backend always uses the system linker. If the user specified
    // --linker internal, fall back to "cc" (the platform's default C compiler).
    let linker_cmd = match &options.linker {
        LinkerMode::System(cmd) => cmd.clone(),
        LinkerMode::Internal => "cc".to_string(),
    };
    link_system_with_warnings(options, &object_files, &linker_cmd, warnings)
}

/// Generate LLVM textual IR from analyzed functions.
///
/// Returns the LLVM IR in human-readable `.ll` format. Used by `--emit asm`
/// to produce inspectable IR in place of native assembly.
pub fn generate_llvm_ir(
    functions: &[FunctionWithCfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    interface_defs: &[gruel_air::InterfaceDef],
    interface_vtables: &std::collections::HashMap<
        (gruel_air::StructId, gruel_air::InterfaceId),
        Vec<(gruel_air::StructId, lasso::Spur)>,
    >,
    opt_level: OptLevel,
) -> CompileResult<String> {
    let cfgs: Vec<&Cfg> = functions.iter().map(|f| &f.cfg).collect();
    gruel_codegen_llvm::generate_ir(
        &cfgs,
        type_pool,
        strings,
        interner,
        interface_defs,
        interface_vtables,
        opt_level,
    )
}

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Output from semantic analysis (compile_to_air).
///
/// This struct provides access to the typed IR (AIR) for each function,
/// useful for testing semantic analysis without generating machine code.
#[derive(Debug)]
pub struct AirOutput {
    /// The abstract syntax tree.
    pub ast: Ast,
    /// String interner used during compilation.
    pub interner: ThreadedRodeo,
    /// The untyped IR (RIR).
    pub rir: Rir,
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// Type intern pool containing all struct and enum definitions.
    pub type_pool: TypeInternPool,
    /// String literals.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

/// Compile source code up to AIR (typed IR) without building CFG.
///
/// This is a test helper that runs: lexing → parsing → AST to RIR → semantic analysis.
/// It stops before CFG construction, making it fast for testing type checking
/// and semantic analysis.
///
/// # Example
///
/// ```ignore
/// use gruel_compiler::compile_to_air;
///
/// let result = compile_to_air("fn main() -> i32 { 1 + 2 * 3 }");
/// assert!(result.is_ok());
/// ```
pub fn compile_to_air(source: &str) -> MultiErrorResult<AirOutput> {
    // Lexing
    let lexer = Lexer::new(source);
    let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from)?;

    // Parsing
    let parser = Parser::new(tokens, interner);
    let (ast, interner) = parser.parse()?;

    // AST to RIR (untyped IR)
    let astgen = AstGen::new(&ast, &interner);
    let rir = astgen.generate();

    // Semantic analysis (RIR to AIR)
    let sema = Sema::new(&rir, &interner, PreviewFeatures::new());
    let sema_output = sema.analyze_all()?;

    Ok(AirOutput {
        ast,
        interner,
        rir,
        functions: sema_output.functions,
        type_pool: sema_output.type_pool,
        strings: sema_output.strings,
        warnings: sema_output.warnings,
    })
}

/// Compile source code up to CFG (control flow graph).
///
/// This is an alias for `compile_frontend` that provides a more intuitive name
/// for test code. It runs the full frontend pipeline:
/// lexing → parsing → AST to RIR → semantic analysis → CFG construction.
///
/// # Example
///
/// ```ignore
/// use gruel_compiler::compile_to_cfg;
///
/// let result = compile_to_cfg("fn main() -> i32 { if true { 1 } else { 2 } }");
/// assert!(result.is_ok());
/// let state = result.unwrap();
/// assert_eq!(state.functions.len(), 1);
/// ```
pub fn compile_to_cfg(source: &str) -> MultiErrorResult<CompileState> {
    compile_frontend(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_simple() {
        let output = compile("fn main() -> i32 { 42 }").unwrap();
        // Should produce a valid executable (ELF on Linux, Mach-O on macOS)
        let magic = &output.elf[0..4];
        let is_elf = magic == [0x7F, b'E', b'L', b'F'];
        let is_macho = magic == 0xFEEDFACF_u32.to_le_bytes();
        assert!(
            is_elf || is_macho,
            "should produce valid ELF or Mach-O binary"
        );
    }

    #[test]
    fn test_compile_no_main() {
        let result = compile("fn foo() -> i32 { 42 }");
        assert!(result.is_err());
    }

    #[test]
    fn test_unused_variable_warning() {
        let output = compile("fn main() -> i32 { let x = 42; 0 }").unwrap();
        assert_eq!(output.warnings.len(), 1);
        assert!(output.warnings[0].to_string().contains("unused variable"));
        assert!(output.warnings[0].to_string().contains("'x'"));
    }

    #[test]
    fn test_underscore_prefix_no_warning() {
        let output = compile("fn main() -> i32 { let _x = 42; 0 }").unwrap();
        assert_eq!(output.warnings.len(), 0);
    }

    #[test]
    fn test_used_variable_no_warning() {
        let output = compile("fn main() -> i32 { let x = 42; x }").unwrap();
        assert_eq!(output.warnings.len(), 0);
    }

    #[test]
    fn test_compile_frontend_includes_warnings() {
        let state = compile_frontend("fn main() -> i32 { let x = 42; 0 }").unwrap();
        assert_eq!(state.warnings.len(), 1);
        assert!(state.warnings[0].to_string().contains("unused variable"));
    }

    #[test]
    fn test_multiple_errors_collected() {
        // Test that errors from multiple functions are collected together
        // Use examples that both result in type mismatch errors
        // Note: Functions must be called from main() to be analyzed (lazy analysis)
        let source = r#"
            fn foo() -> i32 { true }
            fn bar() -> i32 { false }
            fn main() -> i32 { foo() + bar() }
        "#;
        let result = compile_frontend(source);
        let errors = match result {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        // Should have at least 2 errors (one from foo, one from bar)
        assert!(
            errors.len() >= 2,
            "expected at least 2 errors, got {}",
            errors.len()
        );

        // All errors should be type mismatches (returning bool where i32 expected)
        for error in errors.iter() {
            assert!(
                error.to_string().contains("type mismatch"),
                "expected type mismatch error, got: {}",
                error
            );
        }
    }

    #[test]
    fn test_multiple_errors_display() {
        // Use examples that both result in type mismatch errors
        // Note: Functions must be called from main() to be analyzed (lazy analysis)
        let source = r#"
            fn foo() -> i32 { true }
            fn bar() -> i32 { false }
            fn main() -> i32 { foo() + bar() }
        "#;
        let errors = match compile_frontend(source) {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        // Display should show both errors
        let display = errors.to_string();
        assert!(
            display.contains("type mismatch"),
            "display should contain error message"
        );
        if errors.len() > 1 {
            assert!(
                display.contains("more error"),
                "display should indicate more errors"
            );
        }
    }

    #[test]
    fn test_single_error_still_works() {
        // Single error should still be collected and returned properly
        let source = "fn main() -> i32 { true }";
        let errors = match compile_frontend(source) {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        assert_eq!(errors.len(), 1);
        assert!(
            errors
                .first()
                .unwrap()
                .to_string()
                .contains("type mismatch")
        );
    }

    // ========================================================================
    // Multi-file Symbol Merging Tests
    // ========================================================================

    #[test]
    fn test_merge_symbols_no_duplicates() {
        let sources = vec![
            SourceFile::new("main.gruel", "fn main() -> i32 { 0 }", FileId::new(1)),
            SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let merged = merge_symbols(parsed);
        assert!(merged.is_ok(), "merge should succeed with no duplicates");

        let program = merged.unwrap();
        assert_eq!(program.ast.items.len(), 2, "should have 2 items");
    }

    #[test]
    fn test_merge_symbols_duplicate_function() {
        let sources = vec![
            SourceFile::new("a.gruel", "fn foo() -> i32 { 1 }", FileId::new(1)),
            SourceFile::new("b.gruel", "fn foo() -> i32 { 2 }", FileId::new(2)),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let result = merge_symbols(parsed);
        assert!(result.is_err(), "merge should fail with duplicate function");

        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1, "should have 1 error");
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("function `foo`"),
            "error should mention the function name"
        );
    }

    #[test]
    fn test_merge_symbols_duplicate_struct() {
        let sources = vec![
            SourceFile::new(
                "a.gruel",
                "struct Point { x: i32 } fn main() -> i32 { 0 }",
                FileId::new(1),
            ),
            SourceFile::new("b.gruel", "struct Point { y: i32 }", FileId::new(2)),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let result = merge_symbols(parsed);
        assert!(result.is_err(), "merge should fail with duplicate struct");

        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1, "should have 1 error");
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("struct `Point`"),
            "error should mention the struct name"
        );
    }

    #[test]
    fn test_merge_symbols_duplicate_enum() {
        let sources = vec![
            SourceFile::new(
                "a.gruel",
                "enum Color { Red } fn main() -> i32 { 0 }",
                FileId::new(1),
            ),
            SourceFile::new("b.gruel", "enum Color { Blue }", FileId::new(2)),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let result = merge_symbols(parsed);
        assert!(result.is_err(), "merge should fail with duplicate enum");

        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1, "should have 1 error");
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("enum `Color`"),
            "error should mention the enum name"
        );
    }

    #[test]
    fn test_merge_symbols_struct_enum_conflict() {
        // Struct and enum with the same name should conflict
        let sources = vec![
            SourceFile::new(
                "a.gruel",
                "struct Foo { x: i32 } fn main() -> i32 { 0 }",
                FileId::new(1),
            ),
            SourceFile::new("b.gruel", "enum Foo { Bar }", FileId::new(2)),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let result = merge_symbols(parsed);
        assert!(
            result.is_err(),
            "merge should fail when struct and enum have same name"
        );

        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1, "should have 1 error");
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("Foo") && err_msg.contains("conflicts"),
            "error should mention the conflict: {}",
            err_msg
        );
    }

    #[test]
    fn test_merge_symbols_multiple_duplicates() {
        // Multiple duplicates should report multiple errors
        let sources = vec![
            SourceFile::new(
                "a.gruel",
                "fn foo() -> i32 { 1 } fn bar() -> i32 { 2 }",
                FileId::new(1),
            ),
            SourceFile::new(
                "b.gruel",
                "fn foo() -> i32 { 3 } fn bar() -> i32 { 4 }",
                FileId::new(2),
            ),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let result = merge_symbols(parsed);
        assert!(
            result.is_err(),
            "merge should fail with duplicate functions"
        );

        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 2, "should have 2 errors for 2 duplicates");
    }

    #[test]
    fn test_merge_symbols_with_struct_methods() {
        // Structs with inline methods from different files should be allowed
        let sources = vec![
            SourceFile::new(
                "a.gruel",
                "struct Point { x: i32, fn get_x(self) -> i32 { self.x } } fn main() -> i32 { 0 }",
                FileId::new(1),
            ),
            SourceFile::new("b.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
        ];
        let parsed = parse_all_files(&sources).unwrap();
        let result = merge_symbols(parsed);
        assert!(result.is_ok(), "struct methods should not cause conflicts");
    }

    // ========================================================================
    // Cross-File Semantic Analysis Tests
    // ========================================================================

    #[test]
    fn test_cross_file_function_call() {
        // Function in main.gruel calls function in utils.gruel
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { helper() }",
                FileId::new(1),
            ),
            SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file function call should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_function_call_with_args() {
        // Function in main.gruel calls function in utils.gruel with arguments
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { add(10, 32) }",
                FileId::new(1),
            ),
            SourceFile::new(
                "utils.gruel",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file function call with args should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_struct_usage() {
        // Struct defined in types.gruel, used in main.gruel
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { let p = Point { x: 1, y: 2 }; p.x + p.y }",
                FileId::new(1),
            ),
            SourceFile::new(
                "types.gruel",
                "struct Point { x: i32, y: i32 }",
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file struct usage should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_struct_as_function_param() {
        // Struct defined in types.gruel, function in utils.gruel takes it as param
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { let p = Point { x: 10, y: 5 }; get_sum(p) }",
                FileId::new(1),
            ),
            SourceFile::new(
                "types.gruel",
                "struct Point { x: i32, y: i32 }",
                FileId::new(2),
            ),
            SourceFile::new(
                "utils.gruel",
                "fn get_sum(p: Point) -> i32 { p.x + p.y }",
                FileId::new(3),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file struct as function param should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_enum_usage() {
        // Enum defined in types.gruel, used in main.gruel
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                r#"fn main() -> i32 {
                    let c = Color::Red;
                    match c { Color::Red => 1, Color::Green => 2, Color::Blue => 3 }
                }"#,
                FileId::new(1),
            ),
            SourceFile::new(
                "types.gruel",
                "enum Color { Red, Green, Blue }",
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file enum usage should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_no_main_function() {
        // No main function in any file
        let sources = vec![
            SourceFile::new("a.gruel", "fn foo() -> i32 { 1 }", FileId::new(1)),
            SourceFile::new("b.gruel", "fn bar() -> i32 { 2 }", FileId::new(2)),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(result.is_err(), "should fail without main function");

        let errors = result.unwrap_err();
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("main") && err_msg.contains("function"),
            "error should mention missing main function: {}",
            err_msg
        );
    }

    #[test]
    fn test_cross_file_duplicate_main() {
        // main() defined in multiple files
        let sources = vec![
            SourceFile::new("a.gruel", "fn main() -> i32 { 1 }", FileId::new(1)),
            SourceFile::new("b.gruel", "fn main() -> i32 { 2 }", FileId::new(2)),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(result.is_err(), "should fail with duplicate main");

        let errors = result.unwrap_err();
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("main"),
            "error should mention duplicate main: {}",
            err_msg
        );
    }

    #[test]
    fn test_cross_file_undefined_function() {
        // main.gruel calls function that doesn't exist
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { nonexistent() }",
                FileId::new(1),
            ),
            SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(result.is_err(), "should fail with undefined function");

        let errors = result.unwrap_err();
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("nonexistent") || err_msg.contains("undefined"),
            "error should mention undefined function: {}",
            err_msg
        );
    }

    #[test]
    fn test_cross_file_three_files_chain() {
        // main.gruel -> utils.gruel -> math.gruel chain of calls
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { compute(6, 7) }",
                FileId::new(1),
            ),
            SourceFile::new(
                "utils.gruel",
                "fn compute(a: i32, b: i32) -> i32 { multiply(a, b) }",
                FileId::new(2),
            ),
            SourceFile::new(
                "math.gruel",
                "fn multiply(x: i32, y: i32) -> i32 { x * y }",
                FileId::new(3),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "chain of cross-file calls should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_mutual_calls() {
        // Two files calling each other (mutual recursion possible)
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                r#"fn main() -> i32 { is_even(4) }
                fn is_even(n: i32) -> i32 { if n == 0 { 1 } else { is_odd(n - 1) } }"#,
                FileId::new(1),
            ),
            SourceFile::new(
                "utils.gruel",
                "fn is_odd(n: i32) -> i32 { if n == 0 { 0 } else { is_even(n - 1) } }",
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "mutual cross-file calls should compile: {:?}",
            result.err()
        );
    }

    // ========================================================================
    // Module Import Tests
    // ========================================================================

    #[test]
    fn test_module_member_access() {
        // Test that @import returns a module type and member access works
        // Note: In Phase 1, all files are merged into the same namespace,
        // so math.add() looks up "add" in the global function table.
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                r#"fn main() -> i32 {
                    let math = @import("math.gruel");
                    math.add(1, 2)
                }"#,
                FileId::new(1),
            ),
            SourceFile::new(
                "math.gruel",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "module member access should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_member_access_multiple_functions() {
        // Test accessing multiple functions from an imported module
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                r#"fn main() -> i32 {
                    let math = @import("math.gruel");
                    let sum = math.add(10, 20);
                    let diff = math.sub(sum, 5);
                    diff
                }"#,
                FileId::new(1),
            ),
            SourceFile::new(
                "math.gruel",
                r#"fn add(a: i32, b: i32) -> i32 { a + b }
                fn sub(a: i32, b: i32) -> i32 { a - b }"#,
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "module with multiple functions should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_undefined_function_error() {
        // Test that accessing an undefined function in a module produces an error
        let sources = vec![
            SourceFile::new(
                "main.gruel",
                r#"fn main() -> i32 {
                    let math = @import("math.gruel");
                    math.nonexistent(1, 2)
                }"#,
                FileId::new(1),
            ),
            SourceFile::new(
                "math.gruel",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                FileId::new(2),
            ),
        ];
        let result = compile_multi_file_with_options(&sources, &CompileOptions::default());
        assert!(
            result.is_err(),
            "undefined module function should fail to compile"
        );
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("undefined function") || err.contains("nonexistent"),
            "error should mention undefined function: {}",
            err
        );
    }
}

// ============================================================================
// Integration Unit Tests
// ============================================================================
//
// These tests verify the compilation pipeline without execution. They test:
// - Type checking and semantic analysis
// - CFG construction
// - Error message quality
//
// Benefits:
// - Fast: No file I/O, no process spawning, no execution
// - Comprehensive: Tests full parse→sema→codegen pipeline
// - Debuggable: Can inspect intermediate IRs in tests

#[cfg(test)]
mod integration_tests {
    use super::*;

    // ========================================================================
    // Integer Types
    // ========================================================================

    mod integer_types {
        use super::*;

        #[test]
        fn signed_integer_return() {
            assert!(compile_to_air("fn main() -> i8 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> i16 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> i32 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> i64 { 42 }").is_ok());
        }

        #[test]
        fn unsigned_integer_return() {
            assert!(compile_to_air("fn main() -> u8 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> u16 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> u32 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> u64 { 42 }").is_ok());
        }

        #[test]
        fn integer_type_mismatch() {
            let result = compile_to_air("fn main() -> i32 { let x: i64 = 1; x }");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("type mismatch") || err.contains("expected"));
        }

        #[test]
        fn integer_literal_type_inference() {
            // Type inferred from return type
            assert!(compile_to_air("fn main() -> i64 { 100 }").is_ok());
            // Type inferred from annotation
            assert!(compile_to_air("fn main() -> i32 { let x: i64 = 100; 0 }").is_ok());
        }
    }

    // ========================================================================
    // Boolean Type
    // ========================================================================

    mod boolean_type {
        use super::*;

        #[test]
        fn boolean_literals() {
            assert!(compile_to_air("fn main() -> bool { true }").is_ok());
            assert!(compile_to_air("fn main() -> bool { false }").is_ok());
        }

        #[test]
        fn boolean_in_condition() {
            assert!(compile_to_cfg("fn main() -> i32 { if true { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn non_boolean_condition_rejected() {
            let result = compile_to_air("fn main() -> i32 { if 1 { 1 } else { 0 } }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Unit Type
    // ========================================================================

    mod unit_type {
        use super::*;

        #[test]
        fn unit_return_type() {
            assert!(compile_to_air("fn main() -> () { () }").is_ok());
        }

        #[test]
        fn unit_in_expression() {
            assert!(compile_to_air("fn main() -> () { let _x = (); () }").is_ok());
        }

        #[test]
        fn implicit_unit_return() {
            assert!(compile_to_air("fn foo() -> () { } fn main() -> i32 { 0 }").is_ok());
        }
    }

    // ========================================================================
    // Arithmetic Operations
    // ========================================================================

    mod arithmetic {
        use super::*;

        #[test]
        fn basic_addition() {
            assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        }

        #[test]
        fn basic_subtraction() {
            assert!(compile_to_air("fn main() -> i32 { 5 - 3 }").is_ok());
        }

        #[test]
        fn basic_multiplication() {
            assert!(compile_to_air("fn main() -> i32 { 3 * 4 }").is_ok());
        }

        #[test]
        fn basic_division() {
            assert!(compile_to_air("fn main() -> i32 { 10 / 2 }").is_ok());
        }

        #[test]
        fn basic_modulo() {
            assert!(compile_to_air("fn main() -> i32 { 10 % 3 }").is_ok());
        }

        #[test]
        fn unary_negation() {
            assert!(compile_to_air("fn main() -> i32 { -42 }").is_ok());
        }

        #[test]
        fn operator_precedence() {
            // Multiplication before addition
            let state = compile_to_cfg("fn main() -> i32 { 1 + 2 * 3 }").unwrap();
            assert_eq!(state.functions.len(), 1);
        }

        #[test]
        fn chained_operations() {
            assert!(compile_to_air("fn main() -> i32 { 1 + 2 + 3 + 4 }").is_ok());
        }

        #[test]
        fn mixed_type_arithmetic_rejected() {
            let result = compile_to_air("fn main() -> i32 { 1 + true }");
            assert!(result.is_err());
        }

        #[test]
        fn unsigned_arithmetic() {
            assert!(compile_to_air("fn main() -> u32 { 10 + 5 }").is_ok());
            assert!(compile_to_air("fn main() -> u32 { 10 - 5 }").is_ok());
            assert!(compile_to_air("fn main() -> u32 { 10 * 5 }").is_ok());
        }
    }

    // ========================================================================
    // Comparison Operations
    // ========================================================================

    mod comparison {
        use super::*;

        #[test]
        fn equality_comparison() {
            assert!(compile_to_air("fn main() -> bool { 1 == 1 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 1 != 2 }").is_ok());
        }

        #[test]
        fn ordering_comparison() {
            assert!(compile_to_air("fn main() -> bool { 1 < 2 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 2 > 1 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 1 <= 2 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 2 >= 1 }").is_ok());
        }

        #[test]
        fn boolean_equality() {
            assert!(compile_to_air("fn main() -> bool { true == true }").is_ok());
            assert!(compile_to_air("fn main() -> bool { true != false }").is_ok());
        }

        #[test]
        fn comparison_returns_bool() {
            let result = compile_to_air("fn main() -> i32 { 1 < 2 }");
            assert!(result.is_err()); // Type mismatch: bool vs i32
        }

        #[test]
        fn mixed_type_comparison_rejected() {
            let result = compile_to_air("fn main() -> bool { 1 == true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Logical Operations
    // ========================================================================

    mod logical {
        use super::*;

        #[test]
        fn logical_and() {
            assert!(compile_to_cfg("fn main() -> bool { true && false }").is_ok());
        }

        #[test]
        fn logical_or() {
            assert!(compile_to_cfg("fn main() -> bool { true || false }").is_ok());
        }

        #[test]
        fn logical_not() {
            assert!(compile_to_air("fn main() -> bool { !true }").is_ok());
        }

        #[test]
        fn chained_logical() {
            assert!(compile_to_cfg("fn main() -> bool { true && false || true }").is_ok());
        }

        #[test]
        fn logical_with_non_bool_rejected() {
            let result = compile_to_air("fn main() -> bool { 1 && true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Bitwise Operations
    // ========================================================================

    mod bitwise {
        use super::*;

        #[test]
        fn bitwise_and() {
            assert!(compile_to_air("fn main() -> i32 { 5 & 3 }").is_ok());
        }

        #[test]
        fn bitwise_or() {
            assert!(compile_to_air("fn main() -> i32 { 5 | 3 }").is_ok());
        }

        #[test]
        fn bitwise_xor() {
            assert!(compile_to_air("fn main() -> i32 { 5 ^ 3 }").is_ok());
        }

        #[test]
        fn bitwise_not() {
            assert!(compile_to_air("fn main() -> i32 { ~5 }").is_ok());
        }

        #[test]
        fn shift_left() {
            assert!(compile_to_air("fn main() -> i32 { 1 << 4 }").is_ok());
        }

        #[test]
        fn shift_right() {
            assert!(compile_to_air("fn main() -> i32 { 16 >> 2 }").is_ok());
        }

        #[test]
        fn bitwise_on_bool_rejected() {
            let result = compile_to_air("fn main() -> bool { true & false }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - If Expressions
    // ========================================================================

    mod if_expressions {
        use super::*;

        #[test]
        fn basic_if_else() {
            assert!(compile_to_cfg("fn main() -> i32 { if true { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn if_with_condition_expr() {
            assert!(compile_to_cfg("fn main() -> i32 { if 1 < 2 { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn nested_if() {
            let src = "fn main() -> i32 { if true { if false { 1 } else { 2 } } else { 3 } }";
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn if_branches_must_match_type() {
            let result = compile_to_air("fn main() -> i32 { if true { 1 } else { true } }");
            assert!(result.is_err());
        }

        #[test]
        fn if_result_type_checked() {
            let result = compile_to_air("fn main() -> bool { if true { 1 } else { 0 } }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - Match Expressions
    // ========================================================================

    mod match_expressions {
        use super::*;

        #[test]
        fn match_on_integer() {
            let src = r#"
                fn main() -> i32 {
                    let x = 1;
                    match x {
                        1 => 10,
                        2 => 20,
                        _ => 0,
                    }
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn match_on_boolean() {
            let src = r#"
                fn main() -> i32 {
                    match true {
                        true => 1,
                        false => 0,
                    }
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn match_exhaustiveness_required() {
            // Missing case should error
            let result = compile_to_air(
                r#"
                fn main() -> i32 {
                    match 1 {
                        1 => 10,
                    }
                }
            "#,
            );
            assert!(result.is_err());
        }

        #[test]
        fn match_branches_must_match_type() {
            let result = compile_to_air(
                r#"
                fn main() -> i32 {
                    match true {
                        true => 1,
                        false => true,
                    }
                }
            "#,
            );
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - Loops
    // ========================================================================

    mod loops {
        use super::*;

        #[test]
        fn while_loop_basic() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while x < 10 {
                        x = x + 1;
                    }
                    x
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn while_with_break() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while true {
                        x = x + 1;
                        if x == 5 {
                            break;
                        }
                    }
                    x
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn while_with_continue() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    let mut sum = 0;
                    while x < 10 {
                        x = x + 1;
                        if x == 5 {
                            continue;
                        }
                        sum = sum + x;
                    }
                    sum
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn break_outside_loop_rejected() {
            let result = compile_to_air("fn main() -> i32 { break; 0 }");
            assert!(result.is_err());
        }

        #[test]
        fn continue_outside_loop_rejected() {
            let result = compile_to_air("fn main() -> i32 { continue; 0 }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Let Bindings
    // ========================================================================

    mod let_bindings {
        use super::*;

        #[test]
        fn basic_let() {
            assert!(compile_to_air("fn main() -> i32 { let x = 42; x }").is_ok());
        }

        #[test]
        fn let_with_type_annotation() {
            assert!(compile_to_air("fn main() -> i32 { let x: i32 = 42; x }").is_ok());
        }

        #[test]
        fn mutable_let() {
            let src = "fn main() -> i32 { let mut x = 1; x = 2; x }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn immutable_assignment_rejected() {
            let result = compile_to_air("fn main() -> i32 { let x = 1; x = 2; x }");
            assert!(result.is_err());
        }

        #[test]
        fn shadowing_allowed() {
            let src = "fn main() -> i32 { let x = 1; let x = 2; x }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn shadowing_can_change_type() {
            let src = "fn main() -> bool { let x = 1; let x = true; x }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn undefined_variable_rejected() {
            let result = compile_to_air("fn main() -> i32 { x }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Functions
    // ========================================================================

    mod functions {
        use super::*;

        #[test]
        fn function_call() {
            let src = r#"
                fn add(a: i32, b: i32) -> i32 { a + b }
                fn main() -> i32 { add(1, 2) }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn function_forward_reference() {
            let src = r#"
                fn main() -> i32 { foo() }
                fn foo() -> i32 { 42 }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn recursion() {
            let src = r#"
                fn factorial(n: i32) -> i32 {
                    if n <= 1 { 1 } else { n * factorial(n - 1) }
                }
                fn main() -> i32 { factorial(5) }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn mutual_recursion() {
            let src = r#"
                fn is_even(n: i32) -> bool {
                    if n == 0 { true } else { is_odd(n - 1) }
                }
                fn is_odd(n: i32) -> bool {
                    if n == 0 { false } else { is_even(n - 1) }
                }
                fn main() -> i32 { if is_even(4) { 1 } else { 0 } }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn wrong_argument_count_rejected() {
            let src = r#"
                fn add(a: i32, b: i32) -> i32 { a + b }
                fn main() -> i32 { add(1) }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn wrong_argument_type_rejected() {
            let src = r#"
                fn foo(x: i32) -> i32 { x }
                fn main() -> i32 { foo(true) }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn undefined_function_rejected() {
            let result = compile_to_air("fn main() -> i32 { unknown() }");
            assert!(result.is_err());
        }

        #[test]
        fn return_type_mismatch_rejected() {
            let result = compile_to_air("fn main() -> i32 { true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Structs
    // ========================================================================

    mod structs {
        use super::*;

        #[test]
        fn struct_definition() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 { 0 }
            "#;
            let result = compile_to_air(src).unwrap();
            // type_pool includes builtin types (String) plus user-defined structs
            // There's 1 builtin (String) + 1 user-defined (Point) = 2 total structs
            let all_struct_ids = result.type_pool.all_struct_ids();
            assert_eq!(all_struct_ids.len(), 2);
            // Verify Point is present
            let point_name = result.interner.get_or_intern("Point");
            let point_interned = result.type_pool.get_struct_by_name(point_name);
            assert!(
                point_interned.is_some(),
                "Point struct should exist in pool"
            );
        }

        #[test]
        fn struct_literal() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let _p = Point { x: 1, y: 2 };
                    0
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_field_access() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 10, y: 20 };
                    p.x + p.y
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_field_order_independent() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { y: 2, x: 1 };
                    p.x
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_unknown_field_rejected() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 1, z: 2 };
                    0
                }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn struct_equality() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> bool {
                    let a = Point { x: 1, y: 2 };
                    let b = Point { x: 1, y: 2 };
                    a == b
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_move_semantics() {
            // After moving a struct, it should not be usable
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn consume(p: Point) -> i32 { p.x }
                fn main() -> i32 {
                    let p = Point { x: 1, y: 2 };
                    let _a = consume(p);
                    p.x
                }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Enums
    // ========================================================================

    mod enums {
        use super::*;

        #[test]
        fn enum_definition() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 { 0 }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn enum_variant_access() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let _c = Color::Red;
                    0
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn enum_match() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let c = Color::Green;
                    match c {
                        Color::Red => 1,
                        Color::Green => 2,
                        Color::Blue => 3,
                    }
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn enum_comparison_via_match() {
            // Enum equality comparison is done via match, not ==
            // (== is not yet implemented for enums)
            let src = r#"
                enum Color { Red, Green, Blue }
                fn eq(a: Color, b: Color) -> bool {
                    match a {
                        Color::Red => match b { Color::Red => true, _ => false },
                        Color::Green => match b { Color::Green => true, _ => false },
                        Color::Blue => match b { Color::Blue => true, _ => false },
                    }
                }
                fn main() -> i32 { if eq(Color::Red, Color::Red) { 1 } else { 0 } }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn unknown_enum_variant_rejected() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let _c = Color::Yellow;
                    0
                }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Arrays
    // ========================================================================

    mod arrays {
        use super::*;

        #[test]
        fn array_literal() {
            let src = "fn main() -> i32 { let _arr: [i32; 3] = [1, 2, 3]; 0 }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn array_indexing() {
            let src = "fn main() -> i32 { let arr: [i32; 3] = [1, 2, 3]; arr[1] }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn array_element_assignment() {
            let src = r#"
                fn main() -> i32 {
                    let mut arr: [i32; 3] = [1, 2, 3];
                    arr[0] = 10;
                    arr[0]
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn array_wrong_length_rejected() {
            let src = "fn main() -> i32 { let _arr: [i32; 3] = [1, 2]; 0 }";
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn array_mixed_types_rejected() {
            let src = "fn main() -> i32 { let _arr: [i32; 2] = [1, true]; 0 }";
            let result = compile_to_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Strings
    // ========================================================================

    mod strings {
        use super::*;

        #[test]
        fn string_literal() {
            let src = r#"fn main() -> i32 { let _s = "hello"; 0 }"#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn string_with_quote_escape() {
            // String escape sequences: \" is supported
            let src = r#"fn main() -> i32 { let _s = "hello\"world"; 0 }"#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn string_with_backslash_escape() {
            // String escape sequences: \\ is supported
            let src = r#"fn main() -> i32 { let _s = "hello\\world"; 0 }"#;
            assert!(compile_to_air(src).is_ok());
        }
    }

    // ========================================================================
    // Block Expressions
    // ========================================================================

    mod blocks {
        use super::*;

        #[test]
        fn block_returns_final_expression() {
            let src = "fn main() -> i32 { { 1; 2; 3 } }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn block_with_let_bindings() {
            let src = "fn main() -> i32 { { let x = 1; let y = 2; x + y } }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn nested_blocks() {
            let src = "fn main() -> i32 { { { { 42 } } } }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn block_scoping() {
            // Variable should not be accessible outside block
            let result = compile_to_air("fn main() -> i32 { { let x = 1; } x }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Never Type
    // ========================================================================

    mod never_type {
        use super::*;

        #[test]
        fn return_is_never() {
            let src = "fn main() -> i32 { return 42; }";
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn break_is_never() {
            let src = r#"
                fn main() -> i32 {
                    while true {
                        break;
                    }
                    0
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn never_in_if_branch() {
            let src = "fn main() -> i32 { if true { 1 } else { return 2; } }";
            assert!(compile_to_cfg(src).is_ok());
        }
    }

    // ========================================================================
    // Type Intrinsics
    // ========================================================================

    mod intrinsics {
        use super::*;

        #[test]
        fn size_of_intrinsic() {
            // @size_of returns usize
            let src = "fn main() -> i32 { let n: usize = @size_of(i32); @cast(n) }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn align_of_intrinsic() {
            // @align_of returns usize
            let src = "fn main() -> i32 { let n: usize = @align_of(i64); @cast(n) }";
            assert!(compile_to_air(src).is_ok());
        }
    }

    // ========================================================================
    // CFG Construction
    // ========================================================================

    mod cfg_construction {
        use super::*;

        #[test]
        fn cfg_has_correct_function_count() {
            let src = r#"
                fn foo() -> i32 { 1 }
                fn bar() -> i32 { 2 }
                fn main() -> i32 { foo() + bar() }
            "#;
            let state = compile_to_cfg(src).unwrap();
            assert_eq!(state.functions.len(), 3);
        }

        #[test]
        fn cfg_branches_for_if() {
            let src = "fn main() -> i32 { if true { 1 } else { 0 } }";
            let state = compile_to_cfg(src).unwrap();
            // CFG should have multiple blocks for branching
            let main_cfg = &state.functions[0].cfg;
            assert!(main_cfg.blocks().len() >= 3); // entry, then, else, merge
        }

        #[test]
        fn cfg_loop_for_while() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while x < 10 { x = x + 1; }
                    x
                }
            "#;
            let state = compile_to_cfg(src).unwrap();
            let main_cfg = &state.functions[0].cfg;
            assert!(main_cfg.blocks().len() >= 3); // header, body, exit
        }
    }

    // ========================================================================
    // Error Messages
    // ========================================================================

    mod error_messages {
        use super::*;

        #[test]
        fn type_mismatch_error_is_descriptive() {
            let result = compile_to_air("fn main() -> i32 { true }");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("type mismatch") || err.contains("expected"));
            assert!(err.contains("i32") || err.contains("bool"));
        }

        #[test]
        fn undefined_variable_error_is_descriptive() {
            let result = compile_to_air("fn main() -> i32 { unknown_var }");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("undefined") || err.contains("unknown"));
        }

        #[test]
        fn missing_field_error_is_descriptive() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 1 };
                    0
                }
            "#;
            let result = compile_to_air(src);
            let err = result.unwrap_err().to_string();
            assert!(err.contains("missing") || err.contains("field"));
        }
    }

    // ========================================================================
    // Warnings
    // ========================================================================

    mod warnings {
        use super::*;

        #[test]
        fn unused_variable_warning() {
            let result = compile_to_air("fn main() -> i32 { let x = 42; 0 }").unwrap();
            assert_eq!(result.warnings.len(), 1);
            assert!(result.warnings[0].to_string().contains("unused"));
        }

        #[test]
        fn underscore_prefix_suppresses_warning() {
            let result = compile_to_air("fn main() -> i32 { let _x = 42; 0 }").unwrap();
            assert_eq!(result.warnings.len(), 0);
        }

        #[test]
        fn used_variable_no_warning() {
            let result = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();
            assert_eq!(result.warnings.len(), 0);
        }
    }

    // ========================================================================
    // Edge Cases
    // ========================================================================

    mod edge_cases {
        use super::*;

        #[test]
        fn empty_function_body() {
            assert!(compile_to_air("fn main() -> () { }").is_ok());
        }

        #[test]
        fn deeply_nested_expressions() {
            let src = "fn main() -> i32 { ((((((1 + 2)))))) }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn many_parameters() {
            let src = r#"
                fn many(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
                    a + b + c + d + e + f
                }
                fn main() -> i32 { many(1, 2, 3, 4, 5, 6) }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn long_chain_of_operations() {
            let src = "fn main() -> i32 { 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn multiple_functions_same_local_names() {
            let src = r#"
                fn foo() -> i32 { let x = 1; x }
                fn bar() -> i32 { let x = 2; x }
                fn main() -> i32 { foo() + bar() }
            "#;
            assert!(compile_to_air(src).is_ok());
        }
    }

    // ========================================================================
    // Multi-file Parsing
    // ========================================================================

    mod multi_file_parsing {
        use super::*;

        #[test]
        fn parse_single_file() {
            let sources = vec![SourceFile::new(
                "main.gruel",
                "fn main() -> i32 { 42 }",
                FileId::new(1),
            )];
            let program = parse_all_files(&sources).unwrap();
            assert_eq!(program.files.len(), 1);
            assert_eq!(program.files[0].path, "main.gruel");
            assert_eq!(program.files[0].file_id, FileId::new(1));
        }

        #[test]
        fn parse_multiple_files() {
            let sources = vec![
                SourceFile::new(
                    "main.gruel",
                    "fn main() -> i32 { helper() }",
                    FileId::new(1),
                ),
                SourceFile::new("utils.gruel", "fn helper() -> i32 { 42 }", FileId::new(2)),
            ];
            let program = parse_all_files(&sources).unwrap();
            assert_eq!(program.files.len(), 2);

            // Check that both files were parsed
            let paths: Vec<_> = program.files.iter().map(|f| f.path.as_str()).collect();
            assert!(paths.contains(&"main.gruel"));
            assert!(paths.contains(&"utils.gruel"));
        }

        #[test]
        fn parse_many_files_in_parallel() {
            // Create 10 files to exercise parallel parsing
            let sources: Vec<_> = (1..=10)
                .map(|i| {
                    SourceFile::new(
                        // Leak the string to get a &'static str
                        Box::leak(format!("file{}.gruel", i).into_boxed_str()),
                        Box::leak(format!("fn func{}() -> i32 {{ {} }}", i, i).into_boxed_str()),
                        FileId::new(i as u32),
                    )
                })
                .collect();

            let program = parse_all_files(&sources).unwrap();
            assert_eq!(program.files.len(), 10);

            // All functions should be in their respective ASTs
            for (i, file) in program.files.iter().enumerate() {
                assert!(!file.ast.items.is_empty(), "File {} has no items", i);
            }
        }

        #[test]
        fn parse_error_in_single_file() {
            let sources = vec![SourceFile::new(
                "bad.gruel",
                "fn main( { }", // Missing closing paren
                FileId::new(1),
            )];

            let result = parse_all_files(&sources);
            assert!(result.is_err());

            let errors = result.unwrap_err();
            assert!(!errors.is_empty());
        }

        #[test]
        fn parse_error_in_multiple_files() {
            let sources = vec![
                SourceFile::new("good.gruel", "fn good() -> i32 { 42 }", FileId::new(1)),
                SourceFile::new(
                    "bad.gruel",
                    "fn bad( { }", // Parse error
                    FileId::new(2),
                ),
            ];

            let result = parse_all_files(&sources);
            assert!(result.is_err());

            // The error should still report, and we should get at least one error
            let errors = result.unwrap_err();
            assert!(!errors.is_empty());
        }

        #[test]
        fn lexer_error_in_file() {
            let sources = vec![SourceFile::new(
                "lexer_error.gruel",
                "fn main() -> i32 { $ }", // '$' is not a valid token
                FileId::new(1),
            )];

            let result = parse_all_files(&sources);
            assert!(result.is_err());
        }

        #[test]
        fn interner_merges_across_files() {
            let sources = vec![
                SourceFile::new("a.gruel", "fn foo() -> i32 { 1 }", FileId::new(1)),
                SourceFile::new("b.gruel", "fn bar() -> i32 { 2 }", FileId::new(2)),
            ];

            let program = parse_all_files(&sources).unwrap();

            // The merged interner should contain both "foo" and "bar"
            let has_foo = program.interner.iter().any(|(_, s)| s == "foo");
            let has_bar = program.interner.iter().any(|(_, s)| s == "bar");

            assert!(has_foo, "Interner should contain 'foo'");
            assert!(has_bar, "Interner should contain 'bar'");
        }

        #[test]
        fn empty_file_parses_ok() {
            let sources = vec![SourceFile::new("empty.gruel", "", FileId::new(1))];

            let program = parse_all_files(&sources).unwrap();
            assert_eq!(program.files.len(), 1);
            assert!(program.files[0].ast.items.is_empty());
        }

        #[test]
        fn file_ids_preserved() {
            let sources = vec![
                SourceFile::new("a.gruel", "fn a() -> i32 { 1 }", FileId::new(42)),
                SourceFile::new("b.gruel", "fn b() -> i32 { 2 }", FileId::new(99)),
            ];

            let program = parse_all_files(&sources).unwrap();

            let file_ids: Vec<_> = program.files.iter().map(|f| f.file_id).collect();
            assert!(file_ids.contains(&FileId::new(42)));
            assert!(file_ids.contains(&FileId::new(99)));
        }
    }

    // ------------------------------------------------------------------
    // ADR-0051 Phase 3: end-to-end sema + CFG with recursive AirPattern.
    // Exercises the recursive lowering path on the existing flat RIR
    // shape; phase 4 will unlock true nesting (tuple / struct match
    // roots, nested variant fields).
    // ------------------------------------------------------------------
    mod recursive_cfg_lowering {
        use super::*;
        use gruel_cfg::CfgBuilder;
        use gruel_rir::AstGen;

        /// Run the whole pipeline (sema + CFG per function) with the
        /// ADR-0051 recursive lowering flag enabled, panicking on any
        /// compile error or CFG construction failure. Returns the number
        /// of functions successfully built.
        fn compile_with_recursive(source: &str) -> usize {
            let lexer = Lexer::new(source);
            let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from).unwrap();
            let parser = Parser::new(tokens, interner);
            let (ast, interner) = parser.parse().unwrap();
            let astgen = AstGen::new(&ast, &interner);
            let rir = astgen.generate();

            let sema = Sema::new(&rir, &interner, PreviewFeatures::new());
            let sema_output = sema.analyze_all().unwrap();

            let mut count = 0;
            for func in sema_output.functions {
                // Skip comptime-only functions.
                if func.air.return_type() == Type::COMPTIME_TYPE {
                    continue;
                }
                let _cfg_output = CfgBuilder::build(&func, &sema_output.type_pool);
                count += 1;
            }
            count
        }

        #[test]
        fn match_on_int_builds_cfg() {
            assert!(
                compile_with_recursive("fn main() -> i32 { match 1 { 1 => 10, 2 => 20, _ => 0 } }")
                    >= 1
            );
        }

        #[test]
        fn match_on_bool_builds_cfg() {
            assert!(
                compile_with_recursive("fn main() -> i32 { match true { true => 1, false => 0 } }")
                    >= 1
            );
        }

        #[test]
        fn match_on_unit_enum_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "enum Color { Red, Green, Blue }
                 fn main() -> i32 {
                     let c = Color::Red;
                     match c {
                         Color::Red => 1,
                         Color::Green => 2,
                         Color::Blue => 3,
                     }
                 }"
                ) >= 1
            );
        }

        #[test]
        fn match_on_data_variant_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "enum Opt { Some(i32), None }
                 fn main() -> i32 {
                     let o = Opt::Some(5);
                     match o {
                         Opt::Some(x) => x,
                         Opt::None => 0,
                     }
                 }"
                ) >= 1
            );
        }

        #[test]
        fn match_on_struct_variant_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "enum Shape { Circle { radius: i32 }, Square { side: i32 } }
                 fn main() -> i32 {
                     let s = Shape::Circle { radius: 5 };
                     match s {
                         Shape::Circle { radius } => radius,
                         Shape::Square { side } => side,
                     }
                 }"
                ) >= 1
            );
        }

        #[test]
        fn match_with_rest_in_data_variant_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "enum T { Triple(i32, i32, i32) }
                 fn main() -> i32 {
                     let t = T::Triple(1, 2, 3);
                     match t {
                         T::Triple(x, ..) => x,
                     }
                 }"
                ) >= 1
            );
        }

        #[test]
        fn match_with_rest_in_struct_variant_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "enum Pt { Coord { x: i32, y: i32 } }
                 fn main() -> i32 {
                     let p = Pt::Coord { x: 1, y: 2 };
                     match p {
                         Pt::Coord { x, .. } => x,
                     }
                 }"
                ) >= 1
            );
        }

        #[test]
        fn match_tuple_literal_root_builds_cfg() {
            // ADR-0051 Phase 4b: top-level tuple pattern with literal
            // leaves. Previously this shape would have gone through the
            // astgen tuple-match elaborator; with the flag on it lowers
            // via recursive AirPattern::Tuple + CFG cascading dispatch.
            assert!(
                compile_with_recursive(
                    "fn main() -> i32 {
                         let t = (1, 2);
                         match t {
                             (1, 2) => 3,
                             _ => 0,
                         }
                     }"
                ) >= 1
            );
        }

        #[test]
        fn match_tuple_bool_mix_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "fn main() -> i32 {
                         let t = (true, 5);
                         match t {
                             (true, 5) => 1,
                             (false, _) => 2,
                             _ => 0,
                         }
                     }"
                ) >= 1
            );
        }

        /// ADR-0051 Phase 4c: binding leaves inside tuple arms emit
        /// StorageLive + FieldGet + Alloc at sema time, with the
        /// binding registered in the arm scope so the body resolves.
        #[test]
        fn match_tuple_with_binding_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "fn main() -> i32 {
                         let t = (1, 2);
                         match t {
                             (a, 1) => a,
                             _ => 0,
                         }
                     }"
                ) >= 1
            );
        }

        #[test]
        fn match_tuple_all_bindings_builds_cfg() {
            assert!(
                compile_with_recursive(
                    "fn main() -> i32 {
                         let t = (1, 2);
                         match t {
                             (a, b) => a + b,
                         }
                     }"
                ) >= 1
            );
        }
    }
}
