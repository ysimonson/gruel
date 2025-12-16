//! Code generation for the Rue compiler.
//!
//! This crate converts AIR (Analyzed Intermediate Representation) to machine code.
//! Currently only x86-64 is supported.
//!
//! ## Pipeline
//!
//! ```text
//! AIR → X86Mir (virtual registers) → Register Allocation → Machine Code
//! ```
//!
//! The x86-64 backend uses a Machine IR (MIR) that closely matches x86-64
//! instructions but uses virtual registers. Register allocation then maps
//! virtual registers to physical registers before final emission.

pub mod x86_64;

pub use x86_64::{generate, MachineCode};

// Re-export commonly used types for convenience
pub use x86_64::{Operand, Reg, VReg, X86Inst, X86Mir};

use rue_air::Air;

/// Code generator that wraps the x86-64 backend.
///
/// This provides a similar API to the old CodeGen for compatibility.
pub struct CodeGen<'a> {
    air: &'a Air,
}

impl<'a> CodeGen<'a> {
    /// Create a new code generator for the given AIR.
    pub fn new(air: &'a Air) -> Self {
        Self { air }
    }

    /// Generate machine code from the AIR.
    pub fn generate(self) -> MachineCode {
        x86_64::generate(self.air)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::{AirInst, AirInstData, Type};
    use rue_span::Span;

    #[test]
    fn test_codegen_api_compatibility() {
        let mut air = Air::new(Type::I32);

        let const_ref = air.add_inst(AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        air.add_inst(AirInst {
            data: AirInstData::Ret(const_ref),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        // Test the old-style API
        let codegen = CodeGen::new(&air);
        let machine_code = codegen.generate();

        // Should generate working code
        assert!(!machine_code.code.is_empty());

        // Last two bytes should be syscall (0F 05)
        let len = machine_code.code.len();
        assert_eq!(machine_code.code[len - 2], 0x0F);
        assert_eq!(machine_code.code[len - 1], 0x05);
    }

    #[test]
    fn test_generate_function() {
        let mut air = Air::new(Type::I32);

        let const_ref = air.add_inst(AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        air.add_inst(AirInst {
            data: AirInstData::Ret(const_ref),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        // Test the new direct API
        let machine_code = generate(&air);
        assert!(!machine_code.code.is_empty());
    }
}
