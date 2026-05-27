//! AST walker that emits canonical Gruel text into a [`Printer`].
//!
//! Subsequent ADR-0093 phases extend the set of nodes handled here:
//! - Phase 1: smallest case (covered in `lib.rs` baseline).
//! - Phase 2: expressions, statements, patterns, types.
//! - Phase 3: top-level items (Function, StructDecl, EnumDecl, …).
//! - Phase 4: comment / blank-line weaving via a trivia table.
//!
//! Exhaustive `match`es are deliberate: a new AST node kind triggers a
//! compile-time error here rather than silently re-emitting nothing.

use gruel_parser::ast::{
    AnonFnExpr, AnonStructField, ArgMode, ArrayLitExpr, AssignStatement, AssignTarget,
    AssocFnCallExpr, Ast, BinaryExpr, BinaryOp, BlockExpr, BoolLit, BreakExpr, CallArg, CallExpr,
    CharLit, CheckedBlockExpr, ComptimeBlockExpr, ComptimeUnrollForExpr, ConstDecl, ContinueExpr,
    DeriveDecl, Directive, DirectiveArg, EnumDecl, EnumStructLitExpr, EnumVariant,
    EnumVariantField, EnumVariantKind, Expr, ExternFn, FieldDecl, FieldExpr, FieldInit,
    FieldPattern, FloatLit, ForExpr, Function, IfExpr, IndexExpr, IntLit, InterfaceDecl,
    IntrinsicArg, IntrinsicCallExpr, Item, LetStatement, LinkExternBlock, LinkMode, LoopExpr,
    MatchArm, MatchExpr, Method, MethodCallExpr, MethodSig, NegIntLit, Param, ParamMode, ParenExpr,
    PathExpr, PathPattern, Pattern, RangeExpr, ReturnExpr, SelfParam, SelfReceiverKind, Statement,
    StringLit, StructDecl, StructLitExpr, TupleElemPattern, TupleExpr, TupleIndexExpr, TypeExpr,
    TypeLitExpr, UnaryExpr, UnaryOp, UnitLit, Visibility, WhileExpr,
};
use gruel_util::Span;

use crate::Printer;

fn item_span(item: &Item) -> Span {
    match item {
        Item::Function(f) => f.span,
        Item::Struct(s) => s.span,
        Item::Enum(e) => e.span,
        Item::Interface(i) => i.span,
        Item::Derive(d) => d.span,
        Item::Const(c) => c.span,
        Item::LinkExtern(b) => b.span,
        Item::Error(s) => *s,
    }
}

fn statement_span(s: &Statement) -> Span {
    match s {
        Statement::Let(l) => l.span,
        Statement::Assign(a) => a.span,
        Statement::Expr(e) => e.span(),
    }
}

// ---------- top-level walkers ----------

pub fn emit_ast(p: &mut Printer<'_>, ast: &Ast) {
    if let Some(doc) = &ast.module_doc {
        // Module-level doc lives at the very top.
        emit_doc(p, doc);
    }
    for (i, item) in ast.items.iter().enumerate() {
        let span = item_span(item);
        if i > 0 || ast.module_doc.is_some() {
            p.blank_line();
        }
        // Drain `// comments` and blank-line runs preceding this item.
        p.drain_trivia_before(span.start);
        emit_item(p, item);
        p.mark_emitted_end(span.end);
    }
    // Flush any trailing trivia past the last item (file-trailing comments).
    p.drain_trivia_remaining();
}

pub fn emit_item(p: &mut Printer<'_>, item: &Item) {
    match item {
        Item::Function(f) => emit_function(p, f),
        Item::Struct(s) => emit_struct_decl(p, s),
        Item::Enum(e) => emit_enum_decl(p, e),
        Item::Interface(i) => emit_interface_decl(p, i),
        Item::Derive(d) => emit_derive_decl(p, d),
        Item::Const(c) => emit_const_decl(p, c),
        Item::LinkExtern(b) => emit_link_extern(p, b),
        Item::Error(_) => panic!("gruel-fmt: Item::Error in successfully-parsed AST"),
    }
}

// ---------- top-level items (Phase 3) ----------

fn emit_doc(p: &mut Printer<'_>, doc: &gruel_parser::ast::Doc) {
    for line in doc.body.split('\n') {
        if line.is_empty() {
            p.write_str("///");
        } else {
            p.write_str("/// ");
            p.write_str(line);
        }
        p.newline();
    }
}

fn emit_directives(p: &mut Printer<'_>, dirs: &[Directive]) {
    for dir in dirs {
        emit_directive(p, dir);
        p.newline();
    }
}

fn emit_directive(p: &mut Printer<'_>, dir: &Directive) {
    p.write_str("@");
    p.write_ident(dir.name.name);
    if !dir.args.is_empty() {
        p.write_str("(");
        for (i, arg) in dir.args.iter().enumerate() {
            if i > 0 {
                p.write_str(", ");
            }
            emit_directive_arg(p, arg);
        }
        p.write_str(")");
    }
}

