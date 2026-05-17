use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use std::process::Command;

use clap::{Args, Parser, Subcommand};
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{EnvFilter, fmt};

mod timing;

use gruel_cache::{CacheKind, CacheStore};
use gruel_compiler::{
    CompileOptions, FileId, Lexer, LinkerMode, MultiFileFormatter, OptLevel, ParsedProgram,
    PreviewFeature, PreviewFeatures, SourceFile, SourceInfo,
    compile_frontend_from_ast_with_options_full_target, compile_multi_file_with_options,
    generate_llvm_ir, merge_symbols,
};
use gruel_rir::RirPrinter;
use gruel_target::Target;

/// Compilation stages that can be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum EmitStage {
    /// Emit tokens from the lexer.
    Tokens,
    /// Emit the abstract syntax tree.
    Ast,
    /// Emit RIR (untyped intermediate representation).
    Rir,
    /// Emit AIR (typed intermediate representation).
    Air,
    /// Emit CFG (control flow graph).
    Cfg,
    /// Emit LLVM IR (human-readable `.ll` format).
    Asm,
}

impl EmitStage {
    fn from_name(s: &str) -> Result<Self, String> {
        match s {
            "tokens" => Ok(EmitStage::Tokens),
            "ast" => Ok(EmitStage::Ast),
            "rir" => Ok(EmitStage::Rir),
            "air" => Ok(EmitStage::Air),
            "cfg" => Ok(EmitStage::Cfg),
            "asm" => Ok(EmitStage::Asm),
            other => Err(format!(
                "unknown emit stage '{}' (expected tokens|ast|rir|air|cfg|asm)",
                other
            )),
        }
    }
}

/// Log level for tracing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
enum LogLevel {
    /// No logging output (default).
    #[default]
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Convert to tracing Level, returns None for Off.
    fn to_tracing_level(self) -> Option<Level> {
        match self {
            LogLevel::Off => None,
            LogLevel::Error => Some(Level::ERROR),
            LogLevel::Warn => Some(Level::WARN),
            LogLevel::Info => Some(Level::INFO),
            LogLevel::Debug => Some(Level::DEBUG),
            LogLevel::Trace => Some(Level::TRACE),
        }
    }
}

/// Log format for tracing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
enum LogFormat {
    /// Human-readable text format (default).
    #[default]
    Text,
    /// Machine-readable JSON format.
    Json,
}

/// ADR-0089: doc output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DocFormat {
    Markdown,
    Html,
}

/// ADR-0074 Phase 6: print incremental-compilation cache statistics
/// to stdout and exit. Format is human-readable (per-kind entry count
/// + bytes + total).
fn run_cache_stats(dir: &std::path::Path) {
    if !dir.exists() {
        println!(
            "cache directory {} does not exist (no builds yet)",
            dir.display()
        );
        return;
    }
    let store = match CacheStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error opening cache at {}: {}", dir.display(), e);
            std::process::exit(1);
        }
    };
    let stats = match store.stats() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading cache stats: {}", e);
            std::process::exit(1);
        }
    };
    println!("gruel cache: {}", dir.display());
    let mut total_entries = 0usize;
    let mut total_bytes = 0u64;
    for (kind, st) in stats {
        let kind_name = match kind {
            CacheKind::Parse => "parse",
            CacheKind::Air => "air",
            CacheKind::LlvmIr => "llvm-ir",
        };
        println!(
            "  {:8} {:>6} entries  {:>10}",
            kind_name,
            st.entries,
            human_bytes(st.bytes),
        );
        total_entries += st.entries;
        total_bytes += st.bytes;
    }
    println!(
        "  {:8} {:>6} entries  {:>10}",
        "total",
        total_entries,
        human_bytes(total_bytes),
    );
}

fn run_cache_clean(dir: &std::path::Path) {
    if !dir.exists() {
        println!("cache directory {} already clean", dir.display());
        return;
    }
    let store = match CacheStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error opening cache at {}: {}", dir.display(), e);
            std::process::exit(1);
        }
    };
    if let Err(e) = store.clean() {
        eprintln!("error cleaning cache: {}", e);
        std::process::exit(1);
    }
    println!("cleaned cache at {}", dir.display());
}

/// ADR-0089: parse each source file and write per-file docs.
///
/// The doc subcommand is independent of sema/codegen: we run lexer +
/// parser only and hand the resulting AST + interner to
/// `gruel_doc::DocSite`.
fn run_doc(opts: &DocOpts) {
    use gruel_compiler::Parser;
    use gruel_doc::DocSite;
    use std::path::PathBuf;

    let sources: Vec<(String, String)> = opts
        .source_paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("Error reading {}: {}", path, e);
                std::process::exit(1);
            });
            (path.clone(), content)
        })
        .collect();

    let out_dir = PathBuf::from(&opts.output_dir);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error creating doc output dir {}: {}", out_dir.display(), e);
        std::process::exit(1);
    }

    let mut site = DocSite::default();

    for (idx, (path, source)) in sources.iter().enumerate() {
        let file_id = FileId::new((idx + 1) as u32);
        let lexer =
            Lexer::with_interner_and_file_id(source.as_str(), lasso::ThreadedRodeo::new(), file_id);
        let (tokens, interner) = match lexer.tokenize() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("lex error in {}: {:?}", path, e);
                std::process::exit(1);
            }
        };
        let parser = Parser::new(tokens, interner)
            .with_preview_features(opts.preview_features.clone())
            .with_source(source.as_str());
        let (ast, interner) = match parser.parse() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("parse error in {}: {:?}", path, e);
                std::process::exit(1);
            }
        };
        let stem = PathBuf::from(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let file = DocSite::from_ast(stem, &ast, &interner);
        site.push(file);
    }

    if let Err(e) = write_doc_site(&site, &out_dir, opts.format) {
        eprintln!("error writing docs: {}", e);
        std::process::exit(1);
    }
    println!(
        "wrote {} doc file(s) to {}",
        site.files.len(),
        out_dir.display()
    );
}

fn write_doc_site(
    site: &gruel_doc::DocSite,
    out_dir: &std::path::Path,
    format: DocFormat,
) -> std::io::Result<()> {
    match format {
        DocFormat::Markdown => write_doc_site_markdown(site, out_dir),
        DocFormat::Html => write_doc_site_html(site, out_dir),
    }
}

fn write_doc_site_markdown(
    site: &gruel_doc::DocSite,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    use gruel_doc::markdown::{render_index_with, render_markdown_with};

    // Top-level index.md links each file's index.
    let mut index = String::from("# Documentation\n\n");
    for file in &site.files {
        index.push_str(&format!("- [{stem}]({stem}/index.md)\n", stem = file.stem));
    }
    std::fs::write(out_dir.join("index.md"), index)?;

    for file in &site.files {
        let table = file.link_table();
        let file_dir = out_dir.join(&file.stem);
        std::fs::create_dir_all(&file_dir)?;
        std::fs::write(file_dir.join("index.md"), render_index_with(file, &table))?;
        for item in &file.items {
            std::fs::write(
                file_dir.join(format!("{}.md", item.slug)),
                render_markdown_with(item, &table),
            )?;
        }
    }
    Ok(())
}

fn write_doc_site_html(
    site: &gruel_doc::DocSite,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    use gruel_doc::html::{render_html_with, render_index_html_with, render_site_index_html};

    std::fs::write(
        out_dir.join("index.html"),
        render_site_index_html(&site.files),
    )?;
    for file in &site.files {
        let table = file.link_table();
        let file_dir = out_dir.join(&file.stem);
        std::fs::create_dir_all(&file_dir)?;
        std::fs::write(
            file_dir.join("index.html"),
            render_index_html_with(file, &table),
        )?;
        let siblings: Vec<(String, String)> = file
            .items
            .iter()
            .map(|i| (i.slug.clone(), format!("{} {}", i.kind.label(), i.name)))
            .collect();
        for item in &file.items {
            let page = render_html_with(item, &file.stem, &siblings, "index.html", &table);
            std::fs::write(file_dir.join(format!("{}.html", item.slug)), page)?;
        }
    }
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

/// Version string for the gruel compiler.
///
/// Includes the git SHA and (if applicable) a `+dirty` marker, embedded
/// by `build.rs` per ADR-0074. These are diagnostic only — they let users
/// answer "which build of gruel am I running, and why did my cache
/// invalidate?" — and are NOT mixed into cache keys themselves (the
/// binary-bytes hash already covers everything they encode).
const VERSION: &str = env!("GRUEL_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "gruel",
    version = VERSION,
    about = "Gruel compiler",
    long_about = "Gruel compiler.\n\nGlobal options (--log-level, --log-format) work before or after the subcommand. \
                  Each subcommand has its own option set.",
    disable_help_subcommand = true,
)]
struct Cli {
    /// Set logging level.
    #[arg(long, value_name = "LEVEL", default_value = "off", global = true)]
    log_level: LogLevel,

