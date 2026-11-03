//! Bounded Linux x86-64 backend: Core -> typed DAG plan -> instructions -> ELF64.
//! No assembler, LLVM, C or Rust compiler is invoked to build the Mira executable.
use crate::runtime::lower;
use mira_core::*;
use std::collections::BTreeMap;
#[derive(Default)]
pub(crate) struct Code {
    pub(crate) bytes: Vec<u8>,
}
impl Code {
    pub(crate) fn emit(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }
    pub(crate) fn u32(&mut self, x: u32) {
        self.emit(&x.to_le_bytes());
    }
    pub(crate) fn imm(&mut self, prefix: &[u8], x: i64) {
        self.emit(prefix);
        self.emit(&x.to_le_bytes());
    }
    pub(crate) fn jump(&mut self, prefix: &[u8]) -> usize {
        self.emit(prefix);
        let at = self.bytes.len();
        self.u32(0);
        at
    }
    pub(crate) fn patch(&mut self, at: usize, to: usize) {
        self.bytes[at..at + 4].copy_from_slice(&((to as i64 - at as i64 - 4) as i32).to_le_bytes());
    }
}
pub fn compile(program: &Program) -> Result<Vec<u8>> {
    let plan = lower(program)?;
    let executable = crate::composition::compile(program)?;
    let mut code = Code::default();
    // Reserved callee-preserved registers bound recursion/loops in native behavior.
    code.emit(&[0x49, 0x89, 0xe7, 0x49, 0x81, 0xef]);
    code.u32(1024 * 1024);
    code.imm(&[0x49, 0xbe], 1_000_000);
    // Reserve 512 KiB of activation storage above the call stack.
    code.emit(&[0x49, 0x81, 0xef]);
    code.u32(crate::data_runtime::ARENA_BYTES as u32);
    code.emit(&[0x48, 0x89, 0xe3, 0x48, 0x81, 0xec]);
    code.u32(crate::data_runtime::ARENA_BYTES as u32);
    code.emit(&[0x49, 0x89, 0xe4]);
    let mut traps = crate::native_behavior::Traps::new();
    let mut behavior_calls = Vec::new();
    let mut slots = BTreeMap::new();
    let mut cursor = 0u32;
    for op in &plan.instructions {
        let cell = program.root.find_cell(op.cell).ok_or("IR cell missing")?;
        for port in program
            .ports(cell)
            .iter()
            .filter(|p| p.direction == Direction::Out)
        {
            slots.insert((op.cell, port.name.to_string()), (cursor, cursor + 8));
            cursor += 16;
        }
    }
    let mut calls = Vec::new();
    let mut overflow = Vec::new();
    code.emit(&[0x48, 0x81, 0xec]);
    code.u32(cursor);
    fn load(code: &mut Code, offset: u32) {
        code.emit(&[0x48, 0x8b, 0x84, 0x24]);
        code.u32(offset);
    }
    fn flag(code: &mut Code, offset: u32, value: u32) {
        code.emit(&[0x48, 0xc7, 0x84, 0x24]);
        code.u32(offset);
        code.u32(value);
    }
    fn absent(code: &mut Code, offset: u32) -> usize {
        code.emit(&[0x48, 0x83, 0xbc, 0x24]);
        code.u32(offset);
        code.emit(&[0]);
        code.jump(&[0x0f, 0x84])
    }
    fn publish(code: &mut Code, slot: (u32, u32)) {
        code.emit(&[0x48, 0x89, 0x84, 0x24]);
        code.u32(slot.0);
        flag(code, slot.1, 1);
    }
    for (_, presence) in slots.values() {
        flag(&mut code, *presence, 0);
    }
    for op in &plan.instructions {
        let mut skips = Vec::new();
        for input in op.inputs.iter().chain(&op.guards) {
            let slot = *slots
                .get(&(input.cell, input.port.clone()))
                .ok_or("IR dependency missing")?;
            skips.push(absent(&mut code, slot.1));
        }
        if let Some(input) = op.inputs.first() {
            let slot = slots[&(input.cell, input.port.clone())];
            load(&mut code, slot.0);
        }
        match op.definition {
            Definition::Code(module) => {
                for (i, input) in op.inputs.iter().enumerate() {
                    let slot = slots[&(input.cell, input.port.clone())];
                    load(&mut code, slot.0 + (i * 8) as u32);
                    code.emit(&[0x50]);
                }
                let call = code.jump(&[0xe8]);
                behavior_calls.push((
                    call,
                    *executable
                        .entries
                        .get(&op.cell)
                        .ok_or("missing contextual entry")?,
                ));
                if !op.inputs.is_empty() {
                    code.emit(&[0x48, 0x81, 0xc4]);
                    code.u32((op.inputs.len() * 8) as u32);
                }
                let module = mira_core::code::find(&program.code_modules, module)
                    .ok_or("native result definition missing")?;
                if let Some(fields) =
                    port_result_fields(&program.code_modules, &program.global_types, module)?
                {
                    // The function returns its ordinary aggregate address in RAX.
                    // Project each 8-byte record member after a single call.
                    code.emit(&[0x48, 0x89, 0xc2]); // mov rdx,rax
                    for (index, field) in fields.iter().enumerate() {
                        code.emit(&[0x48, 0x8b, 0x82]); // mov rax,[rdx+disp32]
                        code.u32((index * 8) as u32);
                        let slot = *slots
                            .get(&(op.cell, field.name.clone()))
                            .ok_or("named native output slot missing")?;
                        publish(&mut code, slot);
                    }
                } else if let Some(slot) = slots.get(&(op.cell, "out".into())) {
                    publish(&mut code, *slot);
                }
            }
            Definition::Graph => return Err("unlowered Z graph in native plan".into()),
            Definition::Delay | Definition::Neural | Definition::Adaptive(_) => {
                return Err("stateful graph requires the Mira System tick runtime".into());
            }
            Definition::Const | Definition::ConstBool => {
                code.imm(&[0x48, 0xb8], op.parameter);
                publish(&mut code, slots[&(op.cell, "out".into())]);
            }
            Definition::Add | Definition::Mul => {
                code.imm(&[0x48, 0xb9], op.parameter);
                if op.definition == Definition::Add {
                    code.emit(&[0x48, 0x01, 0xc8]);
                } else {
                    code.emit(&[0x48, 0x0f, 0xaf, 0xc1]);
                }
                overflow.push(code.jump(&[0x0f, 0x80]));
                publish(&mut code, slots[&(op.cell, "out".into())]);
            }
            Definition::Branch(cmp, _) => {
                code.imm(&[0x48, 0xb9], op.parameter);
                code.emit(&[0x48, 0x39, 0xc8]);
                let opcode = match cmp {
                    Comparison::Lt => 0x8c,
                    Comparison::Le => 0x8e,
                    Comparison::Gt => 0x8f,
                    Comparison::Ge => 0x8d,
                    Comparison::Eq => 0x84,
                    Comparison::Ne => 0x85,
                };
                let yes = code.jump(&[0x0f, opcode]);
                publish(&mut code, slots[&(op.cell, "no".into())]);
                let done = code.jump(&[0xe9]);
                let at = code.bytes.len();
                code.patch(yes, at);
                publish(&mut code, slots[&(op.cell, "yes".into())]);
                let at = code.bytes.len();
                code.patch(done, at);
            }
            Definition::AsEvent(_) => {
                code.imm(&[0x48, 0xb8], 1);
                publish(&mut code, slots[&(op.cell, "out".into())]);
            }
        }
        for jump in skips {
            let at = code.bytes.len();
            code.patch(jump, at);
        }
    }
    for output in &plan.outputs {
        if let Some(c) = program.root.find_cell(output.endpoint.cell) {
            if program
                .ports(c)
                .iter()
                .any(|p| p.name == output.endpoint.port && matches!(p.ty, Type::Data(_)))
            {
                return Err("native terminal display of aggregates is not yet supported; return a scalar field or run in the editor".into());
            }
        }
        let slot = slots[&(output.endpoint.cell, output.endpoint.port.clone())];
        let skip = absent(&mut code, slot.1);
        load(&mut code, slot.0);
        calls.push(code.jump(&[0xe8]));
        let at = code.bytes.len();
        code.patch(skip, at);
    }
    code.emit(&[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
    let procedures = executable.definitions;
    let labels = crate::native_behavior::emit(
        &mut code,
        &procedures,
        &mut traps,
        &mut behavior_calls,
        crate::native_behavior::Target::Linux,
        &program.data_types(),
    )?;
    let printer = code.bytes.len();
    // i64 decimal writer; unsigned magnitude also handles i64::MIN.
    code.emit(&[
        0x48, 0x83, 0xec, 40, 0x48, 0x8d, 0x74, 0x24, 39, 0xc6, 0x06, 10, 0x41, 0xb8, 1, 0, 0, 0,
        0x45, 0x31, 0xc9, 0x48, 0x85, 0xc0,
    ]);
    let positive = code.jump(&[0x0f, 0x89]);
    code.emit(&[0x48, 0xf7, 0xd8, 0x41, 0xb1, 1]);
    let pos = code.bytes.len();
    code.patch(positive, pos);
    code.emit(&[0x41, 0xba, 10, 0, 0, 0]);
    let digit = code.bytes.len();
    code.emit(&[
        0x31, 0xd2, 0x49, 0xf7, 0xf2, 0x80, 0xc2, 48, 0x48, 0xff, 0xce, 0x88, 0x16, 0x49, 0xff,
        0xc0, 0x48, 0x85, 0xc0,
    ]);
    let again = code.jump(&[0x0f, 0x85]);
    code.patch(again, digit);
    code.emit(&[0x45, 0x84, 0xc9]);
    let no_sign = code.jump(&[0x0f, 0x84]);
    code.emit(&[0x48, 0xff, 0xce, 0xc6, 0x06, 45, 0x49, 0xff, 0xc0]);
    let write = code.bytes.len();
    code.patch(no_sign, write);
    code.emit(&[
        0x4c, 0x89, 0xc2, 0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0x0f, 0x05, 0x48, 0x83, 0xc4, 40,
        0xc3,
    ]);
    for call in calls {
        code.patch(call, printer);
    }
    overflow.append(&mut traps.overflow);
    use crate::linker::{
        self, Layout, Object, Placement, Relocation, RelocationKind, Section, SectionKind, Symbol,
    };
    let mut rodata = Vec::new();
    let mut symbols = vec![Symbol {
        name: "native.entry".into(),
        section: ".text".into(),
        offset: 0,
    }];
    let mut relocations = Vec::new();
    for (jumps, message, status) in [
        (overflow, b"Mira: i64 overflow\n".as_slice(), 70),
        (traps.division, b"Mira: division by zero\n".as_slice(), 71),
        (
            traps.delivery,
            b"Mira: graph output requires exactly one active delivery\n".as_slice(),
            77,
        ),
        (
            traps.budget,
            b"Mira: execution budget exceeded\n".as_slice(),
            72,
        ),
        (
            traps.memory,
            b"Mira: activation data arena exhausted\n".as_slice(),
            75,
        ),
        (
            traps.bounds,
            b"Mira: array index or memory range out of bounds\n".as_slice(),
            76,
        ),
        (
            traps.invalid,
            b"Mira: invalid operand, shift count or I/O port\n".as_slice(),
            74,
        ),
        (
            traps.stack,
            b"Mira: call stack budget exceeded\n".as_slice(),
            73,
        ),
    ] {
        let fail = code.bytes.len();
        for j in jumps {
            code.patch(j, fail);
        }
        code.emit(&[0xb8, 1, 0, 0, 0, 0xbf, 2, 0, 0, 0]);
        let relocation = code.jump(&[0x48, 0x8d, 0x35]);
        code.emit(&[0xba]);
        code.u32(message.len() as u32);
        code.emit(&[0x0f, 0x05, 0xb8, 60, 0, 0, 0, 0xbf]);
        code.u32(status);
        code.emit(&[0x0f, 0x05]);
        let symbol = format!("trap.message.{}", rodata.len());
        symbols.push(Symbol {
            name: symbol.clone(),
            section: ".rodata".into(),
            offset: rodata.len() as u64,
        });
        rodata.extend_from_slice(message);
        relocations.push(Relocation {
            section: ".text".into(),
            offset: relocation as u64,
            symbol,
            addend: 0,
            kind: RelocationKind::Rel32,
        });
    }
    let mut object = Object {
        sections: vec![
            Section::initialized(".text", SectionKind::Code, 16, code.bytes),
            Section::initialized(".rodata", SectionKind::Rodata, 1, rodata),
        ],
        symbols,
        relocations,
    };
    behavior_calls.extend(traps.procedure_addresses);
    crate::native_behavior::link_procedures(&mut object, &labels, &behavior_calls)?;
    let image = linker::link(
        &object,
        &Layout {
            base: 0x401000,
            limit: 0x401000 + linker::MAX_IMAGE_BYTES,
            sections: vec![
                Placement {
                    section: ".text".into(),
                    address: Some(0x401000),
                },
                Placement {
                    section: ".rodata".into(),
                    address: None,
                },
            ],
        },
    )?;
    let mut elf = crate::bootstrap_scalar::elf_header(
        4096 + image.bytes.len(),
        image.symbols["native.entry"],
    )?;
    elf.extend(image.bytes);
    Ok(elf)
}