fn emit_directive_arg(p: &mut Printer<'_>, arg: &DirectiveArg) {
    match arg {
        DirectiveArg::Ident(ident) => p.write_ident(ident.name),
        DirectiveArg::String(lit) => emit_string_literal(p, lit),
    }
}

fn emit_visibility(p: &mut Printer<'_>, vis: Visibility) {
    if matches!(vis, Visibility::Public) {
        p.write_str("pub ");
    }
}

fn emit_function(p: &mut Printer<'_>, f: &Function) {
    if let Some(doc) = &f.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &f.directives);
    emit_visibility(p, f.visibility);
    // `is_unchecked` is derived from `@mark(unchecked)` in `directives` — no
    // standalone keyword to emit here.
    p.write_str("fn ");
    p.write_ident(f.name.name);
    emit_param_list(p, &f.params);
    if let Some(ret) = &f.return_type {
        p.write_str(" -> ");
        emit_type_expr(p, ret);
    }
    p.write_str(" ");
    if let Expr::Block(block) = &f.body {
        emit_block(p, block);
    } else {
        // Function bodies are always blocks per the grammar; if a non-block
        // ever appears, the formatter notices loudly.
        panic!("gruel-fmt: Function body is not a BlockExpr");
    }
    p.newline();
}

fn emit_struct_decl(p: &mut Printer<'_>, s: &StructDecl) {
    if let Some(doc) = &s.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &s.directives);
    emit_visibility(p, s.visibility);
    p.write_str("struct ");
    p.write_ident(s.name.name);
    p.write_str(" {");
    if s.fields.is_empty() && s.methods.is_empty() {
        p.drain_trivia_before(s.span.end);
        p.write_str("}");
    } else {
        p.newline();
        p.indent();
        for field in &s.fields {
            p.drain_trivia_before(field.span.start);
            emit_field_decl(p, field);
            p.mark_emitted_end(field.span.end);
            p.write_str(",");
            p.drain_trailing_comment_on_line();
            p.newline();
        }
        if !s.fields.is_empty() && !s.methods.is_empty() {
            p.blank_line();
        }
        for (i, m) in s.methods.iter().enumerate() {
            if i > 0 {
                p.blank_line();
            }
            p.drain_trivia_before(m.span.start);
            emit_method(p, m);
            p.mark_emitted_end(m.span.end);
            p.newline();
        }
        p.drain_trivia_before(s.span.end);
        p.dedent();
        p.write_str("}");
    }
    p.newline();
}

fn emit_field_decl(p: &mut Printer<'_>, field: &FieldDecl) {
    if let Some(doc) = &field.doc {
        emit_doc(p, doc);
    }
    emit_visibility(p, field.visibility);
    p.write_ident(field.name.name);
    p.write_str(": ");
    emit_type_expr(p, &field.ty);
}

fn emit_enum_decl(p: &mut Printer<'_>, e: &EnumDecl) {
    if let Some(doc) = &e.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &e.directives);
    emit_visibility(p, e.visibility);
    p.write_str("enum ");
    p.write_ident(e.name.name);
    p.write_str(" {");
    if e.variants.is_empty() && e.methods.is_empty() {
        p.write_str("}");
    } else {
        p.newline();
        p.indent();
        for variant in &e.variants {
            emit_enum_variant(p, variant);
            p.write_str(",");
            p.newline();
        }
        if !e.variants.is_empty() && !e.methods.is_empty() {
            p.blank_line();
        }
        for (i, m) in e.methods.iter().enumerate() {
            if i > 0 {
                p.blank_line();
            }
            emit_method(p, m);
            p.newline();
        }
        p.dedent();
        p.write_str("}");
    }
    p.newline();
}

fn emit_enum_variant(p: &mut Printer<'_>, v: &EnumVariant) {
    if let Some(doc) = &v.doc {
        emit_doc(p, doc);
    }
    p.write_ident(v.name.name);
    match &v.kind {
        EnumVariantKind::Unit => {}
        EnumVariantKind::Tuple(tys) => {
            p.write_str("(");
            for (i, ty) in tys.iter().enumerate() {
                if i > 0 {
                    p.write_str(", ");
                }
                emit_type_expr(p, ty);
            }
            p.write_str(")");
        }
        EnumVariantKind::Struct(fields) => {
            p.write_str(" {");
            if fields.is_empty() {
                p.write_str("}");
            } else {
                p.newline();
                p.indent();
                for f in fields {
                    emit_enum_variant_field(p, f);
                    p.write_str(",");
                    p.newline();
                }
                p.dedent();
                p.write_str("}");
            }
        }
    }
}

fn emit_enum_variant_field(p: &mut Printer<'_>, f: &EnumVariantField) {
    if let Some(doc) = &f.doc {
        emit_doc(p, doc);
    }
    emit_visibility(p, f.visibility);
    p.write_ident(f.name.name);
    p.write_str(": ");
    emit_type_expr(p, &f.ty);
}

