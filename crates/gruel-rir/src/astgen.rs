//! AST to RIR generation.
//!
//! AstGen converts the Abstract Syntax Tree into RIR instructions.
//! This is analogous to Zig's AstGen phase.

use lasso::{Spur, ThreadedRodeo};

use gruel_intrinsics::is_type_intrinsic;
use gruel_parser::ast::{
    BlockExpr, ConstDecl, DeriveDecl, Directives, DropFn, FieldPattern, Ident, SelfParam,
    TupleElemPattern,
};
use gruel_parser::{
    ArgMode, AssignTarget, Ast, BinaryOp, CallArg, Directive, DirectiveArg, EnumDecl, Expr,
    Function, IntrinsicArg, Item, Method, ParamMode, Pattern, Statement, StructDecl, TypeExpr,
    UnaryOp, ast::Visibility,
};

use crate::inst::{
    FunctionSpan, Inst, InstData, InstRef, Rir, RirArgMode, RirCallArg, RirDestructureField,
    RirDirective, RirParam, RirParamMode, RirPattern, RirPatternBinding, RirStructField,
    RirStructPatternBinding,
};

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// The AST being processed
    ast: &'a Ast,
    /// String interner for symbols (thread-safe, takes shared reference)
    interner: &'a ThreadedRodeo,
    /// Output RIR
    rir: Rir,
    /// Counter for generating unique synthetic binding names used by the
    /// refutable-nested-match elaborator for its intermediate scrutinee
    /// locals. Shared by `fresh_nested_pat_name`.
    nested_pat_counter: u32,
}

impl<'a> AstGen<'a> {
    /// Create a new AstGen for the given AST.
    pub fn new(ast: &'a Ast, interner: &'a ThreadedRodeo) -> Self {
        Self {
            ast,
            interner,
            rir: Rir::new(),
            nested_pat_counter: 0,
        }
    }

    /// Generate a fresh synthetic symbol name for an intermediate binding
    /// used by `try_elaborate_refutable_nested_match` when rewriting
    /// refutable-nested variant arms. Interned once and reused as both the
    /// destructure-field binding name and the `VarRef` key.
    fn fresh_nested_pat_name(&mut self) -> Spur {
        let n = self.nested_pat_counter;
        self.nested_pat_counter += 1;
        self.interner.get_or_intern(format!("__nested_pat_{}", n))
    }

    /// Generate RIR from the AST.
    pub fn generate(mut self) -> Rir {
        for item in &self.ast.items {
            self.gen_item(item);
        }
        self.rir
    }

    fn gen_item(&mut self, item: &Item) {
        match item {
            Item::Function(func) => {
                self.gen_function(func);
            }
            Item::Struct(struct_decl) => {
                self.gen_struct(struct_decl);
            }
            Item::Enum(enum_decl) => {
                self.gen_enum(enum_decl);
            }
            Item::Interface(iface) => {
                self.gen_interface(iface);
            }
            Item::Derive(derive_decl) => {
                self.gen_derive(derive_decl);
            }
            Item::DropFn(drop_fn) => {
                self.gen_drop_fn(drop_fn);
            }
            Item::Const(const_decl) => {
                self.gen_const(const_decl);
            }
            // Error nodes from parser recovery are skipped - errors were already reported
            Item::Error(_) => {}
        }
    }

