//! Bounded static linking for Mira's emitted machine code, not an ELF-object loader.
//! Addresses belong to a caller-selected flat image. Containers and page permissions
//! remain the responsibility of the ELF/boot profile; section kinds do not enforce W^X.
use mira_core::Result;
use std::collections::{BTreeMap, BTreeSet};

/// Bound allocations even when a malformed layout asks for an enormous hole.
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    Code,
    Rodata,
    Data,
    Bss,
}
#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub kind: SectionKind,
    pub alignment: u64,
    pub bytes: Vec<u8>,
    /// Includes zero-initialized storage. Initialized sections must match bytes.len().
    pub size: u64,
}
impl Section {
    pub fn initialized(
        name: impl Into<String>,
        kind: SectionKind,
        alignment: u64,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            name: name.into(),
            kind,
            alignment,
            size: bytes.len() as u64,
            bytes,
        }
    }
    pub fn bss(name: impl Into<String>, alignment: u64, size: u64) -> Self {
        Self {
            name: name.into(),
            kind: SectionKind::Bss,
            alignment,
            bytes: Vec::new(),
            size,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub section: String,
    pub offset: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocationKind {
    /// S + A, checked to fit an unsigned 64-bit address.
    Abs64,
    /// S + A - (P + 4), checked to fit the x86 signed 32-bit displacement.
    Rel32,
}
#[derive(Clone, Debug)]
pub struct Relocation {
    pub section: String,
    pub offset: u64,
    pub symbol: String,
    pub addend: i64,
    pub kind: RelocationKind,
}
#[derive(Clone, Debug, Default)]
pub struct Object {
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
    pub relocations: Vec<Relocation>,
}
#[derive(Clone, Debug)]
pub struct Placement {
    pub section: String,
    /// None places the section immediately after its predecessor, aligned.
    pub address: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct Layout {
    pub base: u64,
    /// Exclusive address limit, including alignment gaps and BSS.
    pub limit: u64,
    /// Every section must occur exactly once; order is explicit and deterministic.
    pub sections: Vec<Placement>,
}
#[derive(Clone, Debug)]
pub struct LinkedSection {
    pub name: String,
    pub kind: SectionKind,
    pub address: u64,
    pub offset: u64,
    pub size: u64,
    pub alignment: u64,
}
#[derive(Clone, Debug)]
pub struct Image {
    pub base: u64,
    /// Initialized image bytes; trailing BSS is omitted.
    pub bytes: Vec<u8>,
    pub memory_size: u64,
    pub sections: Vec<LinkedSection>,
    pub symbols: BTreeMap<String, u64>,
}
impl Image {
    /// Raw boot payloads do not have a loader-level BSS contract, so include zeros.
    pub fn into_flat_bytes(mut self) -> Vec<u8> {
        self.bytes.resize(self.memory_size as usize, 0);
        self.bytes
    }
}
fn aligned(value: u64, alignment: u64) -> Result<u64> {
    if !alignment.is_power_of_two() || alignment > MAX_IMAGE_BYTES {
        return Err(
            "linker: section alignment must be a power of two within the image bound".into(),
        );
    }
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| "linker: alignment overflow".into())
}

pub fn link(object: &Object, layout: &Layout) -> Result<Image> {
    if layout.limit < layout.base || layout.limit - layout.base > MAX_IMAGE_BYTES {
        return Err("linker: invalid or excessive image address range".into());
    }
    let mut inputs = BTreeMap::new();
    for section in &object.sections {
        if section.name.is_empty() || inputs.insert(section.name.as_str(), section).is_some() {
            return Err("linker: empty or duplicate section name".into());
        }
        aligned(0, section.alignment)?;
        if (section.kind == SectionKind::Bss && !section.bytes.is_empty())
            || (section.kind != SectionKind::Bss && section.size != section.bytes.len() as u64)
        {
            return Err(format!(
                "linker: invalid initialized size for {}",
                section.name
            ));
        }
    }
    let mut cursor = layout.base;
    let mut initialized_end = 0;
    let mut placed = BTreeMap::new();
    let mut sections = Vec::new();
    for placement in &layout.sections {
        let section = inputs
            .get(placement.section.as_str())
            .ok_or_else(|| format!("linker: missing section {}", placement.section))?;
        if placed.contains_key(section.name.as_str()) {
            return Err(format!("linker: section {} placed twice", section.name));
        }
        let address = match placement.address {
            Some(address) => address,
            None => aligned(cursor, section.alignment)?,
        };
        if address < cursor || address % section.alignment != 0 {
            return Err(format!(
                "linker: overlapping or misaligned section {}",
                section.name
            ));
        }
        let end = address
            .checked_add(section.size)
            .ok_or("linker: section address overflow")?;
        if address > layout.limit || end > layout.limit {
            return Err(format!(
                "linker: section {} exceeds image address limit",
                section.name
            ));
        }
        let linked = LinkedSection {
            name: section.name.clone(),
            kind: section.kind,
            address,
            offset: address - layout.base,
            size: section.size,
            alignment: section.alignment,
        };
        if !section.bytes.is_empty() {
            initialized_end = end - layout.base;
        }
        placed.insert(section.name.as_str(), linked.clone());
        sections.push(linked);
        cursor = end;
    }
    if placed.len() != inputs.len() {
        return Err("linker: layout omits a section".into());
    }
    let mut symbols = BTreeMap::new();
    for symbol in &object.symbols {
        let section = placed
            .get(symbol.section.as_str())
            .ok_or_else(|| format!("linker: symbol {} refers to a missing section", symbol.name))?;
        if symbol.name.is_empty() || symbol.offset > section.size {
            return Err("linker: empty symbol name or symbol outside section".into());
        }
        let address = section
            .address
            .checked_add(symbol.offset)
            .ok_or("linker: symbol address overflow")?;
        if symbols.insert(symbol.name.clone(), address).is_some() {
            return Err(format!("linker: duplicate symbol {}", symbol.name));
        }
    }
    let mut bytes = vec![0; initialized_end as usize];
    for section in &object.sections {
        let offset = placed[section.name.as_str()].offset as usize;
        if !section.bytes.is_empty() {
            bytes[offset..offset + section.bytes.len()].copy_from_slice(&section.bytes);
        }
    }
    let mut occupied = BTreeSet::new();
    for relocation in &object.relocations {
        let section = placed
            .get(relocation.section.as_str())
            .ok_or("linker: relocation section missing")?;
        let width = match relocation.kind {
            RelocationKind::Abs64 => 8,
            RelocationKind::Rel32 => 4,
        };
        let end = relocation
            .offset
            .checked_add(width)
            .ok_or("linker: relocation offset overflow")?;
        if section.kind == SectionKind::Bss || end > section.size {
            return Err("linker: relocation write outside initialized section".into());
        }
        let offset = section
            .offset
            .checked_add(relocation.offset)
            .ok_or("linker: relocation address overflow")?;
        for byte in offset..offset + width {
            if !occupied.insert(byte) {
                return Err("linker: overlapping relocation writes".into());
            }
        }
        let target = *symbols
            .get(&relocation.symbol)
            .ok_or_else(|| format!("linker: unresolved symbol {}", relocation.symbol))?
            as i128
            + relocation.addend as i128;
        let target_address =
            u64::try_from(target).map_err(|_| "linker: relocation target address overflow")?;
        let at = offset as usize;
        match relocation.kind {
            RelocationKind::Abs64 => {
                bytes[at..at + 8].copy_from_slice(&target_address.to_le_bytes());
            }
            RelocationKind::Rel32 => {
                let next = section.address as i128 + relocation.offset as i128 + 4;
                let value = i32::try_from(target - next)
                    .map_err(|_| "linker: rel32 target out of range")?;
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok(Image {
        base: layout.base,
        bytes,
        memory_size: cursor - layout.base,
        sections,
        symbols,
    })
}