fn emit_interface_decl(p: &mut Printer<'_>, i: &InterfaceDecl) {
    if let Some(doc) = &i.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &i.directives);
    emit_visibility(p, i.visibility);
    p.write_str("interface ");
    p.write_ident(i.name.name);
    p.write_str(" {");
    if i.methods.is_empty() {
        p.write_str("}");
    } else {
        p.newline();
        p.indent();
        for (idx, sig) in i.methods.iter().enumerate() {
            if idx > 0 {
                p.newline();
            }
            emit_method_sig(p, sig);
            p.newline();
        }
        p.dedent();
        p.write_str("}");
    }
    p.newline();
}

fn emit_method_sig(p: &mut Printer<'_>, sig: &MethodSig) {
    if let Some(doc) = &sig.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &sig.directives);
    // `is_unchecked` reflects `@mark(unchecked)` in `directives`.
    p.write_str("fn ");
    p.write_ident(sig.name.name);
    p.write_str("(");
    emit_self_param(p, &sig.receiver);
    for param in &sig.params {
        p.write_str(", ");
        emit_param(p, param);
    }
    p.write_str(")");
    if let Some(ret) = &sig.return_type {
        p.write_str(" -> ");
        emit_type_expr(p, ret);
    }
    p.write_str(";");
}

fn emit_derive_decl(p: &mut Printer<'_>, d: &DeriveDecl) {
    if let Some(doc) = &d.doc {
        emit_doc(p, doc);
    }
    p.write_str("derive ");
    p.write_ident(d.name.name);
    p.write_str(" {");
    if d.methods.is_empty() {
        p.write_str("}");
    } else {
        p.newline();
        p.indent();
        for (idx, m) in d.methods.iter().enumerate() {
            if idx > 0 {
                p.blank_line();
            }
            emit_method(p, m);
            p.newline();
        }
        p.dedent();
        p.write_str("}");
    }
    p.newline();
}

fn emit_const_decl(p: &mut Printer<'_>, c: &ConstDecl) {
    if let Some(doc) = &c.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &c.directives);
    emit_visibility(p, c.visibility);
    p.write_str("const ");
    p.write_ident(c.name.name);
    if let Some(ty) = &c.ty {
        p.write_str(": ");
        emit_type_expr(p, ty);
    }
    p.write_str(" = ");
    emit_expr(p, &c.init);
    p.write_str(";");
    p.newline();
}

fn emit_link_extern(p: &mut Printer<'_>, b: &LinkExternBlock) {
    if let Some(doc) = &b.doc {
        emit_doc(p, doc);
    }
    match b.link_mode {
        LinkMode::Dynamic => p.write_str("link_extern("),
        LinkMode::Static => p.write_str("static_link_extern("),
    }
    emit_string_literal(p, &b.library);
    p.write_str(") {");
    if b.items.is_empty() {
        p.write_str("}");
    } else {
        p.newline();
        p.indent();
        for (idx, item) in b.items.iter().enumerate() {
            if idx > 0 {
                p.newline();
            }
            emit_extern_fn(p, item);
            p.newline();
        }
        p.dedent();
        p.write_str("}");
    }
    p.newline();
}

fn emit_extern_fn(p: &mut Printer<'_>, f: &ExternFn) {
    if let Some(doc) = &f.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &f.directives);
    p.write_str("fn ");
    p.write_ident(f.name.name);
    emit_param_list(p, &f.params);
    if let Some(ret) = &f.return_type {
        p.write_str(" -> ");
        emit_type_expr(p, ret);
    }
    p.write_str(";");
}

fn emit_method(p: &mut Printer<'_>, m: &Method) {
    if let Some(doc) = &m.doc {
        emit_doc(p, doc);
    }
    emit_directives(p, &m.directives);
    emit_visibility(p, m.visibility);
    // `is_unchecked` reflects `@mark(unchecked)` in `directives`.
    p.write_str("fn ");
    p.write_ident(m.name.name);
    p.write_str("(");
    let mut first = true;
    if let Some(recv) = &m.receiver {
        emit_self_param(p, recv);
        first = false;
    }
    for param in &m.params {
        if !first {
            p.write_str(", ");
        }
        emit_param(p, param);
        first = false;
    }
    p.write_str(")");
    if let Some(ret) = &m.return_type {
        p.write_str(" -> ");
        emit_type_expr(p, ret);
    }
    p.write_str(" ");
    if let Expr::Block(b) = &m.body {
        emit_block(p, b);
    } else {
        panic!("gruel-fmt: Method body is not a BlockExpr");
    }
}

fn emit_param_list(p: &mut Printer<'_>, params: &[Param]) {
    p.write_str("(");
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        emit_param(p, param);
    }
    p.write_str(")");
}

fn emit_param(p: &mut Printer<'_>, param: &Param) {
    if param.is_comptime || matches!(param.mode, ParamMode::Comptime) {
        p.write_str("comptime ");
    }
    p.write_ident(param.name.name);
    p.write_str(": ");
    emit_type_expr(p, &param.ty);
}

fn emit_self_param(p: &mut Printer<'_>, sp: &SelfParam) {
    match sp.kind {
        SelfReceiverKind::ByValue => p.write_str("self"),
        SelfReceiverKind::Ref => p.write_str("self: Ref(Self)"),
        SelfReceiverKind::MutRef => p.write_str("self: MutRef(Self)"),
    }
}

