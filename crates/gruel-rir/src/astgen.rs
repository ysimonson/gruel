//! AST to RIR generation.
//!
//! AstGen converts the Abstract Syntax Tree into RIR instructions.
//! This is analogous to Zig's AstGen phase.

use lasso::{Spur, ThreadedRodeo};

/// Known type intrinsics that take a type argument rather than an expression.
/// These intrinsics operate on types at compile time (e.g., @size_of(i32)).
const TYPE_INTRINSICS: &[&str] = &["size_of", "align_of", "typeName", "typeInfo"];
use gruel_parser::ast::{ConstDecl, DropFn, FieldPattern, Ident as AstIdent, TupleElemPattern};
use gruel_parser::{
    ArgMode, AssignTarget, Ast, BinaryOp, CallArg, Directive, DirectiveArg, EnumDecl, Expr,
    Function, IntrinsicArg, Item, MatchArm, MatchExpr, Method, ParamMode, Pattern, Statement,
    StructDecl, TypeExpr, UnaryOp, ast::Visibility,
};

use crate::inst::{
    FunctionSpan, Inst, InstData, InstRef, Rir, RirArgMode, RirCallArg, RirDestructureField,
    RirDirective, RirParam, RirParamMode, RirPattern, RirPatternBinding, RirStructPatternBinding,
};

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// The AST being processed
    ast: &'a Ast,
    /// String interner for symbols (thread-safe, takes shared reference)
    interner: &'a ThreadedRodeo,
    /// Output RIR
    rir: Rir,
    /// Counter for generating unique synthetic binding names when elaborating
    /// nested destructuring patterns (ADR-0049 Phase 4).
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

    /// Generate a fresh synthetic symbol name for an intermediate binding in a
    /// nested destructure. The name is interned once and used as both the
    /// destructure-field binding_name and the `VarRef` key for the child
    /// destructure to reference the same local.
    fn fresh_nested_pat_name(&mut self) -> Spur {
        let n = self.nested_pat_counter;
        self.nested_pat_counter += 1;
        self.interner.get_or_intern(format!("__nested_pat_{}", n))
    }

    /// Generate a fresh synthetic symbol name for the tuple-match scrutinee
    /// binding (ADR-0049 Phase 5a). Shares the nested-pat counter so names
    /// are globally unique within a function.
    fn fresh_match_scr_name(&mut self) -> Spur {
        let n = self.nested_pat_counter;
        self.nested_pat_counter += 1;
        self.interner.get_or_intern(format!("__match_scr_{}", n))
    }

    /// Emit a `@panic("message")` call as a single RIR instruction with
    /// type `Never`. Used by the tuple-match elaborator to terminate a
    /// non-exhaustive if-chain at runtime (ADR-0049 Phase 5a).
    fn emit_panic_call(&mut self, message: &str, span: gruel_span::Span) -> InstRef {
        let msg = self.interner.get_or_intern(message);
        let msg_const = self.rir.add_inst(Inst {
            data: InstData::StringConst(msg),
            span,
        });
        let (args_start, args_len) = self.rir.add_inst_refs(&[msg_const]);
        let name = self.interner.get_or_intern_static("panic");
        self.rir.add_inst(Inst {
            data: InstData::Intrinsic {
                name,
                args_start,
                args_len,
            },
            span,
        })
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

        self.rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: enum_decl.visibility == Visibility::Public,
                name,
                variants_start,
                variants_len,
            },
            span: enum_decl.span,
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
                // Top-level struct / tuple / ident patterns at the match arm
                // root are irrefutable — for a match expression with only such
                // an arm, elaborate the whole thing into a let-destructure
                // around the arm body. This keeps existing RIR shapes unchanged
                // while allowing nested pattern syntax.
                if let Some(elaborated) = self.try_elaborate_irrefutable_match(match_expr) {
                    return elaborated;
                }

                // Multi-arm matches with tuple patterns at the top of any arm
                // elaborate into a let-bound scrutinee plus an if/else chain
                // over tuple projections (ADR-0049 Phase 5a).
                if let Some(elaborated) = self.try_elaborate_tuple_match(match_expr) {
                    return elaborated;
                }

                // Arms with refutable nested sub-patterns in variant fields
                // (e.g., `Some(Some(v))`) elaborate into nested matches that
                // fall back to the outer match's wildcard catch-all body
                // (ADR-0049 Phase 5b).
                if let Some(elaborated) = self.try_elaborate_refutable_nested_match(match_expr) {
                    return elaborated;
                }

                let scrutinee = self.gen_expr(&match_expr.scrutinee);
                let mut arms: Vec<(RirPattern, InstRef)> =
                    Vec::with_capacity(match_expr.arms.len());
                for arm in &match_expr.arms {
                    let mut nested: Vec<(Spur, Pattern)> = Vec::new();
                    let pattern = self.gen_match_arm_pattern(&arm.pattern, &mut nested);
                    let body = self.gen_expr(&arm.body);
                    let body = if nested.is_empty() {
                        body
                    } else {
                        self.wrap_match_arm_body_with_destructures(body, nested, arm.body.span())
                    };
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

                let is_type_intrinsic = TYPE_INTRINSICS.contains(&intrinsic_name_str);

                if is_type_intrinsic && intrinsic.args.len() == 1 {
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
                        fields, methods, ..
                    } => {
                        // Generate an anonymous struct type instruction with methods
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
                                fields_start,
                                fields_len,
                                methods_start,
                                methods_len,
                            },
                            span: type_lit.span,
                        })
                    }
                    TypeExpr::AnonymousEnum {
                        variants, methods, ..
                    } => {
                        // Generate an anonymous enum type instruction with methods
                        use gruel_parser::ast::EnumVariantKind;
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
                            TypeExpr::AnonymousStruct { .. } | TypeExpr::AnonymousEnum { .. } => {
                                unreachable!("handled above")
                            }
                            TypeExpr::PointerConst { .. } | TypeExpr::PointerMut { .. } => {
                                // Pointer types as values - use intern_type to get representation
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
                let field = self.interner.get_or_intern(&ti.index.to_string());
                self.rir.add_inst(Inst {
                    data: InstData::FieldGet { base, field },
                    span: ti.span,
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

    /// Elaborate a match expression with refutable nested sub-patterns in
    /// variant fields (e.g., `Some(Some(v))`) by AST rewriting: replace
    /// each refutable sub-pattern with a synthetic `__refut_N` ident
    /// binding, wrap the arm body in a nested match over that binding,
    /// and reuse the outer match's trailing wildcard/ident arm body as
    /// the nested match's fallback (ADR-0049 Phase 5b).
    ///
    /// Returns `None` if:
    /// - No arm has a refutable nested sub-pattern (caller uses normal path).
    /// - The match has no trailing wildcard/ident arm to serve as the
    ///   fallback — that shape requires true recursive dispatch with
    ///   decision-tree construction and is still unsupported.
    ///
    /// The fallback body is cloned once per elaborated arm; since match
    /// arms are mutually exclusive, the duplicated bodies execute at most
    /// once across all paths at runtime.
    fn try_elaborate_refutable_nested_match(&mut self, match_expr: &MatchExpr) -> Option<InstRef> {
        let needs = match_expr
            .arms
            .iter()
            .any(|arm| pattern_has_refutable_nested_sub(&arm.pattern));
        if !needs {
            return None;
        }

        // Locate the last wildcard / ident catch-all arm, if any. Arms
        // after it are unreachable and dropped from the elaboration.
        // When there's no catch-all, the nested matches are expected to
        // be exhaustive across their variant type (enforced by sema).
        let catch_all_idx = match_expr
            .arms
            .iter()
            .rposition(|arm| matches!(&arm.pattern, Pattern::Wildcard(_) | Pattern::Ident { .. }));

        let catch_all_body: Option<Box<Expr>> =
            catch_all_idx.map(|idx| match_expr.arms[idx].body.clone());

        // Group arms by outer variant key, preserving first-occurrence
        // order. Arms sharing an outer variant are merged into a single
        // arm with a nested match over the refutable positions so the
        // first arm doesn't absorb the variant and hide subsequent arms.
        //
        // Arms without a variant key (Path, literal) pass through at
        // their original position.
        let upper = catch_all_idx.unwrap_or(match_expr.arms.len());
        let arms_before_catch: &[MatchArm] = &match_expr.arms[..upper];
        let mut groups: Vec<((Spur, Spur), Vec<usize>)> = Vec::new();
        for (i, arm) in arms_before_catch.iter().enumerate() {
            if let Some(key) = outer_variant_key(&arm.pattern) {
                if let Some((_, list)) = groups.iter_mut().find(|(k, _)| *k == key) {
                    list.push(i);
                } else {
                    groups.push((key, vec![i]));
                }
            }
        }

        // Decide merge strategy per group; bail out to `None` for shapes
        // we don't yet support (multi-field variants with shared outer,
        // struct variants with shared outer, rest patterns in a merged
        // arm) so the normal match path surfaces a clear error.
        let mut merged: std::collections::HashMap<usize, MatchArm> =
            std::collections::HashMap::new();
        let mut absorbed: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (_, idxs) in &groups {
            if idxs.len() < 2 {
                continue;
            }
            let Some(arm) =
                self.merge_group_single_field(&match_expr.arms, idxs, catch_all_body.as_deref())
            else {
                return None;
            };
            let first = idxs[0];
            merged.insert(first, arm);
            for &idx in idxs.iter().skip(1) {
                absorbed.insert(idx);
            }
        }

        let mut new_arms: Vec<MatchArm> = Vec::with_capacity(arms_before_catch.len() + 1);
        for (i, arm) in arms_before_catch.iter().enumerate() {
            if absorbed.contains(&i) {
                continue;
            }
            if let Some(merged_arm) = merged.remove(&i) {
                new_arms.push(merged_arm);
                continue;
            }
            if pattern_has_refutable_nested_sub(&arm.pattern) {
                // Per-arm elaboration requires a catch-all body to use as
                // the nested match's fallback. Without it, fall through
                // to the normal match path (which will panic with a
                // clear message).
                let Some(catch_all_body_ref) = catch_all_body.as_ref() else {
                    return None;
                };
                let mut subs: Vec<(Spur, Pattern)> = Vec::new();
                let new_pattern = self.replace_refutable_nested_subs(&arm.pattern, &mut subs);
                let mut body = (*arm.body).clone();
                for (syn_name, sub_pattern) in subs {
                    body = Expr::Match(MatchExpr {
                        scrutinee: Box::new(Expr::Ident(AstIdent {
                            name: syn_name,
                            span: sub_pattern.span(),
                        })),
                        arms: vec![
                            MatchArm {
                                pattern: sub_pattern.clone(),
                                body: Box::new(body),
                                span: sub_pattern.span(),
                            },
                            MatchArm {
                                pattern: Pattern::Wildcard(sub_pattern.span()),
                                body: Box::new((**catch_all_body_ref).clone()),
                                span: sub_pattern.span(),
                            },
                        ],
                        span: sub_pattern.span(),
                    });
                }
                new_arms.push(MatchArm {
                    pattern: new_pattern,
                    body: Box::new(body),
                    span: arm.span,
                });
            } else {
                new_arms.push(arm.clone());
            }
        }
        if let Some(idx) = catch_all_idx {
            new_arms.push(match_expr.arms[idx].clone());
        }

        let new_match = MatchExpr {
            scrutinee: match_expr.scrutinee.clone(),
            arms: new_arms,
            span: match_expr.span,
        };
        Some(self.gen_expr(&Expr::Match(new_match)))
    }

    /// Merge arms that share a `DataVariant` or `StructVariant` outer
    /// pattern into a single outer arm whose body is a nested `match`
    /// over the field value(s).
    ///
    /// For single-field variants the nested match scrutinee is a bare
    /// ident reference; for multi-field variants we bundle the fresh
    /// idents into a tuple literal and the inner arm patterns become
    /// tuples of the merged arms' sub-patterns — the nested tuple match
    /// then flows through `try_elaborate_tuple_match` (Phase 5a).
    ///
    /// Struct-variant merging uses the first arm's field order as the
    /// canonical layout and reorders each arm's sub-patterns to match.
    /// Rest-pattern (`..`) shares, non-leaf multi-field sub-patterns,
    /// and arms that list different field sets all return `None`.
    fn merge_group_single_field(
        &mut self,
        all_arms: &[MatchArm],
        idxs: &[usize],
        catch_all_body: Option<&Expr>,
    ) -> Option<MatchArm> {
        match &all_arms[idxs[0]].pattern {
            Pattern::DataVariant { .. } => {
                self.merge_group_data_variant(all_arms, idxs, catch_all_body)
            }
            Pattern::StructVariant { .. } => {
                self.merge_group_struct_variant(all_arms, idxs, catch_all_body)
            }
            _ => None,
        }
    }

    /// Data-variant arm merging — see `merge_group_single_field`.
    fn merge_group_data_variant(
        &mut self,
        all_arms: &[MatchArm],
        idxs: &[usize],
        catch_all_body: Option<&Expr>,
    ) -> Option<MatchArm> {
        let template = &all_arms[idxs[0]].pattern;
        let (base, type_name, variant_ident, outer_span, field_count) = match template {
            Pattern::DataVariant {
                base,
                type_name,
                variant,
                span,
                fields,
            } => (base.clone(), *type_name, *variant, *span, fields.len()),
            _ => return None,
        };

        for &i in idxs {
            match &all_arms[i].pattern {
                Pattern::DataVariant { fields, .. } => {
                    if fields.len() != field_count {
                        return None;
                    }
                    for f in fields {
                        match f {
                            TupleElemPattern::Pattern(p) => {
                                // Multi-field merging relies on Phase 5a's
                                // tuple-root elaboration, which only supports
                                // leaf sub-patterns. Single-field variants
                                // bypass tuple construction so any sub-pattern
                                // kind is fine there.
                                if field_count > 1 && !is_leaf_sub_pattern(p) {
                                    return None;
                                }
                            }
                            TupleElemPattern::Rest(_) => return None,
                        }
                    }
                }
                _ => return None,
            }
        }

        // Fresh ident per field position — these replace the refutable
        // sub-patterns in the merged outer arm.
        let fresh_idents: Vec<Spur> = (0..field_count)
            .map(|_| self.fresh_refutable_elab_name())
            .collect();

        // Inner match arms — one per merged arm, preserving order.
        let mut inner_arms: Vec<MatchArm> = Vec::with_capacity(idxs.len() + 1);
        for &i in idxs {
            let arm = &all_arms[i];
            let fields = match &arm.pattern {
                Pattern::DataVariant { fields, .. } => fields,
                _ => unreachable!(),
            };
            let inner_pat = if field_count == 1 {
                match &fields[0] {
                    TupleElemPattern::Pattern(p) => p.clone(),
                    TupleElemPattern::Rest(_) => unreachable!(),
                }
            } else {
                Pattern::Tuple {
                    elems: fields.clone(),
                    span: arm.pattern.span(),
                }
            };
            inner_arms.push(MatchArm {
                pattern: inner_pat,
                body: arm.body.clone(),
                span: arm.span,
            });
        }
        // When the outer match has a catch-all, mirror it as the nested
        // match's wildcard fallback so runtime coverage is preserved.
        // Skip when one of the merged arms is already irrefutable — its
        // nested pattern covers every remaining value of the field
        // type(s), and a trailing wildcard would make the if-chain
        // elaborator trip on unreachable arms.
        //
        // Without a catch-all, the nested match is expected to cover
        // every value of the field type on its own (sema enforces
        // exhaustiveness).
        let any_irrefutable = inner_arms
            .iter()
            .any(|a| is_irrefutable_destructure(&a.pattern));
        if let Some(body) = catch_all_body {
            if !any_irrefutable {
                inner_arms.push(MatchArm {
                    pattern: Pattern::Wildcard(outer_span),
                    body: Box::new(body.clone()),
                    span: outer_span,
                });
            }
        }

        let nested_scrutinee = if field_count == 1 {
            Expr::Ident(AstIdent {
                name: fresh_idents[0],
                span: outer_span,
            })
        } else {
            Expr::Tuple(gruel_parser::ast::TupleExpr {
                elems: fresh_idents
                    .iter()
                    .map(|&n| {
                        Expr::Ident(AstIdent {
                            name: n,
                            span: outer_span,
                        })
                    })
                    .collect(),
                span: outer_span,
            })
        };

        let nested_match = Expr::Match(MatchExpr {
            scrutinee: Box::new(nested_scrutinee),
            arms: inner_arms,
            span: outer_span,
        });

        let outer_fields: Vec<TupleElemPattern> = fresh_idents
            .iter()
            .map(|&n| {
                TupleElemPattern::Pattern(Pattern::Ident {
                    is_mut: false,
                    name: AstIdent {
                        name: n,
                        span: outer_span,
                    },
                    span: outer_span,
                })
            })
            .collect();

        Some(MatchArm {
            pattern: Pattern::DataVariant {
                base,
                type_name,
                variant: variant_ident,
                fields: outer_fields,
                span: outer_span,
            },
            body: Box::new(nested_match),
            span: outer_span,
        })
    }

    /// Struct-variant arm merging — see `merge_group_single_field`.
    /// Uses the first arm's field order as the canonical tuple layout
    /// and looks up every other arm's fields by name to reorder them.
    fn merge_group_struct_variant(
        &mut self,
        all_arms: &[MatchArm],
        idxs: &[usize],
        catch_all_body: Option<&Expr>,
    ) -> Option<MatchArm> {
        let template = &all_arms[idxs[0]].pattern;
        let (base, type_name, variant_ident, outer_span, canonical_names) = match template {
            Pattern::StructVariant {
                base,
                type_name,
                variant,
                span,
                fields,
            } => {
                // Reject merges when the first arm has a `..` rest —
                // proper handling would need to know the variant's full
                // field set, which is only available in sema.
                let mut names: Vec<Spur> = Vec::with_capacity(fields.len());
                for fp in fields {
                    match fp.field_name {
                        Some(ident) => names.push(ident.name),
                        None => return None, // `..` rest — bail
                    }
                }
                (base.clone(), *type_name, *variant, *span, names)
            }
            _ => return None,
        };
        let field_count = canonical_names.len();

        // Validate every arm lists the same field names (any order);
        // multi-field merges additionally require leaf sub-patterns so
        // Phase 5a's tuple elaboration can handle the nested match.
        for &i in idxs {
            match &all_arms[i].pattern {
                Pattern::StructVariant { fields, .. } => {
                    if fields.len() != field_count {
                        return None;
                    }
                    // Check that each canonical name is listed exactly
                    // once (no duplicates, no rest, no unknown names).
                    let mut seen: std::collections::HashSet<Spur> =
                        std::collections::HashSet::new();
                    for fp in fields {
                        let Some(ident) = fp.field_name else {
                            return None;
                        };
                        if !canonical_names.contains(&ident.name) {
                            return None;
                        }
                        if !seen.insert(ident.name) {
                            return None;
                        }
                        if field_count > 1 {
                            let sub_is_leaf = match &fp.sub {
                                None => true,
                                Some(p) => is_leaf_sub_pattern(p),
                            };
                            if !sub_is_leaf {
                                return None;
                            }
                        }
                    }
                }
                _ => return None,
            }
        }

        let fresh_idents: Vec<Spur> = (0..field_count)
            .map(|_| self.fresh_refutable_elab_name())
            .collect();

        // Inner arms: for each original arm, rebuild sub-patterns in
        // canonical order.
        let mut inner_arms: Vec<MatchArm> = Vec::with_capacity(idxs.len() + 1);
        for &i in idxs {
            let arm = &all_arms[i];
            let fields = match &arm.pattern {
                Pattern::StructVariant { fields, .. } => fields,
                _ => unreachable!(),
            };
            let mut ordered_subs: Vec<Pattern> = Vec::with_capacity(field_count);
            for canonical in &canonical_names {
                let fp = fields
                    .iter()
                    .find(|fp| fp.field_name.map(|n| n.name) == Some(*canonical))
                    .expect("validated above");
                let sub = match &fp.sub {
                    None => Pattern::Ident {
                        is_mut: fp.is_mut,
                        name: fp.field_name.expect("validated above; `..` rest rejected"),
                        span: fp.span,
                    },
                    Some(p) => p.clone(),
                };
                ordered_subs.push(sub);
            }
            let inner_pat = if field_count == 1 {
                ordered_subs.into_iter().next().unwrap()
            } else {
                Pattern::Tuple {
                    elems: ordered_subs
                        .into_iter()
                        .map(TupleElemPattern::Pattern)
                        .collect(),
                    span: arm.pattern.span(),
                }
            };
            inner_arms.push(MatchArm {
                pattern: inner_pat,
                body: arm.body.clone(),
                span: arm.span,
            });
        }
        let any_irrefutable = inner_arms
            .iter()
            .any(|a| is_irrefutable_destructure(&a.pattern));
        if let Some(body) = catch_all_body {
            if !any_irrefutable {
                inner_arms.push(MatchArm {
                    pattern: Pattern::Wildcard(outer_span),
                    body: Box::new(body.clone()),
                    span: outer_span,
                });
            }
        }

        let nested_scrutinee = if field_count == 1 {
            Expr::Ident(AstIdent {
                name: fresh_idents[0],
                span: outer_span,
            })
        } else {
            Expr::Tuple(gruel_parser::ast::TupleExpr {
                elems: fresh_idents
                    .iter()
                    .map(|&n| {
                        Expr::Ident(AstIdent {
                            name: n,
                            span: outer_span,
                        })
                    })
                    .collect(),
                span: outer_span,
            })
        };

        let nested_match = Expr::Match(MatchExpr {
            scrutinee: Box::new(nested_scrutinee),
            arms: inner_arms,
            span: outer_span,
        });

        let outer_fields: Vec<FieldPattern> = canonical_names
            .iter()
            .zip(&fresh_idents)
            .map(|(name, &fresh)| FieldPattern {
                field_name: Some(AstIdent {
                    name: *name,
                    span: outer_span,
                }),
                sub: Some(Pattern::Ident {
                    is_mut: false,
                    name: AstIdent {
                        name: fresh,
                        span: outer_span,
                    },
                    span: outer_span,
                }),
                is_mut: false,
                span: outer_span,
            })
            .collect();

        Some(MatchArm {
            pattern: Pattern::StructVariant {
                base,
                type_name,
                variant: variant_ident,
                fields: outer_fields,
                span: outer_span,
            },
            body: Box::new(nested_match),
            span: outer_span,
        })
    }

    /// Walk a match-arm pattern and replace each refutable sub-pattern in
    /// a variant field with a fresh `__refut_N` ident binding, appending
    /// `(fresh_name, sub_pattern)` to `subs` for body elaboration
    /// (ADR-0049 Phase 5b).
    fn replace_refutable_nested_subs(
        &mut self,
        pat: &Pattern,
        subs: &mut Vec<(Spur, Pattern)>,
    ) -> Pattern {
        match pat {
            Pattern::DataVariant {
                base,
                type_name,
                variant,
                fields,
                span,
            } => {
                let new_fields = fields
                    .iter()
                    .map(|e| match e {
                        TupleElemPattern::Pattern(sub) if is_refutable_variant_sub(sub) => {
                            let fresh = self.fresh_refutable_elab_name();
                            subs.push((fresh, sub.clone()));
                            TupleElemPattern::Pattern(Pattern::Ident {
                                is_mut: false,
                                name: AstIdent {
                                    name: fresh,
                                    span: sub.span(),
                                },
                                span: sub.span(),
                            })
                        }
                        _ => e.clone(),
                    })
                    .collect();
                Pattern::DataVariant {
                    base: base.clone(),
                    type_name: *type_name,
                    variant: *variant,
                    fields: new_fields,
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
                let new_fields = fields
                    .iter()
                    .map(|fp| match &fp.sub {
                        Some(sub) if is_refutable_variant_sub(sub) => {
                            let fresh = self.fresh_refutable_elab_name();
                            subs.push((fresh, sub.clone()));
                            FieldPattern {
                                field_name: fp.field_name,
                                sub: Some(Pattern::Ident {
                                    is_mut: false,
                                    name: AstIdent {
                                        name: fresh,
                                        span: sub.span(),
                                    },
                                    span: sub.span(),
                                }),
                                is_mut: fp.is_mut,
                                span: fp.span,
                            }
                        }
                        _ => fp.clone(),
                    })
                    .collect();
                Pattern::StructVariant {
                    base: base.clone(),
                    type_name: *type_name,
                    variant: *variant,
                    fields: new_fields,
                    span: *span,
                }
            }
            _ => pat.clone(),
        }
    }

    fn fresh_refutable_elab_name(&mut self) -> Spur {
        let n = self.nested_pat_counter;
        self.nested_pat_counter += 1;
        self.interner.get_or_intern(format!("__refut_{}", n))
    }

    /// Elaborate a match expression that has any tuple-root arm into a
    /// let-bound scrutinee plus an if/else chain on tuple projections
    /// (ADR-0049 Phase 5a).
    ///
    /// Each arm becomes `(predicate, body_with_bindings)`:
    /// - wildcard/ident tuple elements produce no predicate (idents bind via
    ///   a prepended let);
    /// - literal tuple elements produce an equality check against the
    ///   projected field.
    ///
    /// The chain falls through to the last arm's body when no earlier arm
    /// matches. If the last arm itself has a predicate (non-exhaustive
    /// match), a `@panic("non-exhaustive match")` is emitted as the final
    /// else to keep the if-chain well-typed; sema will ideally catch
    /// non-exhaustive cases separately, but this guarantees the lowering
    /// is always sound.
    fn try_elaborate_tuple_match(
        &mut self,
        match_expr: &gruel_parser::MatchExpr,
    ) -> Option<InstRef> {
        if !match_expr
            .arms
            .iter()
            .any(|arm| matches!(&arm.pattern, Pattern::Tuple { .. }))
        {
            return None;
        }

        // Non-exhaustive tuple matches get a `@panic("non-exhaustive
        // match")` at the tail of the if-chain so the chain stays
        // well-typed and runtime-traps on uncovered values.
        let last_arm = match_expr
            .arms
            .last()
            .expect("parser rejects empty match arms");
        let last_is_catch_all = is_tuple_match_arm_unconditional(&last_arm.pattern);

        let match_span = match_expr.span;

        // Bind the scrutinee to a fresh synthetic local that each arm can read
        // without re-evaluating side effects.
        let scr_name = self.fresh_match_scr_name();
        let scr_val = self.gen_expr(&match_expr.scrutinee);
        let scr_alloc = self.rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start: 0,
                directives_len: 0,
                name: Some(scr_name),
                is_mut: false,
                ty: None,
                init: scr_val,
            },
            span: match_span,
        });

        // Build (predicate, body) for every arm.
        let mut arm_parts: Vec<(Option<InstRef>, InstRef)> =
            Vec::with_capacity(match_expr.arms.len());
        for arm in &match_expr.arms {
            let part = self.gen_tuple_match_arm(&arm.pattern, scr_name, &arm.body, arm.span);
            arm_parts.push(part);
        }

        // Fold the arms into an if/else chain, back to front. When the
        // last arm is unconditional it becomes the terminating
        // else-branch directly; otherwise we emit a `@panic` fallback
        // and wrap the last arm in a Branch that uses it.
        let mut iter = arm_parts.into_iter().rev();
        let (last_pred, last_body) = iter
            .next()
            .expect("match_expr.arms is non-empty (parser rejects empty match)");
        let mut result = if last_is_catch_all {
            debug_assert!(
                last_pred.is_none(),
                "last-arm unconditional check implies no predicate"
            );
            last_body
        } else {
            let cond = last_pred.expect("non-catch-all last arm produces a predicate");
            let fallback = self.emit_panic_call("non-exhaustive match", match_span);
            self.rir.add_inst(Inst {
                data: InstData::Branch {
                    cond,
                    then_block: last_body,
                    else_block: Some(fallback),
                },
                span: match_span,
            })
        };

        for (pred, body) in iter {
            let cond = pred.expect(
                "earlier unconditional arm → subsequent arms are unreachable; \
                 this is caught by AST pattern-validator, so we shouldn't reach here",
            );
            result = self.rir.add_inst(Inst {
                data: InstData::Branch {
                    cond,
                    then_block: body,
                    else_block: Some(result),
                },
                span: match_span,
            });
        }

        // Wrap the scrutinee alloc + final if-chain in a Block.
        let extra_start = self.rir.add_extra(&[scr_alloc.as_u32(), result.as_u32()]);
        Some(self.rir.add_inst(Inst {
            data: InstData::Block {
                extra_start,
                len: 2,
            },
            span: match_span,
        }))
    }

    /// Generate `(predicate, body)` for a single arm of a tuple-root match
    /// (ADR-0049 Phase 5a). The predicate is `None` when the pattern always
    /// matches (wildcard, ident binding, or a tuple of all-irrefutable
    /// leaves); bindings are prepended to the body as a Block.
    fn gen_tuple_match_arm(
        &mut self,
        pattern: &Pattern,
        scr_name: Spur,
        body: &gruel_parser::Expr,
        arm_span: gruel_span::Span,
    ) -> (Option<InstRef>, InstRef) {
        match pattern {
            Pattern::Wildcard(_) => (None, self.gen_expr(body)),
            Pattern::Ident { is_mut, name, span } => {
                // `name => body` binds the whole scrutinee to `name`.
                let scr_ref = self.rir.add_inst(Inst {
                    data: InstData::VarRef { name: scr_name },
                    span: *span,
                });
                let alloc = self.rir.add_inst(Inst {
                    data: InstData::Alloc {
                        directives_start: 0,
                        directives_len: 0,
                        name: Some(name.name),
                        is_mut: *is_mut,
                        ty: None,
                        init: scr_ref,
                    },
                    span: *span,
                });
                let body_inst = self.gen_expr(body);
                let extra_start = self.rir.add_extra(&[alloc.as_u32(), body_inst.as_u32()]);
                let block = self.rir.add_inst(Inst {
                    data: InstData::Block {
                        extra_start,
                        len: 2,
                    },
                    span: arm_span,
                });
                (None, block)
            }
            Pattern::Tuple { elems, span } => {
                let mut predicates: Vec<InstRef> = Vec::new();
                let mut bindings: Vec<u32> = Vec::new();
                let last_index = elems.len().saturating_sub(1);
                for (i, elem) in elems.iter().enumerate() {
                    // A trailing `..` matches the remaining positions with
                    // no predicate and no binding — the scrutinee alloc
                    // still owns those tuple fields, so they drop at scope
                    // exit (ADR-0049 Phase 6).
                    if let TupleElemPattern::Rest(_) = elem {
                        if i != last_index {
                            panic!(
                                "rest pattern `..` in tuple-root match must be at the end (ADR-0049 Phase 6); got element {}",
                                i
                            );
                        }
                        continue;
                    }
                    let field_name = self.interner.get_or_intern(i.to_string());
                    let scr_ref = self.rir.add_inst(Inst {
                        data: InstData::VarRef { name: scr_name },
                        span: *span,
                    });
                    let field_get = self.rir.add_inst(Inst {
                        data: InstData::FieldGet {
                            base: scr_ref,
                            field: field_name,
                        },
                        span: *span,
                    });
                    match elem {
                        TupleElemPattern::Pattern(Pattern::Wildcard(_)) => {
                            // Field dropped; scrutinee alloc owns the tuple
                            // and its destructor will release everything.
                        }
                        TupleElemPattern::Pattern(Pattern::Ident {
                            is_mut,
                            name,
                            span: id_span,
                        }) => {
                            let alloc = self.rir.add_inst(Inst {
                                data: InstData::Alloc {
                                    directives_start: 0,
                                    directives_len: 0,
                                    name: Some(name.name),
                                    is_mut: *is_mut,
                                    ty: None,
                                    init: field_get,
                                },
                                span: *id_span,
                            });
                            bindings.push(alloc.as_u32());
                        }
                        TupleElemPattern::Pattern(Pattern::Int(lit)) => {
                            let lit_const = self.rir.add_inst(Inst {
                                data: InstData::IntConst(lit.value),
                                span: lit.span,
                            });
                            let eq = self.rir.add_inst(Inst {
                                data: InstData::Eq {
                                    lhs: field_get,
                                    rhs: lit_const,
                                },
                                span: lit.span,
                            });
                            predicates.push(eq);
                        }
                        TupleElemPattern::Pattern(Pattern::NegInt(lit)) => {
                            let abs_const = self.rir.add_inst(Inst {
                                data: InstData::IntConst(lit.value),
                                span: lit.span,
                            });
                            let neg_const = self.rir.add_inst(Inst {
                                data: InstData::Neg { operand: abs_const },
                                span: lit.span,
                            });
                            let eq = self.rir.add_inst(Inst {
                                data: InstData::Eq {
                                    lhs: field_get,
                                    rhs: neg_const,
                                },
                                span: lit.span,
                            });
                            predicates.push(eq);
                        }
                        TupleElemPattern::Pattern(Pattern::Bool(lit)) => {
                            let lit_const = self.rir.add_inst(Inst {
                                data: InstData::BoolConst(lit.value),
                                span: lit.span,
                            });
                            let eq = self.rir.add_inst(Inst {
                                data: InstData::Eq {
                                    lhs: field_get,
                                    rhs: lit_const,
                                },
                                span: lit.span,
                            });
                            predicates.push(eq);
                        }
                        TupleElemPattern::Pattern(other) => panic!(
                            "tuple element shape {:?} in match arm not yet supported (ADR-0049 Phase 5b)",
                            other
                        ),
                        TupleElemPattern::Rest(_) => unreachable!("handled at top of loop"),
                    }
                }

                let predicate = match predicates.len() {
                    0 => None,
                    _ => {
                        let mut iter = predicates.into_iter();
                        let mut acc = iter.next().unwrap();
                        for p in iter {
                            acc = self.rir.add_inst(Inst {
                                data: InstData::And { lhs: acc, rhs: p },
                                span: *span,
                            });
                        }
                        Some(acc)
                    }
                };

                let body_inst = self.gen_expr(body);
                let body_block = if bindings.is_empty() {
                    body_inst
                } else {
                    bindings.push(body_inst.as_u32());
                    let extra_start = self.rir.add_extra(&bindings);
                    let len = bindings.len() as u32;
                    self.rir.add_inst(Inst {
                        data: InstData::Block { extra_start, len },
                        span: arm_span,
                    })
                };

                (predicate, body_block)
            }
            other => panic!(
                "top-level {:?} in a multi-arm tuple-root match is not yet supported (ADR-0049)",
                other
            ),
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
            // Top-level Struct/Tuple/Ident patterns are elaborated in
            // `gen_expr` via `try_elaborate_irrefutable_match` before reaching
            // this lowering, so a multi-arm match with one of these at the
            // top is an unsupported shape (would need recursive CFG dispatch
            // — Phase 5). The `nested` parameter is unused on this branch.
            Pattern::Struct { .. } | Pattern::Tuple { .. } | Pattern::Ident { .. } => {
                let _ = nested;
                panic!(
                    "top-level {:?} pattern in a multi-arm match is not yet supported (ADR-0049 Phase 5)",
                    pattern
                );
            }
        }
    }

    /// Convert a match-arm tuple-element sub-pattern into the RIR binding shape
    /// used by `DataVariant`. Leaves (Wildcard, Ident) convert directly; irrefutable
    /// nested destructures (Struct, Tuple) are captured via a synthetic binding
    /// whose name is recorded in `nested` for body elaboration (ADR-0049 Phase 4b).
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
            },
            TupleElemPattern::Pattern(Pattern::Ident { is_mut, name, .. }) => RirPatternBinding {
                is_wildcard: false,
                is_mut: *is_mut,
                name: Some(name.name),
            },
            TupleElemPattern::Pattern(sub @ (Pattern::Struct { .. } | Pattern::Tuple { .. })) => {
                let fresh = self.fresh_nested_pat_name();
                nested.push((fresh, sub.clone()));
                RirPatternBinding {
                    is_wildcard: false,
                    is_mut: false,
                    name: Some(fresh),
                }
            }
            TupleElemPattern::Pattern(other) => panic!(
                "refutable nested sub-patterns in match arms not yet supported (ADR-0049 Phase 5); got {:?}",
                other
            ),
            TupleElemPattern::Rest(_) => {
                // `..` in a data-variant pattern: emit a marker binding
                // with the sentinel name `..`. Sema detects this marker
                // and expands it to wildcards for the remaining variant
                // fields (ADR-0049 Phase 6).
                RirPatternBinding {
                    is_wildcard: true,
                    is_mut: false,
                    name: Some(self.interner.get_or_intern_static("..")),
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
            };
        };
        match &fb.sub {
            None => RirPatternBinding {
                // Shorthand: `field` binds a local named `field`.
                is_wildcard: false,
                is_mut: fb.is_mut,
                name: Some(name.name),
            },
            Some(Pattern::Wildcard(_)) => RirPatternBinding {
                is_wildcard: true,
                is_mut: false,
                name: None,
            },
            Some(Pattern::Ident {
                is_mut,
                name: bind_name,
                ..
            }) => RirPatternBinding {
                is_wildcard: false,
                is_mut: fb.is_mut || *is_mut,
                name: Some(bind_name.name),
            },
            Some(sub @ (Pattern::Struct { .. } | Pattern::Tuple { .. })) => {
                let fresh = self.fresh_nested_pat_name();
                nested.push((fresh, sub.clone()));
                RirPatternBinding {
                    is_wildcard: false,
                    is_mut: false,
                    name: Some(fresh),
                }
            }
            Some(other) => panic!(
                "refutable nested sub-patterns in match arms not yet supported (ADR-0049 Phase 5); got {:?}",
                other
            ),
        }
    }

    /// Wrap a match-arm body with let-destructure statements for each nested
    /// sub-pattern captured during pattern lowering (ADR-0049 Phase 4b).
    /// Each `(fresh_name, sub_pattern)` pair lowers to one or more RIR
    /// StructDestructure instructions consuming a `VarRef` to `fresh_name`.
    /// The original body becomes the block's value.
    fn wrap_match_arm_body_with_destructures(
        &mut self,
        body: InstRef,
        nested: Vec<(Spur, Pattern)>,
        arm_span: gruel_span::Span,
    ) -> InstRef {
        let mut stmts: Vec<u32> = Vec::new();
        for (fresh_name, sub_pattern) in nested {
            let var_ref = self.rir.add_inst(Inst {
                data: InstData::VarRef { name: fresh_name },
                span: sub_pattern.span(),
            });
            let mut emitted = Vec::new();
            self.emit_let_destructure_into(&sub_pattern, var_ref, sub_pattern.span(), &mut emitted);
            for r in emitted {
                stmts.push(r.as_u32());
            }
        }
        stmts.push(body.as_u32());
        let extra_start = self.rir.add_extra(&stmts);
        let len = stmts.len() as u32;
        self.rir.add_inst(Inst {
            data: InstData::Block { extra_start, len },
            span: arm_span,
        })
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
        if let Statement::Let(let_stmt) = stmt {
            if matches!(
                &let_stmt.pattern,
                Pattern::Struct { .. } | Pattern::Tuple { .. }
            ) {
                let init = self.gen_expr(&let_stmt.init);
                let mut emitted = Vec::new();
                self.emit_let_destructure_into(
                    &let_stmt.pattern,
                    init,
                    let_stmt.span,
                    &mut emitted,
                );
                for r in emitted {
                    out.push(r.as_u32());
                }
                return;
            }
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

/// Whether a variant sub-pattern is a leaf that Phase 5a's tuple-root
/// match elaborator can handle as a tuple element (ADR-0049 Phase 5b).
/// Used when merging multi-field variant arms: the merged nested match
/// dispatches on a tuple of the variant's fields, and Phase 5a only
/// supports leaf elements (`Wildcard`, `Ident`, `Int`, `NegInt`, `Bool`).
fn is_leaf_sub_pattern(pat: &Pattern) -> bool {
    matches!(
        pat,
        Pattern::Wildcard(_)
            | Pattern::Ident { .. }
            | Pattern::Int(_)
            | Pattern::NegInt(_)
            | Pattern::Bool(_)
    )
}

/// Extract the `(type_name, variant_name)` identity of a variant pattern,
/// used by the refutable-nested elaborator to detect when multiple arms
/// share an outer variant (ADR-0049 Phase 5b).
fn outer_variant_key(pat: &Pattern) -> Option<(Spur, Spur)> {
    match pat {
        Pattern::DataVariant {
            type_name, variant, ..
        }
        | Pattern::StructVariant {
            type_name, variant, ..
        } => Some((type_name.name, variant.name)),
        _ => None,
    }
}

/// Whether a variant field's sub-pattern is "refutable nested" — i.e. it
/// would need to be checked with cascading dispatch rather than a plain
/// field extraction. Leaves (Wildcard, Ident) and irrefutable destructures
/// (Struct, Tuple) are not refutable nested; everything else is
/// (ADR-0049 Phase 5b).
fn is_refutable_variant_sub(sub: &Pattern) -> bool {
    match sub {
        Pattern::Wildcard(_) | Pattern::Ident { .. } => false,
        Pattern::Struct { .. } | Pattern::Tuple { .. } => false,
        _ => true,
    }
}

/// Whether an arm pattern has any refutable nested sub-pattern in a
/// variant-field position. Used to decide whether
/// `try_elaborate_refutable_nested_match` applies (ADR-0049 Phase 5b).
fn pattern_has_refutable_nested_sub(pat: &Pattern) -> bool {
    match pat {
        Pattern::DataVariant { fields, .. } => fields.iter().any(|e| match e {
            TupleElemPattern::Pattern(sub) => is_refutable_variant_sub(sub),
            TupleElemPattern::Rest(_) => false,
        }),
        Pattern::StructVariant { fields, .. } => fields.iter().any(|fp| match &fp.sub {
            Some(sub) => is_refutable_variant_sub(sub),
            None => false,
        }),
        _ => false,
    }
}

/// Whether a top-level tuple-match arm pattern is unconditional — i.e. it
/// matches every possible scrutinee value, so no if-condition is needed.
/// Wildcard, Ident, or a tuple of all-irrefutable leaves qualify.
fn is_tuple_match_arm_unconditional(pat: &Pattern) -> bool {
    match pat {
        Pattern::Wildcard(_) | Pattern::Ident { .. } => true,
        Pattern::Tuple { elems, .. } => elems.iter().all(|e| match e {
            TupleElemPattern::Pattern(p) => is_irrefutable_destructure(p),
            TupleElemPattern::Rest(_) => true,
        }),
        _ => false,
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
}
