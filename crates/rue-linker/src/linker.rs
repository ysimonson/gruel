//! The linker - combines object files and produces an executable.

use std::collections::HashMap;
use crate::elf::{ObjectFile, Symbol, SymbolBinding, RelocationType};

/// Linker errors.
#[derive(Debug)]
pub enum LinkError {
    /// Undefined symbol reference.
    UndefinedSymbol(String),
    /// Duplicate symbol definition.
    DuplicateSymbol(String),
    /// Unsupported relocation type.
    UnsupportedRelocation(String),
    /// Relocation overflow (value doesn't fit).
    RelocationOverflow { symbol: String, rel_type: String },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::UndefinedSymbol(s) => write!(f, "undefined symbol: {}", s),
            LinkError::DuplicateSymbol(s) => write!(f, "duplicate symbol: {}", s),
            LinkError::UnsupportedRelocation(s) => write!(f, "unsupported relocation: {}", s),
            LinkError::RelocationOverflow { symbol, rel_type } => {
                write!(f, "relocation overflow for {} ({})", symbol, rel_type)
            }
        }
    }
}

impl std::error::Error for LinkError {}

/// The linker.
pub struct Linker {
    /// Base address for the executable.
    base_addr: u64,
    /// Symbol table: name -> (object_index, symbol).
    global_symbols: HashMap<String, (usize, Symbol)>,
    /// All object files we're linking.
    objects: Vec<ObjectFile>,
}

impl Linker {
    /// Create a new linker.
    pub fn new() -> Self {
        Linker {
            base_addr: 0x400000,
            global_symbols: HashMap::new(),
            objects: Vec::new(),
        }
    }

    /// Add an object file to be linked.
    pub fn add_object(&mut self, obj: ObjectFile) -> Result<(), LinkError> {
        let obj_index = self.objects.len();

        // Collect global symbols
        for sym in &obj.symbols {
            if sym.section_index.is_some() &&
               (sym.binding == SymbolBinding::Global || sym.binding == SymbolBinding::Weak) &&
               !sym.name.is_empty()
            {
                if let Some((_, existing)) = self.global_symbols.get(&sym.name) {
                    // Allow weak symbols to be overridden
                    if existing.binding != SymbolBinding::Weak && sym.binding != SymbolBinding::Weak {
                        return Err(LinkError::DuplicateSymbol(sym.name.clone()));
                    }
                    // Keep the non-weak one
                    if existing.binding == SymbolBinding::Weak {
                        self.global_symbols.insert(sym.name.clone(), (obj_index, sym.clone()));
                    }
                } else {
                    self.global_symbols.insert(sym.name.clone(), (obj_index, sym.clone()));
                }
            }
        }

        self.objects.push(obj);
        Ok(())
    }

    /// Link all objects and produce an executable.
    pub fn link(self, entry_point: &str) -> Result<Vec<u8>, LinkError> {
        // Layout constants - use a single program header for simplicity
        const ELF_HEADER_SIZE: u64 = 64;
        const PROGRAM_HEADER_SIZE: u64 = 56;
        const NUM_PROGRAM_HEADERS: u64 = 1;
        const HEADER_SIZE: u64 = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE * NUM_PROGRAM_HEADERS;

        // Code starts right after headers. For ELF loading to work,
        // (vaddr % page_size) must equal (file_offset % page_size).
        // With code at file offset HEADER_SIZE, we set vaddr accordingly.
        let code_start = self.base_addr + HEADER_SIZE;

        // First, collect and merge all code sections
        let mut merged_code = Vec::new();
        let mut merged_rodata = Vec::new();
        let mut pending_relocations = Vec::new();

        // Track where each section ends up in the merged output
        // Key: (object_index, section_index) -> offset in merged section
        let mut section_offsets: HashMap<(usize, usize), u64> = HashMap::new();

        // Merge code sections (.text*)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !section.name.starts_with(".text") || section.data.is_empty() {
                    continue;
                }

                // Align
                let align = section.align.max(1);
                let padding = align_up(merged_code.len() as u64, align) - merged_code.len() as u64;
                merged_code.extend(std::iter::repeat(0xCC).take(padding as usize)); // INT3 padding

                let offset = merged_code.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_code.extend_from_slice(&section.data);