// ---------- expressions (Phase 2) ----------

pub fn emit_expr(p: &mut Printer<'_>, expr: &Expr) {
    match expr {
        Expr::Int(lit) => emit_int_literal(p, lit),
        Expr::Float(lit) => emit_float_literal(p, lit),
        Expr::String(lit) => emit_string_literal(p, lit),
        Expr::Char(lit) => emit_char_literal(p, lit),
        Expr::Bool(lit) => emit_bool_literal(p, lit),
        Expr::Unit(lit) => emit_unit_literal(p, lit),
        Expr::Ident(ident) => p.write_ident(ident.name),
        Expr::SelfExpr(_) => p.write_str("self"),
        Expr::Binary(b) => emit_binary(p, b),
        Expr::Unary(u) => emit_unary(p, u),
        Expr::Paren(par) => emit_paren(p, par),
        Expr::Block(b) => emit_block(p, b),
        Expr::If(if_expr) => emit_if(p, if_expr),
        Expr::Match(m) => emit_match(p, m),
        Expr::While(w) => emit_while(p, w),
        Expr::For(f) => emit_for(p, f),
        Expr::Loop(l) => emit_loop(p, l),
        Expr::Call(c) => emit_call(p, c),
        Expr::Break(b) => emit_break(p, b),
        Expr::Continue(c) => emit_continue(p, c),
        Expr::Return(r) => emit_return(p, r),
        Expr::StructLit(s) => emit_struct_lit(p, s),
        Expr::Field(f) => emit_field_expr(p, f),
        Expr::MethodCall(mc) => emit_method_call(p, mc),
        Expr::IntrinsicCall(ic) => emit_intrinsic_call(p, ic),
        Expr::ArrayLit(a) => emit_array_lit(p, a),
        Expr::Index(idx) => emit_index(p, idx),
        Expr::Path(path) => emit_path(p, path),
        Expr::EnumStructLit(e) => emit_enum_struct_lit(p, e),
        Expr::AssocFnCall(a) => emit_assoc_fn_call(p, a),
        Expr::Comptime(c) => emit_comptime_block(p, c),
        Expr::ComptimeUnrollFor(c) => emit_comptime_unroll_for(p, c),
        Expr::Checked(c) => emit_checked_block(p, c),
        Expr::TypeLit(t) => emit_type_lit(p, t),
        Expr::Tuple(t) => emit_tuple(p, t),
        Expr::TupleIndex(t) => emit_tuple_index(p, t),
        Expr::Range(r) => emit_range(p, r),
        Expr::AnonFn(a) => emit_anon_fn(p, a),
        Expr::Error(_) => panic!("gruel-fmt: Expr::Error in successfully-parsed AST"),
    }
}

fn emit_int_literal(p: &mut Printer<'_>, lit: &IntLit) {
    p.write_str(&lit.value.to_string());
}

fn emit_float_literal(p: &mut Printer<'_>, lit: &FloatLit) {
    let v = f64::from_bits(lit.bits);
    let s = format!("{}", v);
    if s.contains('.')
        || s.contains('e')
        || s.contains('E')
        || s.contains("inf")
        || s.contains("NaN")
    {
        p.write_str(&s);
    } else {
        p.write_str(&s);
        p.write_str(".0");
    }
}

fn emit_string_literal(p: &mut Printer<'_>, lit: &StringLit) {
    p.write_str("\"");
    let s = p.resolve(lit.value).to_string();
    for c in s.chars() {
        match c {
            '\\' => p.write_str("\\\\"),
            '"' => p.write_str("\\\""),
            '\n' => p.write_str("\\n"),
            '\t' => p.write_str("\\t"),
            '\r' => p.write_str("\\r"),
            '\0' => p.write_str("\\0"),
            c => {
                let mut buf = [0u8; 4];
                p.write_str(c.encode_utf8(&mut buf));
            }
        }
    }
    p.write_str("\"");
}

fn emit_char_literal(p: &mut Printer<'_>, lit: &CharLit) {
    p.write_str("'");
    match char::from_u32(lit.value) {
        Some(c) => match c {
            '\\' => p.write_str("\\\\"),
            '\'' => p.write_str("\\'"),
            '\n' => p.write_str("\\n"),
            '\t' => p.write_str("\\t"),
            '\r' => p.write_str("\\r"),
            '\0' => p.write_str("\\0"),
            c => {
                let mut buf = [0u8; 4];
                p.write_str(c.encode_utf8(&mut buf));
            }
        },
        None => {
            // Non-USV code point: emit a \u{...} escape. The lexer doesn't
            // accept this today, but we keep the formatter total.
            p.write_str(&format!("\\u{{{:X}}}", lit.value));
        }
    }
    p.write_str("'");
}

fn emit_bool_literal(p: &mut Printer<'_>, lit: &BoolLit) {
    p.write_str(if lit.value { "true" } else { "false" });
}

fn emit_unit_literal(p: &mut Printer<'_>, _lit: &UnitLit) {
    p.write_str("()");
}

