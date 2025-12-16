//! x86-64 code generation.
//!
//! Generates x86-64 machine code from AIR.

use rue_air::{Air, AirInstData, AirRef, Type};

/// Generated machine code for a function.
pub struct MachineCode {
    /// The raw machine code bytes
    pub code: Vec<u8>,
}

/// x86-64 code generator.
pub struct CodeGen<'a> {
    air: &'a Air,
    code: Vec<u8>,
}

impl<'a> CodeGen<'a> {
    /// Create a new code generator for the given AIR.
    pub fn new(air: &'a Air) -> Self {
        Self {
            air,
            code: Vec::new(),
        }
    }

    /// Generate machine code from the AIR.
    pub fn generate(mut self) -> MachineCode {
        // Walk the AIR to find the return instruction and evaluate its value
        for (_, inst) in self.air.iter() {
            if let AirInstData::Ret(value_ref) = &inst.data {
                let exit_code = self.evaluate(*value_ref);
                self.emit_exit(exit_code);
                break;
            }
        }

        // If no return was found (shouldn't happen for valid AIR), emit exit 0
        if self.code.is_empty() {
            self.emit_exit(0);
        }

        MachineCode { code: self.code }
    }

    /// Evaluate an AIR instruction to get its value.
    ///
    /// For now, this only handles constants. As the language grows,
    /// this will need to generate actual machine code and use registers.
    fn evaluate(&self, inst_ref: AirRef) -> i32 {
        let inst = self.air.get(inst_ref);
        match &inst.data {
            AirInstData::Const(value) => *value as i32,
            AirInstData::Ret(inner) => self.evaluate(*inner),
        }
    }

    /// Emit x86-64 code for exit syscall.
    fn emit_exit(&mut self, exit_code: i32) {
        // mov edi, <exit_code>  ; first arg to syscall
        // Encoding: BF <imm32>
        self.code.push(0xBF);
        self.code.extend_from_slice(&exit_code.to_le_bytes());

        // mov eax, 60  ; syscall number for exit
        // Encoding: B8 <imm32>
        self.code.push(0xB8);
        self.code.extend_from_slice(&60_i32.to_le_bytes());

        // syscall
        // Encoding: 0F 05
        self.code.push(0x0F);
        self.code.push(0x05);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::AirInst;
    use rue_span::Span;

    #[test]
    fn test_generate_exit_code() {
        let mut air = Air::new(Type::I32);

        // Add constant 42
        let const_ref = air.add_inst(AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        // Add return
        air.add_inst(AirInst {
            data: AirInstData::Ret(const_ref),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        let codegen = CodeGen::new(&air);
        let machine_code = codegen.generate();

        // Should generate:
        // mov edi, 42 (BF 2A 00 00 00)
        // mov eax, 60 (B8 3C 00 00 00)
        // syscall (0F 05)
        assert_eq!(machine_code.code.len(), 12);
        assert_eq!(machine_code.code[0], 0xBF);
        assert_eq!(machine_code.code[1], 42);
        assert_eq!(machine_code.code[5], 0xB8);
        assert_eq!(machine_code.code[6], 60);
        assert_eq!(machine_code.code[10], 0x0F);
        assert_eq!(machine_code.code[11], 0x05);
    }
}
