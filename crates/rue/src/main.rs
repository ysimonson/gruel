use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use annotate_snippets::{Level, Renderer, Snippet};
use rue_compiler::{generate_elf, CompileError, ErrorKind, Lexer, Parser, Span};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: rue <source.rue> [output]");
        std::process::exit(1);
    }

    let source_path = &args[1];
    let output_path = args.get(2).map(String::as_str).unwrap_or("a.out");

    // Read source
    let source = fs::read_to_string(source_path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", source_path, e);
        std::process::exit(1);
    });

    // Compile
    if let Err(e) = compile(&source, source_path, output_path) {
        print_error(&e, &source, source_path);
        std::process::exit(1);
    }

    println!("Compiled {} -> {}", source_path, output_path);
}

fn compile(source: &str, source_path: &str, output_path: &str) -> Result<(), CompileError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize()?;

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    // Check for main function
    if !program.functions.iter().any(|f| f.name == "main") {
        return Err(CompileError::new(
            ErrorKind::NoMainFunction,
            Span::default(),
        ));
    }

    let elf = generate_elf(&program);

    // Write output
    fs::write(output_path, &elf).map_err(|_| {
        CompileError::new(
            ErrorKind::UnexpectedCharacter('\0'), // Placeholder - we should add an IO error kind
            Span::default(),
        )
    })?;

    // Make executable
    let path = Path::new(output_path);
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();

    Ok(())
}

fn print_error(error: &CompileError, source: &str, source_path: &str) {
    let message = error.message();
    let renderer = Renderer::plain();

    // For errors without a span (like NoMainFunction), just print the message
    if error.span.start == 0 && error.span.end == 0 && matches!(error.kind, ErrorKind::NoMainFunction) {
        let report = Level::Error.title(&message);
        eprintln!("{}", renderer.render(report));
        return;
    }

    let report = Level::Error
        .title(&message)
        .snippet(
            Snippet::source(source)
                .origin(source_path)
                .fold(true)
                .annotation(Level::Error.span(error.span.start..error.span.end)),
        );

    eprintln!("{}", renderer.render(report));
}