    /// Convert a TypeExpr to its symbol representation.
    /// For named types, returns the existing symbol. For compound types, interns a new string.
    fn intern_type(&mut self, ty: &TypeExpr) -> Spur {
        match ty {
            TypeExpr::Named(ident) => ident.name, // Already a Spur
            TypeExpr::Unit(_) => self.interner.get_or_intern("()"),
            TypeExpr::Never(_) => self.interner.get_or_intern("!"),
            TypeExpr::Array {
                element, length, ..
            } => {
                // For arrays, we need to construct a string representation
                // Get the element symbol first, then look it up
                let elem_sym = self.intern_type(element);
                let elem_name = self.interner.resolve(&elem_sym);
                let s = format!("[{}; {}]", elem_name, length);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::AnonymousStruct { fields, .. } => {
                // For anonymous structs, generate a canonical name representation
                let mut s = String::from("struct { ");
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let name = self.interner.resolve(&field.name.name);
                    let ty_sym = self.intern_type(&field.ty);
                    let ty_name = self.interner.resolve(&ty_sym);
                    s.push_str(name);
                    s.push_str(": ");
                    s.push_str(ty_name);
                }
                s.push_str(" }");
                self.interner.get_or_intern(&s)
            }
            TypeExpr::AnonymousEnum { variants, .. } => {
                // For anonymous enums, generate a canonical name representation
                use gruel_parser::ast::EnumVariantKind;
                let mut s = String::from("enum { ");
                for (i, v) in variants.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let name = self.interner.resolve(&v.name.name);
                    s.push_str(name);
                    match &v.kind {
                        EnumVariantKind::Unit => {}
                        EnumVariantKind::Tuple(types) => {
                            s.push('(');
                            for (j, ty) in types.iter().enumerate() {
                                if j > 0 {
                                    s.push_str(", ");
                                }
                                let ty_sym = self.intern_type(ty);
                                s.push_str(self.interner.resolve(&ty_sym));
                            }
                            s.push(')');
                        }
                        EnumVariantKind::Struct(fields) => {
                            s.push_str(" { ");
                            for (j, f) in fields.iter().enumerate() {
                                if j > 0 {
                                    s.push_str(", ");
                                }
                                let fname = self.interner.resolve(&f.name.name);
                                let ty_sym = self.intern_type(&f.ty);
                                s.push_str(fname);
                                s.push_str(": ");
                                s.push_str(self.interner.resolve(&ty_sym));
                            }
                            s.push_str(" }");
                        }
                    }
                }
                s.push_str(" }");
                self.interner.get_or_intern(&s)
            }
            TypeExpr::PointerConst { pointee, .. } => {
                // ptr const T
                let pointee_sym = self.intern_type(pointee);
                let pointee_name = self.interner.resolve(&pointee_sym);
                let s = format!("ptr const {}", pointee_name);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::PointerMut { pointee, .. } => {
                // ptr mut T
                let pointee_sym = self.intern_type(pointee);
                let pointee_name = self.interner.resolve(&pointee_sym);
                let s = format!("ptr mut {}", pointee_name);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::Tuple { elems, .. } => {
                // Phase 1: just produce a canonical tuple name symbol.
                // Phase 2 will lower tuples to anon structs with numeric field names.
                let mut s = String::from("(");
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let elem_sym = self.intern_type(elem);
                    s.push_str(self.interner.resolve(&elem_sym));
                }
                if elems.len() == 1 {
                    s.push(',');
                }
                s.push(')');
                self.interner.get_or_intern(&s)
            }
            TypeExpr::AnonymousInterface { methods, .. } => {
                // ADR-0057: anonymous interfaces only appear inline in
                // comptime type expressions (`interface { ... }`). The
                // canonical name encodes method names so distinct shapes
                // get distinct symbols.
                let mut s = String::from("interface { ");
                for (i, m) in methods.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let mname = self.interner.resolve(&m.name.name);
                    s.push_str(mname);
                }
                s.push_str(" }");
                self.interner.get_or_intern(&s)
            }
            TypeExpr::TypeCall { callee, args, .. } => {
                // ADR-0057: `Name(arg, ...)` in type position. The canonical
                // name encodes the callee plus stringified args so distinct
                // parameterizations get distinct symbols.
                let mut s = String::from(self.interner.resolve(&callee.name));
                s.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let arg_sym = self.intern_type(a);
                    s.push_str(self.interner.resolve(&arg_sym));
                }
                s.push(')');
                self.interner.get_or_intern(&s)
            }
        }
    }

    fn gen_struct(&mut self, struct_decl: &StructDecl) -> InstRef {
        let directives = self.convert_directives(&struct_decl.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);
        let name = struct_decl.name.name; // Already a Spur
        let fields: Vec<_> = struct_decl
            .fields
            .iter()
            .map(|f| {
                let field_name = f.name.name; // Already a Spur
                let field_type = self.intern_type(&f.ty);
                (field_name, field_type)
            })
            .collect();
        let (fields_start, fields_len) = self.rir.add_field_decls(&fields);

        // Generate each method defined inline in the struct
        let methods: Vec<_> = struct_decl
            .methods
            .iter()
            .map(|m| self.gen_method(m))
            .collect();
        let (methods_start, methods_len) = self.rir.add_inst_refs(&methods);

        self.rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: struct_decl.visibility == Visibility::Public,
                is_linear: struct_decl.is_linear,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: struct_decl.span,
        })
    }

    fn gen_enum(&mut self, enum_decl: &EnumDecl) -> InstRef {
        use gruel_parser::ast::EnumVariantKind;

        let name = enum_decl.name.name; // Already a Spur
        let variants: Vec<(Spur, Vec<Spur>, Vec<Spur>)> = enum_decl
            .variants
            .iter()
            .map(|v| {
                let variant_name = v.name.name;
                match &v.kind {
                    EnumVariantKind::Unit => (variant_name, vec![], vec![]),
                    EnumVariantKind::Tuple(types) => {
                        let field_types: Vec<Spur> =
                            types.iter().map(|ty| self.intern_type(ty)).collect();
                        (variant_name, field_types, vec![])
                    }
                    EnumVariantKind::Struct(fields) => {
                        let field_types: Vec<Spur> =
                            fields.iter().map(|f| self.intern_type(&f.ty)).collect();
                        let field_names: Vec<Spur> = fields.iter().map(|f| f.name.name).collect();
                        (variant_name, field_types, field_names)
                    }
                }
            })
            .collect();
        let (variants_start, variants_len) = self.rir.add_enum_variant_decls(&variants);

        // Generate each method defined inline in the enum (mirrors struct handling).
        let methods: Vec<_> = enum_decl
            .methods
            .iter()
            .map(|m| self.gen_method(m))
            .collect();
        let (methods_start, methods_len) = self.rir.add_inst_refs(&methods);

        self.rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: enum_decl.visibility == Visibility::Public,
                name,
                variants_start,
                variants_len,
                methods_start,
                methods_len,
            },
            span: enum_decl.span,
        })
    }

    fn gen_interface(&mut self, iface: &gruel_parser::ast::InterfaceDecl) -> InstRef {
        use gruel_parser::ast::Visibility;

        // Emit one InterfaceMethodSig instruction per declared method, then
        // an InterfaceDecl that points to them via inst-refs.
        let method_refs: Vec<InstRef> = iface
            .methods
            .iter()
            .map(|sig| {
                let name = sig.name.name;
                let return_type = match &sig.return_type {
                    Some(ty) => self.intern_type(ty),
                    None => self.interner.get_or_intern("()"),
                };
                let params: Vec<_> = sig
                    .params
                    .iter()
                    .map(|p| RirParam {
                        name: p.name.name,
                        ty: self.intern_type(&p.ty),
                        mode: self.convert_param_mode(p.mode),
                        is_comptime: p.is_comptime,
                    })
                    .collect();
                let (params_start, params_len) = self.rir.add_params(&params);

                self.rir.add_inst(Inst {
                    data: InstData::InterfaceMethodSig {
                        name,
                        params_start,
                        params_len,
                        return_type,
                    },
                    span: sig.span,
                })
            })
            .collect();
        let (methods_start, methods_len) = self.rir.add_inst_refs(&method_refs);

        self.rir.add_inst(Inst {
            data: InstData::InterfaceDecl {
                is_pub: iface.visibility == Visibility::Public,
                name: iface.name.name,
                methods_start,
                methods_len,
            },
            span: iface.span,
        })
    }

    fn gen_derive(&mut self, derive_decl: &DeriveDecl) -> InstRef {
        // Each method body is generated like an inline struct/enum method:
        // `gen_method` emits a `FnDecl` instruction whose body uses `Self`
        // as a free type variable, to be bound at derive-expansion time.
        let method_refs: Vec<InstRef> = derive_decl
            .methods
            .iter()
            .map(|m| self.gen_method(m))
            .collect();
        let (methods_start, methods_len) = self.rir.add_inst_refs(&method_refs);

        self.rir.add_inst(Inst {
            data: InstData::DeriveDecl {
                name: derive_decl.name.name,
                methods_start,
                methods_len,
            },
            span: derive_decl.span,
        })
    }

    fn gen_const(&mut self, const_decl: &ConstDecl) -> InstRef {
        let directives = self.convert_directives(&const_decl.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);
        let name = const_decl.name.name; // Already a Spur
        let ty = const_decl.ty.as_ref().map(|t| self.intern_type(t));
        let init = self.gen_expr(&const_decl.init);

        self.rir.add_inst(Inst {
            data: InstData::ConstDecl {
                directives_start,
                directives_len,
                is_pub: const_decl.visibility == Visibility::Public,
                name,
                ty,
                init,
            },
            span: const_decl.span,
        })
    }

    fn gen_drop_fn(&mut self, drop_fn: &DropFn) -> InstRef {
        let type_name = drop_fn.type_name.name; // Already a Spur

        // Generate the body expression
        let body = self.gen_expr(&drop_fn.body);

        self.rir.add_inst(Inst {
            data: InstData::DropFnDecl { type_name, body },
            span: drop_fn.span,
        })
    }

    fn gen_method(&mut self, method: &Method) -> InstRef {
        // Convert directives
        let directives = self.convert_directives(&method.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);

        // Get the method name (already a Symbol) and return type
        let name = method.name.name; // Already a Spur
        let return_type = match &method.return_type {
            Some(ty) => self.intern_type(ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters (excluding self, which is handled specially by sema)
        let params: Vec<_> = method
            .params
            .iter()
            .map(|p| RirParam {
                name: p.name.name, // Already a Spur
                ty: self.intern_type(&p.ty),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.is_comptime,
            })
            .collect();
        let (params_start, params_len) = self.rir.add_params(&params);

        // Record the body start before generating body instructions
        let body_start = InstRef::from_raw(self.rir.current_inst_index());

        // Generate body expression
        let body = self.gen_expr(&method.body);

        // Track whether this method has a self receiver (method vs associated function)
        let has_self = method.receiver.is_some();

        // Emit methods as FnDecl instructions with has_self flag.
        // Sema uses has_self to add the implicit self parameter for methods.
        // Methods don't have their own visibility - they're accessible if the type is accessible.
        // Methods cannot be marked unchecked (that's a function-level modifier).
        let decl = self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
            },
            span: method.span,
        });

        // Record the function span for per-function analysis
        // Methods are also tracked as functions for per-function analysis
        self.rir
            .add_function_span(FunctionSpan::new(name, body_start, decl));

        decl
    }

    /// Convert AST directives to RIR directives
    fn convert_directives(&mut self, directives: &[Directive]) -> Vec<RirDirective> {
        directives
            .iter()
            .map(|d| RirDirective {
                name: d.name.name, // Already a Spur
                args: d
                    .args
                    .iter()
                    .map(|arg| match arg {
                        DirectiveArg::Ident(ident) => ident.name, // Already a Spur
                    })
                    .collect(),
                span: d.span,
            })
            .collect()
    }

    /// Convert AST ParamMode to RIR RirParamMode
    fn convert_param_mode(&self, mode: ParamMode) -> RirParamMode {
        match mode {
            ParamMode::Normal => RirParamMode::Normal,
            ParamMode::Inout => RirParamMode::Inout,
            ParamMode::Borrow => RirParamMode::Borrow,
            ParamMode::Comptime => RirParamMode::Comptime,
        }
    }

    /// Convert AST ArgMode to RIR RirArgMode
    fn convert_arg_mode(&self, mode: ArgMode) -> RirArgMode {
        match mode {
            ArgMode::Normal => RirArgMode::Normal,
            ArgMode::Inout => RirArgMode::Inout,
            ArgMode::Borrow => RirArgMode::Borrow,
        }
    }

    /// Convert a CallArg to RirCallArg
    fn convert_call_arg(&mut self, arg: &CallArg) -> RirCallArg {
        RirCallArg {
            value: self.gen_expr(&arg.expr),
            mode: self.convert_arg_mode(arg.mode),
        }
    }

    fn gen_function(&mut self, func: &Function) -> InstRef {
        // Convert directives
        let directives = self.convert_directives(&func.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);

        // Get the function name (already a Symbol) and return type
        let name = func.name.name; // Already a Spur
        let return_type = match &func.return_type {
            Some(ty) => self.intern_type(ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters
        let params: Vec<_> = func
            .params
            .iter()
            .map(|p| RirParam {
                name: p.name.name, // Already a Spur
                ty: self.intern_type(&p.ty),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.is_comptime,
            })
            .collect();
        let (params_start, params_len) = self.rir.add_params(&params);

        // Record the body start before generating body instructions
        let body_start = InstRef::from_raw(self.rir.current_inst_index());

        // Generate body expression
        let body = self.gen_expr(&func.body);

        // Create function declaration instruction
        // Regular functions don't have a self receiver
        let decl = self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: func.visibility == Visibility::Public,
                is_unchecked: func.is_unchecked,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
            },
            span: func.span,
        });

        // Record the function span for per-function analysis
        self.rir
            .add_function_span(FunctionSpan::new(name, body_start, decl));

        decl
    }

    fn gen_expr(&mut self, expr: &Expr) -> InstRef {
        match expr {
            Expr::Int(lit) => self.rir.add_inst(Inst {
                data: InstData::IntConst(lit.value),
                span: lit.span,
            }),
            Expr::Float(lit) => self.rir.add_inst(Inst {
                data: InstData::FloatConst(lit.bits),
                span: lit.span,
            }),
            Expr::Bool(lit) => self.rir.add_inst(Inst {
                data: InstData::BoolConst(lit.value),
                span: lit.span,
            }),
            Expr::String(lit) => {
                self.rir.add_inst(Inst {
                    data: InstData::StringConst(lit.value), // Already a Spur
                    span: lit.span,
                })
            }
            Expr::Unit(lit) => self.rir.add_inst(Inst {
                data: InstData::UnitConst,
                span: lit.span,
            }),
            Expr::Ident(ident) => {
                self.rir.add_inst(Inst {
                    data: InstData::VarRef { name: ident.name }, // Already a Spur
                    span: ident.span,
                })
            }
            Expr::Binary(bin) => {
                let lhs = self.gen_expr(&bin.left);
                let rhs = self.gen_expr(&bin.right);
                let data = match bin.op {
                    BinaryOp::Add => InstData::Add { lhs, rhs },
                    BinaryOp::Sub => InstData::Sub { lhs, rhs },
                    BinaryOp::Mul => InstData::Mul { lhs, rhs },
                    BinaryOp::Div => InstData::Div { lhs, rhs },
                    BinaryOp::Mod => InstData::Mod { lhs, rhs },
                    BinaryOp::Eq => InstData::Eq { lhs, rhs },
                    BinaryOp::Ne => InstData::Ne { lhs, rhs },
                    BinaryOp::Lt => InstData::Lt { lhs, rhs },
                    BinaryOp::Gt => InstData::Gt { lhs, rhs },
                    BinaryOp::Le => InstData::Le { lhs, rhs },
                    BinaryOp::Ge => InstData::Ge { lhs, rhs },
                    BinaryOp::And => InstData::And { lhs, rhs },
                    BinaryOp::Or => InstData::Or { lhs, rhs },
                    BinaryOp::BitAnd => InstData::BitAnd { lhs, rhs },
                    BinaryOp::BitOr => InstData::BitOr { lhs, rhs },
                    BinaryOp::BitXor => InstData::BitXor { lhs, rhs },
                    BinaryOp::Shl => InstData::Shl { lhs, rhs },
                    BinaryOp::Shr => InstData::Shr { lhs, rhs },
                };
                self.rir.add_inst(Inst {
                    data,
                    span: bin.span,
                })
            }
            Expr::Unary(un) => {
                let operand = self.gen_expr(&un.operand);
                let data = match un.op {
                    UnaryOp::Neg => InstData::Neg { operand },
                    UnaryOp::Not => InstData::Not { operand },
                    UnaryOp::BitNot => InstData::BitNot { operand },
                };
                self.rir.add_inst(Inst {
                    data,
                    span: un.span,
                })
            }
            Expr::Paren(paren) => {
                // Parentheses are transparent in the IR - just generate the inner expression
                self.gen_expr(&paren.inner)
            }
            Expr::Block(block) => self.gen_block(block),
            Expr::If(if_expr) => {
                let cond = self.gen_expr(&if_expr.cond);
                let then_block = self.gen_block(&if_expr.then_block);
                let else_block = if_expr.else_block.as_ref().map(|b| self.gen_block(b));

                self.rir.add_inst(Inst {
                    data: InstData::Branch {
                        cond,
                        then_block,
                        else_block,
                    },
                    span: if_expr.span,
                })
            }
            Expr::While(while_expr) => {
                let cond = self.gen_expr(&while_expr.cond);
                let body = self.gen_block(&while_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::Loop { cond, body },
                    span: while_expr.span,
                })
            }
            Expr::For(for_expr) => {
                let iterable = self.gen_expr(&for_expr.iterable);
                let body = self.gen_block(&for_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::For {
                        binding: for_expr.binding.name,
                        is_mut: for_expr.is_mut,
                        iterable,
                        body,
                    },
                    span: for_expr.span,
                })
            }
            Expr::Loop(loop_expr) => {
                let body = self.gen_block(&loop_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::InfiniteLoop { body },
                    span: loop_expr.span,
                })
            }
            Expr::Match(match_expr) => {
                // ADR-0051: single-arm irrefutable matches still rewrite to
                // a let-destructure so drop ordering and field projections
                // match `let pat = scr; body` semantics exactly.
                if let Some(elaborated) = self.try_elaborate_irrefutable_match(match_expr) {
                    return elaborated;
                }

                let scrutinee = self.gen_expr(&match_expr.scrutinee);
                let mut arms: Vec<(RirPattern, InstRef)> =
                    Vec::with_capacity(match_expr.arms.len());
                for arm in &match_expr.arms {
                    let mut nested: Vec<(Spur, Pattern)> = Vec::new();
                    let pattern = self.gen_match_arm_pattern(&arm.pattern, &mut nested);
                    let body = self.gen_expr(&arm.body);
                    debug_assert!(
                        nested.is_empty(),
                        "ADR-0051: nested-pattern capture is superseded by RirPatternBinding.sub_pattern"
                    );
                    arms.push((pattern, body));
                }
                let (arms_start, arms_len) = self.rir.add_match_arms(&arms);

                self.rir.add_inst(Inst {
                    data: InstData::Match {
                        scrutinee,
                        arms_start,
                        arms_len,
                    },
                    span: match_expr.span,
                })
            }
            Expr::Call(call) => {
                let args: Vec<_> = call.args.iter().map(|a| self.convert_call_arg(a)).collect();
                let (args_start, args_len) = self.rir.add_call_args(&args);

                self.rir.add_inst(Inst {
                    data: InstData::Call {
                        name: call.name.name, // Already a Spur
                        args_start,
                        args_len,
                    },
                    span: call.span,
                })
            }
            Expr::Break(break_expr) => self.rir.add_inst(Inst {
                data: InstData::Break,
                span: break_expr.span,
            }),
            Expr::Continue(continue_expr) => self.rir.add_inst(Inst {
                data: InstData::Continue,
                span: continue_expr.span,
            }),
            Expr::Return(return_expr) => {
                let value = return_expr.value.as_ref().map(|v| self.gen_expr(v));
                self.rir.add_inst(Inst {
                    data: InstData::Ret(value),
                    span: return_expr.span,
                })
            }
            Expr::StructLit(struct_lit) => {
                // Generate module reference if this is a qualified struct literal
                let module = struct_lit
                    .base
                    .as_ref()
                    .map(|base_expr| self.gen_expr(base_expr));

                let fields: Vec<_> = struct_lit
                    .fields
                    .iter()
                    .map(|f| {
                        let field_value = self.gen_expr(&f.value);
                        (f.name.name, field_value) // name is already a Symbol
                    })
                    .collect();
                let (fields_start, fields_len) = self.rir.add_field_inits(&fields);

                self.rir.add_inst(Inst {
                    data: InstData::StructInit {
                        module,
                        type_name: struct_lit.name.name, // Already a Spur
                        fields_start,
                        fields_len,
                    },
                    span: struct_lit.span,
                })
            }
            Expr::EnumStructLit(lit) => {
                let module = lit.base.as_ref().map(|base_expr| self.gen_expr(base_expr));

                let fields: Vec<_> = lit
                    .fields
                    .iter()
                    .map(|f| {
                        let field_value = self.gen_expr(&f.value);
                        (f.name.name, field_value)
                    })
                    .collect();
                let (fields_start, fields_len) = self.rir.add_field_inits(&fields);

                self.rir.add_inst(Inst {
                    data: InstData::EnumStructVariant {
                        module,
                        type_name: lit.type_name.name,
                        variant: lit.variant.name,
                        fields_start,
                        fields_len,
                    },
                    span: lit.span,
                })
            }
            Expr::Field(field_expr) => {
                let base = self.gen_expr(&field_expr.base);

                self.rir.add_inst(Inst {
                    data: InstData::FieldGet {
                        base,
                        field: field_expr.field.name, // Already a Spur
                    },
                    span: field_expr.span,
                })
            }
            Expr::IntrinsicCall(intrinsic) => {
                let name = intrinsic.name.name; // Already a Spur
                let intrinsic_name_str = self.interner.resolve(&name);

                let is_type = is_type_intrinsic(intrinsic_name_str);

                if is_type && intrinsic.args.len() == 1 {
                    // Handle explicit type argument
                    if let IntrinsicArg::Type(ty) = &intrinsic.args[0] {
                        let type_arg = self.intern_type(ty);
                        return self.rir.add_inst(Inst {
                            data: InstData::TypeIntrinsic { name, type_arg },
                            span: intrinsic.span,
                        });
                    }

                    // Handle identifier expression that should be interpreted as a type
                    // (e.g., @size_of(Point) where Point is parsed as Ident expression)
                    if let IntrinsicArg::Expr(Expr::Ident(ident)) = &intrinsic.args[0] {
                        return self.rir.add_inst(Inst {
                            data: InstData::TypeIntrinsic {
                                name,
                                type_arg: ident.name, // Already a Spur
                            },
                            span: intrinsic.span,
                        });
                    }
                }

                // Otherwise, treat as an expression intrinsic
                let args: Vec<_> = intrinsic
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        IntrinsicArg::Expr(expr) => Some(self.gen_expr(expr)),
                        IntrinsicArg::Type(_) => None, // This shouldn't happen for expr intrinsics
                    })
                    .collect();
                let (args_start, args_len) = self.rir.add_inst_refs(&args);

                self.rir.add_inst(Inst {
                    data: InstData::Intrinsic {
                        name,
                        args_start,
                        args_len,
                    },
                    span: intrinsic.span,
                })
            }
            Expr::ArrayLit(array_lit) => {
                let elements: Vec<_> = array_lit
                    .elements
                    .iter()
                    .map(|e| self.gen_expr(e))
                    .collect();
                let (elems_start, elems_len) = self.rir.add_inst_refs(&elements);

                self.rir.add_inst(Inst {
                    data: InstData::ArrayInit {
                        elems_start,
                        elems_len,
                    },
                    span: array_lit.span,
                })
            }
            Expr::Index(index_expr) => {
                let base = self.gen_expr(&index_expr.base);
                let index = self.gen_expr(&index_expr.index);

                self.rir.add_inst(Inst {
                    data: InstData::IndexGet { base, index },
                    span: index_expr.span,
                })
            }
            Expr::Path(path_expr) => {
                // Generate module reference if this is a qualified path
                let module = path_expr
                    .base
                    .as_ref()
                    .map(|base_expr| self.gen_expr(base_expr));

                self.rir.add_inst(Inst {
                    data: InstData::EnumVariant {
                        module,
                        type_name: path_expr.type_name.name, // Already a Spur
                        variant: path_expr.variant.name,     // Already a Spur
                    },
                    span: path_expr.span,
                })
            }
            Expr::MethodCall(method_call) => {
                let receiver = self.gen_expr(&method_call.receiver);
                let args: Vec<_> = method_call
                    .args
                    .iter()
                    .map(|a| self.convert_call_arg(a))
                    .collect();
                let (args_start, args_len) = self.rir.add_call_args(&args);

                self.rir.add_inst(Inst {
                    data: InstData::MethodCall {
                        receiver,
                        method: method_call.method.name, // Already a Spur
                        args_start,
                        args_len,
                    },
                    span: method_call.span,
                })
            }
            Expr::AssocFnCall(assoc_fn_call) => {
                let args: Vec<_> = assoc_fn_call
                    .args
                    .iter()
                    .map(|a| self.convert_call_arg(a))
                    .collect();
                let (args_start, args_len) = self.rir.add_call_args(&args);

                self.rir.add_inst(Inst {
                    data: InstData::AssocFnCall {
                        type_name: assoc_fn_call.type_name.name, // Already a Spur
                        function: assoc_fn_call.function.name,   // Already a Spur
                        args_start,
                        args_len,
                    },
                    span: assoc_fn_call.span,
                })
            }
            Expr::SelfExpr(self_expr) => {
                // `self` in method bodies is just a variable reference to the implicit self parameter
                let name = self.interner.get_or_intern("self");
                self.rir.add_inst(Inst {
                    data: InstData::VarRef { name },
                    span: self_expr.span,
                })
            }
            Expr::Comptime(comptime_block) => {
                // Generate the inner expression, wrapped in a Comptime instruction
                // The semantic analyzer will evaluate this at compile time
                let inner_expr = self.gen_expr(&comptime_block.expr);
                self.rir.add_inst(Inst {
                    data: InstData::Comptime { expr: inner_expr },
                    span: comptime_block.span,
                })
            }
            Expr::ComptimeUnrollFor(unroll) => {
                let iterable = self.gen_expr(&unroll.iterable);
                let body = self.gen_block(&unroll.body);
                self.rir.add_inst(Inst {
                    data: InstData::ComptimeUnrollFor {
                        binding: unroll.binding.name,
                        iterable,
                        body,
                    },
                    span: unroll.span,
                })
            }
            Expr::Checked(checked_block) => {
                // Generate the inner expression, wrapped in a Checked instruction
                // Unchecked operations are only allowed inside checked blocks
                let inner_expr = self.gen_expr(&checked_block.expr);
                self.rir.add_inst(Inst {
                    data: InstData::Checked { expr: inner_expr },
                    span: checked_block.span,
                })
            }
            Expr::TypeLit(type_lit) => {
                // Generate a type constant instruction for type-as-value expressions
                match &type_lit.type_expr {
                    TypeExpr::AnonymousStruct {
                        directives,
                        fields,
                        methods,
                        ..
                    } => {
                        // Generate an anonymous struct type instruction with methods
                        let rir_directives = self.convert_directives(directives);
                        let (directives_start, directives_len) =
                            self.rir.add_directives(&rir_directives);
                        let field_decls: Vec<(Spur, Spur)> = fields
                            .iter()
                            .map(|f| {
                                let name = f.name.name;
                                let ty = self.intern_type(&f.ty);
                                (name, ty)
                            })
                            .collect();
                        let (fields_start, fields_len) = self.rir.add_field_decls(&field_decls);

                        // Generate each method inside the anonymous struct
                        // (reusing gen_method, which generates FnDecl instructions)
                        let method_refs: Vec<InstRef> =
                            methods.iter().map(|m| self.gen_method(m)).collect();
                        let (methods_start, methods_len) = self.rir.add_inst_refs(&method_refs);

                        self.rir.add_inst(Inst {
                            data: InstData::AnonStructType {
                                directives_start,
                                directives_len,
                                fields_start,
                                fields_len,
                                methods_start,
                                methods_len,
                            },
                            span: type_lit.span,
                        })
                    }
                    TypeExpr::AnonymousInterface { methods, .. } => {
                        // ADR-0057: build an `AnonInterfaceType` instruction
                        // carrying one `InterfaceMethodSig` per declared
                        // method. Mirrors the gen_interface flow for named
                        // interfaces but without the enclosing
                        // `InterfaceDecl`.
                        let method_refs: Vec<InstRef> = methods
                            .iter()
                            .map(|sig| {
                                let name = sig.name.name;
                                let return_type = match &sig.return_type {
                                    Some(ty) => self.intern_type(ty),
                                    None => self.interner.get_or_intern("()"),
                                };
                                let params: Vec<_> = sig
                                    .params
                                    .iter()
                                    .map(|p| RirParam {
                                        name: p.name.name,
                                        ty: self.intern_type(&p.ty),
                                        mode: self.convert_param_mode(p.mode),
                                        is_comptime: p.is_comptime,
                                    })
                                    .collect();
                                let (params_start, params_len) = self.rir.add_params(&params);
                                self.rir.add_inst(Inst {
                                    data: InstData::InterfaceMethodSig {
                                        name,
                                        params_start,
                                        params_len,
                                        return_type,
                                    },
                                    span: sig.span,
                                })
                            })
                            .collect();
                        let (methods_start, methods_len) = self.rir.add_inst_refs(&method_refs);
                        self.rir.add_inst(Inst {
                            data: InstData::AnonInterfaceType {
                                methods_start,
                                methods_len,
                            },
                            span: type_lit.span,
                        })
                    }
                    TypeExpr::AnonymousEnum {
                        directives,
                        variants,
                        methods,
                        ..
                    } => {
                        // Generate an anonymous enum type instruction with methods
                        use gruel_parser::ast::EnumVariantKind;
                        let rir_directives = self.convert_directives(directives);
                        let (directives_start, directives_len) =
                            self.rir.add_directives(&rir_directives);
                        let variant_decls: Vec<(Spur, Vec<Spur>, Vec<Spur>)> = variants
                            .iter()
                            .map(|v| {
                                let variant_name = v.name.name;
                                match &v.kind {
                                    EnumVariantKind::Unit => (variant_name, vec![], vec![]),
                                    EnumVariantKind::Tuple(types) => {
                                        let field_types: Vec<Spur> =
                                            types.iter().map(|ty| self.intern_type(ty)).collect();
                                        (variant_name, field_types, vec![])
                                    }
                                    EnumVariantKind::Struct(fields) => {
                                        let field_types: Vec<Spur> = fields
                                            .iter()
                                            .map(|f| self.intern_type(&f.ty))
                                            .collect();
                                        let field_names: Vec<Spur> =
                                            fields.iter().map(|f| f.name.name).collect();
                                        (variant_name, field_types, field_names)
                                    }
                                }
                            })
                            .collect();
                        let (variants_start, variants_len) =
                            self.rir.add_enum_variant_decls(&variant_decls);

                        // Generate each method inside the anonymous enum
                        let method_refs: Vec<InstRef> =
                            methods.iter().map(|m| self.gen_method(m)).collect();
                        let (methods_start, methods_len) = self.rir.add_inst_refs(&method_refs);

                        self.rir.add_inst(Inst {
                            data: InstData::AnonEnumType {
                                directives_start,
                                directives_len,
                                variants_start,
                                variants_len,
                                methods_start,
                                methods_len,
                            },
                            span: type_lit.span,
                        })
                    }
                    _ => {
                        // For named types, unit, never, arrays, and pointers, generate TypeConst
                        let type_name = match &type_lit.type_expr {
                            TypeExpr::Named(ident) => ident.name,
                            TypeExpr::Unit(_) => self.interner.get_or_intern_static("()"),
                            TypeExpr::Never(_) => self.interner.get_or_intern_static("!"),
                            TypeExpr::Array { .. } => {
                                // Array types as values are not yet supported
                                // For now, use a placeholder
                                self.interner.get_or_intern_static("array")
                            }
                            TypeExpr::AnonymousStruct { .. }
                            | TypeExpr::AnonymousEnum { .. }
                            | TypeExpr::AnonymousInterface { .. } => {
                                unreachable!("handled above")
                            }
                            TypeExpr::PointerConst { .. } | TypeExpr::PointerMut { .. } => {
                                // Pointer types as values - use intern_type to get representation
                                self.intern_type(&type_lit.type_expr)
                            }
                            TypeExpr::TypeCall { .. } => {
                                // ADR-0057: parameterized type call as a
                                // type literal. Route through intern_type;
                                // sema's `resolve_type` evaluates the call
                                // at comptime when the symbol is consumed.
                                self.intern_type(&type_lit.type_expr)
                            }
                            TypeExpr::Tuple { .. } => {
                                // Phase 1: route through intern_type. Phase 2 will lower
                                // tuples to anon structs and handle type-value use properly.
                                self.intern_type(&type_lit.type_expr)
                            }
                        };
                        self.rir.add_inst(Inst {
                            data: InstData::TypeConst { type_name },
                            span: type_lit.span,
                        })
                    }
                }
            }
            // Error nodes from parser recovery - generate a unit constant as a placeholder
            // The error was already reported during parsing
            Expr::Error(span) => self.rir.add_inst(Inst {
                data: InstData::UnitConst,
                span: *span,
            }),
            // Tuple literal `(e0, e1, ...)` — lowered in sema to an anon struct
            // with field names "0", "1", ... (ADR-0048).
            Expr::Tuple(tuple) => {
                let elem_refs: Vec<InstRef> =
                    tuple.elems.iter().map(|e| self.gen_expr(e)).collect();
                let elem_u32s: Vec<u32> = elem_refs.iter().map(|r| r.as_u32()).collect();
                let elems_start = self.rir.add_extra(&elem_u32s);
                let elems_len = elem_refs.len() as u32;
                self.rir.add_inst(Inst {
                    data: InstData::TupleInit {
                        elems_start,
                        elems_len,
                    },
                    span: tuple.span,
                })
            }
            // Tuple index `t.N` — lowered to a regular FieldGet whose field symbol
            // is the stringified index. Synthetic names cannot collide with user
            // struct field names (which must start with a letter).
            Expr::TupleIndex(ti) => {
                let base = self.gen_expr(&ti.base);
                let field = self.interner.get_or_intern(ti.index.to_string());
                self.rir.add_inst(Inst {
                    data: InstData::FieldGet { base, field },
                    span: ti.span,
                })
            }
            // Anonymous function expression (ADR-0055): desugar to a lambda-
            // origin anonymous struct with one `__call` method, and produce an
            // empty instance of it.
            //
            // Strategy: synthesize a `Method { name: "__call", receiver: self,
            // params, return_type, body }`, run it through the normal
            // `gen_method` to get a FnDecl InstRef, then emit `AnonFnValue`
            // referencing that method. Sema creates the actual struct type
            // and instantiates it; Phase 3 makes each site unique.
            Expr::AnonFn(anon_fn) => {
                let call_name_sym = self.interner.get_or_intern_static("__call");
                let call_ident = Ident {
                    name: call_name_sym,
                    span: anon_fn.span,
                };
                let synth_method = Method {
                    directives: Directives::new(),
                    name: call_ident,
                    receiver: Some(SelfParam { span: anon_fn.span }),
                    params: anon_fn.params.clone(),
                    return_type: anon_fn.return_type.clone(),
                    body: Expr::Block(BlockExpr {
                        statements: anon_fn.body.statements.clone(),
                        expr: anon_fn.body.expr.clone(),
                        span: anon_fn.body.span,
                    }),
                    span: anon_fn.span,
                };
                let method_ref = self.gen_method(&synth_method);
                self.rir.add_inst(Inst {
                    data: InstData::AnonFnValue { method: method_ref },
                    span: anon_fn.span,
                })
            }
        }
    }

    /// Elaborate a match expression whose single arm has an irrefutable
    /// top-level pattern into a block containing a let-destructure over the
    /// scrutinee plus the arm body (ADR-0049 Phase 4b).
    ///
    /// Covers `match x { name => body }`, `match p { Point { x, y } => body }`,
    /// and `match t { (a, b) => body }` — single-arm matches where the pattern
    /// binds the scrutinee without branching. For multi-arm or refutable
    /// top-level patterns, returns `None` and the caller falls back to the
    /// normal match lowering.
    fn try_elaborate_irrefutable_match(
        &mut self,
        match_expr: &gruel_parser::MatchExpr,
    ) -> Option<InstRef> {
        if match_expr.arms.len() != 1 {
            return None;
        }
        let arm = &match_expr.arms[0];
        match &arm.pattern {
            Pattern::Ident { is_mut, name, span } => {
                // `match x { name => body }` => `{ let name = x; body }`
                let init = self.gen_expr(&match_expr.scrutinee);
                let alloc = self.rir.add_inst(Inst {
                    data: InstData::Alloc {
                        directives_start: 0,
                        directives_len: 0,
                        name: Some(name.name),
                        is_mut: *is_mut,
                        ty: None,
                        init,
                    },
                    span: *span,
                });
                let body = self.gen_expr(&arm.body);
                let extra_start = self.rir.add_extra(&[alloc.as_u32(), body.as_u32()]);
                Some(self.rir.add_inst(Inst {
                    data: InstData::Block {
                        extra_start,
                        len: 2,
                    },
                    span: match_expr.span,
                }))
            }
            Pattern::Struct { .. } | Pattern::Tuple { .. }
                if is_irrefutable_destructure(&arm.pattern) =>
            {
                // `match x { <pattern> => body }` => `{ let <pattern> = x; body }`
                let init = self.gen_expr(&match_expr.scrutinee);
                let mut stmts: Vec<u32> = Vec::new();
                let mut emitted = Vec::new();
                self.emit_let_destructure_into(
                    &arm.pattern,
                    init,
                    arm.pattern.span(),
                    &mut emitted,
                );
                for r in emitted {
                    stmts.push(r.as_u32());
                }
                let body = self.gen_expr(&arm.body);
                stmts.push(body.as_u32());
                let extra_start = self.rir.add_extra(&stmts);
                let len = stmts.len() as u32;
                Some(self.rir.add_inst(Inst {
                    data: InstData::Block { extra_start, len },
                    span: match_expr.span,
                }))
            }
            _ => None,
        }
    }

    /// Lower a match-arm pattern to RIR, collecting nested sub-patterns for
    /// body elaboration (ADR-0049 Phase 4b).
    ///
    /// When a variant field carries an irrefutable nested destructure
    /// (`Some((a, b))`, `Some(Point { x, y })`), the sub-pattern is replaced
    /// with a fresh synthetic binding in the RIR pattern, and the
    /// `(fresh_name, sub_pattern)` pair is appended to `nested`. The caller
    /// prepends a `let <sub_pattern> = <fresh_name>;` to the arm body so the
    /// user's bindings come into scope.
    fn gen_match_arm_pattern(
        &mut self,
        pattern: &Pattern,
        nested: &mut Vec<(Spur, Pattern)>,
    ) -> RirPattern {
        match pattern {
            Pattern::Wildcard(span) => RirPattern::Wildcard(*span),
            Pattern::Int(lit) => RirPattern::Int(lit.value as i64, lit.span),
            // Use wrapping_neg to handle i64::MIN correctly (where value is 9223372036854775808)
            Pattern::NegInt(lit) => RirPattern::Int((lit.value as i64).wrapping_neg(), lit.span),
            Pattern::Bool(lit) => RirPattern::Bool(lit.value, lit.span),
            Pattern::Path(path) => {
                // If there's a base expression (module reference), generate it first
                let module = path.base.as_ref().map(|base| self.gen_expr(base));
                RirPattern::Path {
                    module,
                    type_name: path.type_name.name, // Already a Spur
                    variant: path.variant.name,     // Already a Spur
                    span: path.span,
                }
            }
            Pattern::DataVariant {
                base,
                type_name,
                variant,
                fields,
                span,
            } => {
                let module = base.as_ref().map(|b| self.gen_expr(b));
                let rir_bindings = fields
                    .iter()
                    .map(|elem| self.tuple_elem_to_rir_binding_or_capture(elem, nested))
                    .collect();
                RirPattern::DataVariant {
                    module,
                    type_name: type_name.name,
                    variant: variant.name,
                    bindings: rir_bindings,
                    span: *span,
                }
            }
            Pattern::StructVariant {
                base,
                type_name,
                variant,
                fields,
                span,
            } => {
                let module = base.as_ref().map(|b| self.gen_expr(b));
                let rest_marker = self.interner.get_or_intern_static("..");
                let field_bindings = fields
                    .iter()
                    .map(|fb| {
                        // For `..` rest patterns, synthesize a field_name of
                        // the rest-marker sentinel so sema can detect it
                        // (ADR-0049 Phase 6).
                        let field_name = fb
                            .field_name
                            .as_ref()
                            .map(|ident| ident.name)
                            .unwrap_or(rest_marker);
                        RirStructPatternBinding {
                            field_name,
                            binding: self.field_pattern_to_rir_binding_or_capture(fb, nested),
                        }
                    })
                    .collect();
                RirPattern::StructVariant {
                    module,
                    type_name: type_name.name,
                    variant: variant.name,
                    field_bindings,
                    span: *span,
                }
            }
            // Top-level Struct / Tuple / Ident match arms — sema + CFG
            // consume them directly as `RirPattern::Tuple` / `Struct` /
            // `Ident`, recursively lowering sub-patterns along the way.
            Pattern::Ident { name, is_mut, span } => {
                let _ = nested;
                RirPattern::Ident {
                    name: name.name,
                    is_mut: *is_mut,
                    span: *span,
                }
            }
            Pattern::Tuple { elems, span } => {
                // Record the source index of any `..` rest marker so sema
                // can expand it to wildcards filling the scrutinee's arity
                // (ADR-0049 Phase 6 semantics preserved). The marker is
                // stripped from the RIR elems list and reconstructed in
                // sema.
                let mut rir_elems: Vec<RirPattern> = Vec::with_capacity(elems.len());
                let mut rest_position: Option<u32> = None;
                for (i, e) in elems.iter().enumerate() {
                    match e {
                        TupleElemPattern::Pattern(p) => {
                            rir_elems.push(self.gen_match_arm_pattern(p, nested));
                        }
                        TupleElemPattern::Rest(_) => {
                            rest_position = Some(i as u32);
                        }
                    }
                }
                RirPattern::Tuple {
                    elems: rir_elems,
                    rest_position,
                    span: *span,
                }
            }
            Pattern::Struct {
                type_name,
                fields,
                span,
            } => {
                // AST represents `..` as a `FieldPattern` with
                // `field_name = None`; capture that as a top-level
                // `has_rest` boolean and drop those fields from the
                // recursive-RIR field list.
                let has_rest = fields.iter().any(|f| f.field_name.is_none());
                let rir_fields: Vec<RirStructField> = fields
                    .iter()
                    .filter_map(|f| {
                        let field_name = f.field_name.as_ref()?.name;
                        let pat = match &f.sub {
                            Some(p) => self.gen_match_arm_pattern(p, nested),
                            None => {
                                // Shorthand `{ x }` or `{ mut x }`: bind the
                                // field to a local with the same name.
                                RirPattern::Ident {
                                    name: field_name,
                                    is_mut: f.is_mut,
                                    span: f.span,
                                }
                            }
                        };
                        Some(RirStructField {
                            field_name,
                            pattern: pat,
                        })
                    })
                    .collect();
                RirPattern::Struct {
                    module: None,
                    type_name: type_name.name,
                    fields: rir_fields,
                    has_rest,
                    span: *span,
                }
            }
        }
    }

    /// Convert a match-arm tuple-element sub-pattern into the RIR binding
    /// shape used by `DataVariant`. ADR-0051: any refutable/irrefutable
    /// nested sub-pattern is stored directly on the binding's `sub_pattern`
    /// slot; only bare names and wildcards produce flat bindings.
    fn tuple_elem_to_rir_binding_or_capture(
        &mut self,
        elem: &TupleElemPattern,
        nested: &mut Vec<(Spur, Pattern)>,
    ) -> RirPatternBinding {
        match elem {
            TupleElemPattern::Pattern(Pattern::Wildcard(_)) => RirPatternBinding {
                is_wildcard: true,
                is_mut: false,
                name: None,
                sub_pattern: None,
            },
            TupleElemPattern::Pattern(Pattern::Ident { is_mut, name, .. }) => RirPatternBinding {
                is_wildcard: false,
                is_mut: *is_mut,
                name: Some(name.name),
                sub_pattern: None,
            },
            TupleElemPattern::Pattern(sub) => {
                // Any other sub-pattern (refutable or otherwise) rides on
                // the binding's nested sub_pattern slot. `gen_match_arm_pattern`
                // recursively turns AST into RIR.
                let sub_rir = self.gen_match_arm_pattern(sub, nested);
                RirPatternBinding {
                    is_wildcard: false,
                    is_mut: false,
                    name: None,
                    sub_pattern: Some(Box::new(sub_rir)),
                }
            }
            TupleElemPattern::Rest(_) => {
                // `..` in a data-variant pattern: emit a marker binding
                // with the sentinel name `..`. Sema detects this marker
                // and expands it to wildcards for the remaining variant
                // fields (ADR-0049 Phase 6).
                RirPatternBinding {
                    is_wildcard: true,
                    is_mut: false,
                    name: Some(self.interner.get_or_intern_static("..")),
                    sub_pattern: None,
                }
            }
        }
    }

    /// Convert a match-arm struct-variant field-pattern into the RIR binding
    /// shape. Same rules as `tuple_elem_to_rir_binding_or_capture`.
    fn field_pattern_to_rir_binding_or_capture(
        &mut self,
        fb: &FieldPattern,
        nested: &mut Vec<(Spur, Pattern)>,
    ) -> RirPatternBinding {
        // `..` in a struct-variant pattern: emit a marker binding with the
        // sentinel name `..`. Sema recognizes it as a rest and skips the
        // missing-fields check (ADR-0049 Phase 6).
        let Some(name) = fb.field_name.as_ref() else {
            return RirPatternBinding {
                is_wildcard: true,
                is_mut: false,
                name: Some(self.interner.get_or_intern_static("..")),
                sub_pattern: None,
            };
        };
        match &fb.sub {
            None => RirPatternBinding {
                // Shorthand: `field` binds a local named `field`.
                is_wildcard: false,
                is_mut: fb.is_mut,
                name: Some(name.name),
                sub_pattern: None,
            },
            Some(Pattern::Wildcard(_)) => RirPatternBinding {
                is_wildcard: true,
                is_mut: false,
                name: None,
                sub_pattern: None,
            },
            Some(Pattern::Ident {
                is_mut,
                name: bind_name,
                ..
            }) => RirPatternBinding {
                is_wildcard: false,
                is_mut: fb.is_mut || *is_mut,
                name: Some(bind_name.name),
                sub_pattern: None,
            },
            Some(sub) => {
                let sub_rir = self.gen_match_arm_pattern(sub, nested);
                RirPatternBinding {
                    is_wildcard: false,
                    is_mut: false,
                    name: None,
                    sub_pattern: Some(Box::new(sub_rir)),
                }
            }
        }
    }

    fn gen_block(&mut self, block: &gruel_parser::BlockExpr) -> InstRef {
        if block.statements.is_empty() {
            // No statements, just the final expression
            self.gen_expr(&block.expr)
        } else {
            // Collect all instruction refs for the block
            // statements + 1 for the final expression
            let mut inst_refs = Vec::with_capacity(block.statements.len() + 1);

            // Generate all statements first. A single statement can produce
            // multiple RIR top-level instructions (e.g., a nested let
            // destructure elaborated into a tree of flat StructDestructures —
            // ADR-0049 Phase 4). Each produced InstRef becomes a block
            // statement; sema's block scope sees them in sequence so
            // intermediate bindings remain visible.
            for stmt in &block.statements {
                self.gen_statement_into(stmt, &mut inst_refs);
            }

            // Generate the final expression
            let final_expr = self.gen_expr(&block.expr);
            inst_refs.push(final_expr.as_u32());

            // Store the refs in extra data
            let extra_start = self.rir.add_extra(&inst_refs);
            let len = inst_refs.len() as u32;

            self.rir.add_inst(Inst {
                data: InstData::Block { extra_start, len },
                span: block.span,
            })
        }
    }

    /// Lower a nested let-destructure pattern into a tree of flat
    /// `StructDestructure` instructions plus intermediate synthetic bindings.
    ///
    /// For each field whose sub-pattern is itself a destructure, we emit a
    /// fresh `__nested_pat_N` binding for the outer level and follow with a
    /// child `emit_let_destructure` that consumes it via `VarRef`. Ownership
    /// transfers through the tree: the outer init is consumed by the outer
    /// destructure, each synthetic intermediate is consumed by the child
    /// destructure.
    ///
    /// Emits all instructions into `out`; returns nothing. The caller is
    /// responsible for wrapping a multi-instruction result in a Block if this
    /// let produces more than one RIR instruction.
    fn emit_let_destructure_into(
        &mut self,
        pattern: &Pattern,
        init: InstRef,
        span: gruel_span::Span,
        out: &mut Vec<InstRef>,
    ) {
        // Build the flat destructure fields plus a side-list of child sub-patterns
        // that need their own destructure pass.
        let (type_name, rir_fields, child_destructures) = match pattern {
            Pattern::Struct {
                type_name, fields, ..
            } => {
                let mut rir_fields = Vec::with_capacity(fields.len());
                let mut children: Vec<(Spur, &Pattern)> = Vec::new();
                for fp in fields {
                    // `..` rest pattern in a struct destructure: emit a
                    // marker field whose field_name is the sentinel `..` so
                    // sema recognizes this and relaxes the "all fields
                    // required" rule (ADR-0049 Phase 6).
                    let Some(field_name) = fp.field_name.as_ref() else {
                        rir_fields.push(RirDestructureField {
                            field_name: self.interner.get_or_intern_static(".."),
                            binding_name: None,
                            is_wildcard: true,
                            is_mut: false,
                        });
                        continue;
                    };
                    match &fp.sub {
                        None => rir_fields.push(RirDestructureField {
                            field_name: field_name.name,
                            binding_name: Some(field_name.name),
                            is_wildcard: false,
                            is_mut: fp.is_mut,
                        }),
                        Some(Pattern::Wildcard(_)) => rir_fields.push(RirDestructureField {
                            field_name: field_name.name,
                            binding_name: None,
                            is_wildcard: true,
                            is_mut: false,
                        }),
                        Some(Pattern::Ident {
                            is_mut,
                            name: bind_name,
                            ..
                        }) => rir_fields.push(RirDestructureField {
                            field_name: field_name.name,
                            binding_name: Some(bind_name.name),
                            is_wildcard: false,
                            is_mut: fp.is_mut || *is_mut,
                        }),
                        Some(sub @ (Pattern::Struct { .. } | Pattern::Tuple { .. })) => {
                            let fresh = self.fresh_nested_pat_name();
                            rir_fields.push(RirDestructureField {
                                field_name: field_name.name,
                                binding_name: Some(fresh),
                                is_wildcard: false,
                                is_mut: false,
                            });
                            children.push((fresh, sub));
                        }
                        Some(other) => panic!(
                            "unexpected sub-pattern in let struct destructure: {:?}",
                            other
                        ),
                    }
                }
                (type_name.name, rir_fields, children)
            }
            Pattern::Tuple { elems, .. } => {
                // Tuple destructuring with `..` at any position: elements
                // before the `..` get their literal positional index
                // (`"0"`, `"1"`, …); elements after the `..` get an
                // `..end_N` marker where N is the 0-indexed distance from
                // the end (so `(a, .., b, c)` emits `..end_1` for `b` and
                // `..end_0` for `c`). Sema resolves the end-marker against
                // the inferred tuple arity (ADR-0049 Phase 6).
                let rest_pos = elems
                    .iter()
                    .position(|e| matches!(e, TupleElemPattern::Rest(_)));
                let mut rir_fields = Vec::with_capacity(elems.len());
                let mut children: Vec<(Spur, &Pattern)> = Vec::new();
                for (i, elem) in elems.iter().enumerate() {
                    if let TupleElemPattern::Rest(_) = elem {
                        rir_fields.push(RirDestructureField {
                            field_name: self.interner.get_or_intern_static(".."),
                            binding_name: None,
                            is_wildcard: true,
                            is_mut: false,
                        });
                        continue;
                    }
                    let field_name = match rest_pos {
                        Some(rp) if i > rp => {
                            // Suffix position: encode as distance from end.
                            let from_end = elems.len() - 1 - i;
                            self.interner.get_or_intern(format!("..end_{}", from_end))
                        }
                        _ => self.interner.get_or_intern(i.to_string()),
                    };
                    match elem {
                        TupleElemPattern::Pattern(Pattern::Wildcard(_)) => {
                            rir_fields.push(RirDestructureField {
                                field_name,
                                binding_name: None,
                                is_wildcard: true,
                                is_mut: false,
                            });
                        }
                        TupleElemPattern::Pattern(Pattern::Ident { is_mut, name, .. }) => {
                            rir_fields.push(RirDestructureField {
                                field_name,
                                binding_name: Some(name.name),
                                is_wildcard: false,
                                is_mut: *is_mut,
                            });
                        }
                        TupleElemPattern::Pattern(
                            sub @ (Pattern::Struct { .. } | Pattern::Tuple { .. }),
                        ) => {
                            let fresh = self.fresh_nested_pat_name();
                            rir_fields.push(RirDestructureField {
                                field_name,
                                binding_name: Some(fresh),
                                is_wildcard: false,
                                is_mut: false,
                            });
                            children.push((fresh, sub));
                        }
                        TupleElemPattern::Pattern(other) => panic!(
                            "unexpected sub-pattern in let tuple destructure: {:?}",
                            other
                        ),
                        TupleElemPattern::Rest(_) => unreachable!("handled above"),
                    }
                }
                let tuple_type_name = self.interner.get_or_intern_static("__tuple__");
                (tuple_type_name, rir_fields, children)
            }
            other => panic!(
                "emit_let_destructure called on non-destructure pattern: {:?}",
                other
            ),
        };

        let (fields_start, fields_len) = self.rir.add_destructure_fields(&rir_fields);
        let outer_inst = self.rir.add_inst(Inst {
            data: InstData::StructDestructure {
                type_name,
                fields_start,
                fields_len,
                init,
            },
            span,
        });
        out.push(outer_inst);

        // Emit child destructures recursively.
        for (binding_name, sub) in child_destructures {
            let var_ref = self.rir.add_inst(Inst {
                data: InstData::VarRef { name: binding_name },
                span: sub.span(),
            });
            self.emit_let_destructure_into(sub, var_ref, sub.span(), out);
        }
    }

    /// Generate RIR for a statement, appending produced top-level instruction
    /// refs to `out`. A single AST statement can produce multiple RIR
    /// instructions when lowering nested destructures (ADR-0049 Phase 4).
    fn gen_statement_into(&mut self, stmt: &Statement, out: &mut Vec<u32>) {
        if let Statement::Let(let_stmt) = stmt
            && matches!(
                &let_stmt.pattern,
                Pattern::Struct { .. } | Pattern::Tuple { .. }
            )
        {
            let init = self.gen_expr(&let_stmt.init);
            let mut emitted = Vec::new();
            self.emit_let_destructure_into(&let_stmt.pattern, init, let_stmt.span, &mut emitted);
            for r in emitted {
                out.push(r.as_u32());
            }
            return;
        }
        let single = self.gen_statement(stmt);
        out.push(single.as_u32());
    }

    fn gen_statement(&mut self, stmt: &Statement) -> InstRef {
        match stmt {
            Statement::Let(let_stmt) => match &let_stmt.pattern {
                // Struct / Tuple destructure patterns. Both lower to
                // `InstData::StructDestructure`; tuple patterns use the
                // `__tuple__` sentinel type name (ADR-0048).
                //
                // Nested sub-patterns (ADR-0049) are elaborated in astgen: each
                // nested sub-pattern position gets a synthetic intermediate
                // binding, and a child StructDestructure is emitted against it.
                // This keeps RIR / sema / CFG flat and reuses existing
                // infrastructure unchanged.
                Pattern::Struct { .. } | Pattern::Tuple { .. } => {
                    // Destructure lets are special: they can produce multiple
                    // top-level RIR instructions for nested patterns. Callers
                    // that can fan out (gen_block via gen_statement_into)
                    // handle this. Callers that can't fan out (nothing
                    // currently) would see only the outer instruction.
                    let init = self.gen_expr(&let_stmt.init);
                    let mut emitted = Vec::new();
                    self.emit_let_destructure_into(
                        &let_stmt.pattern,
                        init,
                        let_stmt.span,
                        &mut emitted,
                    );
                    // Safe: emit_let_destructure_into always pushes at least one.
                    *emitted
                        .first()
                        .expect("destructure must emit at least one instruction")
                }
                pattern => {
                    let directives = self.convert_directives(&let_stmt.directives);
                    let (directives_start, directives_len) = self.rir.add_directives(&directives);
                    let is_mut = match pattern {
                        Pattern::Ident { is_mut, .. } => *is_mut || let_stmt.is_mut,
                        _ => let_stmt.is_mut,
                    };
                    let name = match pattern {
                        Pattern::Ident { name, .. } => Some(name.name),
                        Pattern::Wildcard(_) => None,
                        _ => unreachable!(
                            "Phase 1 let-statement parser only emits Ident/Wildcard/Struct/Tuple; got {:?}",
                            pattern
                        ),
                    };
                    let ty = let_stmt.ty.as_ref().map(|t| self.intern_type(t));
                    let init = self.gen_expr(&let_stmt.init);
                    self.rir.add_inst(Inst {
                        data: InstData::Alloc {
                            directives_start,
                            directives_len,
                            name,
                            is_mut,
                            ty,
                            init,
                        },
                        span: let_stmt.span,
                    })
                }
            },
            Statement::Assign(assign) => {
                let value = self.gen_expr(&assign.value);
                match &assign.target {
                    AssignTarget::Var(ident) => {
                        self.rir.add_inst(Inst {
                            data: InstData::Assign {
                                name: ident.name, // Already a Spur
                                value,
                            },
                            span: assign.span,
                        })
                    }
                    AssignTarget::Field(field_expr) => {
                        let base = self.gen_expr(&field_expr.base);
                        self.rir.add_inst(Inst {
                            data: InstData::FieldSet {
                                base,
                                field: field_expr.field.name, // Already a Spur
                                value,
                            },
                            span: assign.span,
                        })
                    }
                    AssignTarget::Index(index_expr) => {
                        let base = self.gen_expr(&index_expr.base);
                        let index = self.gen_expr(&index_expr.index);
                        self.rir.add_inst(Inst {
                            data: InstData::IndexSet { base, index, value },
                            span: assign.span,
                        })
                    }
                }
            }
            Statement::Expr(expr) => {
                // Expression statements are evaluated for side effects
                // The result is discarded, but we still return the InstRef
                self.gen_expr(expr)
            }
        }
    }
}