fn emit_binary(p: &mut Printer<'_>, b: &BinaryExpr) {
    emit_expr(p, &b.left);
    p.write_str(" ");
    p.write_str(binary_op_str(b.op));
    p.write_str(" ");
    emit_expr(p, &b.right);
}

fn emit_unary(p: &mut Printer<'_>, u: &UnaryExpr) {
    p.write_str(unary_op_str(u.op));
    emit_expr(p, &u.operand);
}

fn emit_paren(p: &mut Printer<'_>, par: &ParenExpr) {
    p.write_str("(");
    emit_expr(p, &par.inner);
    p.write_str(")");
}

pub fn emit_block(p: &mut Printer<'_>, block: &BlockExpr) {
    p.write_str("{");
    if block.statements.is_empty() && matches!(*block.expr, Expr::Unit(_)) {
        // Empty block: still drain trivia (`{ // comment }` keeps the
        // comment) so it doesn't escape into surrounding context.
        p.drain_trivia_before(block.span.end);
        p.write_str("}");
        return;
    }
    p.newline();
    p.indent();
    for stmt in &block.statements {
        let span = statement_span(stmt);
        p.drain_trivia_before(span.start);
        emit_statement(p, stmt);
        p.mark_emitted_end(span.end);
        p.drain_trailing_comment_on_line();
        p.newline();
    }
    if !matches!(*block.expr, Expr::Unit(_)) {
        let span = block.expr.span();
        p.drain_trivia_before(span.start);
        emit_expr(p, &block.expr);
        p.mark_emitted_end(span.end);
        p.drain_trailing_comment_on_line();
        p.newline();
    }
    // Drain any trailing trivia inside this block (e.g. comment just before `}`).
    p.drain_trivia_before(block.span.end);
    p.dedent();
    p.write_str("}");
}

fn emit_if(p: &mut Printer<'_>, e: &IfExpr) {
    if e.is_comptime {
        p.write_str("comptime ");
    }
    p.write_str("if ");
    emit_expr(p, &e.cond);
    p.write_str(" ");
    emit_block(p, &e.then_block);
    if let Some(else_block) = &e.else_block {
        p.write_str(" else ");
        // `else if` collapses if the else block is exactly one if-expression
        // with no other statements.
        if else_block.statements.is_empty()
            && let Expr::If(inner) = &*else_block.expr
        {
            emit_if(p, inner);
            return;
        }
        emit_block(p, else_block);
    }
}

fn emit_match(p: &mut Printer<'_>, m: &MatchExpr) {
    p.write_str("match ");
    emit_expr(p, &m.scrutinee);
    p.write_str(" {");
    if m.arms.is_empty() {
        p.write_str("}");
        return;
    }
    p.newline();
    p.indent();
    for arm in &m.arms {
        emit_match_arm(p, arm);
        p.write_str(",");
        p.newline();
    }
    p.dedent();
    p.write_str("}");
}

fn emit_match_arm(p: &mut Printer<'_>, arm: &MatchArm) {
    emit_pattern(p, &arm.pattern);
    // ADR-0079: a `comptime_unroll for` arm pattern is followed directly by
    // its body block — no `=>` between them. Every other pattern uses
    // `pat => body`.
    match &arm.pattern {
        Pattern::ComptimeUnrollArm { .. } => p.write_str(" "),
        _ => p.write_str(" => "),
    }
    emit_expr(p, &arm.body);
}

fn emit_while(p: &mut Printer<'_>, w: &WhileExpr) {
    p.write_str("while ");
    emit_expr(p, &w.cond);
    p.write_str(" ");
    emit_block(p, &w.body);
}

fn emit_for(p: &mut Printer<'_>, f: &ForExpr) {
    p.write_str("for ");
    if f.is_mut {
        p.write_str("mut ");
    }
    p.write_ident(f.binding.name);
    p.write_str(" in ");
    emit_expr(p, &f.iterable);
    p.write_str(" ");
    emit_block(p, &f.body);
}

fn emit_loop(p: &mut Printer<'_>, l: &LoopExpr) {
    p.write_str("loop ");
    emit_block(p, &l.body);
}

fn emit_call(p: &mut Printer<'_>, c: &CallExpr) {
    p.write_ident(c.name.name);
    emit_call_args(p, &c.args);
}

fn emit_call_args(p: &mut Printer<'_>, args: &[CallArg]) {
    p.write_str("(");
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        match arg.mode {
            ArgMode::Normal => {}
        }
        emit_expr(p, &arg.expr);
    }
    p.write_str(")");
}

fn emit_break(p: &mut Printer<'_>, _b: &BreakExpr) {
    p.write_str("break");
}

fn emit_continue(p: &mut Printer<'_>, _c: &ContinueExpr) {
    p.write_str("continue");
}

fn emit_return(p: &mut Printer<'_>, r: &ReturnExpr) {
    p.write_str("return");
    if let Some(value) = &r.value {
        p.write_str(" ");
        emit_expr(p, value);
    }
}

