//! Snapshot tests for `gruel-fmt`.
//!
//! Each test asserts that a known input formats to a known expected output.
//! Tests grow alongside the implementation phases.

use gruel_fmt::format_source;

fn assert_fmt(input: &str, expected: &str) {
    let got = format_source(input).expect("format_source failed to parse the snapshot input");
    assert_eq!(
        got, expected,
        "\n--- expected ---\n{expected}\n--- got ---\n{got}\n"
    );
}

#[test]
fn smallest_main() {
    // The Phase 1 smallest case (ADR-0093).
    let input = "fn main() -> i32 { 0 }";
    let expected = "fn main() -> i32 {\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn smallest_main_already_canonical() {
    let canonical = "fn main() -> i32 {\n    0\n}\n";
    assert_fmt(canonical, canonical);
}

#[test]
fn smallest_main_extra_whitespace() {
    let input = "fn   main(  )   ->   i32   {  0   }";
    let expected = "fn main() -> i32 {\n    0\n}\n";
    assert_fmt(input, expected);
}

// ---- Phase 2: expressions ----

#[test]
fn binary_operators() {
    let input = "fn main() -> i32 { 1+2*3 - 4 / 5 }";
    let expected = "fn main() -> i32 {\n    1 + 2 * 3 - 4 / 5\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn parens_preserved() {
    let input = "fn main() -> i32 { (1+2)*3 }";
    let expected = "fn main() -> i32 {\n    (1 + 2) * 3\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn unary_operators() {
    let input = "fn main() -> i32 { let x = -1; let y = !true; 0 }";
    let expected = "fn main() -> i32 {\n    let x = -1;\n    let y = !true;\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn if_else_chain() {
    let input = "fn main() -> i32 { if 1 < 2 { 10 } else if 1 == 1 { 20 } else { 30 } }";
    let expected = "fn main() -> i32 {\n    if 1 < 2 {\n        10\n    } else if 1 == 1 {\n        20\n    } else {\n        30\n    }\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn match_arms_one_per_line() {
    let input = "fn main() -> i32 { match 1 { 1 => 10, 2 => 20, _ => 30 } }";
    let expected = "fn main() -> i32 {\n    match 1 {\n        1 => 10,\n        2 => 20,\n        _ => 30,\n    }\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn while_loop() {
    let input = "fn main() -> i32 { let mut x = 0; while x < 10 { x = x + 1; } x }";
    let expected = "fn main() -> i32 {\n    let mut x = 0;\n    while x < 10 {\n        x = x + 1;\n    }\n    x\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn for_loop() {
    let input = "fn main() -> i32 { for x in [1,2,3] { } 0 }";
    let expected = "fn main() -> i32 {\n    for x in [1, 2, 3] {}\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn return_statement() {
    // The parser canonicalizes a trailing `return X;` into the block's final
    // expression position, so the `;` round-trips away.
    let input = "fn main() -> i32 { return 42; }";
    let expected = "fn main() -> i32 {\n    return 42\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn function_call() {
    let input = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() -> i32 { add(1,2) }";
    let expected =
        "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() -> i32 {\n    add(1, 2)\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn array_literal_and_index() {
    let input = "fn main() -> i32 { let a = [1,2,3]; a[0] }";
    let expected = "fn main() -> i32 {\n    let a = [1, 2, 3];\n    a[0]\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn string_literal_escapes() {
    let input = r#"fn main() -> i32 { let s = "hello\n\tworld"; 0 }"#;
    let expected = "fn main() -> i32 {\n    let s = \"hello\\n\\tworld\";\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn char_literal() {
    let input = "fn main() -> i32 { let c = 'a'; 0 }";
    let expected = "fn main() -> i32 {\n    let c = 'a';\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn intrinsic_call() {
    let input = "fn main() -> i32 { @dbg(1+2) }";
    let expected = "fn main() -> i32 {\n    @dbg(1 + 2)\n}\n";
    assert_fmt(input, expected);
}

// ---- Phase 3: top-level items ----

