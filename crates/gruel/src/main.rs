use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Command;

use clap::Parser;
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{EnvFilter, fmt};

mod timing;

use gruel_compiler::{
    CompileOptions, FileId, Lexer, LinkerMode, MultiFileFormatter, OptLevel, ParsedProgram,
    PreviewFeature, PreviewFeatures, SourceFile, SourceInfo,
    compile_frontend_from_ast_with_options, compile_multi_file_with_options, generate_llvm_ir,
    merge_symbols,
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
    long_about = "Usage: gruel [options] <source.gruel> [output]\n       gruel [options] <source1.gruel> <source2.gruel> ... -o <output>",
    disable_help_subcommand = true,
)]
struct Cli {
    /// Source files to compile. Multiple files require -o/--output.
    sources: Vec<String>,

    /// Output path (required for multiple source files).
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

    /// Emit intermediate representation and exit (can be repeated).
    #[arg(long, value_name = "STAGE")]
    emit: Vec<EmitStage>,

    /// Enable a preview feature (can be repeated).
    #[arg(long, value_name = "FEATURE")]
    preview: Vec<PreviewFeature>,

    /// Set logging level.
    #[arg(long, value_name = "LEVEL", default_value = "off")]
    log_level: LogLevel,

    /// Set logging format.
    #[arg(long, value_name = "FMT", default_value = "text")]
    log_format: LogFormat,

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
    /// Requires --preview incremental_compilation.
    /// Defaults to `target/gruel-cache/` next to the first source file.
    /// Also overridable via `GRUEL_CACHE_DIR` env var.
    #[arg(long, value_name = "PATH", env = "GRUEL_CACHE_DIR")]
    cache_dir: Option<String>,
}

struct Options {
    /// Source files to compile. In single-file mode, contains one path.
    /// In multi-file mode, contains multiple paths.
    source_paths: Vec<String>,
    output_path: String,
    emit_stages: Vec<EmitStage>,
    target: Target,
    linker: LinkerMode,
    opt_level: OptLevel,
    preview_features: PreviewFeatures,
    log_level: LogLevel,
    log_format: LogFormat,
    time_passes: bool,
    benchmark_json: bool,
    /// Number of parallel jobs (0 = auto-detect, use all cores).
    jobs: usize,
    /// When true, suppress stderr printing of comptime `@dbg` output.
    capture_comptime_dbg: bool,
    /// Optional explicit cache directory (ADR-0074). When `None` and
    /// `incremental_compilation` is enabled, the driver uses
    /// `target/gruel-cache/` next to the first source file.
    /// Phase 2 wires this into the compilation pipeline; Phase 1 only
    /// plumbs the field and validates the preview-feature gate.
    #[allow(dead_code)]
    cache_dir: Option<String>,
}

/// Result of parsing command-line arguments.
enum ParseResult {
    /// Successfully parsed options.
    Options(Options),
    /// Parsing failed with an error (already printed).
    Error,
    /// User requested help or version (already printed, should exit 0).
    Exit,
}

fn cli_to_options(cli: Cli) -> Result<Options, String> {
    // Resolve --debug/--release/--opt-level into a single OptLevel.
    let opt_level = if cli.debug {
        OptLevel::O0
    } else if cli.release {
        OptLevel::O3
    } else {
        match cli.opt_level {
            Some(0) => OptLevel::O0,
            Some(1) => OptLevel::O1,
            Some(2) => OptLevel::O2,
            Some(3) => OptLevel::O3,
            None => OptLevel::default(),
            Some(_) => unreachable!("clap value_parser bounds to 0..=3"),
        }
    };

    let (source_paths, output_path) = if let Some(out) = cli.output {
        if cli.sources.is_empty() {
            return Err("Error: No source file specified".to_string());
        }
        (cli.sources, out)
    } else {
        match cli.sources.len() {
            0 => return Err("Error: No source file specified".to_string()),
            1 => (cli.sources, "a.out".to_string()),
            2 => {
                let mut s = cli.sources;
                let out = s.pop().unwrap();
                (s, out)
            }
            _ => {
                return Err(
                    "Error: multiple source files require -o to specify output path\n\
                     Usage: gruel a.gruel b.gruel -o output"
                        .to_string(),
                );
            }
        }
    };

    let linker = match cli.linker.as_deref() {
        None => LinkerMode::default(),
        Some("internal") => LinkerMode::Internal,
        Some(cmd) => LinkerMode::System(cmd.to_string()),
    };

    let preview_features: PreviewFeatures = cli.preview.into_iter().collect();

    // ADR-0074: --cache-dir is meaningful only when the incremental cache
    // is enabled. Reject explicit --cache-dir without the preview gate
    // rather than silently ignoring it.
    if cli.cache_dir.is_some()
        && !preview_features.contains(&PreviewFeature::IncrementalCompilation)
    {
        return Err(
            "--cache-dir requires --preview incremental_compilation (ADR-0074)".to_string(),
        );
    }

    Ok(Options {
        source_paths,
        output_path,
        emit_stages: cli.emit,
        target: cli.target,
        linker,
        opt_level,
        preview_features,
        log_level: cli.log_level,
        log_format: cli.log_format,
        time_passes: cli.time_passes,
        benchmark_json: cli.benchmark_json,
        jobs: cli.jobs,
        capture_comptime_dbg: cli.capture_comptime_dbg,
        cache_dir: cli.cache_dir,
    })
}

