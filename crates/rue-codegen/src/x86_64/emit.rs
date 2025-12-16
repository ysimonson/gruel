//! X86-64 instruction encoding.
//!
//! This phase converts X86Mir instructions (with physical registers) to
//! machine code bytes.

use super::mir::{Reg, X86Inst, X86Mir};
use super::EmittedRelocation;

/// X86-64 instruction emitter.
pub struct Emitter<'a> {
    mir: &'a X86Mir,
    code: Vec<u8>,
    relocations: Vec<EmittedRelocation>,
}

impl<'a> Emitter<'a> {
    /// Create a new emitter.
    pub fn new(mir: &'a X86Mir) -> Self {
        Self {
            mir,
            code: Vec::new(),
            relocations: Vec::new(),
        }
    }

    /// Emit machine code for all instructions.
    ///
    /// Returns (code bytes, relocations).
    pub fn emit(mut self) -> (Vec<u8>, Vec<EmittedRelocation>) {
        for inst in self.mir.iter() {
            self.emit_inst(inst);
        }
        (self.code, self.relocations)
    }

    /// Emit a single instruction.
    fn emit_inst(&mut self, inst: &X86Inst) {
        match inst {
            X86Inst::MovRI32 { dst, imm } => {
                self.emit_mov_ri32(dst.as_physical(), *imm);
            }
            X86Inst::MovRI64 { dst, imm } => {
                self.emit_mov_ri64(dst.as_physical(), *imm);
            }
            X86Inst::MovRR { dst, src } => {
                self.emit_mov_rr(dst.as_physical(), src.as_physical());
            }
            X86Inst::CallRel { symbol } => {
                self.emit_call_rel(symbol);
            }
            X86Inst::Syscall => {
                self.emit_syscall();
            }
            X86Inst::Ret => {
                self.emit_ret();
            }
        }
    }

