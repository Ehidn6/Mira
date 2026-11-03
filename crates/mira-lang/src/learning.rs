//! Managed CPU learning experiments over the same System/Program as Studio.
//! Corpus/provenance/experiment records are service metadata, not graph edit history.
use crate::system::{self as runtime, History};
use mira_core::{
    learning::*,
    system::{Authority, System},
    *,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
const MAX_SAMPLE_BYTES: usize = 256 * 1024;

/// Resolve typed data attached to a real graph identity. Text explanations are never implicitly
/// decoded as samples or promoted to training material.
pub struct GraphSource<'a> {
    pub system: &'a System,
}
impl DataSource for GraphSource<'_> {
    fn load(&self, id: Id, max_bytes: usize) -> Result<Vec<u8>> {
        match self
            .system
            .data
            .get(&id)
            .ok_or("learning resource missing")?
        {
            mira_core::neural::GraphData::Data { type_name, bytes }
                if matches!(
                    type_name.as_str(),
                    "mira.learning.scalar-example.v1" | "mira.learning.buffer-example.v1"
                ) =>
            {
                if bytes.len() > max_bytes {
                    return Err("learning resource exceeds sample limit".into());
                }
                Ok(bytes.clone())
            }
            _ => Err("resource is not an explicitly typed learning sample".into()),
        }
    }
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metric {
    pub loss: u64,
    pub observations: u64,
}
pub struct Assessment {
    before: Metric,
    after: Metric,
    candidate: [u8; 32],
    policies: [u8; 32],
    materials: Vec<Id>,
}
impl Assessment {
    pub fn before(&self) -> Metric {
        self.before
    }
    pub fn after(&self) -> Metric {
        self.after
    }
    pub fn improved(&self) -> bool {
        self.before.observations == self.after.observations && self.after.loss < self.before.loss
    }
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TrainingBatch {
    pub materials: Vec<Id>,
    pub epochs: u32,
    pub policies: [u8; 32],
    pub before: [u8; 32],
    pub after: [u8; 32],
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub scope: Id,
    pub method: Method,
    pub epochs: u32,
    pub training_materials: Vec<Id>,
    pub training_fingerprints: BTreeMap<Id, [u8; 32]>,
    pub training_policy_history: Vec<[u8; 32]>,
    pub batches: Vec<TrainingBatch>,
    pub engine_revision: Option<String>,
    pub evaluation_materials: Vec<Id>,
    pub baseline: Metric,
    pub learned: Metric,
    pub policies: [u8; 32],
    pub source_system: [u8; 32],
    pub trained_system: [u8; 32],
    pub engine_version: String,
    #[serde(default)]
    pub typed_profile: Option<TypedProfile>,
}
/// An explicit resource contract for tensor learning. Parameters may include
/// weights and topology buffers. Recurrent/output/checkpoint storage belongs to
/// `state` and is retained inside an experiment, never silently deployed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypedProfile {
    pub owner: Id,
    pub train: Id,
    pub infer: Id,
    pub loss: Id,
    pub inputs: BTreeSet<Id>,
    pub parameters: BTreeSet<Id>,
    pub state: BTreeSet<Id>,
    pub scratch_bytes: usize,
    /// f32 losses are converted to nonnegative integer units with rounding up.
    /// Choose the resolution explicitly; differences below it are not accepted.
    pub loss_scale: u32,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BufferExample {
    pub inputs: BTreeMap<Id, data::Buffer>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Adaptation {
    pub before: Metric,
    pub after: Metric,
    /// None means the candidate was evaluated but did not improve the objective.
    pub receipt: Option<Receipt>,
}
pub struct Experiment {
    original: System,
    baseline: System,
    candidate: System,
    scope: Id,
    training: BTreeMap<Id, Material>,
    training_policies: Vec<[u8; 32]>,
    batches: Vec<TrainingBatch>,
    method: Method,
    epochs: u32,
    published: bool,
    typed: Option<TypedProfile>,
}
fn fingerprint(value: &impl Serialize) -> Result<[u8; 32]> {
    Ok(*blake3::hash(&serde_json::to_vec(value).map_err(|e| e.to_string())?).as_bytes())
}
fn cases(
    registry: &Registry,
    source: &dyn DataSource,
    materials: &[Id],
    purpose: Use,
    scope: Id,
    method: Method,
    now: u64,
) -> Result<Vec<Example>> {
    if materials.is_empty() || materials.len() > 1024 {
        return Err("learning corpus must contain 1..1024 samples".into());
    }
    materials
        .iter()
        .map(|id| {
            let m = registry.authorize(*id, purpose, scope, now)?;
            if method == Method::TeacherStudent && purpose == Use::LocalTraining {
                registry.authorize(*id, Use::Distillation, scope, now)?;
            }
            let bytes = source.load(m.payload, MAX_SAMPLE_BYTES)?;
            if bytes.len() > MAX_SAMPLE_BYTES || *blake3::hash(&bytes).as_bytes() != m.fingerprint {
                return Err("learning material fingerprint changed".into());
            }
            let sample: Example = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if sample.inputs.len() > MAX_ELEMENTS
                || sample.targets.len() > MAX_ELEMENTS
                || sample.expected.len() > MAX_ELEMENTS
            {
                return Err("learning sample budget".into());
            }
            Ok(sample)
        })
        .collect()
}
fn inputs(sample: &Example) -> Result<BTreeMap<(Id, String), i64>> {
    let mut signals = BTreeMap::new();
    for s in &sample.inputs {
        if signals.insert((s.graph, s.port.clone()), s.value).is_some() {
            return Err("duplicate learning input".into());
        }
    }
    Ok(signals)
}
fn copy_weights(destination: &mut System, trained: &System, buffers: &BTreeSet<Id>) -> Result<()> {
    if destination.program.code_modules != trained.program.code_modules
        || destination.program.root.find(trained.program.root.id) != Some(&trained.program.root)
    {
        return Err(
            "parameter publication cannot replace a program; use a checked structural proposal"
                .into(),
        );
    }
    for (id, learned) in &trained.parameters {
        let p = destination.parameters.entry(*id).or_default();
        p.weight = learned.weight;
        p.bias = learned.bias;
        for (slot, value) in &learned.slots {
            if slot.starts_with("param_") {
                p.slots.insert(slot.clone(), *value);
            }
        }
    }
    for id in buffers {
        if destination.program.root.find_cell(*id).is_some() {
            let buffer = trained
                .buffers
                .get(id)
                .ok_or("trained parameter buffer missing")?;
            buffer.validate(&destination.program.data_types())?;
            if destination
                .buffers
                .get(id)
                .is_some_and(|old| old.ty != buffer.ty)
            {
                return Err("trained parameter buffer type changed".into());
            }
            destination.buffers.insert(*id, buffer.clone());
        }
    }
    Ok(())
}
fn freeze(e: &mut System) {
    for info in mira_core::inspect::cells(&e.program) {
        if matches!(
            info.cell.definition,
            Definition::Neural | Definition::Adaptive(_)
        ) {
            e.parameters.entry(info.cell.id).or_default().trainable = false;
        }
    }
}
impl Experiment {
    /// Isolate one existing graph and its identity-addressed runtime state.
    /// Only explicitly supplied corpus inputs enter the run.
    pub fn begin(system: &System, scope: Id, method: Method) -> Result<Self> {
        runtime::check(system)?;
        let graph = system
            .program
            .root
            .find(scope)
            .ok_or("learning scope graph missing")?
            .clone();
        let mut baseline = system.clone();
        baseline.program.root = graph;
        baseline.pending.clear();
        baseline
            .parameters
            .retain(|id, _| baseline.program.root.find_cell(*id).is_some());
        baseline
            .buffers
            .retain(|id, _| baseline.program.root.find_cell(*id).is_some());
        baseline
            .delays
            .retain(|id, _| baseline.program.root.find_cell(*id).is_some());
        baseline.data.retain(|id, _| {
            baseline.program.root.find(*id).is_some()
                || baseline.program.root.find_cell(*id).is_some()
        });
        baseline.memories.retain(|id, _| {
            baseline.program.root.find(*id).is_some()
                || baseline.program.root.find_cell(*id).is_some()
        });
        baseline.protected.retain(|id| {
            baseline.program.root.find(*id).is_some()
                || baseline.program.root.find_cell(*id).is_some()
        });
        baseline.validate()?;
        Ok(Self {
            original: system.clone(),
            candidate: baseline.clone(),
            baseline,
            scope,
            training: BTreeMap::new(),
            training_policies: Vec::new(),
            batches: Vec::new(),
            method,
            epochs: 0,
            published: false,
            typed: None,
        })
    }
    /// Isolate the same System component as scalar experiments, then select
    /// actual Mira behaviors and resource roles rather than a second NN engine.
    pub fn begin_typed(
        system: &System,
        scope: Id,
        method: Method,
        profile: TypedProfile,
    ) -> Result<Self> {
        let mut experiment = Self::begin(system, scope, method)?;
        validate_profile(&experiment.baseline, &profile)?;
        experiment.typed = Some(profile);
        Ok(experiment)
    }
    pub fn typed_profile(&self) -> Option<&TypedProfile> {
        self.typed.as_ref()
    }
    pub fn candidate(&self) -> &System {
        &self.candidate
    }
    /// A failed batch leaves the experiment and the live system untouched.
    pub fn train(
        &mut self,
        registry: &Registry,
        source: &dyn DataSource,
        materials: &[Id],
        epochs: u32,
        now: u64,
    ) -> Result<()> {
        if self.published
            || epochs == 0
            || epochs > 1000
            || materials
                .len()
                .checked_mul(epochs as usize)
                .is_none_or(|n| n > 10000)
        {
            return Err("invalid/finished learning experiment or CPU run budget".into());
        }
        for (id, previous) in &self.training {
            if registry.materials.get(id) != Some(previous) {
                return Err("training material provenance changed; start a new experiment".into());
            }
        }
        let policies = fingerprint(registry)?;
        let before = fingerprint(&self.candidate)?;
        let mut draft = self.candidate.clone();
        if let Some(profile) = &self.typed {
            let samples = buffer_cases(
                registry,
                source,
                materials,
                Use::LocalTraining,
                self.scope,
                self.method,
                now,
            )?;
            for _ in 0..epochs {
                for sample in &samples {
                    load_buffers(&mut draft, profile, sample)?;
                    let previous = draft.buffers.clone();
                    let activation = draft
                        .tick
                        .checked_add(1)
                        .ok_or("typed learning tick exhausted")?;
                    crate::runtime::execute_behavior_system(
                        &mut draft,
                        profile.owner,
                        profile.train,
                        activation,
                        false,
                        profile.scratch_bytes,
                    )?;
                    enforce_writes(
                        &draft,
                        &previous,
                        &profile.parameters.union(&profile.state).copied().collect(),
                    )?;
                    draft.tick = activation;
                }
            }
        } else {
            let samples = cases(
                registry,
                source,
                materials,
                Use::LocalTraining,
                self.scope,
                self.method,
                now,
            )?;
            runtime::check_executable(&self.candidate)?;
            for _ in 0..epochs {
                for sample in &samples {
                    let mut targets = BTreeMap::new();
                    for t in &sample.targets {
                        if targets.insert(t.cell, t.value).is_some() {
                            return Err("duplicate learning target".into());
                        }
                    }
                    engine::tick(
                        &mut draft,
                        &inputs(sample)?,
                        &targets,
                        &mut runtime::Host::default(),
                    )?;
                }
            }
        }
        runtime::check(&draft)?;
        self.epochs = self
            .epochs
            .checked_add(epochs)
            .ok_or("learning epoch overflow")?;
        self.batches.push(TrainingBatch {
            materials: materials.to_vec(),
            epochs,
            policies,
            before,
            after: fingerprint(&draft)?,
        });
        self.candidate = draft;
        for id in materials {
            self.training.insert(
                *id,
                registry
                    .materials
                    .get(id)
                    .ok_or("training material missing")?
                    .clone(),
            );
        }
        self.training_policies.push(policies);
        Ok(())
    }
    fn score(&self, model: &System, samples: &[Example], loss: Id) -> Result<Metric> {
        let module = mira_core::code::find(&model.program.code_modules, loss)
            .ok_or("Mira loss definition missing")?;
        if module.output != Type::I64
            || module.parameters.len() != 2
            || module.parameters.iter().any(|p| p.ty != Type::I64)
        {
            return Err("loss(actual, expected) must have two i64 inputs and an i64 result".into());
        }
        let mut metric = Metric {
            loss: 0,
            observations: 0,
        };
        for sample in samples {
            if sample.expected.is_empty() {
                return Err("evaluation sample has no expected observations".into());
            }
            // Each case starts from the same baseline state. Evaluation freezes
            // learning even when a graph carries an ordinary target input port.
            let mut state = model.clone();
            freeze(&mut state);
            let report = engine::tick(
                &mut state,
                &inputs(sample)?,
                &BTreeMap::new(),
                &mut runtime::Host::default(),
            )?;
            let mut vm = mira_core::vm::Vm::default();
            for expected in &sample.expected {
                let actual = *report
                    .deliveries
                    .get(&(expected.cell, expected.port.clone()))
                    .ok_or("evaluation observation is inactive/missing")?;
                let value = engine::BehaviorHost::call(
                    &mut vm,
                    &model.program,
                    loss,
                    None,
                    &[actual, expected.value],
                )?;
                let value = u64::try_from(value).map_err(|_| "loss must be nonnegative")?;
                metric.loss = metric
                    .loss
                    .checked_add(value)
                    .ok_or("evaluation loss overflow")?;
                metric.observations = metric
                    .observations
                    .checked_add(1)
                    .ok_or("evaluation count overflow")?;
            }
        }
        Ok(metric)
    }
    pub fn compare(
        &self,
        registry: &Registry,
        source: &dyn DataSource,
        materials: &[Id],
        loss: Id,
        now: u64,
    ) -> Result<Assessment> {
        let buffers = self
            .typed
            .as_ref()
            .map(|p| p.parameters.clone())
            .unwrap_or_default();
        let mut learned = self.baseline.clone();
        copy_weights(&mut learned, &self.candidate, &buffers)?;
        let (before, after) = if let Some(profile) = &self.typed {
            if loss != profile.loss {
                return Err("evaluation loss differs from the typed profile".into());
            }
            let samples = buffer_cases(
                registry,
                source,
                materials,
                Use::Evaluation,
                self.scope,
                self.method,
                now,
            )?;
            (
                typed_score(&self.baseline, &samples, profile)?,
                typed_score(&learned, &samples, profile)?,
            )
        } else {
            let samples = cases(
                registry,
                source,
                materials,
                Use::Evaluation,
                self.scope,
                self.method,
                now,
            )?;
            runtime::check_executable(&learned)?;
            (
                self.score(&self.baseline, &samples, loss)?,
                self.score(&learned, &samples, loss)?,
            )
        };
        Ok(Assessment {
            before,
            after,
            candidate: fingerprint(&self.candidate)?,
            policies: fingerprint(registry)?,
            materials: materials.to_vec(),
        })
    }
    /// One autonomous local adaptation cycle. No UI confirmation is involved:
    /// a candidate is trained and evaluated in isolation, then only an improved
    /// candidate is admitted by the existing authority/provenance boundary.
    pub fn adapt(
        &mut self,
        system: &mut System,
        history: &mut History,
        authority: &Authority,
        registry: &Registry,
        source: &dyn DataSource,
        training: &[Id],
        evaluation: &[Id],
        epochs: u32,
        loss: Id,
        now: u64,
    ) -> Result<Adaptation> {
        if *system != self.original {
            return Err("autonomous learning base changed".into());
        }
        self.train(registry, source, training, epochs, now)?;
        let assessment = self.compare(registry, source, evaluation, loss, now)?;
        let receipt = if assessment.improved() {
            Some(self.publish(system, history, authority, registry, &assessment, now)?)
        } else {
            None
        };
        Ok(Adaptation {
            before: assessment.before(),
            after: assessment.after(),
            receipt,
        })
    }
    /// Explicit owner action. The cloud tool dispatcher has no publication tool.
    /// Scalar parameters and explicitly declared tensor/topology parameter
    /// resources are deployed. Recurrent/checkpoint state, queues and structural
    /// revision remain those of the live system.
    pub fn publish(
        &mut self,
        system: &mut System,
        history: &mut History,
        authority: &Authority,
        registry: &Registry,
        assessment: &Assessment,
        now: u64,
    ) -> Result<Receipt> {
        if self.published || *system != self.original {
            return Err("learning base changed or experiment already published".into());
        }
        if !assessment.improved()
            || assessment.candidate != fingerprint(&self.candidate)?
            || assessment.policies != fingerprint(registry)?
        {
            return Err("unimproved or stale learning assessment".into());
        }
        for (id, material) in &self.training {
            if registry.materials.get(id) != Some(material) {
                return Err("training material provenance changed".into());
            }
            registry.authorize(*id, Use::LocalTraining, self.scope, now)?;
            if self.method == Method::TeacherStudent {
                registry.authorize(*id, Use::Distillation, self.scope, now)?;
            }
        }
        for id in &assessment.materials {
            registry.authorize(*id, Use::Evaluation, self.scope, now)?;
        }
        let mut draft = system.clone();
        draft.edit(authority, |s| {
            copy_weights(
                s,
                &self.candidate,
                &self
                    .typed
                    .as_ref()
                    .map(|p| p.parameters.clone())
                    .unwrap_or_default(),
            )
        })?;
        draft.revision = system.revision;
        runtime::check(&draft)?;
        let receipt = Receipt {
            scope: self.scope,
            method: self.method,
            epochs: self.epochs,
            training_materials: self.training.keys().copied().collect(),
            training_fingerprints: self
                .training
                .iter()
                .map(|(id, m)| (*id, m.fingerprint))
                .collect(),
            training_policy_history: self.training_policies.clone(),
            batches: self.batches.clone(),
            engine_revision: option_env!("MIRA_BUILD_REVISION").map(str::to_owned),
            evaluation_materials: assessment.materials.clone(),
            baseline: assessment.before,
            learned: assessment.after,
            policies: assessment.policies,
            source_system: fingerprint(system)?,
            trained_system: fingerprint(&draft)?,
            engine_version: env!("CARGO_PKG_VERSION").into(),
            typed_profile: self.typed.clone(),
        };
        history.checkpoint(system, format!("Learning · {} epochs", self.epochs), true);
        *system = draft;
        self.published = true;
        Ok(receipt)
    }
}

fn validate_profile(system: &System, profile: &TypedProfile) -> Result<()> {
    if profile.scratch_bytes > crate::data_runtime::ARENA_BYTES
        || profile.loss_scale == 0
        || profile.parameters.is_empty()
    {
        return Err(
            "typed learning requires bounded scratch, nonzero loss scale and parameter resources"
                .into(),
        );
    }
    if !profile.inputs.is_disjoint(&profile.parameters)
        || !profile.inputs.is_disjoint(&profile.state)
        || !profile.parameters.is_disjoint(&profile.state)
    {
        return Err("typed input, parameter and recurrent state resources must be disjoint".into());
    }
    let owner = system
        .program
        .root
        .find_cell(profile.owner)
        .ok_or("typed learning owner missing")?;
    if owner.space.is_none() {
        return Err("typed learning owner needs an internal resource graph".into());
    }
    for (id, output) in [
        (profile.train, Type::Unit),
        (profile.infer, Type::Unit),
        (profile.loss, Type::F32),
    ] {
        let behavior = mira_core::code::find(&system.program.code_modules, id)
            .ok_or("typed learning behavior missing")?;
        if !behavior.parameters.is_empty() || behavior.output != output {
            return Err("typed train/infer must be () -> unit and loss () -> f32".into());
        }
    }
    for id in profile
        .inputs
        .iter()
        .chain(&profile.parameters)
        .chain(&profile.state)
    {
        let cell = system
            .program
            .root
            .find_cell(*id)
            .ok_or("typed learning resource missing")?;
        let ports = system.program.ports(cell);
        if ports.iter().any(|p| p.direction == Direction::In) {
            return Err("typed learning resources cannot have inputs".into());
        }
        let ty = ports
            .iter()
            .find_map(|p| match (p.direction, p.ty) {
                (Direction::Out, Type::Data(id)) => Some(id),
                _ => None,
            })
            .ok_or("typed learning resource must return Data")?;
        data::Buffer {
            ty,
            words: vec![0; data::find(&system.program.data_types(), ty)?.len()],
        }
        .validate(&system.program.data_types())?;
    }
    Ok(())
}
fn buffer_cases(
    registry: &Registry,
    source: &dyn DataSource,
    materials: &[Id],
    purpose: Use,
    scope: Id,
    method: Method,
    now: u64,
) -> Result<Vec<BufferExample>> {
    if materials.is_empty() || materials.len() > 1024 {
        return Err("learning corpus must contain 1..1024 samples".into());
    }
    materials
        .iter()
        .map(|id| {
            let material = registry.authorize(*id, purpose, scope, now)?;
            if method == Method::TeacherStudent && purpose == Use::LocalTraining {
                registry.authorize(*id, Use::Distillation, scope, now)?;
            }
            let bytes = source.load(material.payload, MAX_SAMPLE_BYTES)?;
            if bytes.len() > MAX_SAMPLE_BYTES
                || *blake3::hash(&bytes).as_bytes() != material.fingerprint
            {
                return Err("learning material fingerprint changed".into());
            }
            let sample: BufferExample =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if sample.inputs.len() > MAX_ELEMENTS {
                return Err("typed learning sample budget".into());
            }
            Ok(sample)
        })
        .collect()
}
fn load_buffers(system: &mut System, profile: &TypedProfile, sample: &BufferExample) -> Result<()> {
    if sample.inputs.keys().copied().collect::<BTreeSet<_>>() != profile.inputs {
        return Err("sample input resources differ from typed profile".into());
    }
    let types = system.program.data_types();
    for (id, buffer) in &sample.inputs {
        buffer.validate(&types)?;
        let owner = system
            .program
            .root
            .find_cell(*id)
            .ok_or("sample resource missing")?;
        if !system
            .program
            .ports(owner)
            .iter()
            .any(|p| p.direction == Direction::Out && p.ty == Type::Data(buffer.ty))
        {
            return Err("sample buffer type differs from resource".into());
        }
        system.buffers.insert(*id, buffer.clone());
    }
    Ok(())
}
fn enforce_writes(
    system: &System,
    before: &BTreeMap<Id, data::Buffer>,
    allowed: &BTreeSet<Id>,
) -> Result<()> {
    for (id, buffer) in &system.buffers {
        let changed = match before.get(id) {
            Some(previous) => previous != buffer,
            None => buffer.words.iter().any(|v| *v != 0),
        };
        if changed && !allowed.contains(id) {
            return Err(format!("typed learning wrote undeclared resource #{id}"));
        }
    }
    if before.keys().any(|id| !system.buffers.contains_key(id)) {
        return Err("typed execution removed an existing resource".into());
    }
    Ok(())
}
fn typed_score(
    model: &System,
    samples: &[BufferExample],
    profile: &TypedProfile,
) -> Result<Metric> {
    validate_profile(&model, profile)?;
    let mut metric = Metric {
        loss: 0,
        observations: 0,
    };
    for sample in samples {
        let mut state = model.clone();
        load_buffers(&mut state, profile, sample)?;
        let before = state.buffers.clone();
        crate::runtime::execute_behavior_system(
            &mut state,
            profile.owner,
            profile.infer,
            1,
            false,
            profile.scratch_bytes,
        )?;
        enforce_writes(&state, &before, &profile.state)?;
        let activation = crate::runtime::execute_behavior_system(
            &mut state,
            profile.owner,
            profile.loss,
            2,
            true,
            profile.scratch_bytes,
        )?;
        let word = activation
            .terminals
            .first()
            .ok_or("typed loss returned no value")?
            .1;
        let value = mira_core::vm::word_f32(word)? as f64;
        if value < 0.0 {
            return Err("typed loss must be nonnegative".into());
        }
        let units = (value * profile.loss_scale as f64).ceil();
        if !units.is_finite() || units >= u64::MAX as f64 {
            return Err("typed loss overflows configured metric units".into());
        }
        metric.loss = metric
            .loss
            .checked_add(units as u64)
            .ok_or("evaluation loss overflow")?;
        metric.observations = metric
            .observations
            .checked_add(1)
            .ok_or("evaluation count overflow")?;
    }
    Ok(metric)
}