fn emit_struct_lit(p: &mut Printer<'_>, s: &StructLitExpr) {
    if let Some(base) = &s.base {
        emit_expr(p, base);
        p.write_str(".");
    }
    p.write_ident(s.name.name);
    emit_field_inits(p, &s.fields);
}

fn emit_field_inits(p: &mut Printer<'_>, fields: &[FieldInit]) {
    if fields.is_empty() {
        p.write_str(" {}");
        return;
    }
    p.write_str(" { ");
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        emit_field_init(p, f);
    }
    p.write_str(" }");
}

fn emit_field_init(p: &mut Printer<'_>, f: &FieldInit) {
    p.write_ident(f.name.name);
    p.write_str(": ");
    emit_expr(p, &f.value);
}

fn emit_field_expr(p: &mut Printer<'_>, f: &FieldExpr) {
    emit_expr(p, &f.base);
    p.write_str(".");
    p.write_ident(f.field.name);
}

fn emit_method_call(p: &mut Printer<'_>, m: &MethodCallExpr) {
    emit_expr(p, &m.receiver);
    p.write_str(".");
    p.write_ident(m.method.name);
    emit_call_args(p, &m.args);
}

fn emit_intrinsic_call(p: &mut Printer<'_>, c: &IntrinsicCallExpr) {
    p.write_str("@");
    p.write_ident(c.name.name);
    p.write_str("(");
    for (i, arg) in c.args.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        match arg {
            IntrinsicArg::Expr(e) => emit_expr(p, e),
            IntrinsicArg::Type(t) => emit_type_expr(p, t),
        }
    }
    p.write_str(")");
}

fn emit_array_lit(p: &mut Printer<'_>, a: &ArrayLitExpr) {
    p.write_str("[");
    for (i, elem) in a.elements.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        emit_expr(p, elem);
    }
    p.write_str("]");
}

fn emit_index(p: &mut Printer<'_>, i: &IndexExpr) {
    emit_expr(p, &i.base);
    p.write_str("[");
    emit_expr(p, &i.index);
    p.write_str("]");
}

fn emit_path(p: &mut Printer<'_>, path: &PathExpr) {
    if let Some(base) = &path.base {
        emit_expr(p, base);
        p.write_str(".");
    }
    p.write_ident(path.type_name.name);
    p.write_str("::");
    p.write_ident(path.variant.name);
}

fn emit_enum_struct_lit(p: &mut Printer<'_>, e: &EnumStructLitExpr) {
    if let Some(base) = &e.base {
        emit_expr(p, base);
        p.write_str(".");
    }
    p.write_ident(e.type_name.name);
    p.write_str("::");
    p.write_ident(e.variant.name);
    emit_field_inits(p, &e.fields);
}

fn emit_assoc_fn_call(p: &mut Printer<'_>, a: &AssocFnCallExpr) {
    if let Some(base) = &a.base {
        emit_expr(p, base);
        p.write_str(".");
    }
    p.write_ident(a.type_name.name);
    if !a.type_args.is_empty() {
        p.write_str("(");
        for (i, t) in a.type_args.iter().enumerate() {
            if i > 0 {
                p.write_str(", ");
            }
            emit_expr(p, t);
        }
        p.write_str(")");
    }
    p.write_str("::");
    p.write_ident(a.function.name);
    emit_call_args(p, &a.args);
}

fn emit_comptime_block(p: &mut Printer<'_>, c: &ComptimeBlockExpr) {
    p.write_str("comptime ");
    if let Expr::Block(b) = &*c.expr {
        emit_block(p, b);
    } else {
        // `comptime <expr>` syntax — emit verbatim.
        emit_expr(p, &c.expr);
    }
}

fn emit_comptime_unroll_for(p: &mut Printer<'_>, c: &ComptimeUnrollForExpr) {
    p.write_str("comptime_unroll for ");
    p.write_ident(c.binding.name);
    p.write_str(" in ");
    emit_expr(p, &c.iterable);
    p.write_str(" ");
    emit_block(p, &c.body);
}

fn emit_checked_block(p: &mut Printer<'_>, c: &CheckedBlockExpr) {
    p.write_str("checked ");
    if let Expr::Block(b) = &*c.expr {
        emit_block(p, b);
    } else {
        emit_expr(p, &c.expr);
    }
}

fn emit_type_lit(p: &mut Printer<'_>, t: &TypeLitExpr) {
    emit_type_expr(p, &t.type_expr);
}

fn emit_tuple(p: &mut Printer<'_>, t: &TupleExpr) {
    p.write_str("(");
    for (i, elem) in t.elems.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        emit_expr(p, elem);
    }
    if t.elems.len() == 1 {
        // 1-tuples require a trailing comma to disambiguate from parens.
        p.write_str(",");
    }
    p.write_str(")");
}

fn emit_tuple_index(p: &mut Printer<'_>, t: &TupleIndexExpr) {
    emit_expr(p, &t.base);
    p.write_str(".");
    p.write_str(&t.index.to_string());
}

fn emit_range(p: &mut Printer<'_>, r: &RangeExpr) {
    if let Some(lo) = &r.lo {
        emit_expr(p, lo);
    }
    p.write_str("..");
    if let Some(hi) = &r.hi {
        emit_expr(p, hi);
    }
}

