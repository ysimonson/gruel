#![no_main]
use gruel_codegen::x86_64::{Emitter, Operand, Reg, X86Inst, X86Mir};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }

    let mut mir = X86Mir::new();
    let num_labels = (data[0] % 8) as u32 + 1;
    let mut idx = 1;

    let labels: Vec<_> = (0..num_labels).map(|_| mir.alloc_label()).collect();

    while idx < data.len() {
        let opcode = data[idx] % 40;
        idx += 1;

        let reg1_idx = data.get(idx).copied().unwrap_or(0) % 14;
        idx += 1;

        let op1 = Operand::Physical(reg_from_index(reg1_idx));
        let label_idx = data.get(idx).copied().unwrap_or(0) as usize % labels.len();
        idx += 1;

        let inst = match opcode {
            0..=19 => {
                let reg2_idx = data.get(idx).copied().unwrap_or(0) % 14;
                idx += 1;
                let op2 = Operand::Physical(reg_from_index(reg2_idx));
                match opcode {
                    0 => X86Inst::MovRR { dst: op1, src: op2 },
                    1 => X86Inst::AddRR { dst: op1, src: op2 },
                    2 => X86Inst::SubRR { dst: op1, src: op2 },
                    3 => X86Inst::CmpRR { src1: op1, src2: op2 },
                    4 => X86Inst::XorRR { dst: op1, src: op2 },
                    _ => X86Inst::MovRI32 { dst: op1, imm: opcode as i32 },
                }
            }
            20..=24 => X86Inst::Label { id: labels[label_idx] },
            25 => X86Inst::Jz { label: labels[label_idx] },
            26 => X86Inst::Jnz { label: labels[label_idx] },
            27 => X86Inst::Jo { label: labels[label_idx] },
            28 => X86Inst::Jb { label: labels[label_idx] },
            29 => X86Inst::Jae { label: labels[label_idx] },
            30 => X86Inst::Jbe { label: labels[label_idx] },
            31 => X86Inst::Jge { label: labels[label_idx] },
            32 => X86Inst::Jle { label: labels[label_idx] },
            33 => X86Inst::Jmp { label: labels[label_idx] },
            _ => X86Inst::Ret,
        };

        mir.push(inst);
    }

    for label in &labels {
        mir.push(X86Inst::Label { id: *label });
    }
    mir.push(X86Inst::Ret);

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