    /// Set logging format.
    #[arg(long, value_name = "FMT", default_value = "text", global = true)]
    log_format: LogFormat,

    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Compile sources to a binary.
    Build(BuildArgs),
    /// Compile sources to a temporary binary and execute it.
    Run(RunArgs),
    /// Type-check sources without producing a binary.
    Check(CheckArgs),
    /// Generate documentation (ADR-0089).
    Doc(DocArgs),
    /// Emit intermediate representation(s) and exit.
    Emit(EmitArgs),
    /// Manage the incremental compilation cache (ADR-0074).
    Cache {
        #[command(subcommand)]
        action: CacheActionArgs,
    },
}

#[derive(Args, Debug)]
struct BuildArgs {
    /// Source files to compile. Multiple files require -o/--output.
    sources: Vec<String>,

    /// Output binary path (required for multiple source files).
    #[arg(short, long, value_name = "PATH")]
    output: Option<String>,

    /// Compilation target.
    #[arg(long, value_name = "TARGET", default_value_t = Target::host())]
    target: Target,

    /// Linker to use: "internal" or a system command like "clang".
    #[arg(long, value_name = "LINKER")]
    linker: Option<String>,

    /// Optimization level (0..3).
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u8).range(0..=3),
        conflicts_with_all = ["debug", "release"],
    )]
    opt_level: Option<u8>,

    /// Build without optimizations (equivalent to --opt-level=0).
    #[arg(long, conflicts_with = "release")]
    debug: bool,

    /// Build with full optimizations (equivalent to --opt-level=3).
    #[arg(long)]
    release: bool,

    /// Number of parallel jobs (0 = auto-detect).
    #[arg(short = 'j', long, value_name = "N", default_value_t = 0)]
    jobs: usize,

    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,

    /// Suppress stderr printing of comptime @dbg output (still buffered).
    #[arg(long)]
    capture_comptime_dbg: bool,

    /// Show timing for each compilation pass.
    #[arg(long)]
    time_passes: bool,

    /// Output timing as JSON (for benchmarking).
    #[arg(long)]
    benchmark_json: bool,

    /// Cache directory for incremental compilation (ADR-0074).
    /// Defaults to `target/gruel-cache/` next to the first source file.
    /// Also overridable via `GRUEL_CACHE_DIR` env var. Ignored when
    /// `--no-cache` is set.
    #[arg(long, value_name = "PATH", env = "GRUEL_CACHE_DIR")]
    cache_dir: Option<String>,

    /// Disable the incremental-compilation cache for this build
    /// (ADR-0074). Overrides `--cache-dir` and `GRUEL_CACHE_DIR`.
    #[arg(long)]
    no_cache: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Source files to compile.
    sources: Vec<String>,

    /// Arguments to forward to the compiled program (everything after `--`).
    #[arg(last = true)]
    program_args: Vec<String>,

    /// Compilation target.
    #[arg(long, value_name = "TARGET", default_value_t = Target::host())]
    target: Target,

    /// Linker to use: "internal" or a system command like "clang".
    #[arg(long, value_name = "LINKER")]
    linker: Option<String>,

    /// Optimization level (0..3).
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u8).range(0..=3),
        conflicts_with_all = ["debug", "release"],
    )]
    opt_level: Option<u8>,

    /// Build without optimizations (equivalent to --opt-level=0).
    #[arg(long, conflicts_with = "release")]
    debug: bool,

    /// Build with full optimizations (equivalent to --opt-level=3).
    #[arg(long)]
    release: bool,

    /// Number of parallel jobs (0 = auto-detect).
    #[arg(short = 'j', long, value_name = "N", default_value_t = 0)]
    jobs: usize,

    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,

    /// Suppress stderr printing of comptime @dbg output (still buffered).
    #[arg(long)]
    capture_comptime_dbg: bool,

    /// Show timing for each compilation pass.
    #[arg(long)]
    time_passes: bool,

    /// Cache directory for incremental compilation (ADR-0074).
    #[arg(long, value_name = "PATH", env = "GRUEL_CACHE_DIR")]
    cache_dir: Option<String>,

    /// Disable the incremental-compilation cache for this build (ADR-0074).
    #[arg(long)]
    no_cache: bool,
}

#[derive(Args, Debug)]
struct CheckArgs {
    /// Source files to type-check.
    sources: Vec<String>,

    /// Compilation target (affects `@target_arch()` / `@target_os()`).
    #[arg(long, value_name = "TARGET", default_value_t = Target::host())]
    target: Target,

    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,

    /// Suppress stderr printing of comptime @dbg output (still buffered).
    #[arg(long)]
    capture_comptime_dbg: bool,

    /// Show timing for each compilation pass.
    #[arg(long)]
    time_passes: bool,

    /// Output timing as JSON (for benchmarking).
    #[arg(long)]
    benchmark_json: bool,
}

#[derive(Args, Debug)]
struct DocArgs {
    /// Source files to document.
    sources: Vec<String>,

    /// Output format.
    #[arg(long, value_name = "FORMAT", default_value = "markdown")]
    format: DocFormat,

    /// Output directory.
    #[arg(long, value_name = "DIR", default_value = "target/doc")]
    output_dir: String,

    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,
}

#[derive(Args, Debug)]
struct EmitArgs {
    /// Comma-separated list of stages: tokens,ast,rir,air,cfg,asm
    #[arg(value_name = "STAGES")]
    stages: String,

    /// Source files to compile.
    sources: Vec<String>,

    /// Compilation target.
    #[arg(long, value_name = "TARGET", default_value_t = Target::host())]
    target: Target,

    /// Optimization level (0..3).
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u8).range(0..=3),
        conflicts_with_all = ["debug", "release"],
    )]
    opt_level: Option<u8>,

    /// Build without optimizations (equivalent to --opt-level=0).
    #[arg(long, conflicts_with = "release")]
    debug: bool,

    /// Build with full optimizations (equivalent to --opt-level=3).
    #[arg(long)]
    release: bool,

    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,

    /// Suppress stderr printing of comptime @dbg output (still buffered).
    #[arg(long)]
    capture_comptime_dbg: bool,

    /// Show timing for each compilation pass.
    #[arg(long)]
    time_passes: bool,

    /// Output timing as JSON (for benchmarking).
    #[arg(long)]
    benchmark_json: bool,
}

#[derive(Subcommand, Debug)]
enum CacheActionArgs {
    /// Print cache statistics.
    Stats {
        /// Cache directory (defaults to `target/gruel-cache`).
        #[arg(
            long,
            value_name = "PATH",
            env = "GRUEL_CACHE_DIR",
            default_value = "target/gruel-cache"
        )]
        cache_dir: String,
    },
    /// Wipe the cache directory.
    Clean {
        /// Cache directory (defaults to `target/gruel-cache`).
        #[arg(
            long,
            value_name = "PATH",
            env = "GRUEL_CACHE_DIR",
            default_value = "target/gruel-cache"
        )]
        cache_dir: String,
    },
}

/// Resolved global options (the bits independent of subcommand).
struct GlobalOpts {
    log_level: LogLevel,
    log_format: LogFormat,
}

/// Resolved options for `gruel build`.
struct BuildOpts {
    source_paths: Vec<String>,
    output_path: String,
    target: Target,
    linker: LinkerMode,
    opt_level: OptLevel,
    jobs: usize,
    preview_features: PreviewFeatures,
    capture_comptime_dbg: bool,
    time_passes: bool,
    benchmark_json: bool,
    cache_dir: Option<String>,
    no_cache: bool,
}

/// Resolved options for `gruel run`.
struct RunOpts {
    source_paths: Vec<String>,
    program_args: Vec<String>,
    target: Target,
    linker: LinkerMode,
    opt_level: OptLevel,
    jobs: usize,
    preview_features: PreviewFeatures,
    capture_comptime_dbg: bool,
    time_passes: bool,
    cache_dir: Option<String>,
    no_cache: bool,
}

/// Resolved options for `gruel check`.
struct CheckOpts {
    source_paths: Vec<String>,
    target: Target,
    preview_features: PreviewFeatures,
    capture_comptime_dbg: bool,
    time_passes: bool,
    benchmark_json: bool,
}

/// Resolved options for `gruel doc`.
struct DocOpts {
    source_paths: Vec<String>,
    format: DocFormat,
    output_dir: String,
    preview_features: PreviewFeatures,
}

/// Resolved options for `gruel emit`.
struct EmitOpts {
    source_paths: Vec<String>,
    stages: Vec<EmitStage>,
    target: Target,
    opt_level: OptLevel,
    preview_features: PreviewFeatures,
    capture_comptime_dbg: bool,
    time_passes: bool,
    benchmark_json: bool,
}

