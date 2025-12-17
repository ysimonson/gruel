use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use annotate_snippets::{Level, Renderer, Snippet};
use rue_compiler::{compile, compile_to_air, generate_mir, CompileError};
use rue_rir::RirPrinter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DumpMode {
    None,
    Rir,
    Air,
    Mir,
}

struct Options {
    source_path: String,
    output_path: String,
    dump_mode: DumpMode,
}

fn print_usage() {
    eprintln!("Usage: rue [options] <source.rue> [output]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --dump-rir    Dump RIR (untyped intermediate representation)");
    eprintln!("  --dump-air    Dump AIR (typed intermediate representation)");
    eprintln!("  --dump-mir    Dump MIR (machine intermediate representation)");
    eprintln!("  --help        Show this help message");
}

fn parse_args() -> Option<Options> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_usage();
        return None;
    }

    let mut dump_mode = DumpMode::None;
    let mut positional = Vec::new();

    for arg in &args {
        match arg.as_str() {
            "--dump-rir" => dump_mode = DumpMode::Rir,
            "--dump-air" => dump_mode = DumpMode::Air,
            "--dump-mir" => dump_mode = DumpMode::Mir,
            "--help" | "-h" => {
                print_usage();
                return None;
            }
            _ if arg.starts_with('-') => {
                eprintln!("Unknown option: {}", arg);
                print_usage();
                return None;
            }
            _ => positional.push(arg.clone()),
        }
    }

    if positional.is_empty() {
        eprintln!("Error: No source file specified");
        print_usage();
        return None;
    }

    let source_path = positional[0].clone();
    let output_path = positional.get(1).cloned().unwrap_or_else(|| "a.out".to_string());

    Some(Options {
        source_path,
        output_path,
        dump_mode,
    })
}

fn main() {
    let options = match parse_args() {
        Some(opts) => opts,
        None => std::process::exit(1),
    };

    // Read source
    let source = fs::read_to_string(&options.source_path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", options.source_path, e);
        std::process::exit(1);
    });

    // Handle dump modes
    if options.dump_mode != DumpMode::None {
        match compile_to_air(&source) {
            Ok(state) => {
                match options.dump_mode {
                    DumpMode::Rir => {
                        let printer = RirPrinter::new(&state.rir, &state.interner);
                        println!("{}", printer);
                    }
                    DumpMode::Air => {
                        for func in &state.functions {
                            println!("function {}:", func.name);
                            println!("{}", func.air);
                        }
                    }
                    DumpMode::Mir => {
                        for func in &state.functions {
                            let mir = generate_mir(&func.air, func.num_locals, func.num_params, &func.name);
                            println!("function {}:", func.name);
                            println!("{}", mir);
                        }
                    }
                    DumpMode::None => unreachable!(),
                }
            }
            Err(e) => {
                print_error(&e, &source, &options.source_path);
                std::process::exit(1);
            }
        }
        return;
    }

    // Normal compilation
    match compile(&source) {
        Ok(elf) => {
            // Write output
            if let Err(e) = fs::write(&options.output_path, &elf) {
                eprintln!("Error writing {}: {}", options.output_path, e);
                std::process::exit(1);
            }

            // Make executable
            let path = Path::new(&options.output_path);
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();

            println!("Compiled {} -> {}", options.source_path, options.output_path);
        }
        Err(e) => {
            print_error(&e, &source, &options.source_path);
            std::process::exit(1);
        }
    }
}

fn print_error(error: &CompileError, source: &str, source_path: &str) {
    let message = error.to_string();
    let renderer = Renderer::plain();

    // For errors without a span, just print the message
    let Some(span) = error.span() else {
        let report = Level::Error.title(&message);
        eprintln!("{}", renderer.render(report));
        return;
    };

    let report = Level::Error.title(&message).snippet(
        Snippet::source(source)
            .origin(source_path)
            .fold(true)
            .annotation(Level::Error.span(span.start as usize..span.end as usize)),
    );

    eprintln!("{}", renderer.render(report));
}
