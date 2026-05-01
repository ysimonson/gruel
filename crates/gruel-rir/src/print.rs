//! Pretty-printer for RIR.
//!
//! Renders a full RIR program (instructions, patterns, function spans,
//! interface decls) in a human-readable, indented format. Used by the
//! `--emit rir` CLI flag and by RIR golden tests.

use std::fmt;

use crate::inst::{InstData, Rir, RirArgMode, RirCallArg, RirParamMode, RirPattern};

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b lasso::ThreadedRodeo,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b lasso::ThreadedRodeo) -> Self {
        Self { rir, interner }
    }

    /// Format a call argument with its mode prefix.
    fn format_call_arg(arg: &RirCallArg) -> String {
        match arg.mode {
            RirArgMode::Inout => format!("inout {}", arg.value),
            RirArgMode::Borrow => format!("borrow {}", arg.value),
            RirArgMode::Normal => format!("{}", arg.value),
        }
    }

    /// Format a list of call arguments.
    fn format_call_args(args: &[RirCallArg]) -> String {
        args.iter()
            .map(Self::format_call_arg)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Format a pattern for printing.
    fn format_pattern(&self, pat: &RirPattern) -> String {
        match pat {
            RirPattern::Wildcard(_) => "_".to_string(),
            RirPattern::Int(n, _) => n.to_string(),
            RirPattern::Bool(b, _) => b.to_string(),
            RirPattern::Path {
                module,
                type_name,
                variant,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("%{}..", module_ref.as_u32())
                } else {
                    String::new()
                };
                format!(
                    "{}{}::{}",
                    prefix,
                    self.interner.resolve(type_name),
                    self.interner.resolve(variant)
                )
            }
            RirPattern::DataVariant {
                module,
                type_name,
                variant,
                bindings,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("%{}..", module_ref.as_u32())
                } else {
                    String::new()
                };
                let binding_strs: Vec<String> = bindings
                    .iter()
                    .map(|b| {
                        if b.is_wildcard {
                            "_".to_string()
                        } else {
                            let name = b
                                .name
                                .map(|s| self.interner.resolve(&s).to_string())
                                .unwrap_or_else(|| "_".to_string());
                            if b.is_mut {
                                format!("mut {}", name)
                            } else {
                                name
                            }
                        }
                    })
                    .collect();
                format!(
                    "{}{}::{}({})",
                    prefix,
                    self.interner.resolve(type_name),
                    self.interner.resolve(variant),
                    binding_strs.join(", ")
                )
            }
            RirPattern::StructVariant {
                module,
                type_name,
                variant,
                field_bindings,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("%{}..", module_ref.as_u32())
                } else {
                    String::new()
                };
                let field_strs: Vec<String> = field_bindings
                    .iter()
                    .map(|fb| {
                        let field = self.interner.resolve(&fb.field_name);
                        if fb.binding.is_wildcard {
                            format!("{}: _", field)
                        } else {
                            let name = fb
                                .binding
                                .name
                                .map(|s| self.interner.resolve(&s).to_string())
                                .unwrap_or_else(|| "_".to_string());
                            if fb.binding.is_mut {
                                format!("{}: mut {}", field, name)
                            } else if name == field {
                                field.to_string()
                            } else {
                                format!("{}: {}", field, name)
                            }
                        }
                    })
                    .collect();
                format!(
                    "{}{}::{} {{ {} }}",
                    prefix,
                    self.interner.resolve(type_name),
                    self.interner.resolve(variant),
                    field_strs.join(", ")
                )
            }
            RirPattern::Ident { name, is_mut, .. } => {
                let n = self.interner.resolve(name);
                if *is_mut {
                    format!("mut {}", n)
                } else {
                    n.to_string()
                }
            }
            RirPattern::Tuple { elems, .. } => {
                let parts: Vec<String> = elems.iter().map(|e| self.format_pattern(e)).collect();
                format!("({})", parts.join(", "))
            }
            RirPattern::Struct {
                module,
                type_name,
                fields,
                has_rest,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("%{}..", module_ref.as_u32())
                } else {
                    String::new()
                };
                let mut parts: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{}: {}",
                            self.interner.resolve(&f.field_name),
                            self.format_pattern(&f.pattern)
                        )
                    })
                    .collect();
                if *has_rest {
                    parts.push("..".to_string());
                }
                format!(
                    "{}{} {{ {} }}",
                    prefix,
                    self.interner.resolve(type_name),
                    parts.join(", ")
                )
            }
        }
    }

    /// Format the RIR as a string.
    pub fn render(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        for (inst_ref, inst) in self.rir.iter() {
            write!(out, "{} = ", inst_ref).unwrap();
            match &inst.data {
                // Constants
                InstData::IntConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::FloatConst(bits) => {
                    writeln!(out, "const {}", f64::from_bits(*bits)).unwrap()
                }
                InstData::BoolConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::StringConst(s) => {
                    writeln!(out, "const {:?}", self.interner.resolve(s)).unwrap()
                }
                InstData::UnitConst => writeln!(out, "const ()").unwrap(),

                InstData::Bin { op, lhs, rhs } => writeln!(out, "{} {}, {}", op, lhs, rhs).unwrap(),
                InstData::Unary { op, operand } => writeln!(out, "{} {}", op, operand).unwrap(),
                InstData::MakeRef { operand, is_mut } => writeln!(
                    out,
                    "make_ref{} {}",
                    if *is_mut { "_mut" } else { "" },
                    operand
                )
                .unwrap(),
                InstData::BareRangeSubscript => writeln!(out, "bare_range_subscript").unwrap(),
                InstData::MakeSlice {
                    base,
                    lo,
                    hi,
                    is_mut,
                } => {
                    write!(
                        out,
                        "make_slice{} {}",
                        if *is_mut { "_mut" } else { "" },
                        base
                    )
                    .unwrap();
                    if let Some(lo) = lo {
                        write!(out, ", lo={}", lo).unwrap();
                    }
                    if let Some(hi) = hi {
                        write!(out, ", hi={}", hi).unwrap();
                    }
                    writeln!(out).unwrap();
                }

                // Control flow
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    if let Some(else_b) = else_block {
                        writeln!(out, "branch {}, {}, {}", cond, then_block, else_b).unwrap();
                    } else {
                        writeln!(out, "branch {}, {}", cond, then_block).unwrap();
                    }
                }
                InstData::Loop { cond, body } => writeln!(out, "loop {}, {}", cond, body).unwrap(),
                InstData::For {
                    binding,
                    is_mut,
                    iterable,
                    body,
                } => {
                    let mut_str = if *is_mut { "mut " } else { "" };
                    writeln!(
                        out,
                        "for {}{} in {}, {}",
                        mut_str,
                        self.interner.resolve(binding),
                        iterable,
                        body
                    )
                    .unwrap()
                }
                InstData::InfiniteLoop { body } => writeln!(out, "infinite_loop {}", body).unwrap(),
                InstData::Match {
                    scrutinee,
                    arms_start,
                    arms_len,
                } => {
                    let arms = self.rir.get_match_arms(*arms_start, *arms_len);
                    let arms_str: Vec<String> = arms
                        .iter()
                        .map(|(pat, body)| format!("{} => {}", self.format_pattern(pat), body))
                        .collect();
                    writeln!(out, "match {} {{ {} }}", scrutinee, arms_str.join(", ")).unwrap();
                }
                InstData::Break => writeln!(out, "break").unwrap(),
                InstData::Continue => writeln!(out, "continue").unwrap(),

                // Functions
                InstData::FnDecl {
                    directives_start: _,
                    directives_len: _,
                    is_pub,
                    is_unchecked,
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    receiver_mode: _,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let unchecked_str = if *is_unchecked { "unchecked " } else { "" };
                    let name_str = self.interner.resolve(name);
                    let ret_str = self.interner.resolve(return_type);
                    let self_str = if *has_self { "self, " } else { "" };
                    let params = self.rir.get_params(*params_start, *params_len);
                    let params_str: Vec<String> = params
                        .iter()
                        .map(|p| {
                            let mode_prefix = match p.mode {
                                RirParamMode::Inout => "inout ",
                                RirParamMode::Borrow => "borrow ",
                                RirParamMode::Comptime => "comptime ",
                                RirParamMode::Normal => "",
                            };
                            format!(
                                "{}{}: {}",
                                mode_prefix,
                                self.interner.resolve(&p.name),
                                self.interner.resolve(&p.ty)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "{}{}fn {}({}{}) -> {} {{",
                        pub_str,
                        unchecked_str,
                        name_str,
                        self_str,
                        params_str.join(", "),
                        ret_str
                    )
                    .unwrap();
                    writeln!(out, "    {}", body).unwrap();
                    writeln!(out, "}}").unwrap();
                }
                InstData::ConstDecl {
                    directives_start: _,
                    directives_len: _,
                    is_pub,
                    name,
                    ty,
                    init,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(name);
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.resolve(&t)))
                        .unwrap_or_default();
                    writeln!(out, "{}const {}{} = {}", pub_str, name_str, ty_str, init).unwrap();
                }
                InstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(out, "ret {}", inner).unwrap();
                    } else {
                        writeln!(out, "ret").unwrap();
                    }
                }
                InstData::Call {
                    name,
                    args_start,
                    args_len,
                } => {
                    let name_str = self.interner.resolve(name);
                    let args = self.rir.get_call_args(*args_start, *args_len);
                    writeln!(out, "call {}({})", name_str, Self::format_call_args(&args)).unwrap();
                }
                InstData::Intrinsic {
                    name,
                    args_start,
                    args_len,
                } => {
                    let name_str = self.interner.resolve(name);
                    let args = self.rir.get_inst_refs(*args_start, *args_len);
                    let args_str: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
                    writeln!(out, "intrinsic @{}({})", name_str, args_str.join(", ")).unwrap();
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    let name_str = self.interner.resolve(name);
                    let type_str = self.interner.resolve(type_arg);
                    writeln!(out, "type_intrinsic @{}({})", name_str, type_str).unwrap();
                }
                InstData::TypeInterfaceIntrinsic {
                    name,
                    type_arg,
                    interface_arg,
                } => {
                    let name_str = self.interner.resolve(name);
                    let type_str = self.interner.resolve(type_arg);
                    let iface_str = self.interner.resolve(interface_arg);
                    writeln!(
                        out,
                        "type_intrinsic @{}({}, {})",
                        name_str, type_str, iface_str
                    )
                    .unwrap();
                }
                InstData::ParamRef { index, name } => {
                    writeln!(out, "param {} ({})", index, self.interner.resolve(name)).unwrap();
                }
                InstData::Block { extra_start, len } => {
                    writeln!(out, "block({}, {})", extra_start, len).unwrap();
                }

                // Variables
                InstData::Alloc {
                    directives_start: _,
                    directives_len: _,
                    name,
                    is_mut,
                    ty,
                    init,
                } => {
                    let name_str = name
                        .map(|n| self.interner.resolve(&n).to_string())
                        .unwrap_or_else(|| "_".to_string());
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.resolve(&t)))
                        .unwrap_or_default();
                    writeln!(out, "alloc {}{}{}= {}", mut_str, name_str, ty_str, init).unwrap();
                }
                InstData::StructDestructure {
                    type_name,
                    fields_start,
                    fields_len,
                    init,
                } => {
                    let type_str = self.interner.resolve(type_name);
                    let fields = self.rir.get_destructure_fields(*fields_start, *fields_len);
                    let field_strs: Vec<String> = fields
                        .iter()
                        .map(|f| {
                            let name = self.interner.resolve(&f.field_name);
                            if f.is_wildcard {
                                format!("{}: _", name)
                            } else if let Some(binding) = f.binding_name {
                                let binding_str = self.interner.resolve(&binding);
                                format!("{}: {}", name, binding_str)
                            } else {
                                name.to_string()
                            }
                        })
                        .collect();
                    writeln!(
                        out,
                        "destructure {} {{ {} }} = {}",
                        type_str,
                        field_strs.join(", "),
                        init
                    )
                    .unwrap();
                }
                InstData::VarRef { name } => {
                    writeln!(out, "var_ref {}", self.interner.resolve(name)).unwrap();
                }
                InstData::Assign { name, value } => {
                    writeln!(out, "assign {} = {}", self.interner.resolve(name), value).unwrap();
                }

                // Structs
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    is_pub,
                    is_linear,
                    name,
                    fields_start,
                    fields_len,
                    methods_start,
                    methods_len,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(name);
                    let fields = self.rir.get_field_decls(*fields_start, *fields_len);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, ftype)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(fname),
                                self.interner.resolve(ftype)
                            )
                        })
                        .collect();
                    let directives = self.rir.get_directives(*directives_start, *directives_len);
                    let linear_str = if *is_linear { "linear " } else { "" };
                    let directives_str = if directives.is_empty() {
                        String::new()
                    } else {
                        let dir_names: Vec<String> = directives
                            .iter()
                            .map(|d| format!("@{}", self.interner.resolve(&d.name)))
                            .collect();
                        format!("{} ", dir_names.join(" "))
                    };
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let methods_str = if methods.is_empty() {
                        String::new()
                    } else {
                        let method_refs: Vec<String> =
                            methods.iter().map(|m| format!("{}", m)).collect();
                        format!(" methods: [{}]", method_refs.join(", "))
                    };
                    writeln!(
                        out,
                        "{}{}{}struct {} {{ {} }}{}",
                        directives_str,
                        pub_str,
                        linear_str,
                        name_str,
                        fields_str.join(", "),
                        methods_str
                    )
                    .unwrap();
                }
                InstData::StructInit {
                    module,
                    type_name,
                    fields_start,
                    fields_len,
                } => {
                    let module_str = module.map(|m| format!("{}.", m)).unwrap_or_default();
                    let type_str = self.interner.resolve(type_name);
                    let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, value)| {
                            format!("{}: {}", self.interner.resolve(fname), value)
                        })
                        .collect();
                    writeln!(
                        out,
                        "struct_init {}{} {{ {} }}",
                        module_str,
                        type_str,
                        fields_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::FieldGet { base, field } => {
                    writeln!(out, "field_get {}.{}", base, self.interner.resolve(field)).unwrap();
                }
                InstData::FieldSet { base, field, value } => {
                    writeln!(
                        out,
                        "field_set {}.{} = {}",
                        base,
                        self.interner.resolve(field),
                        value
                    )
                    .unwrap();
                }

                // Enums
                InstData::EnumDecl {
                    is_pub,
                    name,
                    variants_start,
                    variants_len,
                    methods_start,
                    methods_len,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(name);
                    let variants = self
                        .rir
                        .get_enum_variant_decls(*variants_start, *variants_len);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .map(|(v, field_types, field_names)| {
                            let vname = self.interner.resolve(v).to_string();
                            if field_types.is_empty() {
                                vname
                            } else if field_names.is_empty() {
                                // Tuple variant
                                let field_strs: Vec<&str> = field_types
                                    .iter()
                                    .map(|f| self.interner.resolve(f))
                                    .collect();
                                format!("{}({})", vname, field_strs.join(", "))
                            } else {
                                // Struct variant
                                let field_strs: Vec<String> = field_names
                                    .iter()
                                    .zip(field_types.iter())
                                    .map(|(n, t)| {
                                        format!(
                                            "{}: {}",
                                            self.interner.resolve(n),
                                            self.interner.resolve(t)
                                        )
                                    })
                                    .collect();
                                format!("{} {{ {} }}", vname, field_strs.join(", "))
                            }
                        })
                        .collect();
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let methods_str: Vec<String> =
                        methods.iter().map(|m| format!("fn {}", m)).collect();
                    let body_parts: Vec<String> = variants_str
                        .into_iter()
                        .chain(methods_str.into_iter())
                        .collect();
                    writeln!(
                        out,
                        "{}enum {} {{ {} }}",
                        pub_str,
                        name_str,
                        body_parts.join(", ")
                    )
                    .unwrap();
                }
                InstData::EnumVariant {
                    module,
                    type_name,
                    variant,
                } => {
                    let module_str = module.map(|m| format!("{}.", m)).unwrap_or_default();
                    writeln!(
                        out,
                        "enum_variant {}{}::{}",
                        module_str,
                        self.interner.resolve(type_name),
                        self.interner.resolve(variant)
                    )
                    .unwrap();
                }
                InstData::EnumStructVariant {
                    module,
                    type_name,
                    variant,
                    fields_start,
                    fields_len,
                } => {
                    let module_str = module.map(|m| format!("{}.", m)).unwrap_or_default();
                    let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                    let field_strs: Vec<String> = fields
                        .iter()
                        .map(|(name, value)| format!("{}: {}", self.interner.resolve(name), value))
                        .collect();
                    writeln!(
                        out,
                        "enum_struct_variant {}{}::{} {{ {} }}",
                        module_str,
                        self.interner.resolve(type_name),
                        self.interner.resolve(variant),
                        field_strs.join(", ")
                    )
                    .unwrap();
                }

                // Arrays
                InstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => {
                    let elements = self.rir.get_inst_refs(*elems_start, *elems_len);
                    let elems_str: Vec<String> =
                        elements.iter().map(|e| format!("{}", e)).collect();
                    writeln!(out, "array_init [{}]", elems_str.join(", ")).unwrap();
                }
                InstData::IndexGet { base, index } => {
                    writeln!(out, "index_get {}[{}]", base, index).unwrap();
                }
                InstData::IndexSet { base, index, value } => {
                    writeln!(out, "index_set {}[{}] = {}", base, index, value).unwrap();
                }

                // Methods
                InstData::MethodCall {
                    receiver,
                    method,
                    args_start,
                    args_len,
                } => {
                    let args = self.rir.get_call_args(*args_start, *args_len);
                    writeln!(
                        out,
                        "method_call {}.{}({})",
                        receiver,
                        self.interner.resolve(method),
                        Self::format_call_args(&args)
                    )
                    .unwrap();
                }
                InstData::AssocFnCall {
                    type_name,
                    function,
                    args_start,
                    args_len,
                } => {
                    let args = self.rir.get_call_args(*args_start, *args_len);
                    writeln!(
                        out,
                        "assoc_fn_call {}::{}({})",
                        self.interner.resolve(type_name),
                        self.interner.resolve(function),
                        Self::format_call_args(&args)
                    )
                    .unwrap();
                }

                // Interfaces
                InstData::InterfaceDecl {
                    is_pub,
                    name,
                    methods_start,
                    methods_len,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(name);
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let method_refs: Vec<String> =
                        methods.iter().map(|m| format!("{}", m)).collect();
                    writeln!(
                        out,
                        "{}interface {} {{ {} }}",
                        pub_str,
                        name_str,
                        method_refs.join(", ")
                    )
                    .unwrap();
                }
                InstData::InterfaceMethodSig {
                    name,
                    params_start,
                    params_len,
                    return_type,
                    receiver_mode,
                } => {
                    let params = self.rir.get_params(*params_start, *params_len);
                    let params_str: Vec<String> = params
                        .iter()
                        .map(|p| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&p.name),
                                self.interner.resolve(&p.ty)
                            )
                        })
                        .collect();
                    let recv = match *receiver_mode {
                        1 => "inout self",
                        2 => "borrow self",
                        _ => "self",
                    };
                    writeln!(
                        out,
                        "interface_method_sig {}({}{}{}) -> {}",
                        self.interner.resolve(name),
                        recv,
                        if params.is_empty() { "" } else { ", " },
                        params_str.join(", "),
                        self.interner.resolve(return_type)
                    )
                    .unwrap();
                }

                // Drop
                InstData::DropFnDecl { type_name, body } => {
                    writeln!(out, "drop fn {}(self) {{", self.interner.resolve(type_name)).unwrap();
                    writeln!(out, "    {}", body).unwrap();
                    writeln!(out, "}}").unwrap();
                }

                // Derives (ADR-0058)
                InstData::DeriveDecl {
                    name,
                    methods_start,
                    methods_len,
                } => {
                    let name_str = self.interner.resolve(name);
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let method_refs: Vec<String> =
                        methods.iter().map(|m| format!("{}", m)).collect();
                    writeln!(out, "derive {} {{ {} }}", name_str, method_refs.join(", ")).unwrap();
                }

                // Comptime block
                InstData::Comptime { expr } => {
                    writeln!(out, "comptime {{ {} }}", expr).unwrap();
                }

                // Comptime unroll for
                InstData::ComptimeUnrollFor {
                    binding,
                    iterable,
                    body,
                } => writeln!(
                    out,
                    "comptime_unroll for {} in {}, {}",
                    self.interner.resolve(binding),
                    iterable,
                    body
                )
                .unwrap(),

                // Checked block
                InstData::Checked { expr } => {
                    writeln!(out, "checked {{ {} }}", expr).unwrap();
                }

                // Type constant
                InstData::TypeConst { type_name } => {
                    let name = self.interner.resolve(type_name);
                    writeln!(out, "type {}", name).unwrap();
                }

                // Anonymous struct type
                InstData::AnonStructType {
                    fields_start,
                    fields_len,
                    methods_start,
                    methods_len,
                    ..
                } => {
                    write!(out, "struct {{ ").unwrap();
                    let fields = self.rir.get_field_decls(*fields_start, *fields_len);
                    for (i, (name, ty)) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(out, ", ").unwrap();
                        }
                        let name_str = self.interner.resolve(name);
                        let ty_str = self.interner.resolve(ty);
                        write!(out, "{}: {}", name_str, ty_str).unwrap();
                    }
                    // Print methods if any
                    if *methods_len > 0 {
                        let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                        let methods_str: Vec<String> =
                            methods.iter().map(|m| format!("{}", m)).collect();
                        if !fields.is_empty() {
                            write!(out, ", ").unwrap();
                        }
                        write!(out, "methods: [{}]", methods_str.join(", ")).unwrap();
                    }
                    writeln!(out, " }}").unwrap();
                }

                // Anonymous enum type
                InstData::AnonEnumType {
                    variants_start,
                    variants_len,
                    methods_start,
                    methods_len,
                    ..
                } => {
                    write!(out, "enum {{ ").unwrap();
                    let variants = self
                        .rir
                        .get_enum_variant_decls(*variants_start, *variants_len);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .map(|(v, field_types, field_names)| {
                            let vname = self.interner.resolve(v).to_string();
                            if field_types.is_empty() {
                                vname
                            } else if field_names.is_empty() {
                                let field_strs: Vec<&str> = field_types
                                    .iter()
                                    .map(|f| self.interner.resolve(f))
                                    .collect();
                                format!("{}({})", vname, field_strs.join(", "))
                            } else {
                                let field_strs: Vec<String> = field_names
                                    .iter()
                                    .zip(field_types.iter())
                                    .map(|(n, t)| {
                                        format!(
                                            "{}: {}",
                                            self.interner.resolve(n),
                                            self.interner.resolve(t)
                                        )
                                    })
                                    .collect();
                                format!("{} {{ {} }}", vname, field_strs.join(", "))
                            }
                        })
                        .collect();
                    write!(out, "{}", variants_str.join(", ")).unwrap();
                    if *methods_len > 0 {
                        let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                        let methods_str: Vec<String> =
                            methods.iter().map(|m| format!("{}", m)).collect();
                        if !variants_str.is_empty() {
                            write!(out, ", ").unwrap();
                        }
                        write!(out, "methods: [{}]", methods_str.join(", ")).unwrap();
                    }
                    writeln!(out, " }}").unwrap();
                }
                // Tuple lowering (ADR-0048) — printed with the same tuple-literal syntax
                InstData::TupleInit {
                    elems_start,
                    elems_len,
                } => {
                    let elems = self.rir.get_inst_refs(*elems_start, *elems_len);
                    let elems_str: Vec<String> = elems.iter().map(|e| format!("{}", e)).collect();
                    if elems.len() == 1 {
                        writeln!(out, "tuple ({},)", elems_str[0]).unwrap();
                    } else {
                        writeln!(out, "tuple ({})", elems_str.join(", ")).unwrap();
                    }
                }
                // Anonymous function value (ADR-0055) — prints as "anon_fn call=<method ref>"
                InstData::AnonFnValue { method } => {
                    writeln!(out, "anon_fn call={}", method).unwrap();
                }
                // Anonymous interface type (ADR-0057)
                InstData::AnonInterfaceType {
                    methods_start,
                    methods_len,
                } => {
                    let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let refs: Vec<String> = methods.iter().map(|m| format!("{}", m)).collect();
                    writeln!(out, "anon_interface_type {{ {} }}", refs.join(", ")).unwrap();
                }
            }
        }
        out
    }
}

impl fmt::Display for RirPrinter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render())
    }
}
