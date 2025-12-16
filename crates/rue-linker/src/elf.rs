//! ELF64 object file parsing.
//!
//! Parses relocatable ELF64 object files to extract:
//! - Sections (code and data)
//! - Symbols (defined and undefined)
//! - Relocations (patches to apply)

use std::collections::HashMap;

/// A parsed ELF64 relocatable object file.
#[derive(Debug)]
pub struct ObjectFile {
    /// All sections in the object file.
    pub sections: Vec<Section>,
    /// All symbols (both defined and undefined).
    pub symbols: Vec<Symbol>,
    /// Section name to index mapping.
    pub section_map: HashMap<String, usize>,
}

/// A section from an object file.
#[derive(Debug, Clone)]
pub struct Section {
    /// Section name (e.g., ".text.rue_print").
    pub name: String,
    /// Section contents.
    pub data: Vec<u8>,
    /// Section flags.
    pub flags: SectionFlags,
    /// Relocations that apply to this section.
    pub relocations: Vec<Relocation>,
    /// Alignment requirement.
    pub align: u64,
}

/// Section flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SectionFlags(u64);

impl SectionFlags {
    /// Section is writable.
    pub const WRITE: SectionFlags = SectionFlags(0x1);
    /// Section is allocated (loaded into memory).
    pub const ALLOC: SectionFlags = SectionFlags(0x2);
    /// Section is executable.
    pub const EXEC: SectionFlags = SectionFlags(0x4);

    /// Create empty flags.
    pub const fn empty() -> Self {
        SectionFlags(0)
    }

    /// Check if flags contain a specific flag.
    pub const fn contains(self, other: SectionFlags) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for SectionFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        SectionFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for SectionFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A symbol from an object file.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Symbol name.
    pub name: String,
    /// Section index this symbol is defined in (None if undefined).
    pub section_index: Option<usize>,
    /// Offset within the section.
    pub value: u64,
    /// Symbol size.
    pub size: u64,
    /// Symbol binding (local, global, weak).
    pub binding: SymbolBinding,
    /// Symbol type (function, object, etc.).
    pub sym_type: SymbolType,
}

/// Symbol binding type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolBinding {
    Local,
    Global,
    Weak,
}

/// Symbol type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolType {
    None,
    Object,
    Func,
    Section,
    File,
}

/// A relocation entry.
#[derive(Debug, Clone)]
pub struct Relocation {
    /// Offset within the section to patch.
    pub offset: u64,
    /// Symbol index this relocation refers to.
    pub symbol_index: usize,
    /// Relocation type.
    pub rel_type: RelocationType,
    /// Addend value.
    pub addend: i64,
}

/// x86-64 relocation types we support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationType {
    /// R_X86_64_64: 64-bit absolute address.
    Abs64,
    /// R_X86_64_PC32: 32-bit PC-relative address.
    Pc32,
    /// R_X86_64_PLT32: 32-bit PLT-relative (treated as PC32 for static linking).
    Plt32,
    /// R_X86_64_32: 32-bit absolute address.
    Abs32,
    /// R_X86_64_32S: 32-bit signed absolute address.
    Abs32S,
    /// Unknown relocation type.
    Unknown(u32),
}

impl RelocationType {
    fn from_elf(r_type: u32) -> Self {
        match r_type {
            1 => RelocationType::Abs64,
            2 => RelocationType::Pc32,
            4 => RelocationType::Plt32,
            10 => RelocationType::Abs32,
            11 => RelocationType::Abs32S,
            _ => RelocationType::Unknown(r_type),
        }
    }
}

/// Error type for object file parsing.
#[derive(Debug)]
pub enum ParseError {
    /// File is too short.
    TooShort,
    /// Invalid ELF magic number.
    InvalidMagic,
    /// Not a 64-bit ELF file.
    Not64Bit,
    /// Not a little-endian ELF file.
    NotLittleEndian,
    /// Not a relocatable object file.
    NotRelocatable,
    /// Not an x86-64 object file.
    NotX86_64,
    /// Invalid section header.
    InvalidSection(String),
    /// Invalid symbol table.
    InvalidSymbol(String),
    /// Invalid string table.
    InvalidStringTable,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooShort => write!(f, "file is too short to be a valid ELF"),
            ParseError::InvalidMagic => write!(f, "invalid ELF magic number"),
            ParseError::Not64Bit => write!(f, "not a 64-bit ELF file"),
            ParseError::NotLittleEndian => write!(f, "not a little-endian ELF file"),
            ParseError::NotRelocatable => write!(f, "not a relocatable object file"),
            ParseError::NotX86_64 => write!(f, "not an x86-64 object file"),
            ParseError::InvalidSection(s) => write!(f, "invalid section: {}", s),
            ParseError::InvalidSymbol(s) => write!(f, "invalid symbol: {}", s),
            ParseError::InvalidStringTable => write!(f, "invalid string table"),
        }
    }
}