    /// Emit `mov r32, imm32`.
    ///
    /// Encoding: [REX] B8+rd imm32
    /// - REX.B is needed for r8d-r15d
    /// - B8+rd is the opcode (B8 for eax, B9 for ecx, etc.)
    fn emit_mov_ri32(&mut self, dst: Reg, imm: i32) {
        let enc = dst.encoding();

        // REX prefix if needed (for R8-R15)
        if dst.needs_rex() {
            // REX.B = 1 (0x41)
            self.code.push(0x41);
        }

        // Opcode: B8 + (reg & 7)
        self.code.push(0xB8 + (enc & 7));

        // Immediate (32-bit little-endian)
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Emit `mov r64, imm64`.
    ///
    /// Encoding: REX.W B8+rd imm64
    /// - REX.W = 1 for 64-bit operand size
    /// - REX.B = 1 for r8-r15
    fn emit_mov_ri64(&mut self, dst: Reg, imm: i64) {
        let enc = dst.encoding();

        // REX prefix: W=1 (0x48), add B=1 (0x01) if needed
        let rex = 0x48 | if dst.needs_rex() { 0x01 } else { 0x00 };
        self.code.push(rex);

        // Opcode: B8 + (reg & 7)
        self.code.push(0xB8 + (enc & 7));

        // Immediate (64-bit little-endian)
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Emit `mov r64, r64`.
    ///
    /// Encoding: REX.W 89 /r (mov r/m64, r64)
    /// - REX.W = 1 for 64-bit operand size
    /// - REX.R = 1 if src is r8-r15
    /// - REX.B = 1 if dst is r8-r15
    /// - ModR/M byte: mod=11 (register), reg=src, r/m=dst
    fn emit_mov_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix: W=1, R=src.needs_rex, B=dst.needs_rex
        let rex = 0x48
            | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 89 (mov r/m64, r64)
        self.code.push(0x89);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `syscall`.
    ///
    /// Encoding: 0F 05
    fn emit_syscall(&mut self) {
        self.code.push(0x0F);
        self.code.push(0x05);
    }

    /// Emit `ret`.
    ///
    /// Encoding: C3
    fn emit_ret(&mut self) {
        self.code.push(0xC3);
    }

    /// Emit `call rel32` with a relocation.
    ///
    /// Encoding: E8 rel32
    /// The rel32 is a placeholder (0x00000000) that will be patched by the linker.
    fn emit_call_rel(&mut self, symbol: &str) {
        // Opcode: E8 (call rel32)
        self.code.push(0xE8);

        // The relocation offset points to the rel32 displacement
        let reloc_offset = self.code.len() as u64;

        // Placeholder for rel32 (will be filled by linker)
        self.code.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Record relocation: PC-relative, addend = -4 because the displacement
        // is calculated from the end of the instruction (after the 4-byte displacement)
        self.relocations.push(EmittedRelocation {
            offset: reloc_offset,
            symbol: symbol.to_string(),
            addend: -4,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::mir::Operand;

    fn emit_single(inst: X86Inst) -> Vec<u8> {
        let mut mir = X86Mir::new();
        mir.push(inst);
        Emitter::new(&mir).emit().0
    }

    #[test]
    fn test_mov_eax_imm32() {
        let code = emit_single(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 42,
        });
        // mov eax, 42 -> B8 2A 00 00 00
        assert_eq!(code, vec![0xB8, 0x2A, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_mov_edi_imm32() {
        let code = emit_single(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rdi),
            imm: 60,
        });
        // mov edi, 60 -> BF 3C 00 00 00
        assert_eq!(code, vec![0xBF, 0x3C, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_mov_r10d_imm32() {
        let code = emit_single(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::R10),
            imm: 42,
        });
        // mov r10d, 42 -> 41 BA 2A 00 00 00
        assert_eq!(code, vec![0x41, 0xBA, 0x2A, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_mov_rax_imm64() {
        let code = emit_single(X86Inst::MovRI64 {
            dst: Operand::Physical(Reg::Rax),
            imm: 0x1_0000_0000,
        });
        // mov rax, 0x100000000 -> 48 B8 00 00 00 00 01 00 00 00
        assert_eq!(
            code,
            vec![0x48, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn test_mov_r10_imm64() {
        let code = emit_single(X86Inst::MovRI64 {
            dst: Operand::Physical(Reg::R10),
            imm: 0x1_0000_0000,
        });
        // mov r10, 0x100000000 -> 49 BA 00 00 00 00 01 00 00 00
        assert_eq!(
            code,
            vec![0x49, 0xBA, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn test_mov_rdi_rax() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Physical(Reg::Rax),
        });
        // mov rdi, rax -> 48 89 C7
        assert_eq!(code, vec![0x48, 0x89, 0xC7]);
    }

    #[test]
    fn test_mov_rdi_r10() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Physical(Reg::R10),
        });
        // mov rdi, r10 -> 4C 89 D7
        assert_eq!(code, vec![0x4C, 0x89, 0xD7]);
    }

    #[test]
    fn test_mov_r10_rdi() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::R10),
            src: Operand::Physical(Reg::Rdi),
        });
        // mov r10, rdi -> 49 89 FA
        assert_eq!(code, vec![0x49, 0x89, 0xFA]);
    }

    #[test]
    fn test_syscall() {
        let code = emit_single(X86Inst::Syscall);
        assert_eq!(code, vec![0x0F, 0x05]);
    }

    #[test]
    fn test_ret() {
        let code = emit_single(X86Inst::Ret);
        assert_eq!(code, vec![0xC3]);
    }

    #[test]
    fn test_full_exit_sequence() {
        // mov r10d, 42
        // mov rdi, r10
        // mov eax, 60
        // syscall
        let mut mir = X86Mir::new();
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::R10),
            imm: 42,
        });
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Physical(Reg::R10),
        });
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 60,
        });
        mir.push(X86Inst::Syscall);

        let (code, _) = Emitter::new(&mir).emit();

        // 41 BA 2A 00 00 00  mov r10d, 42
        // 4C 89 D7           mov rdi, r10
        // B8 3C 00 00 00     mov eax, 60
        // 0F 05              syscall
        assert_eq!(
            code,
            vec![
                0x41, 0xBA, 0x2A, 0x00, 0x00, 0x00, // mov r10d, 42
                0x4C, 0x89, 0xD7,                   // mov rdi, r10
                0xB8, 0x3C, 0x00, 0x00, 0x00,       // mov eax, 60
                0x0F, 0x05                          // syscall
            ]
        );
    }

    #[test]
    fn test_call_rel() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::CallRel {
            symbol: "__rue_exit".into(),
        });

        let (code, relocs) = Emitter::new(&mir).emit();

        // call rel32 -> E8 00 00 00 00
        assert_eq!(code, vec![0xE8, 0x00, 0x00, 0x00, 0x00]);

        // Should have one relocation
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0].offset, 1); // After the opcode
        assert_eq!(relocs[0].symbol, "__rue_exit");
        assert_eq!(relocs[0].addend, -4);
    }
}
