//! Shared passive information and provenance for user-directed hosted executions.
//! Immutable artifacts reference the real Program; they are not a second editor AST.
use mira_core::{information::*, system::System, *};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn origin(producer: &str, inputs: Vec<Reference>) -> Origin {
    Origin {
        producer: producer.into(),
        inputs,
    }
}
fn transaction<T>(system: &mut System, edit: impl FnOnce(&mut System) -> Result<T>) -> Result<T> {
    let mut draft = system.clone();
    let result = edit(&mut draft)?;
    draft.information.extends(&system.information)?;
    crate::system::check(&draft)?;
    *system = draft;
    Ok(result)
}
pub fn create(system: &mut System, text: &str, set_context: bool) -> Result<Reference> {
    transaction(system, |draft| {
        let id = draft.allocate_id()?;
        let r =
            draft
                .information
                .create(id, Content::Text(text.into()), origin("human", vec![]))?;
        if set_context {
            draft.information.current_context = Some(r.clone());
        }
        Ok(r)
    })
}
pub fn revise(system: &mut System, id: Id, expected: u64, text: &str) -> Result<Reference> {
    transaction(system, |draft| {
        let old = Reference {
            object: id,
            version: expected,
        };
        if !matches!(draft.information.get(&old)?.content, Content::Text(_)) {
            return Err("only text content is edited by this operation".into());
        }
        let r = draft.information.revise(
            id,
            expected,
            Content::Text(text.into()),
            origin("human", vec![old]),
        )?;
        if draft
            .information
            .current_context
            .as_ref()
            .is_some_and(|r| r.object == id)
        {
            draft.information.current_context = Some(r.clone());
        }
        Ok(r)
    })
}
pub fn relate(system: &mut System, from: Id, to: Id, kind: &str) -> Result<Reference> {
    transaction(system, |draft| {
        let from = draft.information.latest(from)?;
        let to = draft.information.latest(to)?;
        let id = draft.allocate_id()?;
        draft.information.create(
            id,
            Content::Relation {
                from: from.clone(),
                to: to.clone(),
                kind: kind.into(),
            },
            origin("human", vec![from, to]),
        )
    })
}
pub fn select(system: &mut System, id: Option<Id>) -> Result<()> {
    transaction(system, |draft| {
        draft.information.current_context =
            id.map(|id| draft.information.latest(id)).transpose()?;
        Ok(())
    })
}
fn digest(format: &str, bytes: &[u8]) -> Digest {
    let mut hash = blake3::Hasher::new();
    hash.update(b"mira.artifact/1\0");
    hash.update(&(format.len() as u64).to_le_bytes());
    hash.update(format.as_bytes());
    hash.update(bytes);
    *hash.finalize().as_bytes()
}
fn artifact<T: Serialize>(store: &mut Store, format: &str, value: &T) -> Result<Digest> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let key = digest(format, &bytes);
    if let Some(old) = store.artifacts.get(&key) {
        if old.format != format || old.bytes != bytes {
            return Err("information artifact digest collision".into());
        }
    } else {
        store.artifacts.insert(
            key,
            Artifact {
                format: format.into(),
                bytes,
            },
        );
    }
    Ok(key)
}
fn canonical(program: &Program) -> Result<Program> {
    let mut program = program.clone();
    // Allocator history is not another version of the algorithm. Actual source
    // identities are retained; only the next unused threshold is normalized.
    program.next_identity = 1;
    program.next_identity = program.next_id()?;
    program.validate()?;
    Ok(program)
}
fn program_reference(system: &mut System) -> Result<(Reference, Digest)> {
    let program = canonical(&system.program)?;
    let key = artifact(&mut system.information, PROGRAM_FORMAT, &program)?;
    if let Some(id) = system.information.program {
        let old = system.information.latest(id)?;
        if system.information.get(&old)?.content == (Content::Program { artifact: key }) {
            return Ok((old, key));
        }
        let r = system.information.revise(
            id,
            old.version,
            Content::Program { artifact: key },
            origin("mira.program", vec![old]),
        )?;
        return Ok((r, key));
    }
    let id = system.allocate_id()?;
    let r = system.information.create(
        id,
        Content::Program { artifact: key },
        origin("mira.program", vec![]),
    )?;
    system.information.program = Some(id);
    Ok((r, key))
}
#[derive(Serialize, Deserialize)]
struct State {
    program: Digest,
    revision: u64,
    tick: u64,
    parameters: BTreeMap<Id, mira_core::system::Parameters>,
    buffers: BTreeMap<Id, data::Buffer>,
    delays: BTreeMap<Id, i64>,
    data: BTreeMap<Id, neural::GraphData>,
    protected: std::collections::BTreeSet<Id>,
    memories: BTreeMap<Id, mira_core::system::AssociativeMemory>,
    pending: Vec<((Id, String), i64)>,
}
impl State {
    fn capture(s: &System, program: Digest) -> Self {
        Self {
            program,
            revision: s.revision,
            tick: s.tick,
            parameters: s.parameters.clone(),
            buffers: s.buffers.clone(),
            delays: s.delays.clone(),
            data: s.data.clone(),
            protected: s.protected.clone(),
            memories: s.memories.clone(),
            pending: s.pending.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        }
    }
}
/// Exact aggregate graphs use artifact-local indexes, never live arena handles.
#[derive(Serialize, Deserialize)]
pub enum Value {
    Scalar(i64),
    Object(usize),
}
#[derive(Serialize, Deserialize)]
pub struct CapturedObject {
    pub ty: Id,
    pub fields: Vec<Value>,
}
#[derive(Serialize, Deserialize)]
pub struct Results {
    pub roots: Vec<(String, Type, Value)>,
    pub objects: Vec<CapturedObject>,
}
fn results(program: &Program, activation: &crate::runtime::Activation) -> Result<Results> {
    let mut result = Results {
        roots: vec![],
        objects: vec![],
    };
    let mut known = BTreeMap::new();
    let mut pending = Vec::new();
    fn value(
        ty: Type,
        v: i64,
        arena: &crate::data_runtime::Arena,
        known: &mut BTreeMap<i64, usize>,
        pending: &mut Vec<(i64, Id)>,
        out: &mut Results,
    ) -> Result<Value> {
        if let Type::Data(id) = ty {
            let object = arena.object(v)?;
            if object.kind != id {
                return Err("result aggregate has wrong nominal type".into());
            }
            if let Some(index) = known.get(&v) {
                return Ok(Value::Object(*index));
            }
            let index = out.objects.len();
            known.insert(v, index);
            pending.push((v, id));
            out.objects.push(CapturedObject {
                ty: id,
                fields: vec![],
            });
            Ok(Value::Object(index))
        } else if ty == Type::Addr {
            Err("raw addresses cannot enter a persistent result".into())
        } else {
            Ok(Value::Scalar(v))
        }
    }
    for (name, v) in &activation.terminals {
        let ty = *activation
            .terminal_types
            .get(name)
            .ok_or("result type missing")?;
        let v = value(
            ty,
            *v,
            &activation.data,
            &mut known,
            &mut pending,
            &mut result,
        )?;
        result.roots.push((name.clone(), ty, v));
    }
    let types = program.data_types();
    while let Some((handle, ty)) = pending.pop() {
        let object = activation.data.object(handle)?;
        let members = data::find(&types, ty)?.member_types();
        if members.len() != object.words.len() {
            return Err("result object layout mismatch".into());
        }
        let mut fields = Vec::new();
        for (ty, v) in members.into_iter().zip(&object.words) {
            fields.push(value(
                ty,
                *v,
                &activation.data,
                &mut known,
                &mut pending,
                &mut result,
            )?);
        }
        result.objects[known[&handle]].fields = fields;
    }
    Ok(result)
}
/// User-directed executions share provenance; internal training iterations need
/// not each create a UI/history object. A failure records diagnostics, not state.
pub fn run(
    system: &mut System,
    cell: Option<Id>,
    activation_id: Option<u64>,
    targets: &BTreeMap<Id, i64>,
) -> Result<crate::runtime::Activation> {
    crate::system::check(system)?;
    let mut draft = system.clone();
    let context = draft.information.current_context.clone();
    let (program, key) = program_reference(&mut draft)?;
    let captured = State::capture(&draft, key);
    let input_state = artifact(&mut draft.information, STATE_FORMAT, &captured)?;
    let prepared = draft.clone();
    let outcome = if let Some(cell) = cell {
        if !targets.is_empty() {
            return Err("selected execution does not accept scalar training targets".into());
        }
        let id = activation_id.unwrap_or(draft.tick.checked_add(1).ok_or("tick exhausted")?);
        crate::runtime::execute_cell_system(&mut draft, cell, id)
    } else {
        crate::system::step(&mut draft, targets)
    };
    if outcome.is_err() {
        draft = prepared;
    }
    let mut outputs = Vec::new();
    let mut exact = None;
    if let Ok(activation) = &outcome {
        let captured = results(&draft.program, activation)?;
        exact = Some(artifact(&mut draft.information, RESULT_FORMAT, &captured)?);
        let types = draft.program.data_types();
        for (name, v) in &activation.terminals {
            let ty = *activation
                .terminal_types
                .get(name)
                .ok_or("result type missing")?;
            outputs.push(Observation {
                label: name.clone(),
                ty: crate::behavior::type_name(ty, &types),
                value: activation.data.format(&types, ty, *v),
            });
        }
    }
    let mut inputs = vec![program.clone()];
    if let Some(context) = &context {
        inputs.push(context.clone());
    }
    let id = draft.allocate_id()?;
    draft.information.create(
        id,
        Content::Execution {
            program,
            context,
            target: cell,
            tick: draft.tick,
            success: outcome.is_ok(),
            input_state,
            result: exact,
            outputs,
            error: outcome.as_ref().err().cloned(),
        },
        origin("mira.runtime", inputs),
    )?;
    draft.information.extends(&system.information)?;
    crate::system::check(&draft)?;
    *system = draft;
    outcome
}
fn state_system(state: State, program: Program) -> Result<System> {
    let mut system = System::from_program(program)?;
    system.revision = state.revision;
    system.tick = state.tick;
    system.parameters = state.parameters;
    system.buffers = state.buffers;
    system.delays = state.delays;
    system.data = state.data;
    system.protected = state.protected;
    system.memories = state.memories;
    for (key, value) in state.pending {
        if system.pending.insert(key, value).is_some() {
            return Err("state artifact has duplicate input signals".into());
        }
    }
    system.validate()?;
    Ok(system)
}
fn check_value(
    types: &[data::Definition],
    objects: &[CapturedObject],
    ty: Type,
    value: &Value,
) -> Result<()> {
    match (ty, value) {
        (Type::Data(id), Value::Object(index))
            if objects.get(*index).is_some_and(|o| o.ty == id) =>
        {
            Ok(())
        }
        (Type::Data(_), _) => Err("result artifact has a missing or mistyped aggregate".into()),
        (Type::Addr, _) => Err("native addresses cannot be persistent result values".into()),
        (_, Value::Object(_)) => Err("result artifact uses an aggregate as a scalar".into()),
        (Type::F32, Value::Scalar(v)) => mira_core::vm::word_f32(*v).map(|_| ()),
        (Type::Bool, Value::Scalar(v)) if matches!(v, 0 | 1) => Ok(()),
        (Type::Event, Value::Scalar(1))
        | (Type::Unit, Value::Scalar(0))
        | (Type::I64, Value::Scalar(_)) => Ok(()),
        _ => {
            let _ = types;
            Err("result artifact contains an invalid scalar".into())
        }
    }
}
fn check_results(
    program: &Program,
    target: Option<Id>,
    results: &Results,
    observations: &[Observation],
) -> Result<()> {
    let types = program.data_types();
    if results.objects.len() > crate::data_runtime::ARENA_BYTES / 8
        || results.roots.len() != observations.len()
    {
        return Err("result artifact size or observations mismatch".into());
    }
    let mut expected = BTreeMap::new();
    if let Some(id) = target {
        let cell = program
            .root
            .find_cell(id)
            .ok_or("execution target absent from program version")?;
        let path = crate::runtime::cell_path(program, id).ok_or("execution target path missing")?;
        for port in program
            .ports(cell)
            .iter()
            .filter(|p| p.direction == Direction::Out)
        {
            expected.insert(
                if port.name == "out" {
                    path.clone()
                } else {
                    format!("{path}.{}", port.name)
                },
                port.ty,
            );
        }
    } else if !program.root.outputs.is_empty() {
        for port in &program.root.outputs {
            expected.insert(format!("{}.{}", program.root.name, port.name), port.ty);
        }
    } else {
        for cell in &program.root.cells {
            let path = format!("{}/{}", program.root.name, cell.name);
            for port in program
                .ports(cell)
                .iter()
                .filter(|p| p.direction == Direction::Out)
            {
                expected.insert(
                    if port.name == "out" {
                        path.clone()
                    } else {
                        format!("{path}.{}", port.name)
                    },
                    port.ty,
                );
            }
        }
    }
    let mut names = std::collections::BTreeSet::new();
    for ((label, ty, value), observation) in results.roots.iter().zip(observations) {
        if !names.insert(label)
            || expected.get(label) != Some(ty)
            || label != &observation.label
            || crate::behavior::type_name(*ty, &types) != observation.ty
        {
            return Err("result artifact disagrees with the pinned program interface".into());
        }
        check_value(&types, &results.objects, *ty, value)?;
    }
    for object in &results.objects {
        let fields = data::find(&types, object.ty)?.member_types();
        if fields.len() != object.fields.len() {
            return Err("result artifact record layout mismatch".into());
        }
        for (ty, value) in fields.iter().zip(&object.fields) {
            check_value(&types, &results.objects, *ty, value)?;
        }
    }
    Ok(())
}
/// Integrity and version coherence, not authentication: no node signatures exist.
pub fn verify(store: &Store) -> Result<()> {
    store.validate()?;
    let mut programs = BTreeMap::new();
    let mut states = BTreeMap::new();
    let mut results = BTreeMap::new();
    for (key, artifact) in &store.artifacts {
        if *key != digest(&artifact.format, &artifact.bytes) {
            return Err("information artifact integrity mismatch".into());
        }
        match artifact.format.as_str() {
            PROGRAM_FORMAT => {
                let program: Program = serde_json::from_slice(&artifact.bytes)
                    .map_err(|e| format!("program artifact: {e}"))?;
                program.validate()?;
                programs.insert(*key, program);
            }
            STATE_FORMAT => {
                let state: State = serde_json::from_slice(&artifact.bytes)
                    .map_err(|e| format!("state artifact: {e}"))?;
                states.insert(*key, state);
            }
            RESULT_FORMAT => {
                let result: Results = serde_json::from_slice(&artifact.bytes)
                    .map_err(|e| format!("result artifact: {e}"))?;
                results.insert(*key, result);
            }
            _ => return Err("unsupported information artifact format".into()),
        }
    }
    for (key, state) in &states {
        let program = programs
            .get(&state.program)
            .ok_or("state program artifact missing")?
            .clone();
        // Validate the complete stored runtime data without including Store again.
        let state: State =
            serde_json::from_slice(&store.artifacts[key].bytes).map_err(|e| e.to_string())?;
        state_system(state, program)?;
    }
    for object in store.objects.values() {
        for version in &object.versions {
            if let Content::Execution {
                program,
                target,
                input_state,
                result,
                outputs,
                ..
            } = &version.content
            {
                let Content::Program { artifact } = store.get(program)?.content else {
                    return Err("execution version is not a program".into());
                };
                let source = programs
                    .get(&artifact)
                    .ok_or("execution program artifact missing")?;
                let state = states
                    .get(input_state)
                    .ok_or("execution state artifact missing")?;
                if state.program != artifact {
                    return Err("execution state belongs to a different program version".into());
                }
                if let Some(result) = result {
                    check_results(
                        source,
                        *target,
                        results
                            .get(result)
                            .ok_or("execution result artifact missing")?,
                        outputs,
                    )?;
                }
            }
        }
    }
    Ok(())
}
/// Exact historical values, independent of the UI's shortened rendering.
pub fn results_at(system: &System, execution: &Reference) -> Result<Results> {
    verify(&system.information)?;
    let Content::Execution {
        result: Some(key), ..
    } = system.information.get(execution)?.content
    else {
        return Err("object has no successful result artifact".into());
    };
    serde_json::from_slice(&system.information.artifacts[&key].bytes).map_err(|e| e.to_string())
}
/// An isolated historical trial input; restoring it does not erase shared history.
pub fn trial_input(system: &System, execution: &Reference) -> Result<System> {
    verify(&system.information)?;
    let Content::Execution { input_state, .. } = system.information.get(execution)?.content else {
        return Err("object is not an execution".into());
    };
    let state: State = serde_json::from_slice(&system.information.artifacts[&input_state].bytes)
        .map_err(|e| e.to_string())?;
    let program = serde_json::from_slice(&system.information.artifacts[&state.program].bytes)
        .map_err(|e| e.to_string())?;
    state_system(state, program)
}
/// Read the exact historical graph. This never replaces the current editable Program.
pub fn program_at(system: &System, reference: &Reference) -> Result<Program> {
    verify(&system.information)?;
    let Content::Program { artifact } = system.information.get(reference)?.content else {
        return Err("object is not a program version".into());
    };
    serde_json::from_slice(&system.information.artifacts[&artifact].bytes)
        .map_err(|e| e.to_string())
}