fn emit_anon_fn(p: &mut Printer<'_>, a: &AnonFnExpr) {
    p.write_str("fn");
    emit_param_list(p, &a.params);
    if let Some(ret) = &a.return_type {
        p.write_str(" -> ");
        emit_type_expr(p, ret);
    }
    p.write_str(" ");
    emit_block(p, &a.body);
}

// ---------- statements (Phase 2) ----------

pub fn emit_statement(p: &mut Printer<'_>, stmt: &Statement) {
    match stmt {
        Statement::Let(l) => emit_let(p, l),
        Statement::Assign(a) => emit_assign(p, a),
        Statement::Expr(e) => {
            emit_expr(p, e);
            // Block-like expressions used as statements don't carry a `;`
            // (matches rustfmt convention: `while {} ;` collapses to
            // `while {}`).
            if !is_block_like_expr(e) {
                p.write_str(";");
            }
        }
    }
}

fn is_block_like_expr(e: &Expr) -> bool {
    // Only expressions the parser accepts as bare statements (no trailing
    // `;` required between this expr and the next statement). `checked`,
    // `comptime`, and `comptime_unroll for` are *expressions*, not
    // statements — they still need a semicolon when used in statement
    // position.
    matches!(
        e,
        Expr::Block(_)
            | Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::For(_)
            | Expr::Loop(_)
    )
}

fn emit_let(p: &mut Printer<'_>, l: &LetStatement) {
    emit_directives(p, &l.directives);
    p.write_str("let ");
    if let Pattern::Ident { name, .. } = &l.pattern {
        if l.is_mut {
            p.write_str("mut ");
        }
        p.write_ident(name.name);
    } else {
        emit_pattern(p, &l.pattern);
    }
    if let Some(ty) = &l.ty {
        p.write_str(": ");
        emit_type_expr(p, ty);
    }
    p.write_str(" = ");
    emit_expr(p, &l.init);
    p.write_str(";");
}

fn emit_assign(p: &mut Printer<'_>, a: &AssignStatement) {
    emit_assign_target(p, &a.target);
    p.write_str(" = ");
    emit_expr(p, &a.value);
    p.write_str(";");
}

fn emit_assign_target(p: &mut Printer<'_>, target: &AssignTarget) {
    match target {
        AssignTarget::Var(ident) => p.write_ident(ident.name),
        AssignTarget::Field(f) => emit_field_expr(p, f),
        AssignTarget::Index(i) => emit_index(p, i),
    }
}

// ---------- patterns (Phase 2) ----------

pub fn emit_pattern(p: &mut Printer<'_>, pat: &Pattern) {
    match pat {
        Pattern::Wildcard(_) => p.write_str("_"),
        Pattern::Ident { is_mut, name, .. } => {
            if *is_mut {
                p.write_str("mut ");
            }
            p.write_ident(name.name);
        }
        Pattern::Int(lit) => p.write_str(&lit.value.to_string()),
        Pattern::NegInt(NegIntLit { value, .. }) => {
            p.write_str("-");
            p.write_str(&value.to_string());
        }
        Pattern::Bool(BoolLit { value, .. }) => {
            p.write_str(if *value { "true" } else { "false" });
        }
        Pattern::Path(pp) => emit_path_pattern(p, pp),
        Pattern::DataVariant {
            base,
            type_name,
            variant,
            fields,
            ..
        } => {
            if let Some(b) = base {
                emit_expr(p, b);
                p.write_str(".");
            }
            p.write_ident(type_name.name);
            p.write_str("::");
            p.write_ident(variant.name);
            p.write_str("(");
            for (i, fld) in fields.iter().enumerate() {
                if i > 0 {
                    p.write_str(", ");
                }
                emit_tuple_elem_pattern(p, fld);
            }
            p.write_str(")");
        }
        Pattern::StructVariant {
            base,
            type_name,
            variant,
            fields,
            ..
        } => {
            if let Some(b) = base {
                emit_expr(p, b);
                p.write_str(".");
            }
            p.write_ident(type_name.name);
            p.write_str("::");
            p.write_ident(variant.name);
            emit_field_patterns(p, fields);
        }
        Pattern::Struct {
            type_name, fields, ..
        } => {
            p.write_ident(type_name.name);
            emit_field_patterns(p, fields);
        }
        Pattern::Tuple { elems, .. } => {
            p.write_str("(");
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    p.write_str(", ");
                }
                emit_tuple_elem_pattern(p, e);
            }
            if elems.len() == 1 {
                p.write_str(",");
            }
            p.write_str(")");
        }
        Pattern::ComptimeUnrollArm {
            binding, iterable, ..
        } => {
            p.write_str("comptime_unroll for ");
            p.write_ident(binding.name);
            p.write_str(" in ");
            emit_expr(p, iterable);
        }
    }
}

fn emit_path_pattern(p: &mut Printer<'_>, pp: &PathPattern) {
    if let Some(base) = &pp.base {
        emit_expr(p, base);
        p.write_str(".");
    }
    p.write_ident(pp.type_name.name);
    p.write_str("::");
    p.write_ident(pp.variant.name);
}