/// Whether a pattern is irrefutable (matches every value of its type).
///
/// Used by `try_elaborate_irrefutable_match` to decide when a tuple / struct
/// match arm can be lowered as a straight let-destructure. Literals and
/// variant patterns are always refutable; wildcard, ident, and a tuple /
/// struct whose every leaf is irrefutable are not.
fn is_irrefutable_destructure(pat: &Pattern) -> bool {
    match pat {
        Pattern::Wildcard(_) | Pattern::Ident { .. } => true,
        Pattern::Int(_) | Pattern::NegInt(_) | Pattern::Bool(_) => false,
        Pattern::Path(_) => false,
        Pattern::DataVariant { .. } | Pattern::StructVariant { .. } => false,
        Pattern::Struct { fields, .. } => fields.iter().all(|f| match &f.sub {
            None => true,
            Some(sub) => is_irrefutable_destructure(sub),
        }),
        Pattern::Tuple { elems, .. } => elems.iter().all(|e| match e {
            TupleElemPattern::Pattern(p) => is_irrefutable_destructure(p),
            TupleElemPattern::Rest(_) => true,
        }),
    }
}

// Note: the flat-lowering helpers `field_pattern_to_rir_destructure_field`,
// `tuple_elem_to_rir_destructure_field`, `tuple_elem_to_rir_binding`, and
// `field_pattern_to_rir_binding` were superseded by
// `AstGen::emit_let_destructure` (Phase 4) and
// `AstGen::tuple_elem_to_rir_binding_or_capture` /
// `AstGen::field_pattern_to_rir_binding_or_capture` (Phase 4b), which handle
// nested destructure and variant sub-pattern shapes uniformly by emitting
// trees of StructDestructure instructions or capturing nested sub-patterns for
// arm-body elaboration.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inst::RirPrinter;
    use gruel_lexer::Lexer;
    use gruel_parser::Parser;

    fn gen_rir(source: &str) -> (Rir, ThreadedRodeo) {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();
        (rir, interner)
    }

    #[test]
    fn test_gen_simple_function() {
        let (rir, interner) = gen_rir("fn main() -> i32 { 42 }");

        // Should have 2 instructions: IntConst(42), FnDecl
        assert_eq!(rir.len(), 2);

        // Check the function declaration
        let (_, fn_inst) = rir.iter().last().unwrap();
        match &fn_inst.data {
            InstData::FnDecl {
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                ..
            } => {
                assert_eq!(interner.resolve(name), "main");
                let params = rir.get_params(*params_start, *params_len);
                assert!(params.is_empty());
                assert_eq!(interner.resolve(return_type), "i32");
                assert!(!has_self); // Regular functions don't have self
                // Body should be the int constant
                let body_inst = rir.get(*body);
                assert!(matches!(body_inst.data, InstData::IntConst(42)));
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_addition() {
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 }");

        // Should have: IntConst(1), IntConst(2), Add, FnDecl
        assert_eq!(rir.len(), 4);

        // Check add instruction
        let add_inst = rir.get(InstRef::from_raw(2));
        match &add_inst.data {
            InstData::Add { lhs, rhs } => {
                assert!(matches!(rir.get(*lhs).data, InstData::IntConst(1)));
                assert!(matches!(rir.get(*rhs).data, InstData::IntConst(2)));
            }
            _ => panic!("expected Add"),
        }
    }

    #[test]
    fn test_gen_precedence() {
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 * 3 }");

        // Should have: IntConst(1), IntConst(2), IntConst(3), Mul, Add, FnDecl
        assert_eq!(rir.len(), 6);

        // Check that add is the body (mul is nested)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Add { lhs, rhs } => {
                        // lhs should be IntConst(1)
                        assert!(matches!(rir.get(*lhs).data, InstData::IntConst(1)));
                        // rhs should be Mul
                        assert!(matches!(rir.get(*rhs).data, InstData::Mul { .. }));
                    }
                    _ => panic!("expected Add"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_negation() {
        let (rir, _) = gen_rir("fn main() -> i32 { -42 }");

        // Should have: IntConst(42), Neg, FnDecl
        assert_eq!(rir.len(), 3);

        // Check neg instruction
        let neg_inst = rir.get(InstRef::from_raw(1));
        match &neg_inst.data {
            InstData::Neg { operand } => {
                assert!(matches!(rir.get(*operand).data, InstData::IntConst(42)));
            }
            _ => panic!("expected Neg"),
        }
    }

    #[test]
    fn test_gen_parens() {
        let (rir, _) = gen_rir("fn main() -> i32 { (1 + 2) * 3 }");

        // Should have: IntConst(1), IntConst(2), Add, IntConst(3), Mul, FnDecl
        // Parens don't generate instructions, they just affect evaluation order
        assert_eq!(rir.len(), 6);

        // Check that mul is the body (add is nested)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Mul { lhs, rhs } => {
                        // lhs should be Add
                        assert!(matches!(rir.get(*lhs).data, InstData::Add { .. }));
                        // rhs should be IntConst(3)
                        assert!(matches!(rir.get(*rhs).data, InstData::IntConst(3)));
                    }
                    _ => panic!("expected Mul"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_all_binary_ops() {
        // Test all binary operators generate correct instructions
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Add { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 - 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Sub { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 * 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Mul { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 / 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Div { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 % 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Mod { .. }
        ));
    }

    #[test]
    fn test_gen_let_binding() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let x = 42; x }");

        // Find the Alloc instruction
        let alloc_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }));
        assert!(alloc_inst.is_some());

        let (_, inst) = alloc_inst.unwrap();
        match &inst.data {
            InstData::Alloc {
                name,
                is_mut,
                ty,
                init,
                ..
            } => {
                assert_eq!(interner.resolve(&name.unwrap()), "x");
                assert!(!is_mut);
                assert!(ty.is_none());
                assert!(matches!(rir.get(*init).data, InstData::IntConst(42)));
            }
            _ => panic!("expected Alloc"),
        }
    }

    #[test]
    fn test_gen_let_mut() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let mut x = 10; x }");

        let alloc_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }));
        assert!(alloc_inst.is_some());

        let (_, inst) = alloc_inst.unwrap();
        match &inst.data {
            InstData::Alloc { name, is_mut, .. } => {
                assert_eq!(interner.resolve(&name.unwrap()), "x");
                assert!(*is_mut);
            }
            _ => panic!("expected Alloc"),
        }
    }

    #[test]
    fn test_gen_var_ref() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let x = 42; x }");

        // The body should be a Block (since there are statements)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Block { extra_start, len } => {
                        // Block contains: Alloc, VarRef
                        assert_eq!(*len, 2);
                        let inst_refs = rir.get_extra(*extra_start, *len);
                        // Last instruction in block is the VarRef
                        let var_ref_inst = rir.get(InstRef::from_raw(inst_refs[1]));
                        match &var_ref_inst.data {
                            InstData::VarRef { name } => {
                                assert_eq!(interner.resolve(name), "x");
                            }
                            _ => panic!("expected VarRef"),
                        }
                    }
                    _ => panic!("expected Block, got {:?}", body_inst.data),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_assignment() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let mut x = 10; x = 20; x }");

        // Find the Assign instruction
        let assign_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Assign { .. }));
        assert!(assign_inst.is_some());

        let (_, inst) = assign_inst.unwrap();
        match &inst.data {
            InstData::Assign { name, value } => {
                assert_eq!(interner.resolve(name), "x");
                assert!(matches!(rir.get(*value).data, InstData::IntConst(20)));
            }
            _ => panic!("expected Assign"),
        }
    }

    #[test]
    fn test_gen_multiple_statements() {
        let (rir, _interner) = gen_rir("fn main() -> i32 { let x = 1; let y = 2; x + y }");

        // Count Alloc instructions
        let alloc_count = rir
            .iter()
            .filter(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }))
            .count();
        assert_eq!(alloc_count, 2);

        // Check the body is a Block containing the allocs and the Add
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Block { extra_start, len } => {
                        // Block contains: Alloc(x), Alloc(y), Add
                        assert_eq!(*len, 3);
                        let inst_refs = rir.get_extra(*extra_start, *len);
                        // Last instruction in block is the Add
                        let add_inst = rir.get(InstRef::from_raw(inst_refs[2]));
                        assert!(matches!(add_inst.data, InstData::Add { .. }));
                    }
                    _ => panic!("expected Block"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    // Struct with methods tests
    #[test]
    fn test_gen_struct_with_method() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn get_x(self) -> i32 {
                    self.x
                }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the StructDecl instruction
        let struct_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::StructDecl { .. }));
        assert!(struct_decl.is_some(), "Expected StructDecl instruction");

        let (_, inst) = struct_decl.unwrap();
        match &inst.data {
            InstData::StructDecl {
                name,
                methods_start,
                methods_len,
                ..
            } => {
                assert_eq!(interner.resolve(name), "Point");
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
                assert_eq!(methods.len(), 1);

                // Check the method is a FnDecl with has_self=true
                let method_inst = rir.get(methods[0]);
                match &method_inst.data {
                    InstData::FnDecl { name, has_self, .. } => {
                        assert_eq!(interner.resolve(name), "get_x");
                        assert!(*has_self);
                    }
                    _ => panic!("expected FnDecl"),
                }
            }
            _ => panic!("expected StructDecl"),
        }
    }

    #[test]
    fn test_gen_struct_with_multiple_methods() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn get_x(self) -> i32 { self.x }
                fn get_y(self) -> i32 { self.y }
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        let struct_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::StructDecl { .. }));
        assert!(struct_decl.is_some());

        let (_, inst) = struct_decl.unwrap();
        match &inst.data {
            InstData::StructDecl {
                methods_start,
                methods_len,
                ..
            } => {
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
                assert_eq!(methods.len(), 3);

                // Check get_x and get_y have self, origin does not
                for method_ref in methods {
                    let method_inst = rir.get(method_ref);
                    match &method_inst.data {
                        InstData::FnDecl { name, has_self, .. } => {
                            let method_name = interner.resolve(name);
                            if method_name == "origin" {
                                assert!(!has_self, "origin should not have self");
                            } else {
                                assert!(*has_self, "{} should have self", method_name);
                            }
                        }
                        _ => panic!("expected FnDecl"),
                    }
                }
            }
            _ => panic!("expected StructDecl"),
        }
    }

    #[test]
    fn test_gen_method_call() {
        let source = r#"
            struct Point {
                x: i32,
                fn get_x(self) -> i32 { self.x }
            }
            fn main() -> i32 {
                let p = Point { x: 42 };
                p.get_x()
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the MethodCall instruction
        let method_call = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::MethodCall { .. }));
        assert!(method_call.is_some(), "Expected MethodCall instruction");

        let (_, inst) = method_call.unwrap();
        match &inst.data {
            InstData::MethodCall {
                receiver: _,
                method,
                args_start,
                args_len,
            } => {
                assert_eq!(interner.resolve(method), "get_x");
                let args = rir.get_call_args(*args_start, *args_len);
                assert!(args.is_empty()); // No explicit args (self is implicit)
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_gen_assoc_fn_call() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 {
                let p = Point::origin();
                0
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the AssocFnCall instruction
        let assoc_fn_call = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::AssocFnCall { .. }));
        assert!(assoc_fn_call.is_some(), "Expected AssocFnCall instruction");

        let (_, inst) = assoc_fn_call.unwrap();
        match &inst.data {
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                assert_eq!(interner.resolve(type_name), "Point");
                assert_eq!(interner.resolve(function), "origin");
                let args = rir.get_call_args(*args_start, *args_len);
                assert!(args.is_empty());
            }
            _ => panic!("expected AssocFnCall"),
        }
    }

    // Pattern tests
    #[test]
    fn test_gen_match_wildcard_pattern() {
        let source = r#"
            fn main() -> i32 {
                let x = 5;
                match x {
                    _ => 42,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        // Find the Match instruction
        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some(), "Expected Match instruction");

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
                assert_eq!(arms.len(), 1);
                assert!(matches!(arms[0].0, RirPattern::Wildcard(_)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_int_patterns() {
        let source = r#"
            fn main() -> i32 {
                let x = 5;
                match x {
                    1 => 10,
                    2 => 20,
                    _ => 0,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
                assert_eq!(arms.len(), 3);
                assert!(matches!(arms[0].0, RirPattern::Int(1, _)));
                assert!(matches!(arms[1].0, RirPattern::Int(2, _)));
                assert!(matches!(arms[2].0, RirPattern::Wildcard(_)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_negative_int_pattern() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = -5;
                match x {
                    -5 => 1,
                    -10 => 2,
                    _ => 0,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
                assert_eq!(arms.len(), 3);
                assert!(matches!(arms[0].0, RirPattern::Int(-5, _)));
                assert!(matches!(arms[1].0, RirPattern::Int(-10, _)));
                assert!(matches!(arms[2].0, RirPattern::Wildcard(_)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_bool_patterns() {
        let source = r#"
            fn main() -> i32 {
                let b = true;
                match b {
                    true => 1,
                    false => 0,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
                assert_eq!(arms.len(), 2);
                assert!(matches!(arms[0].0, RirPattern::Bool(true, _)));
                assert!(matches!(arms[1].0, RirPattern::Bool(false, _)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_enum_patterns() {
        let source = r#"
            enum Color { Red, Green, Blue }
            fn main() -> i32 {
                let c = Color::Red;
                match c {
                    Color::Red => 1,
                    Color::Green => 2,
                    Color::Blue => 3,
                }
            }
        "#;
        let (rir, interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
                assert_eq!(arms.len(), 3);

                // Check first arm is Color::Red
                match &arms[0].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(type_name), "Color");
                        assert_eq!(interner.resolve(variant), "Red");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check second arm is Color::Green
                match &arms[1].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(type_name), "Color");
                        assert_eq!(interner.resolve(variant), "Green");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check third arm is Color::Blue
                match &arms[2].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(type_name), "Color");
                        assert_eq!(interner.resolve(variant), "Blue");
                    }
                    _ => panic!("expected Path pattern"),
                }
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_self_expr() {
        let source = r#"
            struct Point {
                x: i32,
                fn get_x(self) -> i32 { self.x }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the VarRef instruction for "self"
        let self_ref = rir.iter().find(|(_, inst)| match &inst.data {
            InstData::VarRef { name } => interner.resolve(name) == "self",
            _ => false,
        });
        assert!(self_ref.is_some(), "Expected self VarRef instruction");
    }

    #[test]
    fn test_gen_drop_fn() {
        let source = r#"
            struct Resource { value: i32 }
            drop fn Resource(self) { () }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the DropFnDecl instruction
        let drop_fn = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::DropFnDecl { .. }));
        assert!(drop_fn.is_some(), "Expected DropFnDecl instruction");

        let (_, inst) = drop_fn.unwrap();
        match &inst.data {
            InstData::DropFnDecl { type_name, body: _ } => {
                assert_eq!(interner.resolve(type_name), "Resource");
            }
            _ => panic!("expected DropFnDecl"),
        }
    }

    #[test]
    fn test_gen_enum_variant() {
        let source = r#"
            enum Color { Red, Green, Blue }
            fn main() -> i32 {
                let c = Color::Red;
                0
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the EnumVariant instruction
        let enum_variant = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::EnumVariant { .. }));
        assert!(enum_variant.is_some(), "Expected EnumVariant instruction");

        let (_, inst) = enum_variant.unwrap();
        match &inst.data {
            InstData::EnumVariant {
                type_name, variant, ..
            } => {
                assert_eq!(interner.resolve(type_name), "Color");
                assert_eq!(interner.resolve(variant), "Red");
            }
            _ => panic!("expected EnumVariant"),
        }
    }

    #[test]
    fn test_gen_method_with_params() {
        let source = r#"
            struct Counter {
                value: i32,
                fn add(self, amount: i32) -> i32 { self.value + amount }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the struct declaration
        let struct_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::StructDecl { .. }));
        assert!(struct_decl.is_some());

        let (_, inst) = struct_decl.unwrap();
        match &inst.data {
            InstData::StructDecl {
                methods_start,
                methods_len,
                ..
            } => {
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
                let method_inst = rir.get(methods[0]);
                match &method_inst.data {
                    InstData::FnDecl {
                        name,
                        params_start,
                        params_len,
                        has_self,
                        ..
                    } => {
                        assert_eq!(interner.resolve(name), "add");
                        assert!(*has_self);
                        // params should contain 'amount', not 'self'
                        let params = rir.get_params(*params_start, *params_len);
                        assert_eq!(params.len(), 1);
                        assert_eq!(interner.resolve(&params[0].name), "amount");
                    }
                    _ => panic!("expected FnDecl"),
                }
            }
            _ => panic!("expected StructDecl"),
        }
    }

    // RirPrinter integration test with actual generated RIR
    #[test]
    fn test_printer_integration() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 {
                let p = Point::origin();
                p.x
            }
        "#;
        let (rir, interner) = gen_rir(source);

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();

        // Check key elements are present in the output
        assert!(output.contains("struct Point"));
        assert!(output.contains("methods: ["));
        assert!(output.contains("fn origin"));
        assert!(output.contains("fn main"));
        assert!(output.contains("struct_init Point"));
        assert!(output.contains("assoc_fn_call Point::origin"));
        assert!(output.contains("field_get"));
    }

    // ===== Function span tests =====

    #[test]
    fn test_function_spans_simple() {
        let (rir, interner) = gen_rir("fn main() -> i32 { 42 }");

        // Should have exactly one function span
        assert_eq!(rir.function_count(), 1);

        let spans: Vec<_> = rir.functions().collect();
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(interner.resolve(&span.name), "main");

        // The function should have 2 instructions: IntConst(42) and FnDecl
        assert_eq!(span.instruction_count(), 2);

        // The FnDecl should be the last instruction
        let fn_inst = rir.get(span.decl);
        assert!(matches!(fn_inst.data, InstData::FnDecl { .. }));
    }

    #[test]
    fn test_function_spans_multiple_functions() {
        let source = r#"
            fn helper() -> i32 { 1 }
            fn main() -> i32 { 42 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Should have two function spans
        assert_eq!(rir.function_count(), 2);

        let spans: Vec<_> = rir.functions().collect();
        assert_eq!(spans.len(), 2);

        // First function: helper
        assert_eq!(interner.resolve(&spans[0].name), "helper");
        assert_eq!(spans[0].instruction_count(), 2);

        // Second function: main
        assert_eq!(interner.resolve(&spans[1].name), "main");
        assert_eq!(spans[1].instruction_count(), 2);

        // Function spans should be non-overlapping
        assert!(
            spans[0].decl.as_u32() < spans[1].body_start.as_u32(),
            "helper should end before main starts"
        );
    }

    #[test]
    fn test_function_spans_with_methods() {
        let source = r#"
            struct Point {
                x: i32,
                fn get_x(self) -> i32 { self.x }
                fn origin() -> Point { Point { x: 0 } }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Should have three function spans: get_x, origin, main
        assert_eq!(rir.function_count(), 3);

        let spans: Vec<_> = rir.functions().collect();

        // Methods should be tracked as well
        let names: Vec<_> = spans.iter().map(|s| interner.resolve(&s.name)).collect();
        assert!(names.contains(&"get_x"));
        assert!(names.contains(&"origin"));
        assert!(names.contains(&"main"));
    }

    #[test]
    fn test_function_view() {
        let source = r#"
            fn helper(x: i32) -> i32 { x + 1 }
            fn main() -> i32 { helper(41) }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the main function span
        let main_span = rir.find_function(interner.get_or_intern("main")).unwrap();

        // Get a view of main's instructions
        let view = rir.function_view(main_span);

        // The view should contain the right number of instructions
        assert_eq!(view.len(), main_span.instruction_count() as usize);

        // The last instruction should be the FnDecl
        let fn_decl = view.fn_decl();
        match &fn_decl.data {
            InstData::FnDecl { name, .. } => {
                assert_eq!(interner.resolve(name), "main");
            }
            _ => panic!("Expected FnDecl"),
        }

        // We should be able to iterate over the view
        let mut found_call = false;
        for (_, inst) in view.iter() {
            if matches!(inst.data, InstData::Call { .. }) {
                found_call = true;
            }
        }
        assert!(found_call, "main should contain a call to helper");
    }

    #[test]
    fn test_function_span_complex_body() {
        let source = r#"
            fn complex() -> i32 {
                let x = 1;
                let y = 2;
                if x < y {
                    x + y
                } else {
                    x - y
                }
            }
        "#;
        let (rir, interner) = gen_rir(source);

        assert_eq!(rir.function_count(), 1);

        let span = rir
            .find_function(interner.get_or_intern("complex"))
            .unwrap();

        // The function should have multiple instructions for the body
        // At minimum: 2 IntConsts, 2 Allocs, comparison, branches, operations, FnDecl
        assert!(
            span.instruction_count() >= 8,
            "Complex function should have at least 8 instructions, got {}",
            span.instruction_count()
        );

        // Verify the view contains all expected instruction types
        let view = rir.function_view(span);
        let mut has_alloc = false;
        let mut has_branch = false;

        for (_, inst) in view.iter() {
            if matches!(inst.data, InstData::Alloc { .. }) {
                has_alloc = true;
            }
            if matches!(inst.data, InstData::Branch { .. }) {
                has_branch = true;
            }
        }

        assert!(has_alloc, "Function should have Alloc instructions");
        assert!(has_branch, "Function should have Branch instruction");
    }

    #[test]
    fn test_find_function() {
        let source = r#"
            fn foo() -> i32 { 1 }
            fn bar() -> i32 { 2 }
            fn baz() -> i32 { 3 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find existing functions
        let foo_sym = interner.get_or_intern("foo");
        let bar_sym = interner.get_or_intern("bar");
        let baz_sym = interner.get_or_intern("baz");
        let nonexistent_sym = interner.get_or_intern("nonexistent");

        assert!(rir.find_function(foo_sym).is_some());
        assert!(rir.find_function(bar_sym).is_some());
        assert!(rir.find_function(baz_sym).is_some());
        assert!(rir.find_function(nonexistent_sym).is_none());
    }

    #[test]
    fn test_function_span_ordering() {
        let source = r#"
            fn a() -> i32 { 1 }
            fn b() -> i32 { 2 }
            fn c() -> i32 { 3 }
        "#;
        let (rir, _interner) = gen_rir(source);

        let spans: Vec<_> = rir.functions().collect();
        assert_eq!(spans.len(), 3);

        // Verify functions are recorded in source order
        for i in 1..spans.len() {
            assert!(
                spans[i - 1].decl.as_u32() < spans[i].body_start.as_u32(),
                "Function {} should end before function {} starts",
                i - 1,
                i
            );
        }
    }

    #[test]
    fn test_anon_struct_with_methods() {
        // Test that anonymous structs with methods generate AnonStructType with method references
        let source = r#"
            fn MakePoint(comptime T: type) -> type {
                struct {
                    x: T,
                    y: T,

                    fn get_x(self) -> T { self.x }
                    fn origin() -> Self { Self { x: 0, y: 0 } }
                }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the AnonStructType instruction
        let anon_struct = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::AnonStructType { .. }));
        assert!(
            anon_struct.is_some(),
            "Expected to find AnonStructType instruction"
        );

        let (_, inst) = anon_struct.unwrap();
        match &inst.data {
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
                ..
            } => {
                // Should have 2 fields (x and y)
                let fields = rir.get_field_decls(*fields_start, *fields_len);
                assert_eq!(fields.len(), 2);
                assert_eq!(interner.resolve(&fields[0].0), "x");
                assert_eq!(interner.resolve(&fields[1].0), "y");

                // Should have 2 methods (get_x and origin)
                assert_eq!(*methods_len, 2);
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
                assert_eq!(methods.len(), 2);

                // Verify each method is a FnDecl
                for method_ref in methods {
                    let method_inst = rir.get(method_ref);
                    match &method_inst.data {
                        InstData::FnDecl { name, has_self, .. } => {
                            let name_str = interner.resolve(name);
                            // get_x has self, origin doesn't
                            if name_str == "get_x" {
                                assert!(*has_self, "get_x should have self parameter");
                            } else if name_str == "origin" {
                                assert!(!*has_self, "origin should not have self parameter");
                            }
                        }
                        _ => panic!("Expected FnDecl for method"),
                    }
                }
            }
            _ => panic!("Expected AnonStructType"),
        }
    }

    #[test]
    fn test_anon_struct_without_methods() {
        // Test that anonymous structs without methods have zero methods_len
        let source = r#"
            fn MakePair(comptime T: type) -> type {
                struct { first: T, second: T }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, _interner) = gen_rir(source);

        // Find the AnonStructType instruction
        let anon_struct = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::AnonStructType { .. }));
        assert!(
            anon_struct.is_some(),
            "Expected to find AnonStructType instruction"
        );

        let (_, inst) = anon_struct.unwrap();
        match &inst.data {
            InstData::AnonStructType { methods_len, .. } => {
                assert_eq!(*methods_len, 0, "Expected no methods");
            }
            _ => panic!("Expected AnonStructType"),
        }
    }

    #[test]
    fn test_anon_struct_method_function_spans() {
        // Test that methods inside anonymous structs are tracked in function_spans
        let source = r#"
            fn Container(comptime T: type) -> type {
                struct {
                    value: T,
                    fn get(self) -> T { self.value }
                    fn set(self, v: T) -> Self { Self { value: v } }
                }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Should have 4 functions: Container, get, set, main
        assert_eq!(
            rir.function_count(),
            4,
            "Expected 4 functions (Container, get, set, main)"
        );

        // Check that all methods are findable by name
        let get_sym = interner.get_or_intern("get");
        let set_sym = interner.get_or_intern("set");
        assert!(
            rir.find_function(get_sym).is_some(),
            "Should find 'get' method"
        );
        assert!(
            rir.find_function(set_sym).is_some(),
            "Should find 'set' method"
        );
    }

    // ========================================================================
    // ADR-0055 Phase 2: anonymous function RIR lowering
    // ========================================================================

    #[test]
    fn test_anon_fn_lowers_to_call_method_and_value() {
        // `fn(x: i32) -> i32 { x + 1 }` should produce an internal FnDecl
        // named `__call` plus an `AnonFnValue` referring to it.
        let (rir, interner) = gen_rir("fn main() -> i32 { fn(x: i32) -> i32 { x + 1 }; 0 }");

        let call_sym = interner.get("__call").expect("__call should be interned");

        // Locate the synthesized __call method.
        let mut found_call = None;
        let mut found_anon_fn_value = None;
        for (i, inst) in rir.iter() {
            match &inst.data {
                InstData::FnDecl { name, has_self, .. } if *name == call_sym => {
                    assert!(*has_self, "synthesized __call must have self receiver");
                    found_call = Some(i);
                }
                InstData::AnonFnValue { method } => {
                    found_anon_fn_value = Some((i, *method));
                }
                _ => {}
            }
        }

        let call_ref = found_call.expect("expected a FnDecl named __call in RIR");
        let (_, value_method) = found_anon_fn_value.expect("expected an AnonFnValue in RIR");
        assert_eq!(
            value_method, call_ref,
            "AnonFnValue must point at the synthesized __call FnDecl"
        );
    }

    #[test]
    fn test_anon_fn_two_sites_each_get_their_own_call_method() {
        // Two distinct anonymous-function expressions produce two distinct
        // __call FnDecl instructions (one per site), even with matching
        // signatures. This is important for Phase 3's uniqueness work — each
        // site's body must survive into RIR separately.
        let (rir, interner) = gen_rir(
            "fn main() -> i32 {
                fn(x: i32) -> i32 { x + 1 };
                fn(x: i32) -> i32 { x * 2 };
                0
            }",
        );
        let call_sym = interner.get("__call").expect("__call should be interned");
        let count = rir
            .iter()
            .filter(|(_, inst)| matches!(&inst.data, InstData::FnDecl { name, .. } if *name == call_sym))
            .count();
        assert_eq!(
            count, 2,
            "each fn(...) site should produce its own __call FnDecl"
        );
    }

    #[test]
    fn test_anon_fn_zero_params_lowers() {
        let (rir, interner) = gen_rir("fn main() -> i32 { fn() -> i32 { 7 }; 0 }");
        let call_sym = interner.get("__call").expect("__call should be interned");
        let call_inst = rir.iter().find_map(|(_, inst)| {
            if let InstData::FnDecl {
                name,
                params_len,
                has_self,
                ..
            } = &inst.data
                && *name == call_sym
            {
                Some((*params_len, *has_self))
            } else {
                None
            }
        });
        let (params_len, has_self) = call_inst.expect("expected __call FnDecl");
        assert_eq!(params_len, 0);
        assert!(has_self);
    }
}
