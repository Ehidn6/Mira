//! Freestanding QEMU/Multiboot x86-64 profile. No Linux syscalls or hosted runtime.
use crate::native_state as shared;
use crate::{
    native::Code,
    native_behavior::{self, Target, Traps},
};
use mira_core::*;

pub(crate) fn payload(program: &Program, target: Target) -> Result<Vec<u8>> {
    let (lowered, values) = crate::kernel_state::lower(program, &Default::default())?;
    let source_revision = crate::inspect::revision_digest(program);
    payload_lowered(
        &lowered,
        &source_revision,
        target,
        &values,
        &Default::default(),
    )
}
pub(crate) fn payload_system(system: &system::System, target: Target) -> Result<Vec<u8>> {
    let (lowered, values) = crate::kernel_state::lower(&system.program, &system.parameters)?;
    let source_revision = crate::inspect::revision_digest(&system.program);
    payload_lowered(&lowered, &source_revision, target, &values, &system.buffers)
}
fn payload_lowered(
    program: &Program,
    source_revision: &blake3::Hash,
    target: Target,
    values: &[i64],
    buffers: &std::collections::BTreeMap<Id, data::Buffer>,
) -> Result<Vec<u8>> {
    program.validate()?;
    let mut checked = program.clone();
    crate::composition::infer(&mut checked)?;
    if checked.code_modules != program.code_modules {
        return Err("kernel code signatures are stale".into());
    }
    let cell = program
        .root
        .cells
        .iter()
        .find(|c| c.name == "kernel")
        .ok_or(
            "build-kernel needs a root cell named kernel (or a standalone kernel() definition)",
        )?;
    if !matches!(cell.definition, Definition::Code(_)) {
        return Err("the kernel entry must have a code body; it may call child cells including graph bodies".into());
    }
    if let Definition::Code(id) = cell.definition {
        if program
            .code_modules
            .iter()
            .any(|m| m.id == id && m.body.is_empty())
        {
            return Err("kernel entry is empty: write its algorithm before building".into());
        }
    }
    if program
        .ports(cell)
        .iter()
        .any(|p| p.direction == Direction::In)
    {
        return Err("the first kernel entry must have no inputs; use sys.boot_info() for startup information".into());
    }
    let executable = crate::composition::compile_with_source_revision(program, source_revision)?;
    let mut values = values.to_vec();
    let mut buffer_slots = std::collections::BTreeMap::new();
    let references = crate::tensor::persistent_references(&executable.definitions)?;
    for id in buffers.keys() {
        if !references.contains_key(id) {
            return Err("initialized buffer has no persistent accessor".into());
        }
    }
    for (cell, ty) in references {
        let owner = program
            .root
            .find_cell(cell)
            .ok_or("persistent owner missing")?;
        let ports = program.ports(owner);
        if ports.iter().any(|p| p.direction == Direction::In)
            || !ports
                .iter()
                .any(|p| p.direction == Direction::Out && p.ty == Type::Data(ty))
        {
            return Err("persistent buffers require a dedicated typed cell without inputs".into());
        }
        let definition = data::find(&program.data_types(), ty)?.clone();
        let buffer = buffers.get(&cell).cloned().unwrap_or(data::Buffer {
            ty,
            words: vec![0; definition.len()],
        });
        if buffer.ty != ty {
            return Err("persistent buffer owner/type mismatch".into());
        }
        buffer.validate(&program.data_types())?;
        buffer_slots.insert(cell, (values.len(), ty));
        values.extend(buffer.words);
    }
    let entry = *executable
        .entries
        .get(&cell.id)
        .ok_or("kernel entry missing")?;
    crate::effects::verify_kernel(entry, &executable.definitions)?;
    let mut code = Code::default();
    // Establish the x86-64 FXSAVE contract for generated interrupt entry wrappers.
    // CR0: clear EM/TS, set MP. CR4: enable OSFXSR and OSXMMEXCPT.
    code.emit(&[
        0x0f, 0x20, 0xc0, 0x48, 0x83, 0xe0, 0xf3, 0x48, 0x83, 0xc8, 0x02, 0x0f, 0x22, 0xc0,
    ]);
    code.emit(&[
        0x0f, 0x20, 0xe0, 0x48, 0x0d, 0x00, 0x06, 0x00, 0x00, 0x0f, 0x22, 0xe0,
    ]);
    // UEFI supplies its allocated stack floor in r15.
    if target == Target::Kernel {
        code.imm(&[0x49, 0xbf], 0x300000);
    }
    // Lower half of the 1 MiB boot stack is activation-owned object storage.
    code.emit(&[0x4d, 0x89, 0xfc, 0x49, 0x81, 0xc7]);
    code.u32(crate::data_runtime::ARENA_BYTES as u32);
    code.emit(&[0x4c, 0x89, 0xfb]);
    // Keep hardware interrupt frames and handler scratch above the object arena.
    code.emit(&[0x49, 0x81, 0xc7]);
    code.u32(8192);
    code.emit(&[
        0x45, 0x31, 0xc0, 0x45, 0x31, 0xc9, 0x45, 0x31, 0xd2, 0x45, 0x31, 0xdb,
    ]);
    // Mira f32 arithmetic uses round-to-nearest with gradual underflow.
    code.emit(&[
        0x48, 0x83, 0xec, 8, 0x48, 0xc7, 0x04, 0x24, 0x80, 0x1f, 0, 0, 0x0f, 0xae, 0x14, 0x24,
        0x48, 0x83, 0xc4, 8,
    ]);
    let call = code.jump(&[0xe8]);
    code.emit(&[0xfa, 0xf4, 0xeb, 0xfd]); // returning from kernel safely stops the CPU
    let mut traps = Traps::new();
    traps.state_words = values.len();
    traps.descriptor_slots = buffer_slots
        .keys()
        .enumerate()
        .map(|(n, id)| (*id, values.len() * 2 + shared::WORDS + n))
        .collect();
    let descriptors = buffer_slots
        .values()
        .map(|(index, _)| (*index * 8) as i64)
        .collect::<Vec<_>>();
    traps.buffer_slots = buffer_slots;
    let mut calls = vec![(call, entry)];
    let labels = native_behavior::emit(
        &mut code,
        &executable.definitions,
        &mut traps,
        &mut calls,
        target,
        &program.data_types(),
    )?;
    for (jumps, message, status) in [
        (
            traps.memory,
            "MIRA TRAP: activation data arena exhausted\n",
            2,
        ),
        (
            traps.bounds,
            "MIRA TRAP: array index or memory range out of bounds\n",
            3,
        ),
        (
            traps.delivery,
            "MIRA TRAP: graph output requires exactly one active delivery\n",
            3,
        ),
        (
            traps.overflow,
            "MIRA TRAP: arithmetic/address overflow\n",
            4,
        ),
        (traps.division, "MIRA TRAP: division by zero\n", 4),
        (
            traps.invalid,
            "MIRA TRAP: invalid numeric value/operation\n",
            4,
        ),
        (traps.stack, "MIRA TRAP: kernel stack exhausted\n", 2),
        (traps.budget, "MIRA TRAP: execution budget\n", 1),
        (
            traps.activation,
            "MIRA TRAP: conflicting/stale neural activation\n",
            5,
        ),
    ] {
        let at = code.bytes.len();
        for j in jumps {
            code.patch(j, at);
        }
        code.emit(&[0x4d, 0x85, 0xd2]);
        let fatal = code.jump(&[0x0f, 0x84]);
        code.emit(&[0x41, 0xb8]);
        code.u32(status);
        code.emit(&[0x4c, 0x89, 0xdc, 0xc3]); // unwind to the checked neural recovery frame
        code.patch(fatal, code.bytes.len());
        code.emit(&[0x66, 0xba, 0xe9, 0]);
        for byte in message.bytes() {
            code.emit(&[0xb0, byte, 0xee]);
        }
        code.emit(&[0xfa, 0xf4, 0xeb, 0xfd]);
    }
    values.extend(values.clone());
    values.extend([0; shared::WORDS]);
    values.extend(descriptors);
    // The same bounded linker lays out both boot profiles. RIP-relative state
    // references are independent of the actual UEFI allocation address.
    use crate::linker::{
        self, Layout, Object, Placement, Relocation, RelocationKind, Section, SectionKind, Symbol,
    };
    let mut state = Vec::with_capacity(values.len() * 8);
    for value in &values {
        state.extend_from_slice(&value.to_le_bytes());
    }
    let mut object = Object {
        sections: vec![
            Section::initialized(".text", SectionKind::Code, 1, code.bytes),
            Section::initialized(".data", SectionKind::Data, 8, state),
        ],
        symbols: vec![
            Symbol {
                name: "kernel.entry".into(),
                section: ".text".into(),
                offset: 0,
            },
            Symbol {
                name: "kernel.state".into(),
                section: ".data".into(),
                offset: 0,
            },
        ],
        relocations: Vec::new(),
    };
    for (at, index) in traps.persistent {
        if index >= values.len() {
            return Err("kernel state slot is out of range".into());
        }
        object.relocations.push(Relocation {
            section: ".text".into(),
            offset: at as u64,
            symbol: "kernel.state".into(),
            addend: i64::try_from(index.checked_mul(8).ok_or("kernel state offset overflow")?)
                .map_err(|_| "kernel state offset overflow")?,
            kind: RelocationKind::Rel32,
        });
    }
    calls.extend(traps.procedure_addresses);
    native_behavior::link_procedures(&mut object, &labels, &calls)?;
    let image = linker::link(
        &object,
        &Layout {
            base: 0,
            // Multiboot loads at 0x101000 immediately before tables at 0x200000.
            // UEFI allocates the complete image independently of its boot stack
            // and tables; it must not inherit that fixed physical-memory gap.
            limit: if target == Target::Uefi {
                linker::MAX_IMAGE_BYTES
            } else {
                0xff000
            },
            sections: vec![
                Placement {
                    section: ".text".into(),
                    address: Some(0),
                },
                Placement {
                    section: ".data".into(),
                    address: None,
                },
            ],
        },
    )?;
    Ok(image.into_flat_bytes())
}

