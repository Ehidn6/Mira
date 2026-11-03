//! Code backend: 64-bit words, checked aggregate references and ordered stack arguments.
use crate::{native::Code, native_state as shared};
use mira_core::{code::*, *};
use std::collections::BTreeMap;
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Target {
    Linux,
    Kernel,
    Uefi,
}
pub struct Traps {
    pub overflow: Vec<usize>,
    pub division: Vec<usize>,
    pub budget: Vec<usize>,
    pub stack: Vec<usize>,
    pub invalid: Vec<usize>,
    pub memory: Vec<usize>,
    pub bounds: Vec<usize>,
    pub delivery: Vec<usize>,
    handlers: Vec<(usize, Id, bool)>,
    pub(crate) procedure_addresses: Vec<(usize, Id)>,
    pub(crate) persistent: Vec<(usize, usize)>,
    pub(crate) activation: Vec<usize>,
    pub(crate) buffer_slots: BTreeMap<Id, (usize, Id)>,
    pub(crate) state_words: usize,
    pub(crate) descriptor_slots: BTreeMap<Id, usize>,
}
impl Traps {
    pub fn new() -> Self {
        Self {
            overflow: Vec::new(),
            division: Vec::new(),
            budget: Vec::new(),
            stack: Vec::new(),
            invalid: Vec::new(),
            memory: Vec::new(),
            bounds: Vec::new(),
            delivery: Vec::new(),
            handlers: Vec::new(),
            procedure_addresses: Vec::new(),
            persistent: Vec::new(),
            activation: Vec::new(),
            buffer_slots: BTreeMap::new(),
            state_words: 0,
            descriptor_slots: BTreeMap::new(),
        }
    }
}
pub fn emit(
    code: &mut Code,
    definitions: &[CodeModule],
    traps: &mut Traps,
    calls: &mut Vec<(usize, Id)>,
    target: Target,
    data_types: &[data::Definition],
) -> Result<BTreeMap<Id, usize>> {
    let word_instructions = crate::bootstrap_codegen::instructions()?;
    let lowered=crate::bootstrap_lowering::lower(definitions,data_types)?;
    let mut compiled=BTreeMap::new();
    let mut used=std::collections::BTreeSet::new();
    for (id,body) in &lowered.bodies {
        if let Some(code)=crate::bootstrap_scalar::compile(body,&lowered.signatures,target!=Target::Linux).map_err(|e|format!("{}: {e}",body.name))? {
            used.extend(code.calls.iter().map(|(_,id)|*id));
            compiled.insert(*id,code);
        }
    }
    let mut all_definitions=definitions.to_vec();
    all_definitions.extend(lowered.runtime.into_iter().filter(|d|used.contains(&d.id)));
    let definitions=all_definitions.as_slice();
    let mut labels = BTreeMap::new();
    for d in definitions {
        labels.insert(d.id, code.bytes.len());
        let scalar = compiled.remove(&d.id);
        let mut e = Emitter {
            code,
            definitions,
            traps,
            calls,
            next: 0,
            target,
            data_types,
            word_instructions,
            runtime_memory:d.name.starts_with("#stage1.runtime.memory."),
        };
        let mut env = BTreeMap::new();
        // Check the complete frame before any stack write can enter the arena.
        e.code.emit(&[0x48, 0x89, 0xe2, 0x48, 0x81, 0xea]);
        let guard_at = e.code.bytes.len();
        e.code.u32(0);
        e.code.emit(&[0x4c, 0x39, 0xfa]);
        e.traps.stack.push(e.code.jump(&[0x0f, 0x82]));
        e.code.emit(&[0x55, 0x48, 0x89, 0xe5, 0x48, 0x81, 0xec]);
        let frame_at = e.code.bytes.len();
        e.code.u32(0);
        e.tick();
        let scalar_call = if let Some(scalar) = &scalar {
            // Bridge the existing ordered stack ABI to the Mira-emitted scalar
            // body's private context. The original guards/traps remain in force.
            e.next = 17;
            // Statements and expressions charge inside the Mira-emitted body.
            for (i, _) in d.parameters.iter().enumerate() {
                e.code.emit(&[0x48, 0x8b, 0x85]);
                e.code.u32((16 + (d.parameters.len() - i - 1) * 8) as u32);
                e.store(136 - i * 8);
            }
            e.code.emit(&[0x48,0x8d,0xbd]); e.code.u32((-136i32) as u32); // context
            let call = e.code.jump(&[0xe8]);
            for (error, kind) in [(1, 0), (2, 1), (3, 2), (4, 3), (5, 4), (6, 5), (7, 6)] {
                e.code.emit(&[0x48, 0x83, 0x7d, 0xf8, error]);
                let jump = e.code.jump(&[0x0f, 0x84]);
                match kind {
                    0 => e.traps.overflow.push(jump),
                    1 => e.traps.division.push(jump),
                    2 => e.traps.invalid.push(jump),
                    3 => e.traps.budget.push(jump),
                    4 => e.traps.stack.push(jump),
                    5 => e.traps.bounds.push(jump),
                    _ => e.traps.memory.push(jump),
                }
            }
            Some((call, scalar.stack_bytes))
        } else {
            for (i, p) in d.parameters.iter().enumerate() {
                let slot = e.slot();
                env.insert(p.name.clone(), (slot, p.ty));
                e.code.emit(&[0x48, 0x8b, 0x85]);
                e.code.u32((16 + (d.parameters.len() - i - 1) * 8) as u32);
                e.store(slot);
            }
            e.body(&d.body, &mut env)?;
            if d.output == Type::Unit {
                e.zero();
            }
            None
        };
        e.code.emit(&[0x48, 0x89, 0xec, 0x5d, 0xc3]);
        let size = (e.next * 8 + 15) / 16 * 16;
        e.code.bytes[frame_at..frame_at + 4].copy_from_slice(&(size as u32).to_le_bytes());
        let extra = scalar_call.map_or(0, |(_, stack)| stack);
        e.code.bytes[guard_at..guard_at + 4]
            .copy_from_slice(&((size + 24 + extra) as u32).to_le_bytes());
        if let (Some((call, _)), Some(scalar)) = (scalar_call, scalar) {
            let entry = e.code.bytes.len();
            e.code.patch(call, entry);
            e.code.emit(&scalar.bytes);
            e.calls.extend(scalar.calls.iter().map(|(offset,id)|(entry+offset,*id)));
        }
    }
    // Every address uses RIP-relative relocation, including UEFI payloads.
    let mut wrappers = BTreeMap::new();
    for (fixup, target_id, error_code) in traps.handlers.clone() {
        let entry = *wrappers.entry((target_id, error_code)).or_insert_with(|| {
            let at = code.bytes.len();
            emit_interrupt_entry(code, calls, traps, target_id, error_code);
            at
        });
        code.patch(fixup, entry);
    }
    Ok(labels)
}

/// One symbol/relocation path for calls and address-taking in every image profile.
pub(crate) fn link_procedures(
    object: &mut crate::linker::Object,
    labels: &BTreeMap<Id, usize>,
    references: &[(usize, Id)],
) -> Result<()> {
    use crate::linker::{Relocation, RelocationKind, Symbol};
    let mut defined = std::collections::BTreeSet::new();
    for &(offset, id) in references {
        let target = *labels
            .get(&id)
            .ok_or_else(|| format!("unresolved Mira procedure #{id}"))?;
        let name = format!("mira.procedure.{id}");
        if defined.insert(id) {
            object.symbols.push(Symbol {
                name: name.clone(),
                section: ".text".into(),
                offset: target as u64,
            });
        }
        object.relocations.push(Relocation {
            section: ".text".into(),
            offset: offset as u64,
            symbol: name,
            addend: 0,
            kind: RelocationKind::Rel32,
        });
    }
    Ok(())
}

