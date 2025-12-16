//! ELF binary generation for the Rue compiler.
//!
//! Generates minimal ELF64 executables for Linux x86-64.

/// Build a minimal ELF64 executable from machine code.
///
/// The generated binary:
/// - Has a single PT_LOAD segment containing everything
/// - Entry point executes the provided code directly
pub fn build_elf(code: &[u8]) -> Vec<u8> {
    // Memory layout constants
    const BASE_ADDR: u64 = 0x400000;
    const ELF_HEADER_SIZE: u64 = 64;
    const PROGRAM_HEADER_SIZE: u64 = 56;
    const HEADER_SIZE: u64 = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;

    let code_offset = HEADER_SIZE;
    let entry_point = BASE_ADDR + code_offset;
    let file_size = HEADER_SIZE + code.len() as u64;

    let mut elf = Vec::with_capacity(file_size as usize);

    // ===== ELF Header (64 bytes) =====

    // e_ident: ELF magic number and identification
    elf.extend_from_slice(&[
        0x7F, b'E', b'L', b'F', // Magic number
        2,    // 64-bit
        1,    // Little endian
        1,    // ELF version 1
        0,    // OS/ABI: System V
        0, 0, 0, 0, 0, 0, 0, 0, // Padding
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
    fn test_elf_magic() {
        let elf = build_elf(&[]);
        assert_eq!(&elf[0..4], &[0x7F, b'E', b'L', b'F']);
    }

    #[test]
    fn test_elf_64bit() {
        let elf = build_elf(&[]);
        assert_eq!(elf[4], 2); // 64-bit
    }

    #[test]
    fn test_elf_little_endian() {
        let elf = build_elf(&[]);
        assert_eq!(elf[5], 1); // Little endian
    }

    #[test]
    fn test_elf_header_size() {
        let elf = build_elf(&[]);
        // Header should be 64 + 56 = 120 bytes minimum
        assert!(elf.len() >= 120);
    }
}
