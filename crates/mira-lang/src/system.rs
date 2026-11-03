//! Linux development host for the portable Mira System model.
use mira_core::{
    system::{Authority, System},
    *,
};
use std::{collections::BTreeMap, io::Write, path::Path};
const MAGIC: &[u8; 8] = b"MIRA\x04\0\0\0";
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// One activation owns one arena and one shared behavior budget. Nested graph
/// calls borrow that storage through the scheduler rather than constructing VMs.
pub struct Host {
    pub(crate) arena: crate::data_runtime::Arena,
    executable: Option<std::rc::Rc<crate::composition::Executable>>,
    fuel: u64,
    inherited_neural: bool,
    pub(crate) scratch_budget: Option<usize>,
}
impl Default for Host {
    fn default() -> Self {
        Self {
            arena: Default::default(),
            executable: None,
            fuel: 1_000_000,
            inherited_neural: false,
            scratch_budget: None,
        }
    }
}
struct ContextCalls<'a> {
    context: &'a mut dyn engine::Calls,
    executable: std::rc::Rc<crate::composition::Executable>,
}
impl crate::behavior::ExternalCalls for ContextCalls<'_> {
    fn invoke(
        &mut self,
        target: crate::behavior::BoundTarget<'_>,
        inputs: &[i64],
        arena: &mut crate::data_runtime::Arena,
        fuel: &mut u64,
        neural: bool,
    ) -> Result<i64> {
        let name = match target {
            crate::behavior::BoundTarget::Cell(id) => format!("#cell.{id}"),
            crate::behavior::BoundTarget::Graph(id, output) => format!("#graph.{id}.{output}"),
        };
        let mut host = Host {
            arena: std::mem::take(arena),
            executable: Some(self.executable.clone()),
            fuel: *fuel,
            inherited_neural: neural,
            scratch_budget: None,
        };
        let result = self.context.invoke(&mut host, &name, inputs);
        *arena = host.arena;
        *fuel = host.fuel;
        result
    }
    fn begin_scope(&mut self) -> Result<()> {
        self.context.begin_scope()
    }
    fn end_scope(&mut self, commit: bool, read_only: bool) -> Result<()> {
        self.context.end_scope(commit, read_only)
    }
}
impl engine::BehaviorHost for Host {
    fn begin_tick(&mut self, system: &System) -> Result<()> {
        self.arena = Default::default();
        self.arena
            .initialize_buffers(&system.program, &system.buffers)?;
        let updates: Vec<_> = system
            .parameters
            .iter()
            .flat_map(|(owner, p)| p.updates.values().map(move |module| (*owner, *module)))
            .collect();
        let executable = crate::compilation_cache::executable(&system.program, &updates)?;
        crate::runtime::initialize_persistent(&system.program, &executable, &mut self.arena)?;
        if let Some(scratch) = self.scratch_budget {
            let used = self.arena.mark().1;
            if scratch > self.arena.limit.saturating_sub(used) {
                return Err("typed execution scratch exceeds available arena".into());
            }
            self.arena.limit = used + scratch;
        }
        self.executable = Some(executable);
        self.fuel = 1_000_000;
        self.inherited_neural = false;
        Ok(())
    }
    fn charge(&mut self) -> Result<()> {
        self.fuel = self
            .fuel
            .checked_sub(1)
            .ok_or("shared behavior execution budget exceeded")?;
        Ok(())
    }
    fn commit_tick(&mut self, system: &mut System) -> Result<()> {
        system.buffers = self.arena.export_buffers()?;
        Ok(())
    }
    fn extract_outputs(
        &self,
        program: &Program,
        module: &code::CodeModule,
        value: i64,
    ) -> Result<BTreeMap<String, i64>> {
        let mut outputs = BTreeMap::new();
        if module.output_ports {
            let fields = port_result_fields(&program.code_modules, &program.global_types, module)?
                .ok_or("port result fields missing")?;
            let Type::Data(id) = module.output else {
                return Err("port result is not a record".into());
            };
            let record = self.arena.object(value)?;
            if record.kind != id || record.words.len() != fields.len() {
                return Err("named result record layout does not match its declaration".into());
            }
            for (field, value) in fields.iter().zip(&record.words) {
                self.validate_value(program, field.ty, *value)?;
                outputs.insert(field.name.clone(), *value);
            }
        } else if module.output != Type::Unit {
            self.validate_value(program, module.output, value)?;
            outputs.insert("out".into(), value);
        }
        Ok(outputs)
    }
    fn validate_value(&self, _program: &Program, ty: Type, value: i64) -> Result<()> {
        match ty {
            Type::Data(id) if self.arena.object(value)?.kind != id => {
                Err("graph signal has the wrong aggregate type".into())
            }
            Type::Data(_) | Type::I64 => Ok(()),
            Type::F32 => mira_core::vm::word_f32(value).map(|_| ()),
            Type::Bool if matches!(value, 0 | 1) => Ok(()),
            Type::Event if value == 1 => Ok(()),
            Type::Unit if value == 0 => Ok(()),
            _ => Err("invalid graph signal or native-only address".into()),
        }
    }
    fn call(
        &mut self,
        program: &Program,
        behavior: Id,
        cell: Option<Id>,
        inputs: &[i64],
    ) -> Result<i64> {
        self.call_bound(program, behavior, cell, inputs, &mut engine::Detached)
    }
    fn call_bound(
        &mut self,
        program: &Program,
        behavior: Id,
        cell: Option<Id>,
        inputs: &[i64],
        context: &mut dyn engine::Calls,
    ) -> Result<i64> {
        if self.executable.is_none() {
            self.executable = Some(crate::compilation_cache::executable(program, &[])?);
        }
        let executable = self.executable.as_ref().unwrap().clone();
        let entry = *executable
            .bound_modules
            .get(&(cell, behavior))
            .ok_or("behavior has no compiled graph activation entry")?;
        let mut calls = ContextCalls {
            context,
            executable: executable.clone(),
        };
        crate::behavior::execute_contextual(
            &executable,
            entry,
            inputs,
            &mut self.arena,
            &mut calls,
            &mut self.fuel,
            self.inherited_neural,
        )
    }
}
pub fn check(system: &System) -> Result<()> {
    check_system(system, false)?;
    crate::information::verify(&system.information)
}
/// Readiness is separate from saving a well-typed but unfinished editor model.
pub fn check_executable(system: &System) -> Result<()> {
    check_system(system, true)
}
fn check_system(system: &System, ready: bool) -> Result<()> {
    system.validate()?;
    fn walk(e: &mira_core::system::System, ready: bool) -> Result<()> {
        crate::access::validate_source(&e.program)?;
        let mut inferred = e.program.clone();
        crate::composition::infer(&mut inferred)?;
        if inferred.code_modules != e.program.code_modules {
            return Err("stored behavior signatures differ from inferred types".into());
        }
        let updates: Vec<_> = e
            .parameters
            .iter()
            .flat_map(|(owner, p)| p.updates.values().map(move |module| (*owner, *module)))
            .collect();
        let references = crate::composition::references_with_updates(&e.program, &updates)?;
        if ready {
            let root = &e.program.root;
            let mut queue: Vec<_> = root.cells.iter().map(|c| (c.id, true)).collect();
            let mut seen = std::collections::BTreeSet::new();
            let cells = mira_core::inspect::cells(&e.program);
            let cell_index: BTreeMap<_, _> = cells.iter().map(|c| (c.cell.id, c)).collect();
            for port in &root.outputs {
                if !root
                    .links
                    .iter()
                    .any(|l| l.to.cell == root.id && l.to.port == port.name)
                {
                    return Err(format!(
                        "{}/{}: missing output producer",
                        e.program.root.name, port.name
                    ));
                }
            }
            while let Some((id, wired)) = queue.pop() {
                if !seen.insert((id, wired)) {
                    continue;
                }
                let info = *cell_index.get(&id).ok_or("executable cell missing")?;
                let c = info.cell;
                if wired {
                    for p in e
                        .program
                        .ports(c)
                        .iter()
                        .filter(|p| p.direction == Direction::In)
                    {
                        if !info
                            .space
                            .links
                            .iter()
                            .any(|l| l.to.cell == id && l.to.port == p.name)
                        {
                            return Err(format!("{}.{}: missing input", c.name, p.name));
                        }
                    }
                }
                if c.definition == Definition::Graph {
                    let graph = c.space.as_ref().ok_or("graph cell has no Z body")?;
                    for output in &graph.outputs {
                        if !graph
                            .links
                            .iter()
                            .any(|l| l.to.cell == graph.id && l.to.port == output.name)
                        {
                            return Err(format!(
                                "{}/{}: missing output producer",
                                c.name, output.name
                            ));
                        }
                    }
                    queue.extend(graph.cells.iter().map(|c| (c.id, true)));
                }
                if let Some(children) = references.calls.get(&id) {
                    queue.extend(children.iter().map(|id| (*id, false)));
                }
                if let Some(outputs) = references.graphs.get(&id) {
                    let s = c.space.as_ref().ok_or("referenced Z graph missing")?;
                    for output in outputs {
                        let mut needed: Vec<_> = s
                            .links
                            .iter()
                            .filter(|l| l.to.cell == s.id && l.to.port == *output)
                            .map(|l| l.from.cell)
                            .collect();
                        if needed.is_empty() {
                            return Err(format!(
                                "{}/graph.{output}: missing output producer",
                                c.name
                            ));
                        }
                        let mut visited = std::collections::BTreeSet::new();
                        while let Some(child) = needed.pop() {
                            if child == s.id || !visited.insert(child) {
                                continue;
                            }
                            s.cell(child)?;
                            queue.push((child, true));
                            needed.extend(
                                s.links
                                    .iter()
                                    .filter(|l| l.to.cell == child)
                                    .map(|l| l.from.cell),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }
    let updates: Vec<_> = system
        .parameters
        .iter()
        .flat_map(|(owner, p)| p.updates.values().map(move |module| (*owner, *module)))
        .collect();
    crate::compilation_cache::validated(&system.program, &updates, ready, || walk(system, ready))
}
/// Advance the single graph scheduler. Typed handles stay owned by Activation;
/// parameters, recurrent state and buffers are committed together or not at all.
pub fn step(
    system: &mut System,
    targets: &BTreeMap<Id, i64>,
) -> Result<crate::runtime::Activation> {
    check_executable(system)?;
    crate::runtime::scheduled(system, &BTreeMap::new(), targets, None)
}
pub fn tick(system: &mut System, targets: &BTreeMap<Id, i64>) -> Result<engine::TickReport> {
    check_executable(system)?;
    engine::tick(system, &BTreeMap::new(), targets, &mut Host::default())
}
pub fn encode(system: &System) -> Result<Vec<u8>> {
    check(system)?;
    let body = serde_json::to_vec(system).map_err(|e| e.to_string())?;
    if body.len() > MAX_BYTES {
        return Err("system storage budget exceeded".into());
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(blake3::hash(&body).as_bytes());
    bytes.extend(body);
    Ok(bytes)
}
pub fn decode(bytes: &[u8]) -> Result<System> {
    if bytes.len() > MAX_BYTES + 40 {
        return Err("system storage budget exceeded".into());
    }
    if !bytes.starts_with(MAGIC) {
        if bytes.starts_with(b"MIRA") {
            return Err("unsupported Mira project format; legacy formats are not loaded".into());
        }
        let source = std::str::from_utf8(bytes).map_err(|_| "unknown Mira storage encoding")?;
        return System::from_program(crate::syntax::parse(source)?);
    }
    if bytes.len() < 40 || blake3::hash(&bytes[40..]).as_bytes() != &bytes[8..40] {
        return Err("Mira storage integrity check failed".into());
    }
    let system: System = serde_json::from_slice(&bytes[40..]).map_err(|e| e.to_string())?;
    check(&system)?;
    Ok(system)
}
pub fn load(path: &Path) -> Result<System> {
    crate::project_files::load(path).map(|(system, _)| system)
}

pub fn save(path: &Path, system: &System) -> Result<()> {
    let bytes = encode(system)?;
    if decode(&bytes)? != *system {
        return Err("system storage changed model".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let tmp = parent.join(format!(".mira-{}-{nonce}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| e.to_string())
}
#[derive(Clone)]
pub struct Entry {
    pub origin: mira_core::transform::Origin,
    pub explanation: String,
    pub label: String,
    pub before: System,
    pub after: System,
}
/// Structural history and learning snapshots have deliberately different lifetimes.
#[derive(Default)]
pub struct History {
    pub undo: Vec<Entry>,
    pub redo: Vec<Entry>,
    pub learning: Vec<(String, System)>,
    pub checkpoints: Vec<(String, System)>,
    high_water: Id,
    revision: u64,
}
impl History {
    pub fn commit<T>(
        &mut self,
        system: &mut System,
        label: &str,
        authority: &Authority,
        edit: impl FnOnce(&mut System) -> Result<T>,
    ) -> Result<T> {
        let mut result = None;
        let mut draft = system.clone();
        draft.program.next_identity = draft.program.next_identity.max(self.high_water);
        draft.revision = draft.revision.max(self.revision);
        let before = draft.edit(authority, |s| {
            result = Some(edit(s)?);
            Ok(())
        })?;
        check(&draft)?;
        let mut comparable = draft.clone();
        comparable.revision = before.revision;
        if before == comparable {
            return Ok(result.unwrap());
        }
        self.high_water = self.high_water.max(draft.program.next_identity);
        self.revision = draft.revision;
        self.undo.push(Entry {
            origin: Default::default(),
            explanation: String::new(),
            label: label.into(),
            before,
            after: draft.clone(),
        });
        if self.undo.len() > 80 {
            self.undo.remove(0);
        }
        self.redo.clear();
        *system = draft;
        Ok(result.unwrap())
    }
    fn restore(&mut self, system: &mut System, mut snapshot: System) -> Result<()> {
        // Structural undo/rollback must not erase shared activity or old runs.
        snapshot.information = system.information.clone();
        self.high_water = self
            .high_water
            .max(system.program.next_identity)
            .max(snapshot.program.next_identity);
        self.revision = self
            .revision
            .max(system.revision)
            .checked_add(1)
            .ok_or("revision exhausted")?;
        snapshot.program.next_identity = self.high_water;
        snapshot.revision = self.revision;
        check(&snapshot)?;
        *system = snapshot;
        Ok(())
    }
    fn preserve_runtime(snapshot: &mut System, current: &System, entry: &Entry) {
        fn walk(
            target: &mut mira_core::system::System,
            current: &mira_core::system::System,
            before: &mira_core::system::System,
            after: &mira_core::system::System,
        ) {
            for (id, p) in &current.parameters {
                let unchanged = before
                    .program
                    .root
                    .find_cell(*id)
                    .zip(after.program.root.find_cell(*id))
                    .is_some_and(|(a, b)| a.definition == b.definition)
                    && before.parameters.get(id) == after.parameters.get(id);
                if unchanged && target.program.root.find_cell(*id).is_some() {
                    target.parameters.insert(*id, p.clone());
                }
            }
            for (id, buffer) in &current.buffers {
                let unchanged = before
                    .program
                    .root
                    .find_cell(*id)
                    .zip(after.program.root.find_cell(*id))
                    .is_some_and(|(a, b)| a.definition == b.definition)
                    && before.buffers.get(id) == after.buffers.get(id);
                if unchanged
                    && target.program.root.find_cell(*id).is_some()
                    && buffer.validate(&target.program.data_types()).is_ok()
                {
                    target.buffers.insert(*id, buffer.clone());
                }
            }
            for (id, v) in &current.delays {
                if before
                    .program
                    .root
                    .find_cell(*id)
                    .zip(after.program.root.find_cell(*id))
                    .is_some_and(|(a, b)| {
                        a.definition == b.definition && a.parameter == b.parameter
                    })
                    && target.program.root.find_cell(*id).is_some()
                {
                    target.delays.insert(*id, *v);
                }
            }
            for (id, memory) in &current.memories {
                if before.memories.get(id) == after.memories.get(id)
                    && (target.program.root.find(*id).is_some()
                        || target.program.root.find_cell(*id).is_some())
                {
                    target.memories.insert(*id, memory.clone());
                }
            }
        }
        walk(snapshot, current, &entry.before, &entry.after);
        snapshot.tick = current.tick;
        snapshot.pending = current
            .pending
            .iter()
            .filter(|((id, name), _)| {
                snapshot
                    .program
                    .root
                    .find(*id)
                    .is_some_and(|s| s.inputs.iter().any(|p| p.name == *name))
            })
            .map(|(k, v)| (k.clone(), *v))
            .collect();
    }
    pub fn undo(&mut self, system: &mut System) -> Result<()> {
        if let Some(entry) = self.undo.last().cloned() {
            let mut previous = entry.before.clone();
            Self::preserve_runtime(&mut previous, system, &entry);
            self.restore(system, previous)?;
            self.undo.pop();
            self.redo.push(entry);
        }
        Ok(())
    }
    pub fn redo(&mut self, system: &mut System) -> Result<()> {
        if let Some(entry) = self.redo.last().cloned() {
            let mut next = entry.after.clone();
            Self::preserve_runtime(&mut next, system, &entry);
            self.restore(system, next)?;
            self.redo.pop();
            self.undo.push(entry);
        }
        Ok(())
    }
    pub fn checkpoint(&mut self, system: &System, label: String, learning: bool) {
        let list = if learning {
            &mut self.learning
        } else {
            &mut self.checkpoints
        };
        list.push((label, system.clone()));
        if list.len() > 16 {
            list.remove(0);
        }
    }
    pub fn rollback(&mut self, system: &mut System, index: usize, learning: bool) -> Result<()> {
        let list = if learning {
            &self.learning
        } else {
            &self.checkpoints
        };
        let snapshot = list.get(index).ok_or("checkpoint missing")?.1.clone();
        if learning {
            // An old topology must never silently overwrite subsequent edits.
            if snapshot.revision != system.revision {
                return Err("learning checkpoint belongs to another structural revision".into());
            }
            let revision = system.revision;
            self.restore(system, snapshot)?;
            system.revision = revision;
        } else {
            let before = system.clone();
            self.restore(system, snapshot)?;
            self.undo.push(Entry {
                origin: Default::default(),
                explanation: String::new(),
                label: "Restore structural checkpoint".into(),
                before,
                after: system.clone(),
            });
            if self.undo.len() > 80 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        Ok(())
    }
}
/// Compile and evaluate a proposed structural transaction before publishing it.
pub fn accept_proposal(
    system: &mut System,
    history: &mut History,
    authority: &Authority,
    proposed: System,
    evaluate: impl FnOnce(&System, &System) -> Result<bool>,
) -> Result<()> {
    if proposed.revision != system.revision {
        return Err("stale structural proposal".into());
    }
    history.commit(system, "Structural proposal", authority, |draft| {
        if !evaluate(draft, &proposed)? {
            return Err("proposal evaluation rejected".into());
        }
        *draft = proposed;
        Ok(())
    })
}

/// Configure a neural cell through the same model used by Studio and CLI.
pub fn configure(system: &mut System, cell: Id, field: &str, value: &str) -> Result<()> {
    let e = system;
    if !e
        .program
        .root
        .find_cell(cell)
        .is_some_and(|c| matches!(c.definition, Definition::Neural | Definition::Adaptive(_)))
    {
        return Err("select a neural or adaptive cell".into());
    }
    let adaptive = matches!(
        e.program
            .root
            .find_cell(cell)
            .ok_or("cell missing")?
            .definition,
        Definition::Adaptive(_)
    );
    let custom_field = field.starts_with("slot:") || field.starts_with("update:");
    if adaptive && field != "trainable" && !custom_field {
        return Err(
            "adaptive cells are programmed with named Mira slots and update behaviors".into(),
        );
    }
    if !adaptive && custom_field {
        return Err("named slots and updates require a user-defined adaptive cell".into());
    }
    let p = e.parameters.entry(cell).or_default();
    use mira_core::neural::{Activation, LearningRule};
    match field {
        "weight" => p.weight = value.parse().map_err(|_| "invalid weight")?,
        "bias" => p.bias = value.parse().map_err(|_| "invalid bias")?,
        "rate" => p.rate = value.parse().map_err(|_| "invalid rate")?,
        "trainable" => p.trainable = value.parse().map_err(|_| "expected true or false")?,
        "activation" => {
            p.activation = match value {
                "identity" => Activation::Identity,
                "tanh" => Activation::Tanh,
                "sigmoid" => Activation::Sigmoid,
                "relu" => Activation::Relu,
                "gelu" => Activation::Gelu,
                _ => return Err(
                    "unknown activation; define adaptive behavior in Mira for a custom activation"
                        .into(),
                ),
            }
        }
        "learning" => {
            p.learning =
                match value {
                    "none" => LearningRule::None,
                    "gradient" => LearningRule::Gradient,
                    "hebbian" => LearningRule::Hebbian,
                    "oja" => LearningRule::Oja,
                    "reward" => LearningRule::RewardModulated,
                    _ => return Err(
                        "unknown built-in learning rule; adaptive types use Mira update behaviors"
                            .into(),
                    ),
                }
        }
        f if f.starts_with("slot:") => {
            p.slots.insert(
                f[5..].into(),
                value.parse().map_err(|_| "invalid slot value")?,
            );
        }
        f if f.starts_with("update:") => {
            let id = e
                .program
                .code_modules
                .iter()
                .find(|m| m.name == value)
                .ok_or("update behavior name missing")?
                .id;
            p.updates.insert(f[7..].into(), id);
        }
        _ => return Err("unknown neural setting".into()),
    }
    Ok(())
}

pub fn command(args: &[String]) -> Result<()> {
    let action = args
        .first()
        .map(String::as_str)
        .ok_or("mira system <check|import|tick|train|configure|source> INPUT [OUTPUT] ...")?;
    let path = Path::new(args.get(1).ok_or("system command needs INPUT")?);
    let mut system = load(path)?;
    let output =
        || -> Result<&Path> { Ok(Path::new(args.get(2).ok_or("system command needs OUTPUT")?)) };
    match action {
        "check" => {
            check_executable(&system)?;
            println!(
                "OK: graph system, revision {}, tick {}",
                system.revision, system.tick
            );
        }
        "context" => {
            let request = crate::context::Request {
                space: args
                    .get(2)
                    .map(|s| s.parse().map_err(|_| "invalid graph"))
                    .transpose()?,
                after: args
                    .get(3)
                    .map(|s| s.parse().map_err(|_| "invalid cursor"))
                    .transpose()?,
                ..Default::default()
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::context::page(&system, &request)?)
                    .map_err(|e| e.to_string())?
            );
        }
        "source" => print!("{}", crate::syntax::format(&system.program)),
        "import" => save(output()?, &system)?,
        "build-kernel" => std::fs::write(output()?, crate::kernel::compile_system(&system)?)
            .map_err(|e| e.to_string())?,
        "build-uefi" => crate::uefi::write_esp(&crate::uefi::compile_system(&system)?, output()?)?,
        "configure" => {
            let cell = args
                .get(3)
                .ok_or("configure needs CELL FIELD VALUE")?
                .parse()
                .map_err(|_| "invalid cell identity")?;
            let field = args.get(4).ok_or("configure needs FIELD")?;
            let value = args.get(5).ok_or("configure needs VALUE")?;
            system.edit(&Authority::default(), |s| configure(s, cell, field, value))?;
            save(output()?, &system)?;
        }
        "tick" | "train" => {
            let mut targets = BTreeMap::new();
            let count_index = if action == "train" {
                let cell = args
                    .get(3)
                    .ok_or("train needs CELL TARGET")?
                    .parse()
                    .map_err(|_| "invalid cell")?;
                let target = args
                    .get(4)
                    .ok_or("train needs TARGET")?
                    .parse()
                    .map_err(|_| "invalid target")?;
                targets.insert(cell, target);
                5
            } else {
                3
            };
            let count: usize = args
                .get(count_index)
                .map(|s| s.parse().map_err(|_| "invalid tick count"))
                .transpose()?
                .unwrap_or(1);
            if count == 0 || count > 10000 {
                return Err("tick count must be 1..10000".into());
            }
            let mut report = None;
            for _ in 0..count {
                report = Some(step(&mut system, &targets)?);
            }
            let report = report.unwrap();
            save(output()?, &system)?;
            println!("tick {} · {} trained cells", system.tick, report.trained);
            let types = system.program.data_types();
            for (name, value) in &report.terminals {
                let ty = report
                    .terminal_types
                    .get(name)
                    .copied()
                    .unwrap_or(Type::I64);
                println!("{name} = {}", report.data.format(&types, ty, *value));
            }
        }
        _ => return Err("unknown system command".into()),
    }
    Ok(())
}

/// Apply a reviewed proposal without coupling the language to any LLM provider.
pub fn accept_graph_proposal(
    system: &mut System,
    history: &mut History,
    authority: &Authority,
    proposal: &mira_core::transform::Proposal,
    evaluate: impl FnOnce(&System, &System) -> Result<bool>,
) -> Result<()> {
    let candidate = proposal.prepare(system)?;
    let revision = system.revision;
    accept_proposal(system, history, authority, candidate, evaluate)?;
    if system.revision != revision {
        if let Some(entry) = history.undo.last_mut() {
            entry.origin = proposal.origin;
            entry.explanation = proposal.explanation.clone();
            entry.label = proposal.label.clone();
        }
    }
    Ok(())
}
