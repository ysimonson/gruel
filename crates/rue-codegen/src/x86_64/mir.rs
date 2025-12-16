//! X86-64 Machine Intermediate Representation.
//!
//! X86Mir represents x86-64 instructions with virtual registers. This IR:
//! - Maps closely to actual x86-64 instructions
//! - Uses virtual registers (unlimited) that are later allocated to physical registers
//! - Can be emitted to machine code or assembly text

use std::fmt;

/// A virtual register.
///
/// Virtual registers are unlimited and allocated to physical registers
/// during register allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VReg(u32);

impl VReg {
    /// Create a new virtual register with the given index.
    #[inline]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the index of this virtual register.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for VReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A physical x86-64 register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Reg {
    /// Get the register encoding for ModR/M and SIB bytes.
    #[inline]
    pub const fn encoding(self) -> u8 {
        self as u8
    }

    /// Whether this register requires a REX prefix (R8-R15).
    #[inline]
    pub const fn needs_rex(self) -> bool {
        (self as u8) >= 8
    }

    /// The 32-bit version of this register's name.
    pub const fn name32(self) -> &'static str {
        match self {
            Reg::Rax => "eax",
            Reg::Rcx => "ecx",
            Reg::Rdx => "edx",
            Reg::Rbx => "ebx",
            Reg::Rsp => "esp",
            Reg::Rbp => "ebp",
            Reg::Rsi => "esi",
            Reg::Rdi => "edi",
            Reg::R8 => "r8d",
            Reg::R9 => "r9d",
            Reg::R10 => "r10d",
            Reg::R11 => "r11d",
            Reg::R12 => "r12d",
            Reg::R13 => "r13d",
            Reg::R14 => "r14d",
            Reg::R15 => "r15d",
        }
    }

    /// The 64-bit version of this register's name.
    pub const fn name64(self) -> &'static str {
        match self {
            Reg::Rax => "rax",
            Reg::Rcx => "rcx",
            Reg::Rdx => "rdx",
            Reg::Rbx => "rbx",
            Reg::Rsp => "rsp",
            Reg::Rbp => "rbp",
            Reg::Rsi => "rsi",
            Reg::Rdi => "rdi",
            Reg::R8 => "r8",
            Reg::R9 => "r9",
            Reg::R10 => "r10",
            Reg::R11 => "r11",
            Reg::R12 => "r12",
            Reg::R13 => "r13",
            Reg::R14 => "r14",
            Reg::R15 => "r15",
        }
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name64())
    }
}

/// An operand that can be either a virtual or physical register.
///
/// Before register allocation, operands are `Virtual`.
/// After register allocation, operands are `Physical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// A virtual register (pre-regalloc).
    Virtual(VReg),
    /// A physical register (post-regalloc).
    Physical(Reg),
}

impl Operand {
    /// Unwrap this operand as a physical register.
    ///
    /// # Panics
    /// Panics if this is a virtual register.
    #[inline]
    pub fn as_physical(self) -> Reg {
        match self {
            Operand::Physical(reg) => reg,
            Operand::Virtual(vreg) => panic!("expected physical register, got {}", vreg),
        }
    }

    /// Check if this operand is a physical register.
    #[inline]
    pub const fn is_physical(self) -> bool {
        matches!(self, Operand::Physical(_))
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Virtual(vreg) => write!(f, "{}", vreg),
            Operand::Physical(reg) => write!(f, "{}", reg),
        }
    }
}

impl From<VReg> for Operand {
    fn from(vreg: VReg) -> Self {
        Operand::Virtual(vreg)
    }
}

impl From<Reg> for Operand {
    fn from(reg: Reg) -> Self {
        Operand::Physical(reg)
    }
}

/// An x86-64 MIR instruction.
#[derive(Debug, Clone)]
pub enum X86Inst {
    /// `mov dst, imm32` - Move 32-bit immediate to register.
    MovRI32 { dst: Operand, imm: i32 },

    /// `mov dst, imm64` - Move 64-bit immediate to register.
    MovRI64 { dst: Operand, imm: i64 },

    /// `mov dst, src` - Move register to register.
    MovRR { dst: Operand, src: Operand },

    /// `call symbol` - Call a function by symbol name (PC-relative).
    ///
    /// The symbol will be resolved by the linker. This emits a `call rel32`
    /// instruction with a relocation for the target address.
    CallRel { symbol: String },

    /// `syscall` - Invoke system call.
    Syscall,

    /// `ret` - Return from function.
    Ret,
}

impl fmt::Display for X86Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            X86Inst::MovRI32 { dst, imm } => write!(f, "mov {}, {}", dst, imm),
            X86Inst::MovRI64 { dst, imm } => write!(f, "mov {}, {}", dst, imm),
            X86Inst::MovRR { dst, src } => write!(f, "mov {}, {}", dst, src),
            X86Inst::CallRel { symbol } => write!(f, "call {}", symbol),
            X86Inst::Syscall => write!(f, "syscall"),
            X86Inst::Ret => write!(f, "ret"),
        }
    }
}

/// X86-64 MIR for a function.
#[derive(Debug, Default)]
pub struct X86Mir {
    /// The instructions in this function.
    instructions: Vec<X86Inst>,
    /// The next virtual register index.
    next_vreg: u32,
}

impl X86Mir {
    /// Create a new empty X86Mir.
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            next_vreg: 0,
        }
    }

    /// Allocate a new virtual register.
    pub fn alloc_vreg(&mut self) -> VReg {
        let vreg = VReg::new(self.next_vreg);
        self.next_vreg += 1;
        vreg
    }

    /// Get the number of virtual registers allocated.
    #[inline]
    pub fn vreg_count(&self) -> u32 {
        self.next_vreg
    }

    /// Add an instruction.
    pub fn push(&mut self, inst: X86Inst) {
        self.instructions.push(inst);
    }

    /// Get the instructions.
    #[inline]
    pub fn instructions(&self) -> &[X86Inst] {
        &self.instructions
    }

    /// Get mutable access to instructions (for register allocation).
    #[inline]
    pub fn instructions_mut(&mut self) -> &mut [X86Inst] {
        &mut self.instructions
    }

    /// Iterate over instructions.
    pub fn iter(&self) -> impl Iterator<Item = &X86Inst> {
        self.instructions.iter()
    }
}

impl fmt::Display for X86Mir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for inst in &self.instructions {
            writeln!(f, "    {}", inst)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vreg_allocation() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();

        assert_eq!(v0.index(), 0);
        assert_eq!(v1.index(), 1);
        assert_eq!(v2.index(), 2);
        assert_eq!(mir.vreg_count(), 3);
    }

    #[test]
    fn test_reg_encoding() {
        assert_eq!(Reg::Rax.encoding(), 0);
        assert_eq!(Reg::Rdi.encoding(), 7);
        assert_eq!(Reg::R8.encoding(), 8);
        assert_eq!(Reg::R15.encoding(), 15);
    }

    #[test]
    fn test_reg_needs_rex() {
        assert!(!Reg::Rax.needs_rex());
        assert!(!Reg::Rdi.needs_rex());
        assert!(Reg::R8.needs_rex());
        assert!(Reg::R15.needs_rex());
    }

    #[test]
    fn test_instruction_display() {
        let inst = X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rdi),
            imm: 42,
        };
        assert_eq!(format!("{}", inst), "mov rdi, 42");
    }
}
