use crate::parser::{Program, Expr};

/// Generate a minimal ELF64 executable for Linux x86-64.
///
/// The generated binary:
/// - Has a single PT_LOAD segment containing everything
/// - Entry point calls exit syscall with the return value of main()
pub fn generate_elf(program: &Program) -> Vec<u8> {
    // Find main function
    let main_fn = program
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("no main function found");

    // Get the return value
    let exit_code = match &main_fn.body {
        Expr::Int(n, _) => *n as i32,
    };

    // Build the code section
    let code = generate_code(exit_code);

    // Build the ELF
    build_elf(&code)
}

/// Generate x86-64 machine code that exits with the given code.
fn generate_code(exit_code: i32) -> Vec<u8> {
    let mut code = Vec::new();

    // mov edi, <exit_code>  ; first arg to syscall
    // Encoding: BF <imm32>
    code.push(0xBF);
    code.extend_from_slice(&exit_code.to_le_bytes());

    // mov eax, 60  ; syscall number for exit
    // Encoding: B8 <imm32>
    code.push(0xB8);
    code.extend_from_slice(&60_i32.to_le_bytes());

    // syscall
    // Encoding: 0F 05
    code.push(0x0F);
    code.push(0x05);

    code
}

/// Build a minimal ELF64 executable.
fn build_elf(code: &[u8]) -> Vec<u8> {
    // Memory layout constants
    const BASE_ADDR: u64 = 0x400000;
    const ELF_HEADER_SIZE: u64 = 64;
    const PROGRAM_HEADER_SIZE: u64 = 56;
    const HEADER_SIZE: u64 = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;

    let code_offset = HEADER_SIZE;
    let entry_point = BASE_ADDR + code_offset;
    let file_size = HEADER_SIZE + code.len() as u64;

    let mut elf = Vec::new();

    // ===== ELF Header (64 bytes) =====

    // e_ident: ELF magic number and identification
    elf.extend_from_slice(&[
        0x7F, b'E', b'L', b'F',  // Magic number
        2,                       // 64-bit
        1,                       // Little endian
        1,                       // ELF version 1
        0,                       // OS/ABI: System V
        0, 0, 0, 0, 0, 0, 0, 0,  // Padding
    ]);

    // e_type: Executable file
    elf.extend_from_slice(&2_u16.to_le_bytes());

    // e_machine: x86-64
    elf.extend_from_slice(&0x3E_u16.to_le_bytes());

    // e_version: ELF version 1
    elf.extend_from_slice(&1_u32.to_le_bytes());

    // e_entry: Entry point address
    elf.extend_from_slice(&entry_point.to_le_bytes());

    // e_phoff: Program header offset (right after ELF header)
    elf.extend_from_slice(&ELF_HEADER_SIZE.to_le_bytes());

    // e_shoff: Section header offset (none)
    elf.extend_from_slice(&0_u64.to_le_bytes());

    // e_flags: Processor-specific flags
    elf.extend_from_slice(&0_u32.to_le_bytes());

    // e_ehsize: ELF header size
    elf.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());

    // e_phentsize: Program header entry size
    elf.extend_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());

    // e_phnum: Number of program headers
    elf.extend_from_slice(&1_u16.to_le_bytes());

    // e_shentsize: Section header entry size
    elf.extend_from_slice(&0_u16.to_le_bytes());

    // e_shnum: Number of section headers
    elf.extend_from_slice(&0_u16.to_le_bytes());

    // e_shstrndx: Section header string table index
    elf.extend_from_slice(&0_u16.to_le_bytes());

    // ===== Program Header (56 bytes) =====

    // p_type: PT_LOAD (loadable segment)
    elf.extend_from_slice(&1_u32.to_le_bytes());

    // p_flags: PF_R | PF_X (readable and executable)
    elf.extend_from_slice(&0x5_u32.to_le_bytes());

    // p_offset: Offset in file where segment begins
    elf.extend_from_slice(&0_u64.to_le_bytes());

    // p_vaddr: Virtual address where segment is loaded
    elf.extend_from_slice(&BASE_ADDR.to_le_bytes());

    // p_paddr: Physical address (same as virtual for our purposes)
    elf.extend_from_slice(&BASE_ADDR.to_le_bytes());

    // p_filesz: Size of segment in file
    elf.extend_from_slice(&file_size.to_le_bytes());

    // p_memsz: Size of segment in memory
    elf.extend_from_slice(&file_size.to_le_bytes());

    // p_align: Alignment
    elf.extend_from_slice(&0x1000_u64.to_le_bytes());

    // ===== Code =====
    elf.extend_from_slice(code);

    elf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_code() {
        let code = generate_code(42);
        // mov edi, 42 (BF 2A 00 00 00)
        // mov eax, 60 (B8 3C 00 00 00)
        // syscall (0F 05)
        assert_eq!(code.len(), 12);
        assert_eq!(code[0], 0xBF);
        assert_eq!(code[1], 42);
        assert_eq!(code[5], 0xB8);
        assert_eq!(code[6], 60);
        assert_eq!(code[10], 0x0F);
        assert_eq!(code[11], 0x05);
    }

    #[test]
    fn test_elf_header() {
        let elf = build_elf(&[]);
        // Check ELF magic
        assert_eq!(&elf[0..4], &[0x7F, b'E', b'L', b'F']);
        // Check it's 64-bit
        assert_eq!(elf[4], 2);
        // Check it's little endian
        assert_eq!(elf[5], 1);
    }
}