fn emit_field_patterns(p: &mut Printer<'_>, fields: &[FieldPattern]) {
    if fields.is_empty() {
        p.write_str(" {}");
        return;
    }
    p.write_str(" { ");
    for (i, fp) in fields.iter().enumerate() {
        if i > 0 {
            p.write_str(", ");
        }
        emit_field_pattern(p, fp);
    }
    p.write_str(" }");
}

fn emit_field_pattern(p: &mut Printer<'_>, fp: &FieldPattern) {
    match &fp.field_name {
        None => p.write_str(".."),
        Some(name) => match &fp.sub {
            None => {
                if fp.is_mut {
                    p.write_str("mut ");
                }
                p.write_ident(name.name);
            }
            Some(sub) => {
                p.write_ident(name.name);
                p.write_str(": ");
                emit_pattern(p, sub);
            }
        },
    }
}

fn emit_tuple_elem_pattern(p: &mut Printer<'_>, te: &TupleElemPattern) {
    match te {
        TupleElemPattern::Pattern(pat) => emit_pattern(p, pat),
        TupleElemPattern::Rest(_) => p.write_str(".."),
    }
}

// ---------- type expressions ----------

pub fn emit_type_expr(p: &mut Printer<'_>, ty: &TypeExpr) {
    match ty {
        TypeExpr::Named(ident) => p.write_ident(ident.name),
        TypeExpr::Unit(_) => p.write_str("()"),
        TypeExpr::Never(_) => p.write_str("!"),
        TypeExpr::Array {
            element, length, ..
        } => {
            p.write_str("[");
            emit_type_expr(p, element);
            p.write_str("; ");
            p.write_str(&length.to_string());
            p.write_str("]");
        }
        TypeExpr::AnonymousStruct {
            directives,
            fields,
            methods,
            ..
        } => {
            emit_directives_inline(p, directives);
            p.write_str("struct {");
            if fields.is_empty() && methods.is_empty() {
                p.write_str("}");
            } else {
                p.newline();
                p.indent();
                for f in fields {
                    emit_anon_struct_field(p, f);
                    p.write_str(",");
                    p.newline();
                }
                if !fields.is_empty() && !methods.is_empty() {
                    p.blank_line();
                }
                for (i, m) in methods.iter().enumerate() {
                    if i > 0 {
                        p.blank_line();
                    }
                    emit_method(p, m);
                    p.newline();
                }
                p.dedent();
                p.write_str("}");
            }
        }
        TypeExpr::AnonymousEnum {
            directives,
            variants,
            methods,
            ..
        } => {
            emit_directives_inline(p, directives);
            p.write_str("enum {");
            if variants.is_empty() && methods.is_empty() {
                p.write_str("}");
            } else {
                p.newline();
                p.indent();
                for v in variants {
                    emit_enum_variant(p, v);
                    p.write_str(",");
                    p.newline();
                }
                if !variants.is_empty() && !methods.is_empty() {
                    p.blank_line();
                }
                for (i, m) in methods.iter().enumerate() {
                    if i > 0 {
                        p.blank_line();
                    }
                    emit_method(p, m);
                    p.newline();
                }
                p.dedent();
                p.write_str("}");
            }
        }
        TypeExpr::AnonymousInterface { methods, .. } => {
            p.write_str("interface {");
            if methods.is_empty() {
                p.write_str("}");
            } else {
                p.newline();
                p.indent();
                for sig in methods {
                    emit_method_sig(p, sig);
                    p.newline();
                }
                p.dedent();
                p.write_str("}");
            }
        }
        TypeExpr::TypeCall { callee, args, .. } => {
            p.write_ident(callee.name);
            p.write_str("(");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    p.write_str(", ");
                }
                emit_type_expr(p, a);
            }
            p.write_str(")");
        }
        TypeExpr::Tuple { elems, .. } => {
            p.write_str("(");
            for (i, t) in elems.iter().enumerate() {
                if i > 0 {
                    p.write_str(", ");
                }
                emit_type_expr(p, t);
            }
            if elems.len() == 1 {
                p.write_str(",");
            }
            p.write_str(")");
        }
    }
}

fn emit_directives_inline(p: &mut Printer<'_>, dirs: &[Directive]) {
    for dir in dirs {
        emit_directive(p, dir);
        p.write_str(" ");
    }
}

fn emit_anon_struct_field(p: &mut Printer<'_>, f: &AnonStructField) {
    if let Some(doc) = &f.doc {
        emit_doc(p, doc);
    }
    p.write_ident(f.name.name);
    p.write_str(": ");
    emit_type_expr(p, &f.ty);
}

// ---------- operator tables ----------

fn binary_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Gt => ">",
        BinaryOp::Le => "<=",
        BinaryOp::Ge => ">=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
    }
}

fn unary_op_str(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
        UnaryOp::BitNot => "~",
        UnaryOp::Ref => "&",
        UnaryOp::MutRef => "&mut ",
    }
}
