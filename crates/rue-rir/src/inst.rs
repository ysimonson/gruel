//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;

use rue_intern::Symbol;
use rue_span::Span;

/// A reference to an instruction in the RIR.
///
/// This is a lightweight handle (4 bytes) that indexes into the instruction array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstRef(u32);

impl InstRef {
    /// Create an instruction reference from a raw index.
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw index.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// The complete RIR for a source file.
#[derive(Debug, Default)]
pub struct Rir {
    /// All instructions in the file
    instructions: Vec<Inst>,
    /// Extra data for variable-length instruction payloads
    extra: Vec<u32>,
}

impl Rir {
    /// Create a new empty RIR.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an instruction and return its reference.
    pub fn add_inst(&mut self, inst: Inst) -> InstRef {
        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        InstRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &Inst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// Get a mutable reference to an instruction.
    #[inline]
    pub fn get_mut(&mut self, inst_ref: InstRef) -> &mut Inst {
        &mut self.instructions[inst_ref.0 as usize]
    }

    /// The number of instructions.
    #[inline]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Whether there are no instructions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Iterate over all instructions with their references.
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &Inst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (InstRef::from_raw(i as u32), inst))
    }

    /// Add extra data and return the start index.
    pub fn add_extra(&mut self, data: &[u32]) -> u32 {
        let start = self.extra.len() as u32;
        self.extra.extend_from_slice(data);
        start
    }

    /// Get extra data by index.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[u32] {
        let start = start as usize;
        let end = start + len as usize;
        &self.extra[start..end]
    }
}

/// A single RIR instruction.
#[derive(Debug, Clone)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// Instruction data - the actual operation.
#[derive(Debug, Clone)]
pub enum InstData {
    /// Integer constant
    IntConst(i64),

    /// Boolean constant
    BoolConst(bool),

    // Binary arithmetic operations
    /// Addition: lhs + rhs
    Add { lhs: InstRef, rhs: InstRef },
    /// Subtraction: lhs - rhs
    Sub { lhs: InstRef, rhs: InstRef },
    /// Multiplication: lhs * rhs
    Mul { lhs: InstRef, rhs: InstRef },
    /// Division: lhs / rhs
    Div { lhs: InstRef, rhs: InstRef },
    /// Modulo: lhs % rhs
    Mod { lhs: InstRef, rhs: InstRef },

    // Comparison operations
    /// Equality: lhs == rhs
    Eq { lhs: InstRef, rhs: InstRef },
    /// Inequality: lhs != rhs
    Ne { lhs: InstRef, rhs: InstRef },
    /// Less than: lhs < rhs
    Lt { lhs: InstRef, rhs: InstRef },
    /// Greater than: lhs > rhs
    Gt { lhs: InstRef, rhs: InstRef },
    /// Less than or equal: lhs <= rhs
    Le { lhs: InstRef, rhs: InstRef },
    /// Greater than or equal: lhs >= rhs
    Ge { lhs: InstRef, rhs: InstRef },

    // Unary operations
    /// Negation: -operand
    Neg { operand: InstRef },

    // Control flow
    /// Branch: if cond then then_block else else_block
    Branch {
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
    },

    /// Function definition
    /// Contains: name symbol, return type symbol, body instruction ref
    FnDecl {
        name: Symbol,
        return_type: Symbol,
        body: InstRef,
    },

    /// Return value from function
    Ret(InstRef),

    /// Block of instructions (for function bodies)
    /// The result is the last instruction in the block
    Block {
        /// Index into extra data where instruction refs start
        extra_start: u32,
        /// Number of instructions in the block
        len: u32,
    },

    // Variable operations
    /// Local variable declaration: allocates storage and initializes
    Alloc {
        /// Variable name
        name: Symbol,
        /// Whether the variable is mutable
        is_mut: bool,
        /// Optional type annotation
        ty: Option<Symbol>,
        /// Initial value instruction
        init: InstRef,
    },

    /// Variable reference: reads the value of a variable
    VarRef {
        /// Variable name
        name: Symbol,
    },

