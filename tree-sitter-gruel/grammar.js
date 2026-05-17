/**
 * Tree-sitter grammar for the Gruel programming language.
 *
 * Acceptance-only mirror of `crates/gruel-parser` (chumsky). Tree shape is
 * tooling-oriented; structural parity with the compiler AST is explicitly
 * not a goal. The `parser_differential` fuzz target and spec-corpus test
 * enforce that this grammar accepts exactly the same inputs as the
 * compiler does (ADR-0090).
 */

const PREC = {
  unary: 13,
  // Bracket-call postfix (method call, indexing, field access, etc.) sits
  // above unary so `&x.y` parses as `&(x.y)`.
  call: 14,
  // Binary operator precedences mirror chumsky_parser.rs.
  multiplicative: 12,
  additive: 11,
  shift: 10,
  comparison: 9,
  bitwise_and: 8,
  bitwise_xor: 7,
  bitwise_or: 6,
  logical_and: 5,
  logical_or: 4,
  // Path / `::` resolution.
  path: 15,
};

module.exports = grammar({
  name: 'gruel',

  extras: ($) => [
    /\s+/,
    $.line_comment,
    // `///` doc comments are emitted as tokens by the lexer but, for tree-
    // sitter acceptance purposes, we treat them as extras.
    $.doc_comment,
  ],

  word: ($) => $.identifier,

  conflicts: ($) => [
    // `Foo { ... }` can be a struct literal *or* a block following an
    // `if`/`while`/`for`/`match` condition. We accept both forms — the
    // GLR parser uses the trailing `{ ... }` shape to choose.
    [$._expression, $.struct_literal_path],
    [$.path_expression, $.struct_literal_path],
    // `@foo(bar)` is shape-compatible with both a directive (item-level
    // attribute) and an intrinsic call (expression). The two rules only
    // ever apply in disjoint positions, but tree-sitter's LR analysis
    // can't see that; declare a conflict so GLR picks the right one.
    [$._directive_arg, $._expression],
    [$._directive_arg, $._literal_expression],
    // A bare `{ ... }` in statement position could be a `bare_block`
    // (control-flow form, no trailing `;`) or the start of a
    // `block_expression` used as a value. Both are valid; the GLR
    // picks based on what follows.
    [$.bare_block, $.block_expression],
    [$.named_type, $._expression],
    [$._type, $.primitive_type_value],
    [$.anonymous_struct_type, $.anonymous_struct_expression],
  ],

  rules: {
    source_file: ($) => seq(repeat($._item)),

    // -------------------------------------------------------------- comments
    // The Gruel lexer treats `//`, `////`, and longer slash runs as plain
    // line comments and `///` (exactly three slashes) as a doc comment.
    // For acceptance-differential purposes we skip both forms uniformly,
    // but we expose them as separate tokens so editor queries can colour
    // them differently.
    line_comment: (_) => token(prec(1, seq('//', /[^\n]*/))),
    doc_comment: (_) => token(prec(2, seq('///', /[^\n]*/))),

    // -------------------------------------------------------------- literals
    integer_literal: (_) => /[0-9]+/,

    float_literal: (_) =>
      token(
        choice(
          /[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?/,
          /[0-9]+[eE][+-]?[0-9]+/,
        ),
      ),

    string_literal: ($) =>
      seq(
        '"',
        repeat(choice($._string_content, $.escape_sequence)),
        '"',
      ),

    _string_content: (_) => token.immediate(prec(1, /[^\\"\n]+/)),

    escape_sequence: (_) =>
      token.immediate(
        seq(
          '\\',
          choice(
            /[\\"nrt0]/,
            /u\{[0-9a-fA-F]{1,6}\}/,
          ),
        ),
      ),

    char_literal: (_) =>
      token(
        seq(
          "'",
          choice(
            /[^\\'\n\r]/,
            seq('\\', choice(/[\\'"nrt0]/, /u\{[0-9a-fA-F]{1,6}\}/)),
          ),
          "'",
        ),
      ),

    boolean_literal: (_) => choice('true', 'false'),

    unit_literal: (_) => prec(1, seq('(', ')')),

    self_literal: (_) => 'self',
    // `Self` appears in both type and value position. The value-form
    // shares its lexical content with the type-form `self_type`; we
    // distinguish them by giving `self_type_literal` lower precedence so
    // tree-sitter prefers `self_type` in type position.
    self_type_literal: (_) => prec(-1, 'Self'),

    identifier: (_) => /[a-zA-Z_][a-zA-Z0-9_]*/,

    // -------------------------------------------------------------- items
    _item: ($) =>
      choice(
        $.function_definition,
        $.struct_declaration,
        $.enum_declaration,
        $.interface_declaration,
        $.derive_declaration,
        $.const_declaration,
        $.link_extern_block,
      ),

    visibility: (_) => 'pub',

    directive: ($) =>
      seq(
        '@',
        field('name', $.identifier),
        optional(
          seq(
            '(',
            sepBy(',', $._directive_arg),
            ')',
          ),
        ),
      ),

    _directive_arg: ($) => choice($.identifier, $.string_literal),

    function_definition: ($) =>
      seq(
        repeat($.directive),
        optional($.visibility),
        'fn',
        field('name', $.identifier),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._type))),
        field('body', $.block),
      ),

    parameter_list: ($) =>
      seq('(', sepBy(',', $.parameter), ')'),

    parameter: ($) =>
      seq(
        optional('comptime'),
        field('name', choice($.identifier, $.self_literal)),
        optional(seq(':', field('type', $._type))),
      ),

    struct_declaration: ($) =>
      seq(
        repeat($.directive),
        optional($.visibility),
        'struct',
        field('name', $.identifier),
        field('body', $.struct_body),
      ),

    // Struct bodies are field declarations (comma-separated, with an
    // optional trailing comma) followed by zero or more methods. Methods
    // do not need commas between them — each ends with `}`. This matches
    // the chumsky parser's two-phase shape.
    struct_body: ($) =>
      seq(
        '{',
        optional(seq(
          $.field_declaration,
          repeat(seq(',', $.field_declaration)),
          optional(','),
        )),
        repeat($.method_definition),
        '}',
      ),

    _struct_member: ($) =>
      choice($.field_declaration, $.method_definition),

    field_declaration: ($) =>
      seq(
        optional($.visibility),
        field('name', $.identifier),
        ':',
        field('type', $._type),
      ),

    method_definition: ($) =>
      seq(
        repeat($.directive),
        optional($.visibility),
        'fn',
        field('name', $.identifier),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._type))),
        field('body', $.block),
      ),

    enum_declaration: ($) =>
      seq(
        repeat($.directive),
        optional($.visibility),
        'enum',
        field('name', $.identifier),
        field('body', $.enum_body),
      ),

    // Enum bodies share the same two-phase shape as struct bodies:
    // variants (comma-separated) then methods (no commas).
    enum_body: ($) =>
      seq(
        '{',
        optional(seq(
          $.enum_variant,
          repeat(seq(',', $.enum_variant)),
          optional(','),
        )),
        repeat($.method_definition),
        '}',
      ),

    enum_variant: ($) =>
      seq(
        field('name', $.identifier),
        optional(choice($.variant_tuple, $.variant_struct)),
      ),

    variant_tuple: ($) =>
      seq('(', sepByCommaWithTrailing($._type), ')'),

    variant_struct: ($) =>
      seq('{', sepByCommaWithTrailing($.field_declaration), '}'),

    interface_declaration: ($) =>
      seq(
        repeat($.directive),
        optional($.visibility),
        'interface',
        field('name', $.identifier),
        field('body', $.interface_body),
      ),

    interface_body: ($) =>
      seq('{', repeat($.interface_method), '}'),

    interface_method: ($) =>
      seq(
        repeat($.directive),
        'fn',
        field('name', $.identifier),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._type))),
        ';',
      ),

    derive_declaration: ($) =>
      seq(
        repeat($.directive),
        'derive',
        field('name', $.identifier),
        field('body', $.derive_body),
      ),

    derive_body: ($) =>
      seq('{', repeat($.method_definition), '}'),

    const_declaration: ($) =>
      seq(
        repeat($.directive),
        optional($.visibility),
        'const',
        field('name', $.identifier),
        optional(seq(':', field('type', $._type))),
        '=',
        field('value', $._expression),
        ';',
      ),

    link_extern_block: ($) =>
      seq(
        choice('link_extern', 'static_link_extern'),
        '(',
        field('library', $.string_literal),
        ')',
        '{',
        repeat($.extern_function_declaration),
        '}',
      ),

    extern_function_declaration: ($) =>
      seq(
        repeat($.directive),
        'fn',
        field('name', $.identifier),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._type))),
        ';',
      ),

    // -------------------------------------------------------------- types
    _type: ($) =>
      choice(
        $.unit_type,
        $.never_type,
        $.array_type,
        $.tuple_type,
        $.primitive_type,
        $.self_type,
        $.type_call,
        $.named_type,
        $.anonymous_struct_type,
      ),

    unit_type: (_) => prec(2, seq('(', ')')),

    never_type: (_) => '!',

    array_type: ($) =>
      seq('[', $._type, ';', $.integer_literal, ']'),

    tuple_type: ($) =>
      seq(
        '(',
        $._type,
        ',',
        sepByCommaWithTrailing($._type),
        ')',
      ),

    primitive_type: (_) =>
      choice(
        'i8', 'i16', 'i32', 'i64', 'isize',
        'u8', 'u16', 'u32', 'u64', 'usize',
        'f16', 'f32', 'f64',
        'bool', 'char',
      ),

    self_type: (_) => 'Self',

    named_type: ($) => $.identifier,

    // Parameterized type call (ADR-0057): `Name(arg, ...)`.
    type_call: ($) =>
      prec(
        1,
        seq(
          field('callee', $.identifier),
          '(',
          sepByCommaWithTrailing1($._type),
          ')',
        ),
      ),

    anonymous_struct_type: ($) =>
      seq(
        'struct',
        '{',
        sepByCommaWithTrailing($.field_declaration),
        '}',
      ),

    // -------------------------------------------------------------- block & statements
    block: ($) =>
      seq(
        '{',
        repeat($._block_statement),
        optional(field('final_expression', $._expression)),
        '}',
      ),

    _block_statement: ($) =>
      choice(
        $.let_statement,
        $.assignment_statement,
        $.expression_statement,
        $.control_flow_statement,
        $.empty_statement,
      ),

    empty_statement: (_) => ';',

    // The `mut` qualifier between `let` and the pattern is part of the
    // pattern (see `identifier_pattern: optional('mut') ident`), not the
    // statement: `let mut x = 1` is parsed as `let` + `mut x` pattern.
    let_statement: ($) =>
      seq(
        repeat($.directive),
        'let',
        field('pattern', $._pattern),
        optional(seq(':', field('type', $._type))),
        optional(seq('=', field('value', $._expression))),
        ';',
      ),

    // Assignment targets are syntactically a subset of expressions; the
    // compiler's sema layer does the actual "is this a place expression"
    // check (variable, field access, index). For acceptance-differential
    // purposes we parse the LHS as any expression.
    assignment_statement: ($) =>
      prec(
        2,
        seq(
          field('target', $._expression),
          '=',
          field('value', $._expression),
          ';',
        ),
      ),

    expression_statement: ($) =>
      seq(field('expression', $._expression), ';'),

    // Mid-block "control flow" expressions can appear without a trailing
    // `;`. Mirrors chumsky's `is_control_flow_expr` set: if/while/for/
    // match/loop/comptime-unroll/blocks. They are still parsed as
    // expressions; this rule simply marks the no-semi statement
    // position.
    control_flow_statement: ($) =>
      prec(
        2,
        choice(
          $.if_expression,
          $.while_expression,
          $.for_expression,
          $.match_expression,
          $.loop_expression,
          $.comptime_unroll_expression,
          $.bare_block,
        ),
      ),

    bare_block: ($) => $.block,

    // -------------------------------------------------------------- patterns
    _pattern: ($) =>
      choice(
        $.wildcard_pattern,
        $.identifier_pattern,
        $.tuple_pattern,
        $.struct_pattern,
        $.integer_pattern,
        $.boolean_pattern,
        $.path_pattern,
        $.tuple_struct_pattern,
      ),

    wildcard_pattern: (_) => '_',
    // Identifier patterns can carry an inner `mut` qualifier when used
    // in destructuring positions like `let (mut x, y) = …`. The
    // top-level `let mut x` form parses the `mut` at the `let_statement`
    // level, not here — see `let_statement`. Both forms are accepted.
    identifier_pattern: ($) => seq(optional('mut'), $.identifier),
    integer_pattern: ($) => seq(optional('-'), $.integer_literal),
    boolean_pattern: ($) => choice('true', 'false'),
    rest_pattern: (_) => '..',

    tuple_pattern: ($) =>
      seq('(', sepByCommaWithTrailing(choice($._pattern, $.rest_pattern)), ')'),

    // Path patterns mirror the path-expression shapes the canonical
    // parser accepts: `Type::Variant`, `Self::Variant`, and the
    // module-qualified `module.Type::Variant` form used in patterns.
    path_pattern: ($) =>
      prec(
        2,
        seq(
          choice($.identifier, $.self_type_literal),
          repeat(seq('.', $.identifier)),
          repeat1(seq('::', $.identifier)),
        ),
      ),

    tuple_struct_pattern: ($) =>
      prec(
        3,
        seq(
          choice($.identifier, $.self_type_literal, $.path_pattern),
          '(',
          sepByCommaWithTrailing(choice($._pattern, $.rest_pattern)),
          ')',
        ),
      ),

    struct_pattern: ($) =>
      prec(
        3,
        seq(
          choice($.identifier, $.path_pattern, $.self_type_literal),
          '{',
          sepByCommaWithTrailing(
            choice(
              $.struct_pattern_field,
              $.rest_pattern,
            ),
          ),
          '}',
        ),
      ),

    struct_pattern_field: ($) =>
      seq(
        optional('mut'),
        field('name', $.identifier),
        optional(seq(':', field('pattern', $._pattern))),
      ),

    // -------------------------------------------------------------- expressions
    _expression: ($) =>
      choice(
        $._literal_expression,
        $.identifier,
        $.self_literal,
        $.self_type_literal,
        $.primitive_type_value,
        $.paren_expression,
        $.tuple_expression,
        $.array_literal_expression,
        $.unary_expression,
        $.binary_expression,
        $.call_expression,
        $.method_call_expression,
        $.field_expression,
        $.tuple_index_expression,
        $.index_expression,
        $.path_expression,
        $.struct_literal_expression,
        $.intrinsic_call_expression,
        $.import_expression,
        $.if_expression,
        $.match_expression,
        $.while_expression,
        $.for_expression,
        $.loop_expression,
        $.return_expression,
        $.break_expression,
        $.continue_expression,
        $.block_expression,
        $.comptime_expression,
        $.comptime_unroll_expression,
        $.checked_expression,
        $.anonymous_function_expression,
        $.anonymous_struct_expression,
        $.anonymous_enum_expression,
        $.anonymous_interface_expression,
      ),

    _literal_expression: ($) =>
      choice(
        $.integer_literal,
        $.float_literal,
        $.string_literal,
        $.char_literal,
        $.boolean_literal,
        $.unit_literal,
      ),

    primitive_type_value: ($) => $.primitive_type,

    paren_expression: ($) =>
      prec(1, seq('(', $._expression, ')')),

    tuple_expression: ($) =>
      prec(
        2,
        seq(
          '(',
          $._expression,
          ',',
          sepByCommaWithTrailing($._expression),
          ')',
        ),
      ),

    array_literal_expression: ($) =>
      seq('[', sepByCommaWithTrailing($._expression), ']'),

    unary_expression: ($) =>
      prec(
        PREC.unary,
        seq(
          field(
            'operator',
            choice('-', '!', '~', seq('&', 'mut'), '&'),
          ),
          field('argument', $._expression),
        ),
      ),

    binary_expression: ($) => {
      const table = [
        [PREC.multiplicative, choice('*', '/', '%')],
        [PREC.additive, choice('+', '-')],
        [PREC.shift, choice('<<', '>>')],
        [PREC.comparison, choice('==', '!=', '<', '>', '<=', '>=')],
        [PREC.bitwise_and, '&'],
        [PREC.bitwise_xor, '^'],
        [PREC.bitwise_or, '|'],
        [PREC.logical_and, '&&'],
        [PREC.logical_or, '||'],
      ];
      return choice(
        ...table.map(([precedence, op]) =>
          prec.left(
            precedence,
            seq(
              field('left', $._expression),
              field('operator', op),
              field('right', $._expression),
            ),
          ),
        ),
      );
    },

    // Postfix operations are all left-associative at the same precedence
    // level. They chain via the shared `_expression` left-hand side.
    // `method_call_expression` is given a higher precedence than the
    // would-be `field_expression + call_expression` two-step so that
    // `a.b(c)` parses as a single method call rather than as a field
    // access followed by an unrelated call (which is what an LR(1)
    // engine would otherwise prefer because of the order it sees the
    // tokens).
    call_expression: ($) =>
      prec.left(
        PREC.call,
        seq(
          field('function', $._expression),
          '(',
          field('arguments', sepByCommaWithTrailing($._call_arg)),
          ')',
        ),
      ),

    _call_arg: ($) => $._expression,

    method_call_expression: ($) =>
      prec.left(
        PREC.call + 1,
        seq(
          field('receiver', $._expression),
          '.',
          field('method', $.identifier),
          '(',
          field('arguments', sepByCommaWithTrailing($._call_arg)),
          ')',
        ),
      ),

    field_expression: ($) =>
      prec.left(
        PREC.call,
        seq(
          field('object', $._expression),
          '.',
          field('field', $.identifier),
        ),
      ),

    tuple_index_expression: ($) =>
      prec.left(
        PREC.call,
        seq(
          field('object', $._expression),
          '.',
          field('index', $.integer_literal),
        ),
      ),

    // The index expression is also where slice ranges live (ADR-0064).
    // `..`, `..hi`, `lo..`, `lo..hi` are accepted only inside `[ ... ]`;
    // the leading `&` / `&mut` is parsed as a unary prefix elsewhere.
    index_expression: ($) =>
      prec.left(
        PREC.call,
        seq(
          field('object', $._expression),
          '[',
          field('index', choice($._expression, $.range_expression)),
          ']',
        ),
      ),

    range_expression: ($) =>
      choice(
        seq($._expression, '..', optional($._expression)),
        seq('..', optional($._expression)),
      ),

    // `::`-separated paths used as expressions. Accepts the canonical
    // forms — `Type::method`, `Self::variant`, `i32::MAX` — plus the
    // type-call form `Type(args)::method` (ADR-0063) and module-qualified
    // paths like `mod.Type::Variant` (ADR-0026).
    path_expression: ($) =>
      prec.right(
        PREC.path,
        seq(
          choice(
            $.identifier,
            $.self_type_literal,
            $.primitive_type,
            $.call_expression,
            $.field_expression,
          ),
          repeat1(seq('::', $.identifier)),
        ),
      ),

    // Struct literal paths use `::` (namespace) only here; `mod.Point { … }`
    // (dot-separated) is parsed as `(mod.Point) { … }` — i.e. the path
    // builds via `field_expression` and then the `{ … }` body is matched
    // by `struct_literal_expression` over an expression. Limiting this
    // rule to single-ident and `::` paths eliminates a conflict with
    // `field_expression`.
    struct_literal_path: ($) =>
      choice(
        $.identifier,
        $.self_type_literal,
        prec.right(PREC.path, seq($.identifier, repeat1(seq('::', $.identifier)))),
      ),

    struct_literal_expression: ($) =>
      prec(
        3,
        seq(
          field('path', $.struct_literal_path),
          '{',
          sepByCommaWithTrailing($.field_init),
          '}',
        ),
      ),

    field_init: ($) =>
      choice(
        seq(field('name', $.identifier), ':', field('value', $._expression)),
        field('name', $.identifier),
      ),

    intrinsic_call_expression: ($) =>
      seq(
        '@',
        field('name', $.identifier),
        '(',
        field('arguments', sepByCommaWithTrailing($._intrinsic_arg)),
        ')',
      ),

    // Intrinsic arguments are expressions, plus a handful of type forms
    // that have no expression counterpart (`[T; N]` and `!`).
    _intrinsic_arg: ($) =>
      choice($.type_array_expression, $.never_type, $._expression),

    // `[T; N]` as a value — a type literal that the canonical parser
    // represents as `Expr::TypeLit(TypeExpr::Array { ... })`. Treating
    // it as a distinct expression shape keeps the GLR happy.
    type_array_expression: ($) =>
      seq('[', $._type, ';', $.integer_literal, ']'),

    import_expression: ($) =>
      seq(
        '@import',
        '(',
        field('arguments', sepByCommaWithTrailing($._expression)),
        ')',
      ),

    // -------------------------------------------------------------- control flow
    // Optional `comptime` prefix marks the branch as comptime-dispatched
    // (ADR-0079 follow-up). Sema enforces the comptime context.
    if_expression: ($) =>
      seq(
        optional('comptime'),
        'if',
        field('condition', $._condition_expression),
        field('consequence', $.block),
        optional(
          seq(
            'else',
            field('alternative', choice($.block, $.if_expression)),
          ),
        ),
      ),

    // To avoid `if Foo { ... }` being parsed as struct literal, restrict
    // condition expressions a little. This matches what chumsky does in
    // practice — struct-literal disambiguation happens via parse state.
    _condition_expression: ($) => $._expression,

    while_expression: ($) =>
      seq(
        'while',
        field('condition', $._condition_expression),
        field('body', $.block),
      ),

    // No outer `mut` on for-loop bindings; the inner pattern carries any
    // mut qualifier via `identifier_pattern: optional('mut') ident`.
    for_expression: ($) =>
      seq(
        'for',
        field('pattern', $._pattern),
        'in',
        field('iterable', $._condition_expression),
        field('body', $.block),
      ),

    loop_expression: ($) => seq('loop', field('body', $.block)),

    match_expression: ($) =>
      seq(
        'match',
        field('subject', $._condition_expression),
        '{',
        sepByCommaWithTrailing($.match_arm),
        '}',
      ),

    match_arm: ($) =>
      choice(
        seq(
          field('pattern', $._pattern),
          optional(seq('if', field('guard', $._expression))),
          '=>',
          field('value', $._expression),
        ),
        // ADR-0079: comptime-unroll arm — `comptime_unroll for v in iter
        // { ... }` generates one arm per iteration at compile time.
        $.comptime_unroll_arm,
      ),

    comptime_unroll_arm: ($) =>
      seq(
        'comptime_unroll',
        'for',
        field('binding', $.identifier),
        'in',
        field('iterable', $._condition_expression),
        field('body', $.block),
      ),

    return_expression: ($) =>
      prec.right(seq('return', optional($._expression))),

    break_expression: (_) => 'break',
    continue_expression: (_) => 'continue',

    block_expression: ($) => $.block,

    comptime_expression: ($) => seq('comptime', $.block),

    // `comptime_unroll for pattern in iter { body }` — see ADR-0083.
    comptime_unroll_expression: ($) =>
      seq(
        'comptime_unroll',
        'for',
        field('pattern', $._pattern),
        'in',
        field('iterable', $._condition_expression),
        field('body', $.block),
      ),

    checked_expression: ($) => seq('checked', $.block),

    anonymous_function_expression: ($) =>
      seq(
        'fn',
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._type))),
        field('body', $.block),
      ),

    anonymous_struct_expression: ($) =>
      seq(
        repeat($.directive),
        'struct',
        '{',
        optional(seq(
          $.field_declaration,
          repeat(seq(',', $.field_declaration)),
          optional(','),
        )),
        repeat($.method_definition),
        '}',
      ),

    anonymous_enum_expression: ($) =>
      seq(
        repeat($.directive),
        'enum',
        '{',
        optional(seq(
          $.enum_variant,
          repeat(seq(',', $.enum_variant)),
          optional(','),
        )),
        repeat($.method_definition),
        '}',
      ),

    // ADR-0079: anonymous interface as a value, used in comptime
    // contexts: `interface { fn size(self) -> i32; }`.
    anonymous_interface_expression: ($) =>
      seq(
        repeat($.directive),
        'interface',
        '{',
        repeat($.interface_method),
        '}',
      ),
  },
});

// ------------------------------------------------------------ helpers

function sepBy(sep, rule) {
  return optional(seq(rule, repeat(seq(sep, rule)), optional(sep)));
}

function sepByCommaWithTrailing(rule) {
  return optional(seq(rule, repeat(seq(',', rule)), optional(',')));
}

function sepByCommaWithTrailing1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}