impl std::error::Error for ParseError {}

impl ObjectFile {
    /// Parse an ELF64 relocatable object file.
    pub fn parse(data: &[u8]) -> Result<Self, ParseError> {
        // Check minimum size for ELF header
        if data.len() < 64 {
            return Err(ParseError::TooShort);
        }

        // Check ELF magic
        if &data[0..4] != b"\x7FELF" {
            return Err(ParseError::InvalidMagic);
        }

        // Check 64-bit
        if data[4] != 2 {
            return Err(ParseError::Not64Bit);
        }

        // Check little-endian
        if data[5] != 1 {
            return Err(ParseError::NotLittleEndian);
        }

        // Check relocatable file (e_type == ET_REL == 1)
        let e_type = u16::from_le_bytes([data[16], data[17]]);
        if e_type != 1 {
            return Err(ParseError::NotRelocatable);
        }

        // Check x86-64 (e_machine == EM_X86_64 == 0x3E)
        let e_machine = u16::from_le_bytes([data[18], data[19]]);
        if e_machine != 0x3E {
            return Err(ParseError::NotX86_64);
        }

        // Parse header fields
        let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap()) as usize;
        let e_shentsize = u16::from_le_bytes([data[58], data[59]]) as usize;
        let e_shnum = u16::from_le_bytes([data[60], data[61]]) as usize;
        let e_shstrndx = u16::from_le_bytes([data[62], data[63]]) as usize;

        // Parse section headers
        let mut sections = Vec::new();
        let mut section_map = HashMap::new();
        let mut symtab_idx = None;
        let mut strtab_idx = None;

        // First pass: collect section info
        struct RawSection {
            name_offset: u32,
            sh_type: u32,
            flags: u64,
            offset: u64,
            size: u64,
            link: u32,
            info: u32,
            align: u64,
            entsize: u64,
        }

        let mut raw_sections = Vec::new();

        for i in 0..e_shnum {
            let sh_offset = e_shoff + i * e_shentsize;
            if sh_offset + e_shentsize > data.len() {
                return Err(ParseError::InvalidSection("section header out of bounds".into()));
            }

            let sh = &data[sh_offset..sh_offset + e_shentsize];
            let name_offset = u32::from_le_bytes(sh[0..4].try_into().unwrap());
            let sh_type = u32::from_le_bytes(sh[4..8].try_into().unwrap());
            let flags = u64::from_le_bytes(sh[8..16].try_into().unwrap());
            let _addr = u64::from_le_bytes(sh[16..24].try_into().unwrap());
            let offset = u64::from_le_bytes(sh[24..32].try_into().unwrap());
            let size = u64::from_le_bytes(sh[32..40].try_into().unwrap());
            let link = u32::from_le_bytes(sh[40..44].try_into().unwrap());
            let info = u32::from_le_bytes(sh[44..48].try_into().unwrap());
            let align = u64::from_le_bytes(sh[48..56].try_into().unwrap());
            let entsize = u64::from_le_bytes(sh[56..64].try_into().unwrap());

            // SHT_SYMTAB = 2
            if sh_type == 2 {
                symtab_idx = Some(i);
                strtab_idx = Some(link as usize);
            }

            raw_sections.push(RawSection {
                name_offset,
                sh_type,
                flags,
                offset,
                size,
                link,
                info,
                align,
                entsize,
            });
        }

        // Get section name string table
        let shstrtab = &raw_sections[e_shstrndx];
        let shstrtab_data = &data[shstrtab.offset as usize..(shstrtab.offset + shstrtab.size) as usize];

        // Helper to read null-terminated string
        let read_string = |strtab: &[u8], offset: usize| -> Result<String, ParseError> {
            let start = offset;
            let mut end = start;
            while end < strtab.len() && strtab[end] != 0 {
                end += 1;
            }
            String::from_utf8(strtab[start..end].to_vec())
                .map_err(|_| ParseError::InvalidStringTable)
        };

