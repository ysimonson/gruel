#![no_main]
use gruel_codegen::x86_64::{Emitter, Operand, Reg, X86Inst, X86Mir};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let mut mir = X86Mir::new();
    let mut idx = 0;

    while idx < data.len() {
        let opcode = data[idx] % 30;
        idx += 1;

        let reg1_idx = data.get(idx).copied().unwrap_or(0) % 14;
        idx += 1;
        let reg2_idx = data.get(idx).copied().unwrap_or(0) % 14;
        idx += 1;

        let reg1 = reg_from_index(reg1_idx);
        let reg2 = reg_from_index(reg2_idx);
        let op1 = Operand::Physical(reg1);
        let op2 = Operand::Physical(reg2);

        let imm32 = if idx + 4 <= data.len() {
            let bytes = [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]];
            idx += 4;
            i32::from_le_bytes(bytes)
        } else {
            0
        };

        let inst = match opcode {
            0 => X86Inst::MovRI32 { dst: op1, imm: imm32 },
            1 => X86Inst::MovRR { dst: op1, src: op2 },
            2 => X86Inst::AddRR { dst: op1, src: op2 },
            3 => X86Inst::AddRR64 { dst: op1, src: op2 },
            4 => X86Inst::SubRR { dst: op1, src: op2 },
            5 => X86Inst::SubRR64 { dst: op1, src: op2 },
            6 => X86Inst::AddRI { dst: op1, imm: imm32 },
            7 => X86Inst::ImulRR { dst: op1, src: op2 },
            8 => X86Inst::Neg { dst: op1 },
            9 => X86Inst::XorRI { dst: op1, imm: imm32 },
            10 => X86Inst::AndRR { dst: op1, src: op2 },
            11 => X86Inst::OrRR { dst: op1, src: op2 },
            12 => X86Inst::XorRR { dst: op1, src: op2 },
            13 => X86Inst::NotR { dst: op1 },
            14 => X86Inst::ShlRI { dst: op1, imm: (imm32 as u8) % 64 },
            15 => X86Inst::ShrRI { dst: op1, imm: (imm32 as u8) % 64 },
            16 => X86Inst::SarRI { dst: op1, imm: (imm32 as u8) % 64 },
            17 => X86Inst::CmpRR { src1: op1, src2: op2 },
            18 => X86Inst::CmpRI { src: op1, imm: imm32 },
            19 => X86Inst::Sete { dst: op1 },
            20 => X86Inst::Setne { dst: op1 },
            21 => X86Inst::Setl { dst: op1 },
            22 => X86Inst::Setg { dst: op1 },
            23 => X86Inst::Movzx { dst: op1, src: op2 },
            24 => X86Inst::TestRR { src1: op1, src2: op2 },
            25 => X86Inst::Push { src: op1 },
            26 => X86Inst::Pop { dst: op1 },
            27 => X86Inst::Cdq,
            28 => X86Inst::Syscall,
            29 => X86Inst::Ret,
            _ => X86Inst::MovRI32 { dst: op1, imm: 0 },
        };

        mir.push(inst);
    }

    let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
    let _ = emitter.emit();
});

fn reg_from_index(idx: u8) -> Reg {
    match idx % 14 {
        0 => Reg::Rax,
        1 => Reg::Rcx,
        2 => Reg::Rdx,
        3 => Reg::Rbx,
        4 => Reg::Rsi,
        5 => Reg::Rdi,
        6 => Reg::R8,
        7 => Reg::R9,
        8 => Reg::R10,
        9 => Reg::R11,
        10 => Reg::R12,
        11 => Reg::R13,
        12 => Reg::R14,
        13 => Reg::R15,
        _ => Reg::Rax,
    }
}