/// Resolved options for `gruel cache <action>`.
enum CacheOpts {
    Stats { cache_dir: String },
    Clean { cache_dir: String },
}

/// The fully-resolved CLI command, after defaults/conflicts have been
/// normalized.
enum ResolvedCommand {
    Build(BuildOpts),
    Run(RunOpts),
    Check(CheckOpts),
    Doc(DocOpts),
    Emit(EmitOpts),
    Cache(CacheOpts),
}

struct ParsedCli {
    globals: GlobalOpts,
    command: ResolvedCommand,
}

/// Result of parsing command-line arguments.
enum ParseResult {
    Ok(ParsedCli),
    /// Parsing failed with an error (already printed).
    Error,
    /// User requested help or version (already printed, should exit 0).
    Exit,
}

fn resolve_opt_level(opt_level: Option<u8>, debug: bool, release: bool) -> OptLevel {
    if debug {
        OptLevel::O0
    } else if release {
        OptLevel::O3
    } else {
        match opt_level {
            Some(0) => OptLevel::O0,
            Some(1) => OptLevel::O1,
            Some(2) => OptLevel::O2,
            Some(3) => OptLevel::O3,
            None => OptLevel::default(),
            Some(_) => unreachable!("clap value_parser bounds to 0..=3"),
        }
    }
}

fn resolve_linker(linker: Option<String>) -> LinkerMode {
    match linker.as_deref() {
        None => LinkerMode::default(),
        Some("internal") => LinkerMode::Internal,
        Some(cmd) => LinkerMode::System(cmd.to_string()),
    }
}

fn resolve_build(args: BuildArgs) -> Result<BuildOpts, String> {
    let opt_level = resolve_opt_level(args.opt_level, args.debug, args.release);
    let linker = resolve_linker(args.linker);
    let preview_features: PreviewFeatures = args.preview.into_iter().collect();

    // Determine source paths and output path. Mirrors the legacy
    // single-binary CLI: with `-o`, all positionals are sources; without
    // `-o`, one positional is "source + default a.out", two positionals
    // are "source + output", three+ is an error.
    let (source_paths, output_path) = if let Some(out) = args.output {
        if args.sources.is_empty() {
            return Err("Error: No source file specified".to_string());
        }
        (args.sources, out)
    } else {
        match args.sources.len() {
            0 => return Err("Error: No source file specified".to_string()),
            1 => (args.sources, "a.out".to_string()),
            2 => {
                let mut s = args.sources;
                let out = s.pop().unwrap();
                (s, out)
            }
            _ => {
                return Err(
                    "Error: multiple source files require -o to specify output path\n\
                     Usage: gruel build a.gruel b.gruel -o output"
                        .to_string(),
                );
            }
        }
    };

    Ok(BuildOpts {
        source_paths,
        output_path,
        target: args.target,
        linker,
        opt_level,
        jobs: args.jobs,
        preview_features,
        capture_comptime_dbg: args.capture_comptime_dbg,
        time_passes: args.time_passes,
        benchmark_json: args.benchmark_json,
        cache_dir: args.cache_dir,
        no_cache: args.no_cache,
    })
}

fn resolve_run(args: RunArgs) -> Result<RunOpts, String> {
    if args.sources.is_empty() {
        return Err("Error: No source file specified".to_string());
    }
    let opt_level = resolve_opt_level(args.opt_level, args.debug, args.release);
    let linker = resolve_linker(args.linker);
    let preview_features: PreviewFeatures = args.preview.into_iter().collect();
    Ok(RunOpts {
        source_paths: args.sources,
        program_args: args.program_args,
        target: args.target,
        linker,
        opt_level,
        jobs: args.jobs,
        preview_features,
        capture_comptime_dbg: args.capture_comptime_dbg,
        time_passes: args.time_passes,
        cache_dir: args.cache_dir,
        no_cache: args.no_cache,
    })
}

fn resolve_check(args: CheckArgs) -> Result<CheckOpts, String> {
    if args.sources.is_empty() {
        return Err("Error: No source file specified".to_string());
    }
    let preview_features: PreviewFeatures = args.preview.into_iter().collect();
    Ok(CheckOpts {
        source_paths: args.sources,
        target: args.target,
        preview_features,
        capture_comptime_dbg: args.capture_comptime_dbg,
        time_passes: args.time_passes,
        benchmark_json: args.benchmark_json,
    })
}

fn resolve_doc(args: DocArgs) -> Result<DocOpts, String> {
    if args.sources.is_empty() {
        return Err("Error: No source file specified".to_string());
    }
    let preview_features: PreviewFeatures = args.preview.into_iter().collect();
    Ok(DocOpts {
        source_paths: args.sources,
        format: args.format,
        output_dir: args.output_dir,
        preview_features,
    })
}

fn resolve_emit(args: EmitArgs) -> Result<EmitOpts, String> {
    if args.sources.is_empty() {
        return Err("Error: No source file specified".to_string());
    }
    let opt_level = resolve_opt_level(args.opt_level, args.debug, args.release);
    let preview_features: PreviewFeatures = args.preview.into_iter().collect();
    let mut stages = Vec::new();
    for part in args.stages.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return Err("Error: empty stage in --emit list".to_string());
        }
        stages.push(EmitStage::from_name(trimmed).map_err(|e| format!("Error: {}", e))?);
    }
    Ok(EmitOpts {
        source_paths: args.sources,
        stages,
        target: args.target,
        opt_level,
        preview_features,
        capture_comptime_dbg: args.capture_comptime_dbg,
        time_passes: args.time_passes,
        benchmark_json: args.benchmark_json,
    })
}

fn resolve_cli(cli: Cli) -> Result<ParsedCli, String> {
    let globals = GlobalOpts {
        log_level: cli.log_level,
        log_format: cli.log_format,
    };
    let command = match cli.command {
        CliCommand::Build(args) => ResolvedCommand::Build(resolve_build(args)?),
        CliCommand::Run(args) => ResolvedCommand::Run(resolve_run(args)?),
        CliCommand::Check(args) => ResolvedCommand::Check(resolve_check(args)?),
        CliCommand::Doc(args) => ResolvedCommand::Doc(resolve_doc(args)?),
        CliCommand::Emit(args) => ResolvedCommand::Emit(resolve_emit(args)?),
        CliCommand::Cache { action } => ResolvedCommand::Cache(match action {
            CacheActionArgs::Stats { cache_dir } => CacheOpts::Stats { cache_dir },
            CacheActionArgs::Clean { cache_dir } => CacheOpts::Clean { cache_dir },
        }),
    };
    Ok(ParsedCli { globals, command })
}

/// Parse CLI arguments into a [`ParsedCli`].
///
/// `argv` accepts anything iterable into [`OsString`] — `std::env::args_os()`
/// at runtime, or a hand-rolled iterator in tests.
fn parse_args<I, T>(argv: I) -> ParseResult
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(argv) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            let _ = e.print();
            return match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => ParseResult::Exit,
                _ => ParseResult::Error,
            };
        }
    };
    match resolve_cli(cli) {
        Ok(parsed) => ParseResult::Ok(parsed),
        Err(msg) => {
            eprintln!("{}", msg);
            ParseResult::Error
        }
    }
}

/// Initialize the tracing subscriber based on CLI options and RUST_LOG.
///
/// Priority: RUST_LOG environment variable takes precedence over --log-level flag.
/// If neither is set and log_level is Off, no subscriber is installed (unless
/// `time_passes` or `benchmark_json` is true, in which case a timing-only subscriber is installed).
///
/// Returns `Some(TimingData)` if `time_passes` or `benchmark_json` is true, which can be used to
/// retrieve the timing report after compilation completes.
fn init_tracing(
    log_level: LogLevel,
    log_format: LogFormat,
    time_passes: bool,
    benchmark_json: bool,
) -> Option<timing::TimingData> {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;

    // RUST_LOG takes priority over --log-level.
    let rust_log = std::env::var("RUST_LOG").ok();
    let logging_enabled = rust_log.is_some() || log_level.to_tracing_level().is_some();
    let needs_timing = time_passes || benchmark_json;

    // No subscriber needed when neither feature is on.
    if !logging_enabled && !needs_timing {
        return None;
    }

    let timing_data = needs_timing.then(timing::TimingData::new);

    let filter = logging_enabled.then(|| match rust_log {
        Some(value) => EnvFilter::try_new(&value).unwrap_or_else(|e| {
            eprintln!("Warning: invalid RUST_LOG value, using default: {}", e);
            EnvFilter::new(
                log_level
                    .to_tracing_level()
                    .unwrap_or(Level::INFO)
                    .to_string(),
            )
        }),
        None => EnvFilter::new(
            log_level
                .to_tracing_level()
                .unwrap_or(Level::INFO)
                .to_string(),
        ),
    });

    let fmt_layer = logging_enabled.then(|| {
        let layer = fmt::layer()
            .with_target(true)
            .with_span_events(FmtSpan::CLOSE)
            .with_writer(std::io::stderr);
        match log_format {
            LogFormat::Text => layer.boxed(),
            LogFormat::Json => layer.json().boxed(),
        }
    });

    let timing_layer = timing_data.clone().map(timing::TimingLayer::new);

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(timing_layer);
    tracing::subscriber::set_global_default(subscriber).expect("failed to set tracing subscriber");

    timing_data
}