                // Collect relocations
                for reloc in &section.relocations {
                    let sym = &obj.symbols[reloc.symbol_index];
                    pending_relocations.push((
                        offset + reloc.offset,
                        sym.name.clone(),
                        sym.section_index,
                        obj_idx,
                        reloc.rel_type,
                        reloc.addend,
                    ));
                }
            }
        }

        // Merge rodata sections (placed right after code, no page break)
        let rodata_offset_in_merged = merged_code.len();

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !section.name.starts_with(".rodata") || section.data.is_empty() {
                    continue;
                }

                let align = section.align.max(1);
                let padding = align_up(merged_rodata.len() as u64, align) - merged_rodata.len() as u64;
                merged_rodata.extend(std::iter::repeat(0).take(padding as usize));

                let offset = merged_rodata.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_rodata.extend_from_slice(&section.data);
            }
        }

        // Virtual addresses - rodata follows code directly
        let code_vaddr = code_start;
        let rodata_vaddr = code_vaddr + rodata_offset_in_merged as u64;

        // Build final symbol addresses
        let mut symbol_addresses: HashMap<String, u64> = HashMap::new();

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for sym in &obj.symbols {
                if sym.name.is_empty() {
                    continue;
                }

                if let Some(sec_idx) = sym.section_index {
                    if let Some(&section_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                        let section = &obj.sections[sec_idx];
                        let base = if section.name.starts_with(".text") {
                            code_vaddr
                        } else if section.name.starts_with(".rodata") {
                            rodata_vaddr
                        } else {
                            continue;
                        };

                        let addr = base + section_offset + sym.value;

                        // Only add global symbols, or section symbols for relocation
                        if sym.binding == SymbolBinding::Global ||
                           sym.binding == SymbolBinding::Weak ||
                           !symbol_addresses.contains_key(&sym.name) {
                            symbol_addresses.insert(sym.name.clone(), addr);
                        }
                    }
                }
            }
        }

        // Also add section symbols for rodata relocation
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if section.name.starts_with(".rodata") {
                    if let Some(&offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                        let addr = rodata_vaddr + offset;
                        // Use section name as fallback
                        symbol_addresses.entry(section.name.clone()).or_insert(addr);
                    }
                }
            }
        }

        // Find entry point
        let entry_addr = *symbol_addresses.get(entry_point)
            .ok_or_else(|| LinkError::UndefinedSymbol(entry_point.to_string()))?;

        // Apply relocations
        for (offset, sym_name, sym_section, obj_idx, rel_type, addend) in pending_relocations {
            // Try to resolve the symbol
            let target_addr = if let Some(&addr) = symbol_addresses.get(&sym_name) {
                addr
            } else if let Some(sec_idx) = sym_section {
                // Section-relative symbol - look up the section's address
                let obj = &self.objects[obj_idx];
                let section = &obj.sections[sec_idx];
                if let Some(&sec_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                    let base = if section.name.starts_with(".text") {
                        code_vaddr
                    } else if section.name.starts_with(".rodata") {
                        rodata_vaddr
                    } else {
                        return Err(LinkError::UndefinedSymbol(sym_name));
                    };
                    base + sec_offset
                } else {
                    return Err(LinkError::UndefinedSymbol(sym_name));
                }
            } else {
                return Err(LinkError::UndefinedSymbol(sym_name.clone()));
            };

            let pc = code_vaddr + offset;
            let patch_offset = offset as usize;

            match rel_type {
                RelocationType::Pc32 | RelocationType::Plt32 => {
                    // S + A - P, where S is symbol address, A is addend, P is place
                    let value = (target_addr as i64 + addend - pc as i64) as i32;
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                RelocationType::Abs64 => {
                    let value = (target_addr as i64 + addend) as u64;
                    merged_code[patch_offset..patch_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                RelocationType::Abs32 => {
                    let value = (target_addr as i64 + addend) as u32;
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                RelocationType::Abs32S => {
                    let value = (target_addr as i64 + addend) as i32;
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                RelocationType::Unknown(t) => {
                    return Err(LinkError::UnsupportedRelocation(format!("unknown type {}", t)));
                }
            }
        }

        // Build the ELF - single segment with code + rodata
        let file_offset = HEADER_SIZE;
        let total_size = merged_code.len() + merged_rodata.len();

        let mut elf = Vec::with_capacity((HEADER_SIZE as usize) + total_size);

        // ===== ELF Header (64 bytes) =====
        elf.extend_from_slice(&[
            0x7F, b'E', b'L', b'F', // Magic
            2,    // 64-bit
            1,    // Little endian
            1,    // ELF version
            0,    // System V ABI
            0, 0, 0, 0, 0, 0, 0, 0, // Padding
        ]);
        elf.extend_from_slice(&2_u16.to_le_bytes()); // e_type: ET_EXEC
        elf.extend_from_slice(&0x3E_u16.to_le_bytes()); // e_machine: x86-64
        elf.extend_from_slice(&1_u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&entry_addr.to_le_bytes()); // e_entry
        elf.extend_from_slice(&ELF_HEADER_SIZE.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_shoff (no sections)
        elf.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&(NUM_PROGRAM_HEADERS as u16).to_le_bytes()); // e_phnum
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shstrndx

        // ===== Single Program Header (PT_LOAD, R+W+X) =====
        elf.extend_from_slice(&1_u32.to_le_bytes()); // p_type: PT_LOAD
        elf.extend_from_slice(&0x7_u32.to_le_bytes()); // p_flags: PF_R | PF_W | PF_X
        elf.extend_from_slice(&file_offset.to_le_bytes()); // p_offset
        elf.extend_from_slice(&code_vaddr.to_le_bytes()); // p_vaddr
        elf.extend_from_slice(&code_vaddr.to_le_bytes()); // p_paddr
        elf.extend_from_slice(&(total_size as u64).to_le_bytes()); // p_filesz
        elf.extend_from_slice(&(total_size as u64).to_le_bytes()); // p_memsz
        elf.extend_from_slice(&0x1000_u64.to_le_bytes()); // p_align

        // Write code
        elf.extend_from_slice(&merged_code);

        // Write rodata (immediately follows code)
        elf.extend_from_slice(&merged_rodata);

        Ok(elf)
    }
}

impl Default for Linker {
    fn default() -> Self {
        Self::new()
    }
}

/// Align a value up to the given alignment.
fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 16), 0);
        assert_eq!(align_up(1, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }
}