    /// Assignment: stores a value into a mutable variable
    Assign {
        /// Variable name
        name: Symbol,
        /// Value to store
        value: InstRef,
    },
}

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b rue_intern::Interner,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b rue_intern::Interner) -> Self {
        Self { rir, interner }
    }

    /// Format the RIR as a string.
    pub fn to_string(&self) -> String {
        let mut out = String::new();
        for (inst_ref, inst) in self.rir.iter() {
            out.push_str(&format!("{} = ", inst_ref));
            match &inst.data {
                InstData::IntConst(v) => {
                    out.push_str(&format!("const {}\n", v));
                }
                InstData::BoolConst(v) => {
                    out.push_str(&format!("const {}\n", v));
                }
                InstData::Add { lhs, rhs } => {
                    out.push_str(&format!("add {}, {}\n", lhs, rhs));
                }
                InstData::Sub { lhs, rhs } => {
                    out.push_str(&format!("sub {}, {}\n", lhs, rhs));
                }
                InstData::Mul { lhs, rhs } => {
                    out.push_str(&format!("mul {}, {}\n", lhs, rhs));
                }
                InstData::Div { lhs, rhs } => {
                    out.push_str(&format!("div {}, {}\n", lhs, rhs));
                }
                InstData::Mod { lhs, rhs } => {
                    out.push_str(&format!("mod {}, {}\n", lhs, rhs));
                }
                InstData::Eq { lhs, rhs } => {
                    out.push_str(&format!("eq {}, {}\n", lhs, rhs));
                }
                InstData::Ne { lhs, rhs } => {
                    out.push_str(&format!("ne {}, {}\n", lhs, rhs));
                }
                InstData::Lt { lhs, rhs } => {
                    out.push_str(&format!("lt {}, {}\n", lhs, rhs));
                }
                InstData::Gt { lhs, rhs } => {
                    out.push_str(&format!("gt {}, {}\n", lhs, rhs));
                }
                InstData::Le { lhs, rhs } => {
                    out.push_str(&format!("le {}, {}\n", lhs, rhs));
                }
                InstData::Ge { lhs, rhs } => {
                    out.push_str(&format!("ge {}, {}\n", lhs, rhs));
                }
                InstData::Neg { operand } => {
                    out.push_str(&format!("neg {}\n", operand));
                }
                InstData::Branch { cond, then_block, else_block } => {
                    if let Some(else_b) = else_block {
                        out.push_str(&format!("branch {}, {}, {}\n", cond, then_block, else_b));
                    } else {
                        out.push_str(&format!("branch {}, {}\n", cond, then_block));
                    }
                }
                InstData::FnDecl { name, return_type, body } => {
                    let name_str = self.interner.get(*name);
                    let ret_str = self.interner.get(*return_type);
                    out.push_str(&format!("fn {}() -> {} {{\n", name_str, ret_str));
                    out.push_str(&format!("    {}\n", body));
                    out.push_str("}\n");
                }
                InstData::Ret(inner) => {
                    out.push_str(&format!("ret {}\n", inner));
                }
                InstData::Block { extra_start, len } => {
                    out.push_str(&format!("block({}, {})\n", extra_start, len));
                }
                InstData::Alloc { name, is_mut, ty, init } => {
                    let name_str = self.interner.get(*name);
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty.map(|t| format!(": {}", self.interner.get(t))).unwrap_or_default();
                    out.push_str(&format!("alloc {}{}{}= {}\n", mut_str, name_str, ty_str, init));
                }
                InstData::VarRef { name } => {
                    let name_str = self.interner.get(*name);
                    out.push_str(&format!("var_ref {}\n", name_str));
                }
                InstData::Assign { name, value } => {
                    let name_str = self.interner.get(*name);
                    out.push_str(&format!("assign {} = {}\n", name_str, value));
                }
            }
        }
        out
    }
}

impl fmt::Display for RirPrinter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inst_ref_size() {
        assert_eq!(std::mem::size_of::<InstRef>(), 4);
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut rir = Rir::new();
        let inst = Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        };
        let inst_ref = rir.add_inst(inst);

        let retrieved = rir.get(inst_ref);
        assert!(matches!(retrieved.data, InstData::IntConst(42)));
    }
}