/// Print timing output based on CLI flags.
fn print_timing_output(
    timing_data: &Option<timing::TimingData>,
    time_passes: bool,
    benchmark_json: bool,
    target: &Target,
    source_metrics: Option<timing::SourceMetrics>,
) {
    if let Some(timing) = timing_data {
        if benchmark_json {
            // JSON output goes to stdout for easy capture
            // Include metadata and source metrics for historical analysis
            println!(
                "{}",
                timing.to_json_with_metrics(
                    &target.to_string(),
                    VERSION,
                    source_metrics,
                    get_peak_memory_bytes(),
                )
            );
        } else if time_passes {
            // Human-readable output goes to stderr
            eprintln!("{}", timing.report());
        }
    }
}

/// Get peak memory usage in bytes (platform-specific).
///
/// Returns None if memory usage cannot be determined.
fn get_peak_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // On Linux, read from /proc/self/status
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmHWM:") {
                    // VmHWM is "high water mark" - peak resident set size
                    // Format: "VmHWM:     12345 kB"
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return Some(kb * 1024);
                        }
                    }
                }
            }
        }
        None
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, use rusage
        use std::mem::MaybeUninit;
        let mut rusage = MaybeUninit::uninit();
        // SAFETY: rusage is properly aligned and getrusage is a standard POSIX call
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, rusage.as_mut_ptr()) };
        if result == 0 {
            // SAFETY: getrusage succeeded, so rusage is initialized
            let rusage = unsafe { rusage.assume_init() };
            // ru_maxrss is in bytes on macOS (unlike Linux where it's in KB)
            Some(rusage.ru_maxrss as u64)
        } else {
            None
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn main() {
    let parsed = match parse_args(std::env::args_os()) {
        ParseResult::Ok(p) => p,
        ParseResult::Exit => std::process::exit(0),
        ParseResult::Error => std::process::exit(1),
    };

    // Decide tracing/timing needs from the chosen subcommand.
    let (time_passes, benchmark_json) = match &parsed.command {
        ResolvedCommand::Build(o) => (o.time_passes, o.benchmark_json),
        ResolvedCommand::Run(o) => (o.time_passes, false),
        ResolvedCommand::Check(o) => (o.time_passes, o.benchmark_json),
        ResolvedCommand::Emit(o) => (o.time_passes, o.benchmark_json),
        ResolvedCommand::Doc(_) | ResolvedCommand::Cache(_) => (false, false),
    };

    let timing_data = init_tracing(
        parsed.globals.log_level,
        parsed.globals.log_format,
        time_passes,
        benchmark_json,
    );

    match parsed.command {
        ResolvedCommand::Build(opts) => run_build(opts, timing_data),
        ResolvedCommand::Run(opts) => run_run(opts, timing_data),
        ResolvedCommand::Check(opts) => run_check(opts, timing_data),
        ResolvedCommand::Doc(opts) => run_doc(&opts),
        ResolvedCommand::Emit(opts) => run_emit(opts, timing_data),
        ResolvedCommand::Cache(CacheOpts::Stats { cache_dir }) => {
            run_cache_stats(std::path::Path::new(&cache_dir));
        }
        ResolvedCommand::Cache(CacheOpts::Clean { cache_dir }) => {
            run_cache_clean(std::path::Path::new(&cache_dir));
        }
    }
}

fn run_build(opts: BuildOpts, timing_data: Option<timing::TimingData>) {
    // Read all source files into memory
    let sources: Vec<(String, String)> = opts
        .source_paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("Error reading {}: {}", path, e);
                std::process::exit(1);
            });
            (path.clone(), content)
        })
        .collect();

    // Build SourceFile structs for multi-file compilation
    let source_files: Vec<SourceFile<'_>> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            SourceFile::new(path.as_str(), content.as_str(), FileId::new((i + 1) as u32))
        })
        .collect();

    // Create multi-file formatter for diagnostics that may span multiple files
    let source_infos: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            (
                FileId::new((i + 1) as u32),
                SourceInfo::new(content.as_str(), path.as_str()),
            )
        })
        .collect();
    let formatter = MultiFileFormatter::new(source_infos);

    // Compute source metrics if benchmark JSON is requested
    let (_primary_path, primary_source) = &sources[0];
    let source_metrics = if opts.benchmark_json {
        let lexer = Lexer::new(primary_source);
        let token_count = match lexer.tokenize() {
            Ok((tokens, _interner)) => tokens.len(),
            Err(_) => 0,
        };
        Some(timing::SourceMetrics {
            bytes: primary_source.len(),
            lines: primary_source.lines().count(),
            tokens: token_count,
        })
    } else {
        None
    };

    // ADR-0074: incremental compilation cache is enabled by default and
    // routed through `cache_dir`. `--no-cache` disables it for a single
    // build. When no explicit cache_dir / GRUEL_CACHE_DIR is provided,
    // fall back to `target/gruel-cache/` next to the first source file.
    let resolved_cache_dir = if opts.no_cache {
        None
    } else {
        Some(match &opts.cache_dir {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let first = std::path::Path::new(&opts.source_paths[0]);
                first
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("target")
                    .join("gruel-cache")
            }
        })
    };
    let compile_options = CompileOptions {
        target: opts.target.clone(),
        linker: opts.linker.clone(),
        opt_level: opts.opt_level,
        preview_features: opts.preview_features.clone(),
        jobs: opts.jobs,
        capture_comptime_dbg: opts.capture_comptime_dbg,
        cache_dir: resolved_cache_dir,
    };
    match compile_multi_file_with_options(&source_files, &compile_options) {
        Ok(output) => {
            if !output.warnings.is_empty() {
                eprintln!("{}", formatter.format_warnings(&output.warnings));
            }

            if let Err(e) = fs::write(&opts.output_path, &output.elf) {
                eprintln!("Error writing {}: {}", opts.output_path, e);
                std::process::exit(1);
            }

            // Make executable (Unix only)
            #[cfg(unix)]
            {
                let path = Path::new(&opts.output_path);
                match fs::metadata(path) {
                    Ok(metadata) => {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o755);
                        if let Err(e) = fs::set_permissions(path, perms) {
                            eprintln!(
                                "Warning: could not set executable permissions on {}: {}",
                                opts.output_path, e
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: could not read file metadata for {}: {}",
                            opts.output_path, e
                        );
                    }
                }
            }

            // Ad-hoc codesign for macOS (required for executables to run on ARM64)
            #[cfg(target_os = "macos")]
            {
                if compile_options.target.is_macho() {
                    let result = Command::new("codesign")
                        .args(["-f", "-s", "-", &opts.output_path])
                        .output();
                    match result {
                        Ok(output) => {
                            if !output.status.success() {
                                eprintln!(
                                    "Warning: codesign failed: {}",
                                    String::from_utf8_lossy(&output.stderr)
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: could not run codesign: {}", e);
                        }
                    }
                }
            }

            // Don't print normal compilation message when using --benchmark-json
            // as it would interfere with JSON parsing
            if !opts.benchmark_json {
                let linker_str = match &opts.linker {
                    LinkerMode::Internal => "internal".to_string(),
                    LinkerMode::System(cmd) => cmd.clone(),
                };
                let source_str = if opts.source_paths.len() == 1 {
                    opts.source_paths[0].clone()
                } else {
                    format!("{} files", opts.source_paths.len())
                };
                println!(
                    "Compiled {} -> {} (target: {}, linker: {})",
                    source_str, opts.output_path, opts.target, linker_str
                );
            }

            print_timing_output(
                &timing_data,
                opts.time_passes,
                opts.benchmark_json,
                &opts.target,
                source_metrics,
            );
        }
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    }
}

fn run_emit(opts: EmitOpts, timing_data: Option<timing::TimingData>) {
    // Read all source files into memory
    let sources: Vec<(String, String)> = opts
        .source_paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("Error reading {}: {}", path, e);
                std::process::exit(1);
            });
            (path.clone(), content)
        })
        .collect();

    let source_files: Vec<SourceFile<'_>> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            SourceFile::new(path.as_str(), content.as_str(), FileId::new((i + 1) as u32))
        })
        .collect();

    let source_infos: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            (
                FileId::new((i + 1) as u32),
                SourceInfo::new(content.as_str(), path.as_str()),
            )
        })
        .collect();
    let formatter = MultiFileFormatter::new(source_infos);

    let (_primary_path, primary_source) = &sources[0];
    let source_metrics = if opts.benchmark_json {
        let lexer = Lexer::new(primary_source);
        let token_count = match lexer.tokenize() {
            Ok((tokens, _interner)) => tokens.len(),
            Err(_) => 0,
        };
        Some(timing::SourceMetrics {
            bytes: primary_source.len(),
            lines: primary_source.lines().count(),
            tokens: token_count,
        })
    } else {
        None
    };

    if let Err(()) = emit_stages(&source_files, &opts, &formatter) {
        std::process::exit(1);
    }
    print_timing_output(
        &timing_data,
        opts.time_passes,
        opts.benchmark_json,
        &opts.target,
        source_metrics,
    );
}