pub fn compile(program: &Program) -> Result<Vec<u8>> {
    envelope(payload(program, Target::Kernel)?)
}
pub fn compile_system(system: &system::System) -> Result<Vec<u8>> {
    envelope(payload_system(system, Target::Kernel)?)
}
fn envelope(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let bootstrap = include_bytes!("../boot/multiboot.bin");
    if bootstrap.len() > 4096 {
        return Err("bootstrap exceeds one page".into());
    }
    // ELF32 is the Multiboot loader envelope; its bootstrap enters long mode
    // before executing the 64-bit Mira payload. All addresses are physical.
    let mut file = vec![0u8; 8192];
    file[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    fn w16(b: &mut [u8], at: usize, n: u16) {
        b[at..at + 2].copy_from_slice(&n.to_le_bytes());
    }
    fn w32(b: &mut [u8], at: usize, n: u32) {
        b[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    w16(&mut file, 16, 2);
    w16(&mut file, 18, 3);
    w32(&mut file, 20, 1);
    w32(&mut file, 24, 0x10000c);
    w32(&mut file, 28, 52);
    w16(&mut file, 40, 52);
    w16(&mut file, 42, 32);
    w16(&mut file, 44, 2);
    let size = 4096 + bytes.len() as u32;
    for (at, offset, address, filesz, memsz, flags) in [
        (52, 4096, 0x100000, size, size, 7),
        (84, 0, 0x200000, 0, 0x200000, 6),
    ] {
        for (delta, value) in [
            (0, 1),
            (4, offset),
            (8, address),
            (12, address),
            (16, filesz),
            (20, memsz),
            (24, flags),
            (28, 4096),
        ] {
            w32(&mut file, at + delta, value);
        }
    }
    file[4096..4096 + bootstrap.len()].copy_from_slice(bootstrap);
    file.extend(bytes);
    Ok(file)
}