        // Second pass: create sections with names
        for (i, raw) in raw_sections.iter().enumerate() {
            let name = read_string(shstrtab_data, raw.name_offset as usize)?;

            // Skip null section, symtab, strtab, rela sections (we'll handle them separately)
            // SHT_NULL=0, SHT_SYMTAB=2, SHT_STRTAB=3, SHT_RELA=4
            if raw.sh_type == 0 || raw.sh_type == 2 || raw.sh_type == 3 || raw.sh_type == 4 {
                sections.push(Section {
                    name: name.clone(),
                    data: Vec::new(),
                    flags: SectionFlags::empty(),
                    relocations: Vec::new(),
                    align: raw.align,
                });
                if !name.is_empty() {
                    section_map.insert(name, i);
                }
                continue;
            }

            let section_data = if raw.size > 0 && raw.offset > 0 {
                data[raw.offset as usize..(raw.offset + raw.size) as usize].to_vec()
            } else {
                Vec::new()
            };

            let mut flags = SectionFlags::empty();
            if raw.flags & 0x1 != 0 {
                flags |= SectionFlags::WRITE;
            }
            if raw.flags & 0x2 != 0 {
                flags |= SectionFlags::ALLOC;
            }
            if raw.flags & 0x4 != 0 {
                flags |= SectionFlags::EXEC;
            }

            sections.push(Section {
                name: name.clone(),
                data: section_data,
                flags,
                relocations: Vec::new(),
                align: raw.align,
            });

            if !name.is_empty() {
                section_map.insert(name, i);
            }
        }

        // Parse symbol table
        let mut symbols = Vec::new();

        if let (Some(symtab_i), Some(strtab_i)) = (symtab_idx, strtab_idx) {
            let symtab = &raw_sections[symtab_i];
            let strtab = &raw_sections[strtab_i];

            let strtab_data = &data[strtab.offset as usize..(strtab.offset + strtab.size) as usize];
            let symtab_data = &data[symtab.offset as usize..(symtab.offset + symtab.size) as usize];

            let sym_count = symtab.size / symtab.entsize;
            for i in 0..sym_count {
                let sym_offset = (i * symtab.entsize) as usize;
                let sym = &symtab_data[sym_offset..sym_offset + 24];

                let st_name = u32::from_le_bytes(sym[0..4].try_into().unwrap());
                let st_info = sym[4];
                let _st_other = sym[5];
                let st_shndx = u16::from_le_bytes([sym[6], sym[7]]);
                let st_value = u64::from_le_bytes(sym[8..16].try_into().unwrap());
                let st_size = u64::from_le_bytes(sym[16..24].try_into().unwrap());

                let name = read_string(strtab_data, st_name as usize)?;

                let binding = match st_info >> 4 {
                    0 => SymbolBinding::Local,
                    1 => SymbolBinding::Global,
                    2 => SymbolBinding::Weak,
                    _ => SymbolBinding::Local,
                };

                let sym_type = match st_info & 0xf {
                    0 => SymbolType::None,
                    1 => SymbolType::Object,
                    2 => SymbolType::Func,
                    3 => SymbolType::Section,
                    4 => SymbolType::File,
                    _ => SymbolType::None,
                };

                // SHN_UNDEF = 0, SHN_ABS = 0xfff1
                let section_index = if st_shndx == 0 || st_shndx >= 0xff00 {
                    None
                } else {
                    Some(st_shndx as usize)
                };

                symbols.push(Symbol {
                    name,
                    section_index,
                    value: st_value,
                    size: st_size,
                    binding,
                    sym_type,
                });
            }
        }

        // Parse relocations
        for raw in raw_sections.iter() {
            // SHT_RELA = 4
            if raw.sh_type != 4 {
                continue;
            }

            let target_section = raw.info as usize;
            if target_section >= sections.len() {
                continue;
            }

            let rela_data = &data[raw.offset as usize..(raw.offset + raw.size) as usize];
            let rela_count = raw.size / raw.entsize;

            for j in 0..rela_count {
                let rela_offset = (j * raw.entsize) as usize;
                let rela = &rela_data[rela_offset..rela_offset + 24];

                let r_offset = u64::from_le_bytes(rela[0..8].try_into().unwrap());
                let r_info = u64::from_le_bytes(rela[8..16].try_into().unwrap());
                let r_addend = i64::from_le_bytes(rela[16..24].try_into().unwrap());

                let r_sym = (r_info >> 32) as usize;
                let r_type = (r_info & 0xffffffff) as u32;

                sections[target_section].relocations.push(Relocation {
                    offset: r_offset,
                    symbol_index: r_sym,
                    rel_type: RelocationType::from_elf(r_type),
                    addend: r_addend,
                });
            }
        }

        Ok(ObjectFile {
            sections,
            symbols,
            section_map,
        })
    }

    /// Find a symbol by name.
    pub fn find_symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name == name)
    }

    /// Get all global/defined symbols.
    pub fn defined_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(|s| {
            s.section_index.is_some() &&
            (s.binding == SymbolBinding::Global || s.binding == SymbolBinding::Weak)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_error_display() {
        assert_eq!(ParseError::InvalidMagic.to_string(), "invalid ELF magic number");
    }
}