#[test]
fn pub_function_with_doc() {
    let input = "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 { a + b }";
    let expected = "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn struct_decl_with_fields() {
    let input = "pub struct Point { pub x: i32, pub y: i32 }";
    let expected = "pub struct Point {\n    pub x: i32,\n    pub y: i32,\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn struct_with_methods() {
    let input = "struct Counter { value: i32, fn inc(self) -> i32 { self.value + 1 } }";
    let expected = "struct Counter {\n    value: i32,\n\n    fn inc(self) -> i32 {\n        self.value + 1\n    }\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn enum_decl_unit_variants() {
    let input = "pub enum Color { Red, Green, Blue }";
    let expected = "pub enum Color {\n    Red,\n    Green,\n    Blue,\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn enum_decl_tuple_variants() {
    let input = "enum Shape { Circle(i32), Rect(i32, i32) }";
    let expected = "enum Shape {\n    Circle(i32),\n    Rect(i32, i32),\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn interface_decl() {
    let input = "interface Drop { fn __drop(self); }";
    let expected = "interface Drop {\n    fn __drop(self);\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn const_decl() {
    let input = "pub const PI: i32 = 3;";
    let expected = "pub const PI: i32 = 3;\n";
    assert_fmt(input, expected);
}

#[test]
fn derive_decl() {
    let input = "derive Show { fn show(self) -> i32 { 0 } }";
    let expected = "derive Show {\n    fn show(self) -> i32 {\n        0\n    }\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn link_extern_block() {
    let input = "link_extern(\"c\") { fn puts(s: i32) -> i32; }";
    let expected = "link_extern(\"c\") {\n    fn puts(s: i32) -> i32;\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn unchecked_fn() {
    // `unchecked` is spelled as `@mark(unchecked)` (ADR-0088); the keyword
    // form was retired in parser Phase 6.
    let input = "@mark(unchecked)\nfn dangerous(p: i32) -> i32 { p }";
    let expected = "@mark(unchecked)\nfn dangerous(p: i32) -> i32 {\n    p\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn comptime_param() {
    let input = "fn id(comptime T: type, x: T) -> T { x }";
    let expected = "fn id(comptime T: type, x: T) -> T {\n    x\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn directive_on_fn() {
    let input = "@allow(unused)\nfn foo() -> i32 { 0 }";
    let expected = "@allow(unused)\nfn foo() -> i32 {\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn module_doc() {
    let input = "/// This module does things.\n\nfn main() -> i32 { 0 }";
    let expected = "/// This module does things.\n\nfn main() -> i32 {\n    0\n}\n";
    assert_fmt(input, expected);
}

// ---- Phase 4: trivia weaving ----

#[test]
fn leading_file_comment() {
    let input = "// header comment\nfn main() -> i32 { 0 }";
    let expected = "// header comment\nfn main() -> i32 {\n    0\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn comment_between_items() {
    let input = "fn a() -> i32 { 1 }\n// thoughts about b\nfn b() -> i32 { 2 }";
    let expected = "fn a() -> i32 {\n    1\n}\n\n// thoughts about b\nfn b() -> i32 {\n    2\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn comment_before_statement() {
    let input = "fn main() -> i32 {\n    // about x\n    let x = 1;\n    x\n}";
    let expected = "fn main() -> i32 {\n    // about x\n    let x = 1;\n    x\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn trailing_comment_on_statement() {
    let input = "fn main() -> i32 {\n    let x = 1; // about x\n    x\n}";
    let expected = "fn main() -> i32 {\n    let x = 1;  // about x\n    x\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn blank_line_run_collapses() {
    let input = "fn a() -> i32 { 1 }\n\n\n\n\nfn b() -> i32 { 2 }";
    let expected = "fn a() -> i32 {\n    1\n}\n\nfn b() -> i32 {\n    2\n}\n";
    assert_fmt(input, expected);
}

#[test]
fn trailing_comment_at_eof() {
    let input = "fn main() -> i32 { 0 }\n// trailing";
    let expected = "fn main() -> i32 {\n    0\n}\n// trailing\n";
    assert_fmt(input, expected);
}

#[test]
fn comment_inside_string_not_treated_as_comment() {
    let input = r#"fn main() -> i32 { let s = "//"; 0 }"#;
    let expected = "fn main() -> i32 {\n    let s = \"//\";\n    0\n}\n";
    assert_fmt(input, expected);
}