/// Same-CPL x86-64 interrupt ABI. The caller installs an interrupt gate with IST=0.
/// Preserve all GPRs and FXSAVE state; return to the CPU frame with IRETQ.
fn emit_interrupt_entry(
    code: &mut Code,
    calls: &mut Vec<(usize, Id)>,
    traps: &mut Traps,
    target: Id,
    error_code: bool,
) {
    if !error_code {
        code.emit(&[0x6a, 0]);
    } // Normalize the optional CPU error slot.
    code.emit(&[0x50, 0x51, 0x52, 0x53, 0x55, 0x56, 0x57]);
    for reg in 0..8 {
        code.emit(&[0x41, 0x50 + reg]);
    }
    code.emit(&[0x48, 0x89, 0xe5]); // rbp = saved-register block; nested Mira calls preserve it.
                                    // An interrupt may use the headroom withheld from ordinary Mira frames.
    code.emit(&[0x49, 0x81, 0xef]);
    code.u32(8192);
    // Check FXSAVE + alignment + arguments before writing scratch below the saved block.
    code.emit(&[0x48, 0x8d, 0x85]);
    code.u32((-1584i32) as u32);
    code.emit(&[0x4c, 0x39, 0xf8]);
    traps.stack.push(code.jump(&[0x0f, 0x82]));
    code.emit(&[0xfc]); // The interrupted DF is restored by IRETQ.
    code.emit(&[0x48, 0x8b, 0x45, 0x78]); // rax = normalized error.
    code.emit(&[0x48, 0x8d, 0x8d]);
    code.u32(128); // rcx = RIP/CS/RFLAGS/RSP/SS frame.
    code.emit(&[0x48, 0x83, 0xe4, 0xf0, 0x48, 0x81, 0xec]);
    code.u32(512);
    code.emit(&[0x48, 0x0f, 0xae, 0x04, 0x24]); // fxsave64 [rsp], 16-byte aligned.
                                                // R8 was saved with the interrupted GPRs; IRQ status is CPU-local too.
    code.emit(&[0x45, 0x31, 0xc0, 0x48, 0x83, 0xec, 16]);
    code.emit(&[0x48, 0x81, 0xec]);
    code.u32(1024);
    code.emit(&[0x49, 0x89, 0xe4, 0x48, 0x8d, 0x9c, 0x24]);
    code.u32(1024);
    code.emit(&[0x41, 0xb9, 1, 0, 0, 0, 0x45, 0x31, 0xd2, 0x45, 0x31, 0xdb]);
    code.emit(&[0x51, 0x50]); // Mira stack ABI: (frame, error).
    calls.push((code.jump(&[0xe8]), target));
    code.emit(&[0x48, 0x81, 0xc4]);
    code.u32(1040);
    code.emit(&[0x48, 0x83, 0xc4, 16]);
    code.emit(&[0x48, 0x0f, 0xae, 0x0c, 0x24]); // fxrstor64 [rsp].
    code.emit(&[0x48, 0x89, 0xec]);
    for reg in (0..8).rev() {
        code.emit(&[0x41, 0x58 + reg]);
    }
    code.emit(&[0x5f, 0x5e, 0x5d, 0x5b, 0x5a, 0x59, 0x58]);
    code.emit(&[0x48, 0x83, 0xc4, 8, 0x48, 0xcf]);
}
struct Emitter<'a> {
    runtime_memory:bool,
    word_instructions: &'a crate::bootstrap_codegen::WordInstructions,
    code: &'a mut Code,
    definitions: &'a [CodeModule],
    traps: &'a mut Traps,
    calls: &'a mut Vec<(usize, Id)>,
    next: usize,
    target: Target,
    data_types: &'a [data::Definition],
}
impl Emitter<'_> {
    fn push_rax(&mut self) {
        self.code
            .emit(&[0x48, 0x8d, 0x54, 0x24, 0xe8, 0x4c, 0x39, 0xfa]);
        self.traps.stack.push(self.code.jump(&[0x0f, 0x82]));
        self.code.emit(&[0x50]);
    }
    fn raw_state(&mut self, prefix: &[u8], index: usize) {
        let at = self.code.jump(prefix);
        self.traps.persistent.push((at, index));
    }
    fn persistent_address(&mut self, index: usize) {
        self.code.emit(&[0x4d, 0x85, 0xd2]);
        let published = self.code.jump(&[0x0f, 0x84]);
        self.code.emit(&[0x4c, 0x89, 0xd0, 0x48, 0x05]);
        self.code.u32((index * 8) as u32);
        let done = self.code.jump(&[0xe9]);
        self.here(published);
        self.raw_state(&[0x48, 0x8d, 0x05], index);
        self.raw_state(
            &[0x48, 0x03, 0x05],
            shared::slot(self.traps.state_words, shared::PUBLISHED),
        );
        self.here(done);
    }
    fn resolve_data(&mut self) {
        if self.target != Target::Linux {self.aggregate(8,0);}
    }
    // RCX is the destination; the Mira-generated barrier preserves RAX.
    fn mark_write(&mut self) {
        if self.target != Target::Linux && self.traps.state_words != 0 {
            self.aggregate(9,(self.traps.state_words * 16) as u32);
        }
    }
    fn status(&mut self, status: u32) {
        self.code.emit(&[0x41, 0xb8]);
        self.code.u32(status);
    }
    /// This gate is held only across metadata operations with local IRQs disabled.
    fn gate(&mut self) {
        self.code.emit(&[0x9c, 0xfa]);
        self.raw_state(
            &[0x48, 0x8d, 0x3d],
            shared::slot(self.traps.state_words, shared::GATE),
        );
        let again = self.code.bytes.len();
        self.code
            .emit(&[0x31, 0xc0, 0xba, 1, 0, 0, 0, 0xf0, 0x48, 0x0f, 0xb1, 0x17]);
        let acquired = self.code.jump(&[0x0f, 0x84]);
        self.code.emit(&[0xf3, 0x90]);
        let retry = self.code.jump(&[0xe9]);
        self.code.patch(retry, again);
        self.here(acquired);
    }
    fn ungate(&mut self) {
        self.raw_state(
            &[0x48, 0x8d, 0x3d],
            shared::slot(self.traps.state_words, shared::GATE),
        );
        self.code.emit(&[0x48, 0xc7, 0x07, 0, 0, 0, 0, 0x9d]);
    }
    fn writer_release(&mut self) {
        self.raw_state(
            &[0x48, 0x8d, 0x3d],
            shared::slot(self.traps.state_words, shared::WRITER),
        );
        self.code.emit(&[0x48, 0xc7, 0x07, 0, 0, 0, 0]);
    }
    fn scratch(&mut self, args: &[Expr], env: &mut BTreeMap<String, (usize, Type)>) -> Result<()> {
        let [Expr::Name(target), bytes] = args else {
            return Err("invalid scratch entry".into());
        };
        crate::effects::verify_scratch(target, self.definitions)?;
        let id = self
            .definitions
            .iter()
            .find(|d| d.name == *target)
            .ok_or("scratch entry missing")?
            .id;
        self.expr(bytes, env)?;
        self.code.emit(&[0x48, 0x85, 0xc0]);
        self.traps.invalid.push(self.code.jump(&[0x0f, 0x88]));
        self.code.emit(&[0x4c, 0x01, 0xe0]); // end = cursor + bytes
        self.traps.memory.push(self.code.jump(&[0x0f, 0x82]));
        self.code.emit(&[0x48, 0x39, 0xd8]);
        self.traps.memory.push(self.code.jump(&[0x0f, 0x87]));
        let limit = self.slot();
        let cursor = self.slot();
        self.code.emit(&[0x48, 0x89, 0x9d]);
        self.code.u32((-(limit as i32)) as u32);
        self.code.emit(&[0x4c, 0x89, 0xa5]);
        self.code.u32((-(cursor as i32)) as u32);
        self.code.emit(&[0x48, 0x89, 0xc3]); // restrict the existing allocator
        self.calls.push((self.code.jump(&[0xe8]), id));
        self.code.emit(&[0x4c, 0x8b, 0xa5]);
        self.code.u32((-(cursor as i32)) as u32);
        self.code.emit(&[0x48, 0x8b, 0x9d]);
        self.code.u32((-(limit as i32)) as u32);
        Ok(())
    }
    fn read_scope(
        &mut self,
        args: &[Expr],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        if self.target == Target::Linux {
            return self.scratch(args, env);
        }
        let [target, bytes] = args else {
            return Err("invalid read scope".into());
        };
        self.expr(bytes, env)?;
        let saved = self.slot();
        self.store(saved);
        let local = format!("#read_size_{saved}");
        let mut scoped_env = env.clone();
        scoped_env.insert(local.clone(), (saved, Type::I64));
        let scoped_args = [target.clone(), Expr::Name(local)];
        // A nested neural read uses its private working bank. A standalone reader
        // holds publication stable until every temporary derived pointer dies.
        self.code.emit(&[0x4d, 0x85, 0xd2]);
        let private = self.code.jump(&[0x0f, 0x85]);
        self.gate();
        self.raw_state(
            &[0x48, 0x8d, 0x15],
            shared::slot(self.traps.state_words, shared::READERS),
        );
        self.code.emit(&[0x48, 0xff, 0x02]);
        self.ungate();
        self.scratch(&scoped_args, &mut scoped_env)?;
        self.push_rax();
        self.gate();
        self.raw_state(
            &[0x48, 0x8d, 0x15],
            shared::slot(self.traps.state_words, shared::READERS),
        );
        self.code.emit(&[0x48, 0xff, 0x0a]);
        self.ungate();
        self.code.emit(&[0x58]);
        let done = self.code.jump(&[0xe9]);
        self.here(private);
        self.scratch(&scoped_args, &mut scoped_env)?;
        self.here(done);
        Ok(())
    }
    fn neural_try(
        &mut self,
        args: &[Expr],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        if self.target == Target::Linux {
            return Err("bounded neural execution requires the MiraOS native target".into());
        }
        let [Expr::Name(target), fuel, scratch] = args else {
            return Err("invalid neural entry".into());
        };
        crate::effects::verify_neural(target, self.definitions)?;
        let target = self
            .definitions
            .iter()
            .find(|d| d.name == *target)
            .ok_or("neural entry missing")?
            .id;
        self.expr(fuel, env)?;
        self.push_rax();
        self.expr(scratch, env)?;
        self.code.emit(&[0x48, 0x89, 0xc2, 0x59]);
        let mut early = vec![];
        self.code.emit(&[0x4d, 0x85, 0xd2]);
        early.push(self.code.jump(&[0x0f, 0x85]));
        self.code.emit(&[0x4d, 0x85, 0xc9]);
        early.push(self.code.jump(&[0x0f, 0x85]));
        self.code.emit(&[0x48, 0x85, 0xc9]);
        early.push(self.code.jump(&[0x0f, 0x8e]));
        self.code.emit(&[0x48, 0x81, 0xf9]);
        self.code.u32(10_000_000);
        early.push(self.code.jump(&[0x0f, 0x8f]));
        self.code.emit(&[0x48, 0x85, 0xd2]);
        early.push(self.code.jump(&[0x0f, 0x88]));
        self.code.emit(&[0x4c, 0x89, 0xe0, 0x48, 0x01, 0xd0]);
        early.push(self.code.jump(&[0x0f, 0x82]));
        self.code.emit(&[0x48, 0x39, 0xd8]);
        early.push(self.code.jump(&[0x0f, 0x87]));
        self.code.emit(&[0x48, 0x8d, 0x84, 0x24]);
        self.code.u32((-120i32) as u32);
        self.code.emit(&[0x4c, 0x39, 0xf8]);
        early.push(self.code.jump(&[0x0f, 0x82]));
        // Only one candidate may use the inactive bank. Busy is a recoverable
        // status, not a spin while another CPU performs expensive learning.
        self.code.emit(&[0x31, 0xc0, 0xbe, 1, 0, 0, 0]);
        self.raw_state(
            &[0x48, 0x8d, 0x3d],
            shared::slot(self.traps.state_words, shared::WRITER),
        );
        self.code.emit(&[0xf0, 0x48, 0x0f, 0xb1, 0x37]);
        let busy = self.code.jump(&[0x0f, 0x85]);
        // Baseline version, saved runtime registers, flags and recovery PC.
        self.raw_state(
            &[0x48, 0x8d, 0x05],
            shared::slot(self.traps.state_words, shared::VERSION),
        );
        self.code.emit(&[0x48, 0x8b, 0x00, 0x50]);
        self.code.emit(&[
            0x55, 0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x41, 0x52, 0x41, 0x53,
            0x41, 0x51, 0x9c,
        ]);
        let recover = self.code.jump(&[0x48, 0x8d, 0x05]);
        self.code.emit(&[0x50, 0x49, 0x89, 0xe3]);
        self.code
            .emit(&[0x49, 0x89, 0xce, 0x4c, 0x89, 0xe3, 0x48, 0x01, 0xd3]); // fuel; scratch limit
                                                                            // Recovery must already be active if copying alone exceeds the budget.
        self.code.imm(&[0x49, 0xba], 1);
        self.code.emit(&[0x49, 0x81, 0xee]);
        self.code.u32(self.traps.state_words as u32);
        self.traps.budget.push(self.code.jump(&[0x0f, 0x88]));
        self.raw_state(&[0x48, 0x8d, 0x35], 0);
        self.raw_state(
            &[0x48, 0x03, 0x35],
            shared::slot(self.traps.state_words, shared::PUBLISHED),
        );
        self.raw_state(&[0x48, 0x8d, 0x3d], 0);
        self.code
            .imm(&[0x48, 0xb8], (self.traps.state_words * 8) as i64);
        self.raw_state(
            &[0x48, 0x2b, 0x05],
            shared::slot(self.traps.state_words, shared::PUBLISHED),
        );
        self.code.emit(&[0x48, 0x01, 0xc7, 0x49, 0x89, 0xfa]);
        self.code.imm(&[0x48, 0xb9], self.traps.state_words as i64);
        self.code.emit(&[0xfc, 0xf3, 0x48, 0xa5]);
        self.calls.push((self.code.jump(&[0xe8]), target));
        self.gate();
        self.raw_state(
            &[0x48, 0x8d, 0x15],
            shared::slot(self.traps.state_words, shared::READERS),
        );
        self.code.emit(&[0x48, 0x83, 0x3a, 0]);
        let readers = self.code.jump(&[0x0f, 0x85]);
        self.raw_state(
            &[0x48, 0x8d, 0x15],
            shared::slot(self.traps.state_words, shared::VERSION),
        );
        self.code.emit(&[0x48, 0x8b, 0x02, 0x49, 0x3b, 0x43, 0x58]);
        let stale = self.code.jump(&[0x0f, 0x85]);
        self.raw_state(&[0x48, 0x8d, 0x05], 0);
        self.code
            .emit(&[0x4c, 0x89, 0xd6, 0x48, 0x29, 0xc6, 0x48, 0x89, 0xf0]);
        self.raw_state(
            &[0x48, 0x87, 0x05],
            shared::slot(self.traps.state_words, shared::PUBLISHED),
        );
        self.code.emit(&[0xf0, 0x48, 0xff, 0x02]);
        self.ungate();
        let committed = self.code.jump(&[0xe9]);
        self.here(readers);
        self.here(stale);
        self.ungate();
        self.traps.activation.push(self.code.jump(&[0xe9]));
        self.here(committed);
        self.status(0);
        self.code.emit(&[0x4c, 0x89, 0xdc, 0x48, 0x83, 0xc4, 8]);
        let success = self.code.jump(&[0xe9]);
        self.here(recover); // trap recovery popped the recovery PC, leaving saved flags.
        self.code.emit(&[0x31, 0xc0]);
        let failed = self.code.jump(&[0xe9]);
        self.here(success);
        self.code.imm(&[0x48, 0xb8], 1);
        self.here(failed);
        self.writer_release();
        self.code.emit(&[
            0x9d, 0x41, 0x59, 0x41, 0x5b, 0x41, 0x5a, 0x41, 0x5f, 0x41, 0x5e, 0x41, 0x5d, 0x41,
            0x5c, 0x5b, 0x5d, 0x48, 0x83, 0xc4, 8,
        ]);
        let end = self.code.jump(&[0xe9]);
        for at in early {
            self.here(at);
        }
        self.status(6);
        self.zero();
        let invalid_done = self.code.jump(&[0xe9]);
        self.here(busy);
        self.status(7);
        self.zero();
        self.here(invalid_done);
        self.here(end);
        Ok(())
    }
    fn slot(&mut self) -> usize {
        self.next += 1;
        self.next * 8
    }
    fn load(&mut self, slot: usize) {
        self.code.emit(self.word_instructions.frame(false, 0));
        self.code.u32((-(slot as i32)) as u32);
    }
    fn right(&mut self, slot: usize) {
        self.code.emit(self.word_instructions.frame(false, 1));
        self.code.u32((-(slot as i32)) as u32);
    }
    fn store(&mut self, slot: usize) {
        self.code.emit(self.word_instructions.frame(true, 0));
        self.code.u32((-(slot as i32)) as u32);
    }
    fn zero(&mut self) {
        self.code.emit(&[0x31, 0xc0]);
    }
    fn immediate(&mut self, register: usize, value: i64) {
        self.code.emit(self.word_instructions.immediate(register));
        self.code.emit(&value.to_le_bytes());
    }
    fn charge(&mut self, cost: usize) {
        let inactive = if self.target != Target::Linux {
            self.code.emit(&[0x4d, 0x85, 0xd2]);
            Some(self.code.jump(&[0x0f, 0x84]))
        } else {
            None
        };
        self.code.emit(&[0x49, 0x81, 0xee]);
        self.code.u32(cost as u32);
        self.traps.budget.push(self.code.jump(&[0x0f, 0x88]));
        if let Some(at) = inactive {
            self.here(at);
        }
    }
    fn tick(&mut self) {
        self.charge(1);
    }
    fn here(&mut self, jump: usize) {
        let at = self.code.bytes.len();
        self.code.patch(jump, at);
    }
    fn bool(&mut self, condition: u8) {
        self.code
            .emit(&[0x0f, condition, 0xc0, 0x48, 0x0f, 0xb6, 0xc0]);
    }
    fn checked(&mut self) {
        let j = self.code.jump(&[0x0f, 0x80]);
        self.traps.overflow.push(j);
    }
    fn float_finite(&mut self) {
        self.code.emit(&[
            0x89, 0xc2, 0x81, 0xe2, 0x00, 0x00, 0x80, 0x7f, 0x81, 0xfa, 0x00, 0x00, 0x80, 0x7f,
        ]);
        self.traps.invalid.push(self.code.jump(&[0x0f, 0x84]));
    }
    fn math(&mut self, op: Binary) {
        if matches!(
            op,
            Binary::FAdd
                | Binary::FSub
                | Binary::FMul
                | Binary::FDiv
                | Binary::FEq
                | Binary::FNe
                | Binary::FLt
                | Binary::FLe
                | Binary::FGt
                | Binary::FGe
        ) {
            self.code
                .emit(&[0x66, 0x0f, 0x6e, 0xc0, 0x66, 0x0f, 0x6e, 0xc9]); // movd xmm0,eax; movd xmm1,ecx
            if matches!(
                op,
                Binary::FEq | Binary::FNe | Binary::FLt | Binary::FLe | Binary::FGt | Binary::FGe
            ) {
                self.code.emit(&[0x0f, 0x2e, 0xc1]);
                self.bool(match op {
                    Binary::FEq => 0x94,
                    Binary::FNe => 0x95,
                    Binary::FLt => 0x92,
                    Binary::FLe => 0x96,
                    Binary::FGt => 0x97,
                    Binary::FGe => 0x93,
                    _ => unreachable!(),
                });
            } else {
                if op == Binary::FDiv {
                    self.code
                        .emit(&[0x89, 0xca, 0x81, 0xe2, 0xff, 0xff, 0xff, 0x7f, 0x85, 0xd2]);
                    self.traps.division.push(self.code.jump(&[0x0f, 0x84]));
                }
                self.code.emit(&[
                    0xf3,
                    0x0f,
                    match op {
                        Binary::FAdd => 0x58,
                        Binary::FSub => 0x5c,
                        Binary::FMul => 0x59,
                        Binary::FDiv => 0x5e,
                        _ => unreachable!(),
                    },
                    0xc1,
                    0x66,
                    0x0f,
                    0x7e,
                    0xc0,
                ]);
                self.float_finite();
            }
            return;
        }
        match op {
            Binary::BitAnd => self.code.emit(self.word_instructions.get(3)),
            Binary::BitOr => self.code.emit(self.word_instructions.get(4)),
            Binary::BitXor => self.code.emit(self.word_instructions.get(5)),
            Binary::Shl | Binary::Shr => {
                self.code.emit(&[0x48, 0x83, 0xf9, 63]);
                let j = self.code.jump(&[0x0f, 0x87]);
                self.traps.invalid.push(j);
                self.code.emit(
                    self.word_instructions
                        .get(if op == Binary::Shl { 6 } else { 7 }),
                );
            }
            Binary::Add => {
                self.code.emit(self.word_instructions.get(0));
                self.checked();
            }
            Binary::Sub => {
                self.code.emit(self.word_instructions.get(1));
                self.checked();
            }
            Binary::Mul => {
                self.code.emit(self.word_instructions.get(2));
                self.checked();
            }
            Binary::Div | Binary::Rem => {
                self.code.emit(&[0x48, 0x85, 0xc9]);
                let j = self.code.jump(&[0x0f, 0x84]);
                self.traps.division.push(j);
                self.code.emit(&[0x48, 0x83, 0xf9, 0xff]);
                let other = self.code.jump(&[0x0f, 0x85]);
                self.code.imm(&[0x48, 0xba], i64::MIN);
                self.code.emit(&[0x48, 0x39, 0xd0]);
                let j = self.code.jump(&[0x0f, 0x84]);
                self.traps.overflow.push(j);
                self.here(other);
                self.code.emit(&[0x48, 0x99, 0x48, 0xf7, 0xf9]);
                if op == Binary::Rem {
                    self.code.emit(&[0x48, 0x89, 0xd0]);
                }
            }
            _ => {
                self.code.emit(&[0x48, 0x39, 0xc8]);
                self.bool(match op {
                    Binary::Eq => 0x94,
                    Binary::Ne => 0x95,
                    Binary::Lt => 0x9c,
                    Binary::Le => 0x9e,
                    Binary::Gt => 0x9f,
                    Binary::Ge => 0x9d,
                    _ => unreachable!(),
                });
            }
        }
    }
    fn ty(&self, e: &Expr, env: &BTreeMap<String, (usize, Type)>) -> Result<Type> {
        let vars = env
            .iter()
            .map(|(name, (_, ty))| (name.clone(), *ty))
            .collect();
        crate::behavior::expression_type(e, &vars, self.definitions, self.data_types)
    }
    fn data_id(&self, e: &Expr, env: &BTreeMap<String, (usize, Type)>) -> Result<Id> {
        if let Type::Data(id) = self.ty(e, env)? {
            Ok(id)
        } else {
            Err("expected aggregate value".into())
        }
    }
    /// The Mira compiler owns the bytes and their fixup positions. This adapter
    /// only supplies the already checked layout constant and common trap labels.
    fn aggregate(&mut self, operation:usize, parameter:u32) {
        let template=self.word_instructions.aggregate(operation);
        let start=self.code.bytes.len();
        self.code.emit(&template.bytes);
        for (offset,kind) in &template.fixups {
            let offset=start+offset;
            match kind {
                -1 => self.code.bytes[offset..offset+4].copy_from_slice(&parameter.to_le_bytes()),
                3 => self.traps.invalid.push(offset),
                6 => self.traps.bounds.push(offset),
                7 => self.traps.memory.push(offset),
                8 => self.traps.persistent.push((offset,0)),
                9 => self.traps.persistent.push((offset,shared::slot(self.traps.state_words,shared::PUBLISHED))),
                10 => self.traps.persistent.push((offset,shared::slot(self.traps.state_words,shared::VERSION))),
                _ => unreachable!("validated Mira aggregate fixup"),
            }
        }
    }
    fn place(&mut self, e: &Expr, env: &mut BTreeMap<String, (usize, Type)>) -> Result<()> {
        match e {
            Expr::Field(base, name) => {
                let id = self.data_id(base, env)?;
                let index = data::find(self.data_types, id)?
                    .field(name)
                    .ok_or("unknown or read-only field")?
                    .0;
                self.expr(base, env)?;
                self.resolve_data();
                self.aggregate(0,(index * 8) as u32);
            }
            Expr::Index(base, index) => {
                let id = self.data_id(base, env)?;
                let length = data::find(self.data_types, id)?.len();
                self.expr(base, env)?;
                self.resolve_data();
                self.push_rax();
                self.expr(index, env)?;
                self.aggregate(1,length as u32);
            }
            _ => return Err("not an assignable data member".into()),
        }
        Ok(())
    }
    /// Reserve deterministic worst-case padding so hosted and native scope
    /// budgets agree even when the native arena base has a different alignment.
    fn allocate_data(&mut self, words: usize, aligned: bool) {
        self.charge(words.max(1));
        self.aggregate(if aligned {3}else{2},(words.max(1)*8) as u32);
    }
    fn expr(&mut self, e: &Expr, env: &mut BTreeMap<String, (usize, Type)>) -> Result<()> {
        self.tick();
        match e {
            Expr::Hole(name) => return Err(format!("cannot compile code hole ?{name}")),
            Expr::Construct(id, args) => {
                let words = data::find(self.data_types, *id)?.len();
                self.allocate_data(words, false);
                self.push_rax();
                if args.is_empty() {
                    self.code.emit(&[0x48, 0x89, 0xc7]);
                    self.aggregate(4,words as u32);
                } else {
                    // UTF-8 constructors lower to literal bytes followed by
                    // zeros. Keep initializer order and effects; only a pure
                    // trailing run can be initialized in bulk. Its expression
                    // fuel is still charged, so this is not a budget bypass.
                    let prefix = args
                        .iter()
                        .rposition(|arg| {
                            !matches!(
                                arg,
                                Expr::Int(0) | Expr::Float(0) | Expr::Bool(false) | Expr::Unit
                            )
                        })
                        .map_or(0, |index| index + 1);
                    let prefix = if args.len() - prefix >= 2 {
                        prefix
                    } else {
                        args.len()
                    };
                    for (index, arg) in args[..prefix].iter().enumerate() {
                        self.expr(arg, env)?;
                        self.aggregate(5,(index * 8) as u32);
                    }
                    let zeros = args.len() - prefix;
                    if zeros != 0 {
                        self.charge(zeros);
                        self.code.emit(&[0x48, 0x8b, 0x3c, 0x24]); // rdi = saved object
                        if prefix != 0 {
                            self.code.emit(&[0x48, 0x81, 0xc7]);
                            self.code.u32((prefix * 8) as u32);
                        }
                        self.aggregate(4,zeros as u32);
                    }
                }
                self.code.emit(&[0x58]);
            }
            Expr::View(_, address) => {
                if self.target == Target::Linux {
                    return Err("typed memory view requires build-kernel / UEFI; host memory is not accessible".into());
                }
                self.expr(address, env)?;
                self.aggregate(7,0);
            }
            Expr::Field(base, name) => {
                let id = self.data_id(base, env)?;
                let d = data::find(self.data_types, id)?;
                if name == "len"
                    && matches!(d.kind, data::Kind::Array { .. } | data::Kind::Tensor { .. })
                {
                    let len = d.len();
                    self.expr(base, env)?;
                    self.code.imm(&[0x48, 0xb8], len as i64);
                } else {
                    self.place(e, env)?;
                    self.aggregate(6,0);
                }
            }
            Expr::Index(..) => {
                self.place(e, env)?;
                self.aggregate(6,0);
            }
            Expr::Float(bits) => self.immediate(0, *bits as i64),
            Expr::Int(n) => self.immediate(0, *n),
            Expr::Bool(v) => self.immediate(0, i64::from(*v)),
            Expr::Unit => self.zero(),
            Expr::Name(n) => self.load(
                env.get(n)
                    .ok_or_else(|| format!("{n}: missing native local"))?
                    .0,
            ),
            Expr::Unary(op, e) => {
                self.expr(e, env)?;
                match op {
                    Unary::FNeg => self.code.emit(&[0x35, 0x00, 0x00, 0x00, 0x80]),
                    Unary::Neg => {
                        self.code.emit(&[0x48, 0xf7, 0xd8]);
                        self.checked();
                    }
                    Unary::Not => {
                        self.code.emit(&[0x48, 0x85, 0xc0]);
                        self.bool(0x94);
                    }
                    Unary::Plus => {}
                    Unary::BitNot => self.code.emit(&[0x48, 0xf7, 0xd0]),
                }
            }
            Expr::Choose(c, a, b) => {
                self.expr(c, env)?;
                self.code.emit(&[0x48, 0x85, 0xc0]);
                let otherwise = self.code.jump(&[0x0f, 0x84]);
                self.expr(a, env)?;
                let done = self.code.jump(&[0xe9]);
                self.here(otherwise);
                self.expr(b, env)?;
                self.here(done);
            }
            Expr::Call(name, args) => {
                if name == "sys.procedure_address" {
                    let [Expr::Name(target)] = args.as_slice() else {
                        return Err("procedure_address expects a behavior symbol".into());
                    };
                    let definition = self
                        .definitions
                        .iter()
                        .find(|d| d.name == *target)
                        .ok_or("procedure address target missing")?;
                    let at = self.code.jump(&[0x48, 0x8d, 0x05]);
                    self.traps.procedure_addresses.push((at, definition.id));
                    return Ok(());
                }
                if name == "sys.call_mira64" {
                    // The code address is evaluated first, but is not a Mira argument.
                    self.expr(&args[0], env)?;
                    let address = self.slot();
                    self.store(address);
                    for arg in &args[1..] {
                        self.expr(arg, env)?;
                        self.push_rax();
                    }
                    self.load(address);
                    self.code.emit(&[0x48, 0x85, 0xc0]);
                    self.traps.invalid.push(self.code.jump(&[0x0f, 0x84]));
                    self.code.emit(&[0xff, 0xd0]);
                    if args.len() > 1 {
                        self.code.emit(&[0x48, 0x81, 0xc4]);
                        self.code.u32(((args.len() - 1) * 8) as u32);
                    }
                    return Ok(());
                }
                if let Some(abi) = crate::intrinsics::foreign_abi(name) {
                    return self.foreign_call(abi, name, args, env);
                }
                if name == "sys.procedure_by_id" {
                    self.expr(&args[0],env)?;
                    let identity=self.slot();self.store(identity);
                    let mut found=Vec::new();
                    for definition in self.definitions {
                        self.load(identity);
                        self.immediate(1,definition.id as i64);
                        self.code.emit(&[0x48,0x39,0xc8]);
                        let next=self.code.jump(&[0x0f,0x85]);
                        let address=self.code.jump(&[0x48,0x8d,0x05]);
                        self.traps.procedure_addresses.push((address,definition.id));
                        found.push(self.code.jump(&[0xe9]));
                        self.here(next);
                    }
                    self.zero();
                    for end in found{self.here(end);}
                    return Ok(());
                }
                if name == "sys.address_of" {
                    return self.place(&args[0],env);
                }
                if name == "sys.data_address" {
                    self.expr(&args[0], env)?;
                    self.resolve_data();
                    return Ok(());
                }
                if name == "sys.neural_try" {
                    return self.neural_try(args, env);
                }
                if matches!(name.as_str(), "sys.scratch" | "sys.read_only") {
                    if name == "sys.read_only" {
                        let Some(Expr::Name(target)) = args.first() else {
                            return Err("read-only entry requires a symbol".into());
                        };
                        crate::access::verify_read_only(target, self.definitions, self.data_types)?;
                    }
                    if name == "sys.read_only" && self.target != Target::Linux {
                        return self.read_scope(args, env);
                    }
                    return self.scratch(args, env);
                }
                if let Some(error_code) = crate::intrinsics::handler_kind(name) {
                    if self.target == Target::Linux {
                        return Err(format!(
                            "{name}: interrupt entries require a native kernel target"
                        ));
                    }
                    let [Expr::Name(target)] = args.as_slice() else {
                        return Err("handler expects a behavior symbol".into());
                    };
                    let id = self
                        .definitions
                        .iter()
                        .find(|d| d.name == *target)
                        .ok_or("handler target missing")?
                        .id;
                    let at = self.code.jump(&[0x48, 0x8d, 0x05]);
                    self.traps.handlers.push((at, id, error_code));
                    return Ok(());
                }
                if crate::intrinsics::signature(name).is_some() {
                    return self.intrinsic(name, args, env);
                }
                for arg in args {
                    self.expr(arg, env)?;
                    self.push_rax();
                }
                let id = self
                    .definitions
                    .iter()
                    .find(|d| d.name == *name)
                    .ok_or_else(|| format!("{name}: missing native behavior"))?
                    .id;
                let call = self.code.jump(&[0xe8]);
                self.calls.push((call, id));
                if !args.is_empty() {
                    self.code.emit(&[0x48, 0x81, 0xc4]);
                    self.code.u32((args.len() * 8) as u32);
                }
            }
            Expr::Binary(Binary::And | Binary::Or, a, b) => {
                let op = if let Expr::Binary(op, ..) = e {
                    *op
                } else {
                    unreachable!()
                };
                self.expr(a, env)?;
                self.code.emit(&[0x48, 0x85, 0xc0]);
                let short = self
                    .code
                    .jump(&[0x0f, if op == Binary::And { 0x84 } else { 0x85 }]);
                self.expr(b, env)?;
                self.here(short);
            }
            Expr::Binary(op, a, b) => {
                self.expr(a, env)?;
                self.push_rax();
                self.expr(b, env)?;
                self.code.emit(&[0x48, 0x89, 0xc1, 0x58]);
                self.math(*op);
            }
        }
        Ok(())
    }
    fn atomic(
        &mut self,
        name: &str,
        args: &[Expr],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        let (name, order, width) =
            crate::intrinsics::atomic_operation(name).ok_or("invalid atomic memory order")?;
        let sequential = order == crate::intrinsics::MemoryOrder::SeqCst;
        for a in args {
            self.expr(a, env)?;
            self.push_rax();
        }
        if args.len() == 3 {
            self.code.emit(&[0x59, 0x58, 0x5a]);
        }
        // desired, expected, address
        else if args.len() == 2 {
            self.code.emit(&[0x58, 0x5a]);
        }
        // value, address
        else {
            self.code.emit(&[0x5a]);
        }
        if width < 64 && args.len() > 1 {
            self.code.imm(&[0x48, 0xbe], ((1u64 << width) - 1) as i64);
            self.code.emit(&[0x48, 0x39, 0xf0]);
            self.traps.invalid.push(self.code.jump(&[0x0f, 0x87]));
            if args.len() == 3 {
                self.code.emit(&[0x48, 0x39, 0xf1]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x87]));
            }
        }
        self.code.emit(&[0xf6, 0xc2, width / 8 - 1]);
        self.traps.invalid.push(self.code.jump(&[0x0f, 0x85]));
        if sequential {
            self.code.emit(&[0x0f, 0xae, 0xf0]);
        }
        let locked = matches!(
            name,
            "sys.atomic_fetch_add64" | "sys.atomic_compare_exchange64"
        );
        if locked {
            self.code.emit(&[0xf0]);
        }
        if width == 64 {
            self.code.emit(&[0x48]);
        } else if width == 16 && name != "sys.atomic_load64" {
            self.code.emit(&[0x66]);
        }
        match name {
            "sys.atomic_load64" if width == 8 => self.code.emit(&[0x0f, 0xb6, 0x02]),
            "sys.atomic_load64" if width == 16 => self.code.emit(&[0x0f, 0xb7, 0x02]),
            "sys.atomic_load64" => self.code.emit(&[0x8b, 0x02]),
            "sys.atomic_store64" => self
                .code
                .emit(&[if width == 8 { 0x88 } else { 0x89 }, 0x02]),
            "sys.atomic_exchange64" => self
                .code
                .emit(&[if width == 8 { 0x86 } else { 0x87 }, 0x02]),
            "sys.atomic_fetch_add64" => {
                self.code
                    .emit(&[0x0f, if width == 8 { 0xc0 } else { 0xc1 }, 0x02])
            }
            "sys.atomic_compare_exchange64" => {
                self.code
                    .emit(&[0x0f, if width == 8 { 0xb0 } else { 0xb1 }, 0x0a])
            }
            _ => return Err("unknown atomic operation".into()),
        }
        let unchanged = if name == "sys.atomic_compare_exchange64" {
            Some(self.code.jump(&[0x0f, 0x85]))
        } else {
            None
        };
        if name != "sys.atomic_load64" {
            self.code.emit(&[0x48, 0x89, 0xd1]);
            self.mark_write();
        }
        if let Some(j) = unchanged {
            self.here(j);
        }
        if sequential {
            self.code.emit(&[0x0f, 0xae, 0xf0]);
        }
        if width == 8 {
            self.code.emit(&[0x0f, 0xb6, 0xc0]);
        } else if width == 16 {
            self.code.emit(&[0x0f, 0xb7, 0xc0]);
        }
        if name == "sys.atomic_store64" {
            self.zero();
        }
        Ok(())
    }
    /// Cross an explicit raw-code boundary. Ordinary Mira calls keep their own ABI.
    fn foreign_call(
        &mut self,
        abi: crate::intrinsics::ForeignAbi,
        name: &str,
        args: &[Expr],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        use crate::intrinsics::ForeignAbi;
        let types = args
            .iter()
            .skip(1)
            .map(|arg| self.ty(arg, env))
            .collect::<Result<Vec<_>>>()?;
        // Evaluate once in source order before marshalling, including nested calls.
        let mut values = Vec::new();
        for arg in args {
            self.expr(arg, env)?;
            let slot = self.slot();
            self.store(slot);
            values.push(slot);
        }
        let old_stack = self.slot();
        self.code.emit(&[0x48, 0x89, 0xe0]);
        self.store(old_stack);
        // These are private Mira status/scope registers, caller-saved in both ABIs.
        let mut saved = Vec::new();
        for reg in 8..12 {
            let slot = self.slot();
            self.code.emit(&[0x4c, 0x89, 0xc0 + (reg - 8) * 8]);
            self.store(slot);
            saved.push((reg, slot));
        }
        let registers: &[u8] = match abi {
            ForeignAbi::SysV64 => &[7, 6, 2, 1, 8, 9], // rdi,rsi,rdx,rcx,r8,r9
            ForeignAbi::Win64 => &[1, 2, 8, 9],
        };
        let shadow = if abi == ForeignAbi::Win64 { 32 } else { 0 };
        // SysV has independent GP/SSE register banks; Win64 assigns the first
        // four argument positions to either GP or SSE registers.
        let mut gp = 0usize;
        let mut sse = 0usize;
        let mut stack_words = 0usize;
        let mut places = Vec::new();
        for (i, ty) in types.iter().enumerate() {
            let float = *ty == Type::F32;
            let reg = if abi == ForeignAbi::Win64 {
                (i < 4).then(|| if float { i as u8 } else { registers[i] })
            } else if float && sse < 8 {
                let reg = sse as u8;
                sse += 1;
                Some(reg)
            } else if !float && gp < registers.len() {
                let reg = registers[gp];
                gp += 1;
                Some(reg)
            } else {
                None
            };
            let offset = shadow + stack_words * 8;
            if reg.is_none() {
                stack_words += 1;
            }
            places.push((float, reg, offset));
        }
        let reserve = (shadow + stack_words * 8 + 15) & !15;
        // Preserve headroom for the return address and SysV red zone. A raw
        // external callee must still respect the caller's real stack allocation.
        self.code
            .emit(&[0x48, 0x89, 0xe0, 0x48, 0x83, 0xe0, 0xf0, 0x48, 0x2d]);
        self.code.u32((reserve + 136) as u32);
        self.code.emit(&[0x4c, 0x39, 0xf8]);
        self.traps.stack.push(self.code.jump(&[0x0f, 0x82]));
        self.code.emit(&[0x48, 0x83, 0xe4, 0xf0, 0x48, 0x81, 0xec]);
        self.code.u32(reserve as u32);
        for (slot, &(float, reg, offset)) in values.iter().skip(1).zip(&places) {
            self.load(*slot);
            if let Some(reg) = reg {
                if float {
                    self.code.emit(&[0x66, 0x0f, 0x6e, 0xc0 + reg * 8]); // movd xmmN,eax
                } else {
                    self.code
                        .emit(&[if reg >= 8 { 0x49 } else { 0x48 }, 0x89, 0xc0 + reg % 8]);
                }
            } else {
                self.code.emit(&[0x48, 0x89, 0x84, 0x24]);
                self.code.u32(offset as u32);
            }
        }
        self.load(values[0]);
        self.code.emit(&[0x48, 0x85, 0xc0]);
        self.traps.invalid.push(self.code.jump(&[0x0f, 0x84]));
        self.code.emit(&[0x49, 0x89, 0xc3, 0xb8]);
        self.code.u32(sse as u32); // SysV AL = number of populated SSE registers.
        self.code.emit(&[0xfc, 0x41, 0xff, 0xd3, 0xfc]);
        match name.rsplit('_').next().unwrap() {
            "f32" => {
                self.code.emit(&[0x66, 0x0f, 0x7e, 0xc0]); // movd eax,xmm0
                self.float_finite();
            }
            "bool" => {
                // Only AL belongs to the bool result; upper RAX may be garbage.
                self.code.emit(&[0x84, 0xc0]);
                self.bool(0x95);
            }
            "void" => self.zero(),
            "i32" => self.code.emit(&[0x48, 0x63, 0xc0]), // movsxd rax,eax
            "u32" => self.code.emit(&[0x89, 0xc0]),       // mov eax,eax
            _ => {}
        }
        let result = self.slot();
        self.store(result);
        self.load(old_stack);
        self.code.emit(&[0x48, 0x89, 0xc4]);
        for (reg, slot) in saved {
            self.load(slot);
            self.code.emit(&[0x49, 0x89, 0xc0 + reg - 8]);
        }
        self.load(result);
        Ok(())
    }

    /// Byte operations over explicitly supplied addresses. Copy has memmove
    /// semantics; neither operation proves that the address names mapped RAM.
    fn memory_bulk(
        &mut self,
        copy: bool,
        args: &[Expr],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        let mut slots = Vec::new();
        for arg in args {
            self.expr(arg, env)?;
            let slot = self.slot();
            self.store(slot);
            slots.push(slot);
        }
        if !copy {
            self.load(slots[1]);
            self.code.emit(&[0x48, 0x3d, 0xff, 0, 0, 0]);
            self.traps.invalid.push(self.code.jump(&[0x0f, 0x87]));
        }
        self.load(slots[2]);
        self.code.emit(&[0x48, 0x3d]);
        self.code.u32(crate::intrinsics::MAX_BULK_BYTES);
        self.traps.bounds.push(self.code.jump(&[0x0f, 0x87]));
        self.code.emit(&[0x48, 0x85, 0xc0]);
        let empty = self.code.jump(&[0x0f, 0x84]);
        self.code.emit(&[0x48, 0x89, 0xc1]);
        let inactive = if self.target != Target::Linux {
            self.code.emit(&[0x4d, 0x85, 0xd2]);
            Some(self.code.jump(&[0x0f, 0x84]))
        } else {
            None
        };
        self.code.emit(&[0x49, 0x29, 0xce]); // r14 -= byte count
        self.traps.budget.push(self.code.jump(&[0x0f, 0x88]));
        if let Some(at) = inactive {
            self.here(at);
        }
        self.load(slots[0]);
        self.code.emit(&[0x48, 0x01, 0xc8]);
        self.traps.bounds.push(self.code.jump(&[0x0f, 0x82]));
        if copy {
            self.load(slots[1]);
            self.code.emit(&[0x48, 0x01, 0xc8]);
            self.traps.bounds.push(self.code.jump(&[0x0f, 0x82]));
        }
        self.load(slots[0]);
        self.code.emit(&[0x48, 0x89, 0xc1]);
        self.mark_write();
        self.code.emit(&[0x48, 0x89, 0xc7]); // rdi = destination
        self.load(slots[1]);
        self.code.emit(&[0x48, 0x89, 0xc6]); // rsi = source (or fill byte)
        self.load(slots[2]);
        self.code.emit(&[0x48, 0x89, 0xc1]);
        if copy {
            self.code.emit(&[0x48, 0x39, 0xf7]);
            let forward = self.code.jump(&[0x0f, 0x86]);
            self.code.emit(&[
                0x48, 0x8d, 0x7c, 0x0f, 0xff, 0x48, 0x8d, 0x74, 0x0e, 0xff, 0xfd, 0xf3, 0xa4, 0xfc,
            ]);
            let done = self.code.jump(&[0xe9]);
            self.here(forward);
            self.code.emit(&[0xfc, 0xf3, 0xa4]);
            self.here(done);
        } else {
            self.code.emit(&[0x48, 0x89, 0xf0, 0xfc, 0xf3, 0xaa]);
        }
        self.here(empty);
        self.zero();
        Ok(())
    }
    fn intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        if let Some(id) = crate::intrinsics::aligned_type(name) {
            self.expr(&args[0], env)?;
            let words = data::find(self.data_types, id)?.len();
            self.allocate_data(words, true);
            self.code.emit(&[0x48, 0x89, 0xc2, 0x48, 0x89, 0xc7]);
            self.code.imm(&[0x48, 0xb9], words as i64);
            self.code
                .emit(&[0x31, 0xc0, 0xfc, 0xf3, 0x48, 0xab, 0x48, 0x89, 0xd0]);
            return Ok(());
        }
        if name.starts_with("sys.atomic_") {
            return self.atomic(name, args, env);
        }
        if matches!(name, "sys.memory_copy" | "sys.memory_fill") {
            return self.memory_bulk(name == "sys.memory_copy", args, env);
        }
        if name == "#kernel.require" && self.target == Target::Linux {
            return Err("native neural activation requires a kernel target".into());
        }
        if name == "#graph.require" || name == "#kernel.require" {
            self.expr(&args[0], env)?;
            self.code.emit(&[0x48, 0x85, 0xc0]);
            let at = self.code.jump(&[0x0f, 0x84]);
            if name == "#kernel.require" {
                self.traps.activation.push(at);
            } else {
                self.traps.delivery.push(at);
            }
            self.zero();
            return Ok(());
        }
        if self.target == Target::Linux
            && !(self.runtime_memory && matches!(name,"sys.load64"|"sys.store64"))
            && !name.starts_with("math.")
            && !name.starts_with("#data.identity.")
            && !name.starts_with("#f32.")
            && !crate::intrinsics::pure_address(name)
        {
            return Err(format!(
                "{name}: privileged operation requires build-kernel (freestanding x86-64)"
            ));
        }
        if name == "sys.memory_fence" {
            self.code.emit(&[0x0f, 0xae, 0xf0]);
            self.zero();
            return Ok(());
        }
        if let Some(rest) = name.strip_prefix("#buffer.") {
            let (cell, ty) = rest.split_once('.').ok_or("invalid buffer reference")?;
            let cell = cell.parse::<Id>().map_err(|_| "invalid buffer owner")?;
            let ty = ty.parse::<Id>().map_err(|_| "invalid buffer type")?;
            if self
                .traps
                .buffer_slots
                .get(&cell)
                .is_none_or(|(_, t)| *t != ty)
            {
                return Err("buffer type mismatch".into());
            }
            let descriptor = *self
                .traps
                .descriptor_slots
                .get(&cell)
                .ok_or("buffer descriptor missing")?;
            self.raw_state(&[0x48, 0x8d, 0x05], descriptor);
            self.code.emit(&[0x48, 0x83, 0xc8, 1]);
            return Ok(());
        }
        if let Some(index) = name.strip_prefix("#kernel.slot.") {
            self.persistent_address(index.parse().map_err(|_| "invalid state slot")?);
            return Ok(());
        }
        if name == "sys.neural_status" {
            self.code.emit(&[0x4c, 0x89, 0xc0]);
            return Ok(());
        }
        if matches!(name, "sys.next_tick" | "sys.current_tick") {
            if name == "sys.next_tick" {
                self.code.emit(&[0x9c, 0xfa]);
            }
            self.persistent_address(0);
            self.code.emit(&[0x48, 0x89, 0xc1, 0x48, 0x8b, 0x01]);
            if name == "sys.next_tick" {
                self.code.emit(&[0x48, 0x83, 0xc0, 1]);
                self.checked();
                self.code.emit(&[0x48, 0x89, 0x01]);
                self.mark_write();
                self.code.emit(&[0x9d]);
            }
            return Ok(());
        }
        for arg in args {
            self.expr(arg, env)?;
            self.push_rax();
        }
        if !args.is_empty() {
            self.code.emit(&[0x48, 0x8b, 0x84, 0x24]);
            self.code.u32(((args.len() - 1) * 8) as u32);
            if args.len() > 1 {
                self.code.emit(&[0x48, 0x8b, 0x0c, 0x24]);
            }
            self.code.emit(&[0x48, 0x81, 0xc4]);
            self.code.u32((args.len() * 8) as u32);
        }
        match name {
            "math.wrapping_add" => self.code.emit(self.word_instructions.get(0)),
            "math.wrapping_sub" => self.code.emit(self.word_instructions.get(1)),
            "math.wrapping_mul" => self.code.emit(self.word_instructions.get(2)),
            "math.rotate_left" => self.code.emit(self.word_instructions.get(8)),
            "math.rotate_right" => self.code.emit(self.word_instructions.get(9)),
            "math.byte_swap" => self.code.emit(self.word_instructions.get(10)),
            "math.leading_zeros" | "math.trailing_zeros" => {
                self.code.emit(&[0x48, 0x85, 0xc0]);
                let zero = self.code.jump(&[0x0f, 0x84]);
                self.code.emit(&[
                    0x48,
                    0x0f,
                    if name == "math.leading_zeros" {
                        0xbd
                    } else {
                        0xbc
                    },
                    0xc0,
                ]);
                if name == "math.leading_zeros" {
                    self.code.emit(&[0x48, 0x83, 0xf0, 63]);
                }
                let done = self.code.jump(&[0xe9]);
                self.here(zero);
                self.code.imm(&[0x48, 0xb8], 64);
                self.here(done);
            }
            "math.count_ones" => {
                // At most 64 iterations; baseline x86-64, no POPCNT requirement.
                self.code.emit(&[0x31, 0xc9]);
                let again = self.code.bytes.len();
                self.code.emit(&[0x48, 0x85, 0xc0]);
                let done = self.code.jump(&[0x0f, 0x84]);
                self.code
                    .emit(&[0x48, 0x8d, 0x50, 0xff, 0x48, 0x21, 0xd0, 0x48, 0xff, 0xc1]);
                let repeat = self.code.jump(&[0xe9]);
                self.code.patch(repeat, again);
                self.here(done);
                self.code.emit(&[0x48, 0x89, 0xc8]);
            }
            "#addr.lt" | "#addr.le" | "#addr.gt" | "#addr.ge" => {
                self.code.emit(&[0x48, 0x39, 0xc8]);
                self.bool(match name {
                    "#addr.lt" => 0x92,
                    "#addr.le" => 0x96,
                    "#addr.gt" => 0x97,
                    _ => 0x93,
                });
            }
            "sys.address_distance" => {
                self.code.emit(&[0x48, 0x29, 0xc1]);
                self.traps.overflow.push(self.code.jump(&[0x0f, 0x82]));
                self.code.emit(&[0x48, 0x89, 0xc8, 0x48, 0x85, 0xc0]);
                self.traps.overflow.push(self.code.jump(&[0x0f, 0x88]));
            }
            "math.sqrt" => {
                self.code.emit(&[
                    0x66, 0x0f, 0x6e, 0xc0, 0xf3, 0x0f, 0x51, 0xc0, 0x66, 0x0f, 0x7e, 0xc0,
                ]);
                self.float_finite();
            }
            "math.to_f32" => {
                self.code
                    .emit(&[0xf3, 0x48, 0x0f, 0x2a, 0xc0, 0x66, 0x0f, 0x7e, 0xc0]);
                self.float_finite();
            }
            "math.bits" | "#f32.bits" => {}
            "math.from_bits" | "#f32.from_bits" => {
                self.code
                    .emit(&[0x48, 0x89, 0xc2, 0x48, 0xc1, 0xea, 0x20, 0x48, 0x85, 0xd2]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x85]));
                self.float_finite();
            }
            n if n.starts_with("#data.identity.") => {}
            "#kernel.region_required" => {
                self.code.emit(&[0x4d, 0x85, 0xd2]);
                self.bool(0x95);
            }
            "sys.cli" | "#kernel.cli" => {
                self.code.emit(&[0xfa]);
                self.zero();
            }
            "sys.sti" | "#kernel.sti" => {
                self.code.emit(&[0xfb]);
                self.zero();
            }
            "sys.hlt" => {
                self.code.emit(&[0xf4]);
                self.zero();
            }
            "sys.sti_hlt" => {
                self.code.emit(&[0xfb, 0xf4]);
                self.zero();
            }
            "sys.pause" => {
                self.code.emit(&[0xf3, 0x90]);
                self.zero();
            }
            "sys.breakpoint" => {
                self.code.emit(&[0xcc]);
                self.zero();
            }
            "sys.flags" | "#kernel.flags" => self.code.emit(&[0x9c, 0x58]),
            "sys.cpuid_eax" | "sys.cpuid_ebx" | "sys.cpuid_ecx" | "sys.cpuid_edx" => {
                // CPUID inputs are unsigned 32-bit words, never silently truncated.
                for operand in [0xc2, 0xca] {
                    // mov rdx,rax / mov rdx,rcx
                    self.code
                        .emit(&[0x48, 0x89, operand, 0x48, 0xc1, 0xea, 32, 0x48, 0x85, 0xd2]);
                    self.traps.invalid.push(self.code.jump(&[0x0f, 0x85]));
                }
                self.code.emit(&[0x53, 0x0f, 0xa2]); // RBX is also the Mira arena limit.
                match name {
                    "sys.cpuid_ebx" => self.code.emit(&[0x89, 0xd8]),
                    "sys.cpuid_ecx" => self.code.emit(&[0x89, 0xc8]),
                    "sys.cpuid_edx" => self.code.emit(&[0x89, 0xd0]),
                    _ => self.code.emit(&[0x89, 0xc0]),
                }
                self.code.emit(&[0x5b]);
            }
            "sys.cpu_local" => {
                self.code.emit(&[
                    0xb9, 1, 1, 0, 0xc0, 0x0f, 0x32, 0x48, 0xc1, 0xe2, 32, 0x48, 0x09, 0xd0,
                ]);
            }
            "sys.set_cpu_local" => {
                // The current boot profile uses four-level canonical addressing.
                self.code.emit(&[
                    0x48, 0x89, 0xc2, 0x48, 0xc1, 0xe2, 16, 0x48, 0xc1, 0xfa, 16, 0x48, 0x39, 0xc2,
                ]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x85]));
                self.code.emit(&[
                    0x48, 0x89, 0xc2, 0x48, 0xc1, 0xea, 32, 0xb9, 1, 1, 0, 0xc0, 0x0f, 0x30,
                ]);
                self.zero();
            }
            "sys.read_cr0" => self.code.emit(&[0x0f, 0x20, 0xc0]),
            "sys.read_cr4" => self.code.emit(&[0x0f, 0x20, 0xe0]),
            "sys.write_cr0" => {
                self.code.emit(&[0x0f, 0x22, 0xc0]);
                self.zero();
            }
            "sys.write_cr4" => {
                self.code.emit(&[0x0f, 0x22, 0xe0]);
                self.zero();
            }
            "sys.ltr" => {
                self.code.emit(&[0x48, 0x3d, 0xff, 0xff, 0, 0]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x87]));
                self.code.emit(&[0x0f, 0x00, 0xd8]);
                self.zero();
            }
            "sys.read_msr" | "sys.write_msr" => {
                self.code
                    .emit(&[0x48, 0x89, 0xc2, 0x48, 0xc1, 0xea, 32, 0x48, 0x85, 0xd2]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x85]));
                if name == "sys.read_msr" {
                    self.code.emit(&[
                        0x89, 0xc1, 0x0f, 0x32, 0x48, 0xc1, 0xe2, 32, 0x48, 0x09, 0xd0,
                    ]);
                } else {
                    self.code.emit(&[
                        0x48, 0x89, 0xca, 0x48, 0xc1, 0xea, 32, 0x89, 0xc6, 0x89, 0xc8, 0x89, 0xf1,
                        0x0f, 0x30,
                    ]);
                    self.zero();
                }
            }
            "sys.swapgs" => {
                self.code.emit(&[0x0f, 0x01, 0xf8]);
                self.zero();
            }
            "sys.acquire_fence" => {
                self.code.emit(&[0x0f, 0xae, 0xe8]);
                self.zero();
            }
            "sys.release_fence" => {
                self.code.emit(&[0x0f, 0xae, 0xf8]);
                self.zero();
            }
            "sys.align_up" => {
                self.code.emit(&[0x48, 0x85, 0xc9]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x8e]));
                self.code
                    .emit(&[0x48, 0x89, 0xca, 0x48, 0xff, 0xca, 0x48, 0x85, 0xd1]);
                self.traps.invalid.push(self.code.jump(&[0x0f, 0x85]));
                self.code.emit(&[0x48, 0x01, 0xd0]);
                self.traps.overflow.push(self.code.jump(&[0x0f, 0x82]));
                self.code.emit(&[0x48, 0xf7, 0xd2, 0x48, 0x21, 0xd0]);
            }
            "sys.cs" => self.code.emit(&[0x31, 0xc0, 0x66, 0x8c, 0xc8]),
            "sys.read_cr2" => self.code.emit(&[0x0f, 0x20, 0xd0]),
            "sys.read_cr3" => self.code.emit(&[0x0f, 0x20, 0xd8]),
            "sys.write_cr3" => {
                self.code.emit(&[0x0f, 0x22, 0xd8]);
                self.zero();
            }
            "sys.invlpg" => {
                self.code.emit(&[0x0f, 0x01, 0x38]);
                self.zero();
            }
            "sys.lidt" => {
                self.code.emit(&[0x0f, 0x01, 0x18]);
                self.zero();
            }
            "sys.sidt" => {
                self.code.emit(&[0x0f, 0x01, 0x08]);
                self.zero();
            }
            "sys.lgdt" => {
                self.code.emit(&[0x0f, 0x01, 0x10]);
                self.zero();
            }
            "sys.sgdt" => {
                self.code.emit(&[0x0f, 0x01, 0x00]);
                self.zero();
            }
            "sys.addr" | "sys.address_value" => {}
            "sys.offset" => {
                self.code.emit(&[0x48, 0x85, 0xc9]);
                let neg = self.code.jump(&[0x0f, 0x88]);
                self.code.emit(&[0x48, 0x01, 0xc8]);
                let j = self.code.jump(&[0x0f, 0x82]);
                self.traps.overflow.push(j);
                let done = self.code.jump(&[0xe9]);
                self.here(neg);
                self.code.emit(&[0x48, 0x01, 0xc8]);
                let j = self.code.jump(&[0x0f, 0x83]);
                self.traps.overflow.push(j);
                self.here(done);
            }
            "sys.load8" => self.code.emit(&[0x48, 0x0f, 0xb6, 0x00]),
            "sys.load16" => self.code.emit(&[0x48, 0x0f, 0xb7, 0x00]),
            "sys.load32" => self.code.emit(&[0x8b, 0x00]),
            "sys.load64" => self.code.emit(&[0x48, 0x8b, 0x00]),
            "sys.store8" => {
                self.code.emit(&[0x88, 0x08]);
                self.code.emit(&[0x48, 0x89, 0xc1]);
                self.mark_write();
                self.zero();
            }
            "sys.store16" => {
                self.code.emit(&[0x66, 0x89, 0x08]);
                self.code.emit(&[0x48, 0x89, 0xc1]);
                self.mark_write();
                self.zero();
            }
            "sys.store32" => {
                self.code.emit(&[0x89, 0x08]);
                self.code.emit(&[0x48, 0x89, 0xc1]);
                self.mark_write();
                self.zero();
            }
            "sys.store64" => {
                self.code.emit(&[0x48, 0x89, 0x08]);
                self.code.emit(&[0x48, 0x89, 0xc1]);
                self.mark_write();
                self.zero();
            }
            "sys.boot_info" => {
                if self.target == Target::Uefi {
                    self.code.emit(&[0x4c, 0x89, 0xe8]);
                } else {
                    self.code.imm(&[0x48, 0xb8], 0x203000);
                    self.code.emit(&[0x48, 0x8b, 0x00]);
                }
            }
            "sys.halt" => self.code.emit(&[0xfa, 0xf4, 0xeb, 0xfd]),
            _ => {
                // Reject out-of-range ports instead of silently truncating them.
                self.code.emit(&[0x48, 0x3d, 0xff, 0xff, 0x00, 0x00]);
                let j = self.code.jump(&[0x0f, 0x87]);
                self.traps.invalid.push(j);
                self.code.emit(&[0x48, 0x89, 0xc2]);
                if name.starts_with("sys.out") {
                    self.code.emit(&[0x48, 0x89, 0xc8]);
                    match name {
                        "sys.out8" => self.code.emit(&[0xee]),
                        "sys.out16" => self.code.emit(&[0x66, 0xef]),
                        "sys.out32" => self.code.emit(&[0xef]),
                        _ => return Err("unknown output intrinsic".into()),
                    }
                    self.zero();
                } else {
                    self.zero();
                    match name {
                        "sys.in8" => self.code.emit(&[0xec]),
                        "sys.in16" => self.code.emit(&[0x66, 0xed]),
                        "sys.in32" => self.code.emit(&[0xed]),
                        _ => return Err("unknown input intrinsic".into()),
                    }
                }
            }
        }
        Ok(())
    }
    fn body(
        &mut self,
        body: &[Statement],
        env: &mut BTreeMap<String, (usize, Type)>,
    ) -> Result<()> {
        if body.is_empty() {
            self.zero();
        }
        for stmt in body {
            self.tick();
            match stmt {
                Statement::Set { target, op, value } => {
                    self.place(target, env)?;
                    self.push_rax();
                    if *op != Assign::Set {
                        self.code.emit(&[0x48, 0x8b, 0x00]);
                        self.push_rax();
                    }
                    self.expr(value, env)?;
                    if *op != Assign::Set {
                        self.code.emit(&[0x48, 0x89, 0xc1, 0x58]);
                        self.math(crate::behavior::assign_binary(*op));
                    }
                    self.code.emit(&[0x59, 0x48, 0x89, 0x01]);
                    self.mark_write();
                    self.zero();
                }
                Statement::Expr(e) => self.expr(e, env)?,
                Statement::Bind {
                    name,
                    ty,
                    op,
                    value,
                    ..
                } => {
                    let slot = if let Some(slot) = env.get(name) {
                        slot.0
                    } else {
                        let ty = match ty {
                            Some(ty) => *ty,
                            None => self.ty(value, env)?,
                        };
                        let s = self.slot();
                        env.insert(name.clone(), (s, ty));
                        s
                    };
                    if *op != Assign::Set {
                        self.load(slot);
                        self.push_rax();
                    }
                    self.expr(value, env)?;
                    if *op != Assign::Set {
                        self.code.emit(&[0x48, 0x89, 0xc1, 0x58]);
                        self.math(match op {
                            Assign::FAdd => Binary::FAdd,
                            Assign::FSub => Binary::FSub,
                            Assign::FMul => Binary::FMul,
                            Assign::FDiv => Binary::FDiv,
                            Assign::Add => Binary::Add,
                            Assign::Sub => Binary::Sub,
                            Assign::Mul => Binary::Mul,
                            Assign::Div => Binary::Div,
                            Assign::Rem => Binary::Rem,
                            Assign::Set => unreachable!(),
                        });
                    }
                    self.store(slot);
                    self.zero();
                }
                Statement::If { condition, yes, no } => {
                    self.expr(condition, env)?;
                    self.code.emit(&[0x48, 0x85, 0xc0]);
                    let otherwise = self.code.jump(&[0x0f, 0x84]);
                    self.body(yes, &mut env.clone())?;
                    let done = self.code.jump(&[0xe9]);
                    self.here(otherwise);
                    self.body(no, &mut env.clone())?;
                    self.here(done);
                    if no.is_empty() {
                        self.zero();
                    }
                }
                Statement::While { condition, body } => {
                    let top = self.code.bytes.len();
                    self.tick();
                    self.expr(condition, env)?;
                    self.code.emit(&[0x48, 0x85, 0xc0]);
                    let done = self.code.jump(&[0x0f, 0x84]);
                    self.body(body, &mut env.clone())?;
                    let back = self.code.jump(&[0xe9]);
                    self.code.patch(back, top);
                    self.here(done);
                    self.zero();
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    let index = self.slot();
                    let limit = self.slot();
                    self.expr(start, env)?;
                    self.store(index);
                    self.expr(end, env)?;
                    self.store(limit);
                    let mut inner = env.clone();
                    inner.insert(name.clone(), (index, Type::I64));
                    let top = self.code.bytes.len();
                    self.tick();
                    self.load(index);
                    self.right(limit);
                    self.code.emit(&[0x48, 0x39, 0xc8]);
                    let done = self.code.jump(&[0x0f, 0x8d]);
                    self.body(body, &mut inner)?;
                    self.load(index);
                    self.code.emit(&[0x48, 0x83, 0xc0, 1]);
                    self.checked();
                    self.store(index);
                    let back = self.code.jump(&[0xe9]);
                    self.code.patch(back, top);
                    self.here(done);
                    self.zero();
                }
            }
        }
        Ok(())
    }
}
