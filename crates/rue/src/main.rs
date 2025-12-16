use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rue_compiler::{Lexer, Parser, generate_elf};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: rue <source.rue> [output]");
        std::process::exit(1);
    }

    let source_path = &args[1];
    let output_path = args.get(2).map(String::as_str).unwrap_or("a.out");

    // Read source
    let source = fs::read_to_string(source_path)
        .unwrap_or_else(|e| {
            eprintln!("Error reading {}: {}", source_path, e);
            std::process::exit(1);
        });

    // Compile
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse();

    let elf = generate_elf(&program);

    // Write output
    fs::write(output_path, &elf)
        .unwrap_or_else(|e| {
            eprintln!("Error writing {}: {}", output_path, e);
            std::process::exit(1);
        });

    // Make executable
    let path = Path::new(output_path);
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();

    println!("Compiled {} -> {}", source_path, output_path);
}