fn run_run(opts: RunOpts, timing_data: Option<timing::TimingData>) {
    // `run` is "build to a temp path, exec, forward the exit code".
    // We construct an output path inside `std::env::temp_dir()` keyed by
    // the first source's file stem so successive runs in the same shell
    // session overwrite the same file (and `--cache-dir` can do its job).
    let stem = std::path::Path::new(&opts.source_paths[0])
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "gruel-run".to_string());
    let pid = std::process::id();
    let output_path = std::env::temp_dir().join(format!("gruel-run-{}-{}", stem, pid));

    let build_opts = BuildOpts {
        source_paths: opts.source_paths,
        output_path: output_path.to_string_lossy().into_owned(),
        target: opts.target,
        linker: opts.linker,
        opt_level: opts.opt_level,
        jobs: opts.jobs,
        preview_features: opts.preview_features,
        capture_comptime_dbg: opts.capture_comptime_dbg,
        time_passes: opts.time_passes,
        benchmark_json: false,
        cache_dir: opts.cache_dir,
        no_cache: opts.no_cache,
    };

    // run_build exits the process on compile failure, so if we reach the
    // line after it the binary is on disk and ready to exec.
    run_build(build_opts, timing_data);

    let status = Command::new(&output_path)
        .args(&opts.program_args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Error executing {}: {}", output_path.display(), e);
            let _ = fs::remove_file(&output_path);
            std::process::exit(1);
        });

    let _ = fs::remove_file(&output_path);

    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    // Killed by a signal — match shell convention (128 + signal).
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            std::process::exit(128 + sig);
        }
    }
    std::process::exit(1);
}

fn run_check(opts: CheckOpts, timing_data: Option<timing::TimingData>) {
    use gruel_compiler::{merge_symbols, parse_all_files_with_preview};

    let sources: Vec<(String, String)> = opts
        .source_paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("Error reading {}: {}", path, e);
                std::process::exit(1);
            });
            (path.clone(), content)
        })
        .collect();

    let source_files: Vec<SourceFile<'_>> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            SourceFile::new(path.as_str(), content.as_str(), FileId::new((i + 1) as u32))
        })
        .collect();

    let source_infos: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            (
                FileId::new((i + 1) as u32),
                SourceInfo::new(content.as_str(), path.as_str()),
            )
        })
        .collect();
    let formatter = MultiFileFormatter::new(source_infos);

    let (_primary_path, primary_source) = &sources[0];
    let source_metrics = if opts.benchmark_json {
        let lexer = Lexer::new(primary_source);
        let token_count = match lexer.tokenize() {
            Ok((tokens, _interner)) => tokens.len(),
            Err(_) => 0,
        };
        Some(timing::SourceMetrics {
            bytes: primary_source.len(),
            lines: primary_source.lines().count(),
            tokens: token_count,
        })
    } else {
        None
    };

    let parsed = match parse_all_files_with_preview(&source_files, &opts.preview_features) {
        Ok(p) => p,
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    };
    let merged = match merge_symbols(parsed) {
        Ok(m) => m,
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    };
    let state = match compile_frontend_from_ast_with_options_full_target(
        merged.ast,
        merged.interner,
        &opts.preview_features,
        opts.capture_comptime_dbg,
        &opts.target,
    ) {
        Ok(state) => state,
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    };

    if !state.warnings.is_empty() {
        eprintln!("{}", formatter.format_warnings(&state.warnings));
    }

    if !opts.benchmark_json {
        let source_str = if opts.source_paths.len() == 1 {
            opts.source_paths[0].clone()
        } else {
            format!("{} files", opts.source_paths.len())
        };
        println!("Checked {} (target: {})", source_str, opts.target);
    }

    print_timing_output(
        &timing_data,
        opts.time_passes,
        opts.benchmark_json,
        &opts.target,
        source_metrics,
    );
}