/// Parse CLI arguments into [`Options`].
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
    match cli_to_options(cli) {
        Ok(opts) => ParseResult::Options(opts),
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
    let options = match parse_args(std::env::args_os()) {
        ParseResult::Options(opts) => opts,
        ParseResult::Exit => std::process::exit(0),
        ParseResult::Error => std::process::exit(1),
    };

    // Initialize tracing based on CLI options
    // Returns timing data if --time-passes or --benchmark-json was specified
    let timing_data = init_tracing(
        options.log_level,
        options.log_format,
        options.time_passes,
        options.benchmark_json,
    );

    // Read all source files into memory
    let sources: Vec<(String, String)> = options
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

    // Also keep a single-file formatter for the primary file (for source metrics)
    let (_primary_path, primary_source) = &sources[0];

    // Compute source metrics if benchmark JSON is requested
    let source_metrics = if options.benchmark_json {
        // We need token count, so do a quick lex
        let lexer = Lexer::new(primary_source);
        let token_count = match lexer.tokenize() {
            Ok((tokens, _interner)) => tokens.len(),
            Err(_) => 0, // If lexing fails, we'll get the error during compilation anyway
        };
        Some(timing::SourceMetrics {
            bytes: primary_source.len(),
            lines: primary_source.lines().count(),
            tokens: token_count,
        })
    } else {
        None
    };

    // Handle emit modes with multi-file support
    if !options.emit_stages.is_empty() {
        if let Err(()) = handle_emit_multi_file(&source_files, &options, &formatter) {
            std::process::exit(1);
        }
        print_timing_output(
            &timing_data,
            options.time_passes,
            options.benchmark_json,
            &options.target,
            source_metrics,
        );
        return;
    }

    // Normal compilation - uses multi-file compilation for all source files.
    //
    // ADR-0074: when --preview incremental_compilation is enabled, route
    // parsing through the on-disk cache. cache_dir defaults to
    // `target/gruel-cache/` next to the first source file when not
    // explicitly provided.
    let resolved_cache_dir = if options
        .preview_features
        .contains(&PreviewFeature::IncrementalCompilation)
    {
        Some(match &options.cache_dir {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let first = std::path::Path::new(&options.source_paths[0]);
                first
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("target")
                    .join("gruel-cache")
            }
        })
    } else {
        None
    };
    let compile_options = CompileOptions {
        target: options.target,
        linker: options.linker.clone(),
        opt_level: options.opt_level,
        preview_features: options.preview_features.clone(),
        jobs: options.jobs,
        capture_comptime_dbg: options.capture_comptime_dbg,
        cache_dir: resolved_cache_dir,
    };
    match compile_multi_file_with_options(&source_files, &compile_options) {
        Ok(output) => {
            // Print warnings using the diagnostic formatter
            if !output.warnings.is_empty() {
                eprintln!("{}", formatter.format_warnings(&output.warnings));
            }

            // Write output
            if let Err(e) = fs::write(&options.output_path, &output.elf) {
                eprintln!("Error writing {}: {}", options.output_path, e);
                std::process::exit(1);
            }

            // Make executable (Unix only)
            #[cfg(unix)]
            {
                let path = Path::new(&options.output_path);
                match fs::metadata(path) {
                    Ok(metadata) => {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o755);
                        if let Err(e) = fs::set_permissions(path, perms) {
                            eprintln!(
                                "Warning: could not set executable permissions on {}: {}",
                                options.output_path, e
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: could not read file metadata for {}: {}",
                            options.output_path, e
                        );
                    }
                }
            }

            // Ad-hoc codesign for macOS (required for executables to run on ARM64)
            #[cfg(target_os = "macos")]
            {
                // Only codesign if target is macOS (cross-compilation check)
                if compile_options.target.is_macho() {
                    let result = Command::new("codesign")
                        .args(["-f", "-s", "-", &options.output_path])
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
            if !options.benchmark_json {
                let linker_str = match &options.linker {
                    LinkerMode::Internal => "internal".to_string(),
                    LinkerMode::System(cmd) => cmd.clone(),
                };
                let source_str = if options.source_paths.len() == 1 {
                    options.source_paths[0].clone()
                } else {
                    format!("{} files", options.source_paths.len())
                };
                println!(
                    "Compiled {} -> {} (target: {}, linker: {})",
                    source_str, options.output_path, options.target, linker_str
                );
            }

            print_timing_output(
                &timing_data,
                options.time_passes,
                options.benchmark_json,
                &options.target,
                source_metrics,
            );
        }
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    }
}

/// Handle emit stages for multi-file compilation.
///
/// For early stages (tokens, ast), each file is processed and labeled individually.
/// For later stages (rir, air, cfg, etc.), the merged program is used.
fn handle_emit_multi_file(
    sources: &[SourceFile<'_>],
    options: &Options,
    formatter: &MultiFileFormatter,
) -> Result<(), ()> {
    // Determine which stages we need
    let needs_tokens = options.emit_stages.contains(&EmitStage::Tokens);
    let needs_ast = options.emit_stages.contains(&EmitStage::Ast);
    let needs_later_stages = options.emit_stages.iter().any(|s| {
        matches!(
            s,
            EmitStage::Rir | EmitStage::Air | EmitStage::Cfg | EmitStage::Asm
        )
    });

    // For tokens, we need to lex each file separately (before parsing merges interners)
    // We'll collect per-file tokens if needed
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
        match gruel_compiler::parse_all_files_with_preview(sources, &options.preview_features) {
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

    // Merge symbols and compile frontend (needed for later stages)
    let frontend_state = if needs_later_stages {
        // Take ownership of the parsed program (already parsed above)
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

        let state = match compile_frontend_from_ast_with_options(
            merged.ast,
            merged.interner,
            &options.preview_features,
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

    // Now emit in order
    for stage in &options.emit_stages {
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
                    };
                    match generate_llvm_ir(&inputs, options.opt_level) {
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

    /// Helper to extract Options from ParseResult, panicking if not Options.
    fn unwrap_options(result: ParseResult) -> Options {
        match result {
            ParseResult::Options(opts) => opts,
            ParseResult::Error => panic!("Expected Options, got Error"),
            ParseResult::Exit => panic!("Expected Options, got Exit"),
        }
    }

    /// Helper to check if result is an error.
    fn is_error(result: &ParseResult) -> bool {
        matches!(result, ParseResult::Error)
    }

    /// Helper to check if result is an exit.
    fn is_exit(result: &ParseResult) -> bool {
        matches!(result, ParseResult::Exit)
    }

    // ========== Basic parsing tests ==========

    #[test]
    fn parse_source_file_only() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_source_and_output() {
        let opts = unwrap_options(parse_args_from(&["source.gruel", "output"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_no_args_returns_error() {
        assert!(is_error(&parse_args_from(&[])));
    }

    // ========== Multi-file argument parsing tests ==========

    #[test]
    fn parse_multi_file_with_output_flag() {
        let opts = unwrap_options(parse_args_from(&["a.gruel", "b.gruel", "-o", "output"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_multi_file_with_output_long_flag() {
        let opts = unwrap_options(parse_args_from(&["a.gruel", "b.gruel", "--output", "out"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.output_path, "out");
    }

    #[test]
    fn parse_multi_file_without_output_flag_error() {
        // Three positional args without -o should error
        assert!(is_error(&parse_args_from(&[
            "a.gruel", "b.gruel", "c.gruel"
        ])));
    }

    #[test]
    fn parse_multi_file_with_options() {
        let opts = unwrap_options(parse_args_from(&[
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
        let opts = unwrap_options(parse_args_from(&["-o", "output", "a.gruel", "b.gruel"]));
        assert_eq!(opts.source_paths, vec!["a.gruel", "b.gruel"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_single_file_with_output_flag() {
        // Even single file can use -o explicitly
        let opts = unwrap_options(parse_args_from(&["source.gruel", "-o", "myprogram"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "myprogram");
    }

    #[test]
    fn parse_output_flag_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "-o"])));
    }

    #[test]
    fn parse_output_long_flag_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "--output"])));
    }

    // ========== --emit tests ==========

    #[test]
    fn parse_emit_tokens() {
        let opts = unwrap_options(parse_args_from(&["--emit", "tokens", "source.gruel"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Tokens]);
    }

    #[test]
    fn parse_emit_ast() {
        let opts = unwrap_options(parse_args_from(&["--emit", "ast", "source.gruel"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Ast]);
    }

    #[test]
    fn parse_emit_rir() {
        let opts = unwrap_options(parse_args_from(&["--emit", "rir", "source.gruel"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Rir]);
    }

    #[test]
    fn parse_emit_air() {
        let opts = unwrap_options(parse_args_from(&["--emit", "air", "source.gruel"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_emit_cfg() {
        let opts = unwrap_options(parse_args_from(&["--emit", "cfg", "source.gruel"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Cfg]);
    }

    #[test]
    fn parse_emit_asm() {
        let opts = unwrap_options(parse_args_from(&["--emit", "asm", "source.gruel"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Asm]);
    }

    #[test]
    fn parse_multiple_emit_stages() {
        let opts = unwrap_options(parse_args_from(&[
            "--emit",
            "tokens",
            "--emit",
            "ast",
            "--emit",
            "air",
            "source.gruel",
        ]));
        assert_eq!(
            opts.emit_stages,
            vec![EmitStage::Tokens, EmitStage::Ast, EmitStage::Air]
        );
    }

    #[test]
    fn parse_emit_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "--emit"])));
    }

    #[test]
    fn parse_emit_invalid_stage() {
        assert!(is_error(&parse_args_from(&[
            "--emit",
            "invalid",
            "source.gruel"
        ])));
    }

    // ========== --target tests ==========

    #[test]
    fn parse_target_x86_64_linux() {
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    #[test]
    fn parse_target_aarch64_macos() {
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "aarch64-macos",
            "source.gruel",
        ]));
        assert_eq!(opts.target, Target::Aarch64Macos);
    }

    #[test]
    fn parse_target_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "--target"])));
    }

    #[test]
    fn parse_target_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--target",
            "invalid",
            "source.gruel"
        ])));
    }

    // ========== --linker tests ==========

    #[test]
    fn parse_linker_internal() {
        let opts = unwrap_options(parse_args_from(&["--linker", "internal", "source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::Internal);
    }

    #[test]
    fn parse_linker_system_clang() {
        let opts = unwrap_options(parse_args_from(&["--linker", "clang", "source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
    }

    #[test]
    fn parse_linker_system_gcc() {
        let opts = unwrap_options(parse_args_from(&["--linker", "gcc", "source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::System("gcc".to_string()));
    }

    #[test]
    fn parse_linker_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "--linker"])));
    }

    // ========== Optimization level tests ==========

    #[test]
    fn parse_opt_level_0() {
        let opts = unwrap_options(parse_args_from(&["--opt-level=0", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_opt_level_1() {
        let opts = unwrap_options(parse_args_from(&["--opt-level=1", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn parse_opt_level_2() {
        let opts = unwrap_options(parse_args_from(&["--opt-level=2", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    #[test]
    fn parse_opt_level_3() {
        let opts = unwrap_options(parse_args_from(&["--opt-level=3", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O3);
    }

    #[test]
    fn parse_opt_level_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--opt-level=9",
            "source.gruel"
        ])));
    }

    // ========== --preview tests ==========

    #[test]
    fn parse_preview_valid_feature() {
        let opts = unwrap_options(parse_args_from(&[
            "--preview",
            "test_infra",
            "source.gruel",
        ]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn parse_preview_multiple_flags() {
        // Test that --preview can be specified multiple times
        // (currently only one feature exists, but the flag can still be repeated)
        let opts = unwrap_options(parse_args_from(&[
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
        assert!(is_error(&parse_args_from(&["source.gruel", "--preview"])));
    }

    #[test]
    fn parse_preview_invalid_feature() {
        assert!(is_error(&parse_args_from(&[
            "--preview",
            "nonexistent",
            "source.gruel"
        ])));
    }

    // ========== --cache-dir tests (ADR-0074) ==========

    #[test]
    fn cache_dir_requires_preview_feature() {
        // Without --preview incremental_compilation, --cache-dir is rejected.
        assert!(is_error(&parse_args_from(&[
            "--cache-dir",
            "/tmp/foo",
            "source.gruel",
        ])));
    }

    #[test]
    fn cache_dir_accepted_with_preview() {
        let opts = unwrap_options(parse_args_from(&[
            "--preview",
            "incremental_compilation",
            "--cache-dir",
            "/tmp/foo",
            "source.gruel",
        ]));
        assert_eq!(opts.cache_dir.as_deref(), Some("/tmp/foo"));
        assert!(
            opts.preview_features
                .contains(&PreviewFeature::IncrementalCompilation)
        );
    }

    #[test]
    fn cache_dir_optional_with_preview() {
        // The preview can be enabled without --cache-dir; the driver will
        // fall back to a default location.
        let opts = unwrap_options(parse_args_from(&[
            "--preview",
            "incremental_compilation",
            "source.gruel",
        ]));
        assert!(opts.cache_dir.is_none());
        assert!(
            opts.preview_features
                .contains(&PreviewFeature::IncrementalCompilation)
        );
    }

    // ========== --log-level tests ==========

    #[test]
    fn parse_log_level_off() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "off", "source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Off);
    }

    #[test]
    fn parse_log_level_error() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "error", "source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Error);
    }

    #[test]
    fn parse_log_level_warn() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "warn", "source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Warn);
    }

    #[test]
    fn parse_log_level_info() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "info", "source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Info);
    }

    #[test]
    fn parse_log_level_debug() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "debug", "source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Debug);
    }

    #[test]
    fn parse_log_level_trace() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "trace", "source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Trace);
    }

    #[test]
    fn parse_log_level_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "--log-level"])));
    }

    #[test]
    fn parse_log_level_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--log-level",
            "invalid",
            "source.gruel"
        ])));
    }

    // ========== --log-format tests ==========

    #[test]
    fn parse_log_format_text() {
        let opts = unwrap_options(parse_args_from(&["--log-format", "text", "source.gruel"]));
        assert_eq!(opts.log_format, LogFormat::Text);
    }

    #[test]
    fn parse_log_format_json() {
        let opts = unwrap_options(parse_args_from(&["--log-format", "json", "source.gruel"]));
        assert_eq!(opts.log_format, LogFormat::Json);
    }

    #[test]
    fn parse_log_format_missing_value() {
        assert!(is_error(&parse_args_from(&[
            "source.gruel",
            "--log-format"
        ])));
    }

    #[test]
    fn parse_log_format_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--log-format",
            "invalid",
            "source.gruel"
        ])));
    }

    // ========== --help and --version tests ==========

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

    // ========== Unknown option tests ==========

    #[test]
    fn parse_unknown_option() {
        assert!(is_error(&parse_args_from(&["--unknown", "source.gruel"])));
    }

    #[test]
    fn parse_unknown_short_option() {
        assert!(is_error(&parse_args_from(&["-x", "source.gruel"])));
    }

    // ========== Combined options tests ==========

    #[test]
    fn parse_all_options_combined() {
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "x86_64-linux",
            "--linker",
            "clang",
            "--opt-level=2",
            "--emit",
            "air",
            "source.gruel",
            "output",
        ]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "output");
        assert_eq!(opts.target, Target::X86_64Linux);
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit_stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_options_after_source() {
        // Options can appear after the source file
        let opts = unwrap_options(parse_args_from(&["source.gruel", "--opt-level=1"]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn parse_mixed_option_positions() {
        let opts = unwrap_options(parse_args_from(&[
            "--opt-level=1",
            "source.gruel",
            "--target",
            "x86_64-linux",
            "output",
        ]));
        assert_eq!(opts.source_paths, vec!["source.gruel"]);
        assert_eq!(opts.output_path, "output");
        assert_eq!(opts.opt_level, OptLevel::O1);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    // ========== Default values tests ==========

    #[test]
    fn parse_defaults_output_path() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_defaults_opt_level() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_defaults_linker() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.linker, LinkerMode::Internal);
    }

    #[test]
    fn parse_defaults_emit_stages_empty() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert!(opts.emit_stages.is_empty());
    }

    #[test]
    fn parse_defaults_log_level() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.log_level, LogLevel::Off);
    }

    #[test]
    fn parse_defaults_log_format() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.log_format, LogFormat::Text);
    }

    #[test]
    fn parse_defaults_time_passes() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert!(!opts.time_passes);
    }

    // ========== --time-passes tests ==========

    #[test]
    fn parse_time_passes() {
        let opts = unwrap_options(parse_args_from(&["--time-passes", "source.gruel"]));
        assert!(opts.time_passes);
    }

    #[test]
    fn parse_time_passes_with_other_options() {
        let opts = unwrap_options(parse_args_from(&[
            "--time-passes",
            "--opt-level=2",
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert!(opts.time_passes);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    // ========== --benchmark-json tests ==========

    #[test]
    fn parse_benchmark_json() {
        let opts = unwrap_options(parse_args_from(&["--benchmark-json", "source.gruel"]));
        assert!(opts.benchmark_json);
    }

    #[test]
    fn parse_benchmark_json_with_other_options() {
        let opts = unwrap_options(parse_args_from(&[
            "--benchmark-json",
            "--opt-level=2",
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert!(opts.benchmark_json);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    #[test]
    fn parse_defaults_benchmark_json() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert!(!opts.benchmark_json);
    }

    #[test]
    fn parse_both_time_passes_and_benchmark_json() {
        // When both are specified, benchmark_json takes precedence (JSON output)
        let opts = unwrap_options(parse_args_from(&[
            "--time-passes",
            "--benchmark-json",
            "source.gruel",
        ]));
        assert!(opts.time_passes);
        assert!(opts.benchmark_json);
    }

    // ========== --jobs tests ==========

    #[test]
    fn parse_jobs_long_form() {
        let opts = unwrap_options(parse_args_from(&["--jobs", "4", "source.gruel"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_short_form() {
        let opts = unwrap_options(parse_args_from(&["-j", "4", "source.gruel"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_attached_form() {
        let opts = unwrap_options(parse_args_from(&["-j4", "source.gruel"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_single_thread() {
        let opts = unwrap_options(parse_args_from(&["-j1", "source.gruel"]));
        assert_eq!(opts.jobs, 1);
    }

    #[test]
    fn parse_jobs_auto_detect() {
        let opts = unwrap_options(parse_args_from(&["--jobs", "0", "source.gruel"]));
        assert_eq!(opts.jobs, 0);
    }

    #[test]
    fn parse_jobs_missing_value() {
        assert!(is_error(&parse_args_from(&["source.gruel", "--jobs"])));
    }

    #[test]
    fn parse_jobs_missing_value_short() {
        assert!(is_error(&parse_args_from(&["source.gruel", "-j"])));
    }

    #[test]
    fn parse_jobs_invalid_value() {
        assert!(is_error(&parse_args_from(&[
            "--jobs",
            "abc",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_jobs_negative_value() {
        // Negative values should fail to parse as usize
        assert!(is_error(&parse_args_from(&[
            "--jobs",
            "-1",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_jobs_with_other_options() {
        let opts = unwrap_options(parse_args_from(&[
            "-j4",
            "--opt-level=2",
            "--target",
            "x86_64-linux",
            "source.gruel",
        ]));
        assert_eq!(opts.jobs, 4);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    #[test]
    fn parse_defaults_jobs() {
        let opts = unwrap_options(parse_args_from(&["source.gruel"]));
        assert_eq!(opts.jobs, 0);
    }

    // ========== --debug / --release tests ==========

    #[test]
    fn parse_debug_flag() {
        let opts = unwrap_options(parse_args_from(&["--debug", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_release_flag() {
        let opts = unwrap_options(parse_args_from(&["--release", "source.gruel"]));
        assert_eq!(opts.opt_level, OptLevel::O3);
    }

    #[test]
    fn parse_debug_release_conflict() {
        assert!(is_error(&parse_args_from(&[
            "--debug",
            "--release",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_release_debug_conflict() {
        assert!(is_error(&parse_args_from(&[
            "--release",
            "--debug",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_debug_with_opt_level_conflict() {
        assert!(is_error(&parse_args_from(&[
            "--debug",
            "--opt-level=2",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_release_with_opt_level_conflict() {
        assert!(is_error(&parse_args_from(&[
            "--release",
            "--opt-level=1",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_opt_level_then_debug_conflict() {
        assert!(is_error(&parse_args_from(&[
            "--opt-level=2",
            "--debug",
            "source.gruel"
        ])));
    }

    #[test]
    fn parse_opt_level_then_release_conflict() {
        assert!(is_error(&parse_args_from(&[
            "--opt-level=1",
            "--release",
            "source.gruel"
        ])));
    }
}