/// Run the pipeline up to each requested stage and print the output.
///
/// For early stages (tokens, ast), each file is processed and labeled individually.
/// For later stages (rir, air, cfg, asm), the merged program is used.
fn emit_stages(
    sources: &[SourceFile<'_>],
    opts: &EmitOpts,
    formatter: &MultiFileFormatter,
) -> Result<(), ()> {
    let needs_tokens = opts.stages.contains(&EmitStage::Tokens);
    let needs_ast = opts.stages.contains(&EmitStage::Ast);
    let needs_later_stages = opts.stages.iter().any(|s| {
        matches!(
            s,
            EmitStage::Rir | EmitStage::Air | EmitStage::Cfg | EmitStage::Asm
        )
    });

    // For tokens, we need to lex each file separately (before parsing merges interners)
    let per_file_tokens: Option<Vec<(String, Vec<gruel_compiler::Token>)>> = if needs_tokens {
        let mut file_tokens = Vec::with_capacity(sources.len());
        for source in sources {
            let lexer = Lexer::new(source.source);
            match lexer.tokenize() {
                Ok((tokens, _interner)) => {
                    file_tokens.push((source.path.to_string(), tokens));
                }
                Err(e) => {
                    eprintln!("{}", formatter.format_error(&e));
                    return Err(());
                }
            }
        }
        Some(file_tokens)
    } else {
        None
    };

    // Parse all files (needed for AST output or later stages)
    let mut parsed: Option<ParsedProgram> = if needs_ast || needs_later_stages {
        match gruel_compiler::parse_all_files_with_preview(sources, &opts.preview_features) {
            Ok(program) => Some(program),
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        }
    } else {
        None
    };

    // For AST output, collect the per-file AST info before merging (which consumes the program)
    let per_file_asts: Option<Vec<(String, gruel_compiler::Ast)>> = if needs_ast {
        parsed.as_ref().map(|program| {
            program
                .files
                .iter()
                .map(|f| (f.path.clone(), f.ast.clone()))
                .collect()
        })
    } else {
        None
    };

    let frontend_state = if needs_later_stages {
        let program = parsed
            .take()
            .expect("parsed should be Some when needs_later_stages is true");

        let merged = match merge_symbols(program) {
            Ok(m) => m,
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        };

        let state = match compile_frontend_from_ast_with_options_full_target(
            merged.ast,
            merged.interner,
            &opts.preview_features,
            opts.capture_comptime_dbg,
            &opts.target,
        ) {
            Ok(state) => state,
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        };

        Some(state)
    } else {
        None
    };

    for stage in &opts.stages {
        match stage {
            EmitStage::Tokens => {
                if let Some(ref file_tokens) = per_file_tokens {
                    for (path, tokens) in file_tokens {
                        println!("=== Tokens ({}) ===", path);
                        for token in tokens {
                            println!("{}", token);
                        }
                        println!();
                    }
                }
            }
            EmitStage::Ast => {
                if let Some(ref asts) = per_file_asts {
                    for (path, ast) in asts {
                        println!("=== AST ({}) ===", path);
                        print!("{}", ast);
                        println!();
                    }
                }
            }
            EmitStage::Rir => {
                println!("=== RIR ===");
                if let Some(ref state) = frontend_state {
                    let printer = RirPrinter::new(&state.rir, &state.interner);
                    println!("{}", printer);
                }
                println!();
            }
            EmitStage::Air => {
                println!("=== AIR ===");
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!("function {}:", func.analyzed.name);
                        println!("{}", func.analyzed.air);
                    }
                }
                println!();
            }
            EmitStage::Cfg => {
                println!("=== CFG ===");
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!("{}", func.cfg);
                    }
                }
                println!();
            }
            EmitStage::Asm => {
                println!("=== LLVM IR ===");
                if let Some(ref state) = frontend_state {
                    let inputs = gruel_compiler::BackendInputs {
                        functions: &state.functions,
                        type_pool: &state.type_pool,
                        strings: &state.strings,
                        bytes: &state.bytes,
                        interner: &state.interner,
                        interface_defs: &state.interface_defs,
                        interface_vtables: &state.interface_vtables,
                        target: &opts.target,
                        // ADR-0085: emit shows IR pre-link; library
                        // flags are linker-only so they don't matter here.
                        extra_link_libraries: &[],
                    };
                    match generate_llvm_ir(&inputs, opts.opt_level) {
                        Ok(ir) => print!("{}", ir),
                        Err(e) => {
                            eprintln!("{}", formatter.format_error(&e));
                            return Err(());
                        }
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `parse_args` from a slice of `&str`, prepending the program name
    /// the same way `std::env::args_os()` would.
    fn parse_args_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    /// Parse args under the `build` subcommand. Most option-parsing tests
    /// drive this so we don't have to prefix every fixture with "build".
    fn parse_build_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(std::iter::once("build".to_string()))
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    fn parse_emit_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(std::iter::once("emit".to_string()))
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    fn parse_doc_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(std::iter::once("doc".to_string()))
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    fn parse_cache_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(std::iter::once("cache".to_string()))
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    fn parse_run_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(std::iter::once("run".to_string()))
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    fn parse_check_from(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = std::iter::once("gruel".to_string())
            .chain(std::iter::once("check".to_string()))
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_args(argv)
    }

    fn unwrap_build(result: ParseResult) -> (GlobalOpts, BuildOpts) {
        match result {
            ParseResult::Ok(ParsedCli {
                globals,
                command: ResolvedCommand::Build(opts),
            }) => (globals, opts),
            ParseResult::Ok(_) => panic!("Expected Build, got different subcommand"),
            ParseResult::Error => panic!("Expected Build, got Error"),
            ParseResult::Exit => panic!("Expected Build, got Exit"),
        }
    }

    fn unwrap_build_opts(result: ParseResult) -> BuildOpts {
        unwrap_build(result).1
    }

    fn unwrap_emit_opts(result: ParseResult) -> EmitOpts {
        match result {
            ParseResult::Ok(ParsedCli {
                command: ResolvedCommand::Emit(opts),
                ..
            }) => opts,
            ParseResult::Ok(_) => panic!("Expected Emit, got different subcommand"),
            ParseResult::Error => panic!("Expected Emit, got Error"),
            ParseResult::Exit => panic!("Expected Emit, got Exit"),
        }
    }

    fn unwrap_doc_opts(result: ParseResult) -> DocOpts {
        match result {
            ParseResult::Ok(ParsedCli {
                command: ResolvedCommand::Doc(opts),
                ..
            }) => opts,
            ParseResult::Ok(_) => panic!("Expected Doc, got different subcommand"),
            ParseResult::Error => panic!("Expected Doc, got Error"),
            ParseResult::Exit => panic!("Expected Doc, got Exit"),
        }
    }

    fn unwrap_cache_opts(result: ParseResult) -> CacheOpts {
        match result {
            ParseResult::Ok(ParsedCli {
                command: ResolvedCommand::Cache(opts),
                ..
            }) => opts,
            ParseResult::Ok(_) => panic!("Expected Cache, got different subcommand"),
            ParseResult::Error => panic!("Expected Cache, got Error"),
            ParseResult::Exit => panic!("Expected Cache, got Exit"),
        }
    }

    fn unwrap_run_opts(result: ParseResult) -> RunOpts {
        match result {
            ParseResult::Ok(ParsedCli {
                command: ResolvedCommand::Run(opts),
                ..
            }) => opts,
            ParseResult::Ok(_) => panic!("Expected Run, got different subcommand"),
            ParseResult::Error => panic!("Expected Run, got Error"),
            ParseResult::Exit => panic!("Expected Run, got Exit"),
        }
    }

    fn unwrap_check_opts(result: ParseResult) -> CheckOpts {
        match result {
            ParseResult::Ok(ParsedCli {
                command: ResolvedCommand::Check(opts),
                ..
            }) => opts,
            ParseResult::Ok(_) => panic!("Expected Check, got different subcommand"),
            ParseResult::Error => panic!("Expected Check, got Error"),
            ParseResult::Exit => panic!("Expected Check, got Exit"),
        }
    }

    fn is_error(result: &ParseResult) -> bool {
        matches!(result, ParseResult::Error)
    }

    fn is_exit(result: &ParseResult) -> bool {
        matches!(result, ParseResult::Exit)
    }

    // ========== Top-level dispatch ==========

    #[test]
    fn no_subcommand_is_error() {
        assert!(is_error(&parse_args_from(&[])));
    }

    #[test]
    fn unknown_subcommand_is_error() {
        assert!(is_error(&parse_args_from(&["banana"])));
    }

    #[test]
    fn build_requires_source() {
        assert!(is_error(&parse_build_from(&[])));
    }

    // ========== gruel build ==========

    #[test]
    fn parse_source_file_only() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_source_and_output_two_positionals() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel", "output"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_multi_file_with_output_flag() {
        let opts = unwrap_build_opts(parse_build_from(&["a.gruel", "b.gruel", "-o", "output"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_multi_file_with_output_long_flag() {
        let opts = unwrap_build_opts(parse_build_from(&["a.gruel", "b.gruel", "--output", "out"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.output_path, "out");
    }

    #[test]
    fn parse_multi_file_without_output_flag_error() {
        // Three positional args without -o should error
        assert!(is_error(&parse_build_from(&[
            "a.gruel", "b.gruel", "c.gruel"
        ])));
    }

    #[test]
    fn parse_multi_file_with_options() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--opt-level=2",
            "main.gruel",
            "utils.gruel",
            "lib.gruel",
            "-o",
            "program",
        ]));
        assert_eq!(
            opts.source_paths,
            vec!["main.gruel", "utils.gruel", "lib.gruel"]
        );
        assert_eq!(opts.output_path, "program");
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    #[test]
    fn parse_output_flag_before_sources() {
        let opts = unwrap_build_opts(parse_build_from(&["-o", "output", "a.gruel", "b.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_single_file_with_output_flag() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel", "-o", "myprogram"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "myprogram");
    }

    #[test]
    fn parse_output_flag_missing_value() {
        assert!(is_error(&parse_build_from(&["source.gruel", "-o"])));
    }

    #[test]
    fn parse_output_long_flag_missing_value() {
        assert!(is_error(&parse_build_from(&["source.gruel", "--output"])));
    }

    // ========== build defaults ==========

    #[test]
    fn parse_defaults_output_path() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_defaults_opt_level() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_defaults_linker() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::Internal);
    }

    #[test]
    fn parse_defaults_time_passes() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert!(!opts.time_passes);
    }

    #[test]
    fn parse_defaults_benchmark_json() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert!(!opts.benchmark_json);
    }

    #[test]
    fn parse_defaults_jobs() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert_eq!(opts.jobs, 0);
    }

    // ========== build: --target ==========

    #[test]
    fn parse_target_x86_64_linux() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
    }

    #[test]
    fn parse_target_aarch64_macos() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--target",
            "aarch64-macos",
            "source.gruel",
        ]));
        assert_eq!(opts.target, "aarch64-macos".parse::<Target>().unwrap());
    }

    #[test]
    fn parse_target_missing_value() {
        assert!(is_error(&parse_build_from(&["source.gruel", "--target"])));
    }

    #[test]
    fn parse_target_invalid() {
        assert!(is_error(&parse_build_from(&[
            "--target",
            "invalid",
            "source.gruel"
        ])));
    }

    // ========== build: --linker ==========

    #[test]
    fn parse_linker_internal() {
        let opts = unwrap_build_opts(parse_build_from(&["--linker", "internal", "source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::Internal);
    }

    #[test]
    fn parse_linker_system_clang() {
        let opts = unwrap_build_opts(parse_build_from(&["--linker", "clang", "source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
    }

    #[test]
    fn parse_linker_system_gcc() {
        let opts = unwrap_build_opts(parse_build_from(&["--linker", "gcc", "source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::System("gcc".to_string()));
    }

    #[test]
    fn parse_linker_missing_value() {
        assert!(is_error(&parse_build_from(&["source.gruel", "--linker"])));
    }

    // ========== build: optimization levels ==========

    #[test]
    fn parse_opt_level_0() {
        let opts = unwrap_build_opts(parse_build_from(&["--opt-level=0", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_opt_level_1() {
        let opts = unwrap_build_opts(parse_build_from(&["--opt-level=1", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn parse_opt_level_2() {
        let opts = unwrap_build_opts(parse_build_from(&["--opt-level=2", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    #[test]
    fn parse_opt_level_3() {
        let opts = unwrap_build_opts(parse_build_from(&["--opt-level=3", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O3);
    }

    #[test]
    fn parse_opt_level_invalid() {
        assert!(is_error(&parse_build_from(&[
            "--opt-level=9",
            "source.gruel"
        ])));
    }

    // ========== build: --debug / --release ==========

    #[test]
    fn parse_debug_flag() {
        let opts = unwrap_build_opts(parse_build_from(&["--debug", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_release_flag() {
        let opts = unwrap_build_opts(parse_build_from(&["--release", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O3);
    }

    #[test]
    fn parse_debug_release_conflict() {
        assert!(is_error(&parse_build_from(&[
            "--debug",
            "--release",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_release_debug_conflict() {
        assert!(is_error(&parse_build_from(&[
            "--release",
            "--debug",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_debug_with_opt_level_conflict() {
        assert!(is_error(&parse_build_from(&[
            "--debug",
            "--opt-level=2",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_release_with_opt_level_conflict() {
        assert!(is_error(&parse_build_from(&[
            "--release",
            "--opt-level=1",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_opt_level_then_debug_conflict() {
        assert!(is_error(&parse_build_from(&[
            "--opt-level=2",
            "--debug",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_opt_level_then_release_conflict() {
        assert!(is_error(&parse_build_from(&[
            "--opt-level=1",
            "--release",
            "source.gruel"
        ])));
    }

    // ========== build: --preview ==========

    #[test]
    fn parse_preview_valid_feature() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--preview",
            "test_infra",
            "source.gruel",
        ]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn parse_preview_multiple_flags() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--preview",
            "test_infra",
            "--preview",
            "test_infra",
            "source.gruel",
        ]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
        assert_eq!(opts.preview_features.len(), 1);
    }

    #[test]
    fn parse_preview_missing_value() {
        assert!(is_error(&parse_build_from(&["source.gruel", "--preview"])));
    }

    #[test]
    fn parse_preview_invalid_feature() {
        assert!(is_error(&parse_build_from(&[
            "--preview",
            "nonexistent",
            "source.gruel"
        ])));
    }

    // ========== build: --cache-dir / --no-cache ==========

    #[test]
    fn cache_dir_accepted() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--cache-dir",
            "/tmp/foo",
            "source.gruel",
        ]));
        assert_eq!(opts.cache_dir.as_deref(), Some("/tmp/foo"));
        assert!(!opts.no_cache);
    }

    #[test]
    fn cache_dir_default_when_omitted() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel"]));
        assert!(opts.cache_dir.is_none());
        assert!(!opts.no_cache);
    }

    #[test]
    fn no_cache_flag_sets_field() {
        let opts = unwrap_build_opts(parse_build_from(&["--no-cache", "source.gruel"]));
        assert!(opts.no_cache);
    }

    // ========== build: --time-passes / --benchmark-json ==========

    #[test]
    fn parse_time_passes() {
        let opts = unwrap_build_opts(parse_build_from(&["--time-passes", "source.gruel"]));
        assert!(opts.time_passes);
    }

    #[test]
    fn parse_time_passes_with_other_options() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--time-passes",
            "--opt-level=2",
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert!(opts.time_passes);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
    }

    #[test]
    fn parse_benchmark_json() {
        let opts = unwrap_build_opts(parse_build_from(&["--benchmark-json", "source.gruel"]));
        assert!(opts.benchmark_json);
    }

    #[test]
    fn parse_benchmark_json_with_other_options() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--benchmark-json",
            "--opt-level=2",
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert!(opts.benchmark_json);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
    }

    #[test]
    fn parse_both_time_passes_and_benchmark_json() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--time-passes",
            "--benchmark-json",
            "source.gruel",
        ]));
        assert!(opts.time_passes);
        assert!(opts.benchmark_json);
    }

    // ========== build: --jobs ==========

    #[test]
    fn parse_jobs_long_form() {
        let opts = unwrap_build_opts(parse_build_from(&["--jobs", "4", "source.gruel"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_short_form() {
        let opts = unwrap_build_opts(parse_build_from(&["-j", "4", "source.gruel"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_attached_form() {
        let opts = unwrap_build_opts(parse_build_from(&["-j4", "source.gruel"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_single_thread() {
        let opts = unwrap_build_opts(parse_build_from(&["-j1", "source.gruel"]));
        assert_eq!(opts.jobs, 1);
    }

    #[test]
    fn parse_jobs_auto_detect() {
        let opts = unwrap_build_opts(parse_build_from(&["--jobs", "0", "source.gruel"]));
        assert_eq!(opts.jobs, 0);
    }

    #[test]
    fn parse_jobs_missing_value() {
        assert!(is_error(&parse_build_from(&["source.gruel", "--jobs"])));
    }

    #[test]
    fn parse_jobs_missing_value_short() {
        assert!(is_error(&parse_build_from(&["source.gruel", "-j"])));
    }

    #[test]
    fn parse_jobs_invalid_value() {
        assert!(is_error(&parse_build_from(&[
            "--jobs",
            "abc",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_jobs_negative_value() {
        assert!(is_error(&parse_build_from(&[
            "--jobs",
            "-1",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_jobs_with_other_options() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "-j4",
            "--opt-level=2",
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert_eq!(opts.jobs, 4);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
    }

    // ========== build: option order ==========

    #[test]
    fn parse_all_options_combined() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--target",
            "x86_64-linux",
            "--linker",
            "clang",
            "--opt-level=2",
            "source.gruel",
            "output",
        ]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "output");
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    #[test]
    fn parse_options_after_source() {
        let opts = unwrap_build_opts(parse_build_from(&["source.gruel", "--opt-level=1"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn parse_mixed_option_positions() {
        let opts = unwrap_build_opts(parse_build_from(&[
            "--opt-level=1",
            "source.gruel",
            "--target",
            "x86_64-linux",
            "output",
        ]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "output");
        assert_eq!(opts.opt_level, OptLevel::O1);
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
    }

    // ========== gruel emit ==========

    #[test]
    fn parse_emit_tokens() {
        let opts = unwrap_emit_opts(parse_emit_from(&["tokens", "source.gruel"]));
        assert_eq!(opts.stages, vec![EmitStage::Tokens]);
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
    }

    #[test]
    fn parse_emit_ast() {
        let opts = unwrap_emit_opts(parse_emit_from(&["ast", "source.gruel"]));
        assert_eq!(opts.stages, vec![EmitStage::Ast]);
    }

    #[test]
    fn parse_emit_rir() {
        let opts = unwrap_emit_opts(parse_emit_from(&["rir", "source.gruel"]));
        assert_eq!(opts.stages, vec![EmitStage::Rir]);
    }

    #[test]
    fn parse_emit_air() {
        let opts = unwrap_emit_opts(parse_emit_from(&["air", "source.gruel"]));
        assert_eq!(opts.stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_emit_cfg() {
        let opts = unwrap_emit_opts(parse_emit_from(&["cfg", "source.gruel"]));
        assert_eq!(opts.stages, vec![EmitStage::Cfg]);
    }

    #[test]
    fn parse_emit_asm() {
        let opts = unwrap_emit_opts(parse_emit_from(&["asm", "source.gruel"]));
        assert_eq!(opts.stages, vec![EmitStage::Asm]);
    }

    #[test]
    fn parse_emit_multiple_stages_comma() {
        let opts = unwrap_emit_opts(parse_emit_from(&["tokens,ast,air", "source.gruel"]));
        assert_eq!(
            opts.stages,
            vec![EmitStage::Tokens, EmitStage::Ast, EmitStage::Air]
        );
    }

    #[test]
    fn parse_emit_missing_value() {
        assert!(is_error(&parse_emit_from(&[])));
    }

    #[test]
    fn parse_emit_invalid_stage() {
        assert!(is_error(&parse_emit_from(&["invalid", "source.gruel"])));
    }

    #[test]
    fn parse_emit_missing_source() {
        assert!(is_error(&parse_emit_from(&["air"])));
    }

    #[test]
    fn parse_emit_multi_file() {
        let opts = unwrap_emit_opts(parse_emit_from(&["air", "a.gruel", "b.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_emit_with_target_and_opt() {
        let opts = unwrap_emit_opts(parse_emit_from(&[
            "asm",
            "--target",
            "x86_64-linux",
            "--opt-level=2",
            "source.gruel",
        ]));
        assert_eq!(opts.stages, vec![EmitStage::Asm]);
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    // ========== gruel doc ==========

    #[test]
    fn parse_doc_defaults() {
        let opts = unwrap_doc_opts(parse_doc_from(&["source.gruel"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.format, DocFormat::Markdown);
        assert_eq!(opts.output_dir, "target/doc");
    }

    #[test]
    fn parse_doc_html() {
        let opts = unwrap_doc_opts(parse_doc_from(&["--format", "html", "source.gruel"]));
        assert_eq!(opts.format, DocFormat::Html);
    }

    #[test]
    fn parse_doc_output_dir() {
        let opts = unwrap_doc_opts(parse_doc_from(&[
            "--output-dir",
            "out/docs",
            "source.gruel",
        ]));
        assert_eq!(opts.output_dir, "out/docs");
    }

    #[test]
    fn parse_doc_multi_file() {
        let opts = unwrap_doc_opts(parse_doc_from(&["a.gruel", "b.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
    }

    #[test]
    fn parse_doc_no_sources_is_error() {
        assert!(is_error(&parse_doc_from(&[])));
    }

    #[test]
    fn parse_doc_invalid_format() {
        assert!(is_error(&parse_doc_from(&[
            "--format",
            "pdf",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_doc_preview() {
        let opts = unwrap_doc_opts(parse_doc_from(&["--preview", "test_infra", "source.gruel"]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
    }

    // ========== gruel cache ==========

    #[test]
    fn parse_cache_stats_defaults() {
        let opts = unwrap_cache_opts(parse_cache_from(&["stats"]));
        match opts {
            CacheOpts::Stats { cache_dir } => assert_eq!(cache_dir, "target/gruel-cache"),
            _ => panic!("expected Stats"),
        }
    }

    #[test]
    fn parse_cache_clean_defaults() {
        let opts = unwrap_cache_opts(parse_cache_from(&["clean"]));
        match opts {
            CacheOpts::Clean { cache_dir } => assert_eq!(cache_dir, "target/gruel-cache"),
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn parse_cache_stats_with_dir() {
        let opts = unwrap_cache_opts(parse_cache_from(&["stats", "--cache-dir", "/tmp/foo"]));
        match opts {
            CacheOpts::Stats { cache_dir } => assert_eq!(cache_dir, "/tmp/foo"),
            _ => panic!("expected Stats"),
        }
    }

    #[test]
    fn parse_cache_clean_with_dir() {
        let opts = unwrap_cache_opts(parse_cache_from(&["clean", "--cache-dir", "/tmp/foo"]));
        match opts {
            CacheOpts::Clean { cache_dir } => assert_eq!(cache_dir, "/tmp/foo"),
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn parse_cache_requires_action() {
        assert!(is_error(&parse_cache_from(&[])));
    }

    #[test]
    fn parse_cache_unknown_action() {
        assert!(is_error(&parse_cache_from(&["nuke"])));
    }

    // ========== gruel run ==========

    #[test]
    fn parse_run_single_source() {
        let opts = unwrap_run_opts(parse_run_from(&["source.gruel"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert!(opts.program_args.is_empty());
    }

    #[test]
    fn parse_run_multi_source() {
        let opts = unwrap_run_opts(parse_run_from(&["a.gruel", "b.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert!(opts.program_args.is_empty());
    }

    #[test]
    fn parse_run_forwards_program_args() {
        let opts = unwrap_run_opts(parse_run_from(&[
            "source.gruel",
            "--",
            "foo",
            "--bar",
            "baz",
        ]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.program_args, vec!["foo", "--bar", "baz"]);
    }

    #[test]
    fn parse_run_no_sources_is_error() {
        assert!(is_error(&parse_run_from(&[])));
    }

    #[test]
    fn parse_run_release_flag() {
        let opts = unwrap_run_opts(parse_run_from(&["--release", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O3);
    }

    #[test]
    fn parse_run_with_target_and_preview() {
        let opts = unwrap_run_opts(parse_run_from(&[
            "--target",
            "x86_64-linux",
            "--preview",
            "test_infra",
            "source.gruel",
        ]));
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn parse_run_no_cache_flag() {
        let opts = unwrap_run_opts(parse_run_from(&["--no-cache", "source.gruel"]));
        assert!(opts.no_cache);
    }

    #[test]
    fn parse_run_args_no_separator_treated_as_sources() {
        // Without `--`, positionals are all source files.
        let opts = unwrap_run_opts(parse_run_from(&["a.gruel", "b.gruel", "c.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel", "c.gruel"]);
        assert!(opts.program_args.is_empty());
    }

    // ========== gruel check ==========

    #[test]
    fn parse_check_single_source() {
        let opts = unwrap_check_opts(parse_check_from(&["source.gruel"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
    }

    #[test]
    fn parse_check_multi_source() {
        let opts = unwrap_check_opts(parse_check_from(&["a.gruel", "b.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
    }

    #[test]
    fn parse_check_no_sources_is_error() {
        assert!(is_error(&parse_check_from(&[])));
    }

    #[test]
    fn parse_check_with_target() {
        let opts = unwrap_check_opts(parse_check_from(&[
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert_eq!(opts.target, "x86_64-linux".parse::<Target>().unwrap());
    }

    #[test]
    fn parse_check_preview() {
        let opts = unwrap_check_opts(parse_check_from(&[
            "--preview",
            "test_infra",
            "source.gruel",
        ]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn parse_check_time_passes() {
        let opts = unwrap_check_opts(parse_check_from(&["--time-passes", "source.gruel"]));
        assert!(opts.time_passes);
    }

    #[test]
    fn parse_check_benchmark_json() {
        let opts = unwrap_check_opts(parse_check_from(&["--benchmark-json", "source.gruel"]));
        assert!(opts.benchmark_json);
    }

    // ========== globals ==========

    #[test]
    fn parse_log_level_off() {
        let (globals, _) = unwrap_build(parse_build_from(&["--log-level", "off", "source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Off);
    }

    #[test]
    fn parse_log_level_error() {
        let (globals, _) =
            unwrap_build(parse_build_from(&["--log-level", "error", "source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Error);
    }

    #[test]
    fn parse_log_level_warn() {
        let (globals, _) = unwrap_build(parse_build_from(&["--log-level", "warn", "source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Warn);
    }

    #[test]
    fn parse_log_level_info() {
        let (globals, _) = unwrap_build(parse_build_from(&["--log-level", "info", "source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Info);
    }

    #[test]
    fn parse_log_level_debug() {
        let (globals, _) =
            unwrap_build(parse_build_from(&["--log-level", "debug", "source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Debug);
    }

    #[test]
    fn parse_log_level_trace() {
        let (globals, _) =
            unwrap_build(parse_build_from(&["--log-level", "trace", "source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Trace);
    }

    #[test]
    fn parse_log_level_global_before_subcommand() {
        // Globals can appear before the subcommand too.
        let result = parse_args_from(&["--log-level", "info", "build", "source.gruel"]);
        let (globals, _) = match result {
            ParseResult::Ok(ParsedCli {
                globals,
                command: ResolvedCommand::Build(opts),
            }) => (globals, opts),
            _ => panic!("expected Build with global log-level"),
        };
        assert_eq!(globals.log_level, LogLevel::Info);
    }

    #[test]
    fn parse_log_level_missing_value() {
        assert!(is_error(&parse_build_from(&[
            "source.gruel",
            "--log-level"
        ])));
    }

    #[test]
    fn parse_log_level_invalid() {
        assert!(is_error(&parse_build_from(&[
            "--log-level",
            "invalid",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_log_format_text() {
        let (globals, _) =
            unwrap_build(parse_build_from(&["--log-format", "text", "source.gruel"]));
        assert_eq!(globals.log_format, LogFormat::Text);
    }

    #[test]
    fn parse_log_format_json() {
        let (globals, _) =
            unwrap_build(parse_build_from(&["--log-format", "json", "source.gruel"]));
        assert_eq!(globals.log_format, LogFormat::Json);
    }

    #[test]
    fn parse_log_format_missing_value() {
        assert!(is_error(&parse_build_from(&[
            "source.gruel",
            "--log-format"
        ])));
    }

    #[test]
    fn parse_log_format_invalid() {
        assert!(is_error(&parse_build_from(&[
            "--log-format",
            "invalid",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_defaults_log_level() {
        let (globals, _) = unwrap_build(parse_build_from(&["source.gruel"]));
        assert_eq!(globals.log_level, LogLevel::Off);
    }

    #[test]
    fn parse_defaults_log_format() {
        let (globals, _) = unwrap_build(parse_build_from(&["source.gruel"]));
        assert_eq!(globals.log_format, LogFormat::Text);
    }

    // ========== --help and --version ==========

    #[test]
    fn parse_help_long() {
        assert!(is_exit(&parse_args_from(&["--help"])));
    }

    #[test]
    fn parse_help_short() {
        assert!(is_exit(&parse_args_from(&["-h"])));
    }

    #[test]
    fn parse_version_long() {
        assert!(is_exit(&parse_args_from(&["--version"])));
    }

    #[test]
    fn parse_version_short() {
        assert!(is_exit(&parse_args_from(&["-V"])));
    }

    #[test]
    fn parse_build_help() {
        assert!(is_exit(&parse_build_from(&["--help"])));
    }

    // ========== unknown options ==========

    #[test]
    fn parse_unknown_option() {
        assert!(is_error(&parse_build_from(&["--unknown", "source.gruel"])));
    }

    #[test]
    fn parse_unknown_short_option() {
        assert!(is_error(&parse_build_from(&["-x", "source.gruel"])));
    }
}
