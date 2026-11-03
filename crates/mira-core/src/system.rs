//! Portable recursive system model and deterministic transaction boundary.
//! The host supplies behavior execution, never authority to mutate this model.
use crate::{
    neural::{Activation, LearningRule},
    *,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Parameters {
    pub weight: f64,
    pub bias: f64,
    pub state: i64,
    pub rate: f64,
    pub trainable: bool,
    pub activation: Activation,
    pub learning: LearningRule,
    /// Named state_*/param_* bindings for an Adaptive Mira definition.
    pub slots: BTreeMap<String, i64>,
    /// Slot -> Mira update behavior; all updates observe the same old snapshot.
    pub updates: BTreeMap<String, Id>,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            weight: 1.,
            bias: 0.,
            state: 0,
            rate: 0.01,
            trainable: true,
            activation: Activation::Identity,
            learning: LearningRule::None,
            slots: BTreeMap::new(),
            updates: BTreeMap::new(),
        }
    }
}
/// Runtime state belongs to identities in one graph program. There is no second
/// component hierarchy or external routing layer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct System {
    pub program: Program,
    #[serde(default)]
    pub information: crate::information::Store,
    pub parameters: BTreeMap<Id, Parameters>,
    pub buffers: BTreeMap<Id, data::Buffer>,
    pub delays: BTreeMap<Id, i64>,
    pub data: BTreeMap<Id, neural::GraphData>,
    pub protected: BTreeSet<Id>,
    pub memories: BTreeMap<Id, AssociativeMemory>,
    pub revision: u64,
    pub tick: u64,
    #[serde(with = "mailbox")]
    pub pending: BTreeMap<(Id, String), i64>,
}
/// Explicit authority issued by the deterministic owner, never by a learned proposal.
#[derive(Clone, Debug, Default)]
pub struct Authority {
    pub writable: BTreeSet<Id>,
}
impl System {
    pub fn from_program(program: Program) -> Result<Self> {
        program.validate()?;
        Ok(Self {
            program,
            information: Default::default(),
            parameters: BTreeMap::new(),
            buffers: BTreeMap::new(),
            delays: BTreeMap::new(),
            data: BTreeMap::new(),
            protected: BTreeSet::new(),
            memories: BTreeMap::new(),
            revision: 0,
            tick: 0,
            pending: BTreeMap::new(),
        })
    }
    pub fn validate(&self) -> Result<()> {
        fn add(ids: &mut BTreeSet<Id>, id: Id) -> Result<()> {
            if id == 0 || !ids.insert(id) {
                return Err("system identity collision".into());
            }
            if ids.len() > MAX_ELEMENTS {
                return Err("graph identity budget exceeded".into());
            }
            Ok(())
        }
        fn graph(s: &Space, ids: &mut BTreeSet<Id>) -> Result<()> {
            add(ids, s.id)?;
            for c in &s.cells {
                add(ids, c.id)?;
                if let Some(s) = &c.space {
                    graph(s, ids)?;
                }
            }
            for l in &s.links {
                add(ids, l.id)?;
            }
            Ok(())
        }
        fn modules(ms: &[code::CodeModule], ids: &mut BTreeSet<Id>) -> Result<()> {
            for m in ms {
                add(ids, m.id)?;
                for t in &m.data_types {
                    add(ids, t.id)?;
                }
                modules(&m.locals, ids)?;
            }
            Ok(())
        }
        fn state(e: &System, ids: &mut BTreeSet<Id>) -> Result<()> {
            e.program.validate()?;
            graph(&e.program.root, ids)?;
            for ty in &e.program.global_types {
                add(ids, ty.id)?;
            }
            modules(&e.program.code_modules, ids)?;
            for (id, buffer) in &e.buffers {
                buffer.validate(&e.program.data_types())?;
                let cell = e
                    .program
                    .root
                    .find_cell(*id)
                    .ok_or("buffer owner missing")?;
                if !e
                    .program
                    .ports(cell)
                    .iter()
                    .any(|p| p.direction == Direction::Out && p.ty == Type::Data(buffer.ty))
                {
                    return Err("buffer owner has a different result type".into());
                }
            }
            for (id, p) in &e.parameters {
                if e.program.root.find_cell(*id).is_none_or(|c| {
                    !matches!(c.definition, Definition::Neural | Definition::Adaptive(_))
                }) {
                    return Err("parameters must belong to a neural cell".into());
                }
                if ![p.weight, p.bias, p.rate].iter().all(|v| v.is_finite()) || p.rate < 0. {
                    return Err("invalid neural parameters".into());
                }
                if let Some(cell) = e.program.root.find_cell(*id) {
                    if let Definition::Adaptive(module) = cell.definition {
                        let m = crate::code::find(&e.program.code_modules, module)
                            .ok_or("adaptive type missing")?;
                        for name in p.slots.keys().chain(p.updates.keys()) {
                            if !m.parameters.iter().any(|p| {
                                p.name == *name
                                    && (name.starts_with("state_") || name.starts_with("param_"))
                            }) {
                                return Err("unknown adaptive state/parameter slot".into());
                            }
                        }
                        for (name, id) in &p.updates {
                            let update = crate::code::find(&e.program.code_modules, *id)
                                .ok_or("adaptive update missing")?;
                            let slot = m
                                .parameters
                                .iter()
                                .find(|p| p.name == *name)
                                .ok_or("adaptive slot missing")?;
                            if update.output != slot.ty {
                                return Err("adaptive update output has wrong slot type".into());
                            }
                            for input in &update.parameters {
                                let ty = if input.name == "result" || input.name == "target" {
                                    m.output
                                } else {
                                    m.parameters
                                        .iter()
                                        .find(|p| p.name == input.name)
                                        .ok_or("adaptive update input missing")?
                                        .ty
                                };
                                if ty != input.ty {
                                    return Err("adaptive update input type mismatch".into());
                                }
                            }
                        }
                    }
                }
            }
            for id in e.delays.keys() {
                if e.program
                    .root
                    .find_cell(*id)
                    .is_none_or(|c| c.definition != Definition::Delay)
                {
                    return Err("state must belong to a delay cell".into());
                }
            }
            for (id, memory) in &e.memories {
                if !ids.contains(id) {
                    return Err("memory owner missing".into());
                }
                if memory.capacity == 0
                    || memory.capacity > 4096
                    || memory.entries.len() > memory.capacity
                {
                    return Err("invalid memory capacity".into());
                }
                if let Some((key, value)) = memory.entries.first() {
                    if key.is_empty()
                        || key.len() > 4096
                        || value.len() > 4096
                        || memory.entries.iter().any(|(k, v)| {
                            k.len() != key.len()
                                || v.len() != value.len()
                                || !k.iter().chain(v).all(|x| x.is_finite())
                        })
                    {
                        return Err("invalid memory contents".into());
                    }
                }
            }
            for id in e.protected.iter().chain(e.data.keys()) {
                if !ids.contains(id) {
                    return Err("runtime owner missing".into());
                }
            }
            Ok(())
        }
        let mut ids = BTreeSet::new();
        state(self, &mut ids)?;
        self.information.validate()?;
        for id in self.information.objects.keys() {
            add(&mut ids, *id)?;
        }
        for ((id, name), value) in &self.pending {
            if *id != self.program.root.id {
                return Err("external signals enter through the root graph interface".into());
            }
            let graph = &self.program.root;
            let p = graph
                .inputs
                .iter()
                .find(|p| p.name == *name)
                .ok_or("mailbox input missing")?;
            if matches!(p.ty, Type::Addr | Type::Data(_) | Type::Unit)
                || p.ty == Type::Bool && !matches!(value, 0 | 1)
                || p.ty == Type::Event && *value != 1
            {
                return Err("invalid mailbox value".into());
            }
        }
        if self.program.next_identity <= *ids.last().ok_or("empty system")? {
            return Err("system identity counter is stale".into());
        }
        Ok(())
    }
    pub fn allocate_id(&mut self) -> Result<Id> {
        let id = self.program.next_id()?;
        self.program.next_identity = id.checked_add(1).ok_or("identity exhausted")?;
        Ok(id)
    }
    /// All writes, including generated subgraphs, pass this deterministic gate.
    pub fn edit(
        &mut self,
        authority: &Authority,
        change: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<Self> {
        let mut draft = self.clone();
        change(&mut draft)?;
        draft.information.extends(&self.information)?;
        for id in &self.protected {
            if authority.writable.contains(id) {
                continue;
            }
            let graph = self.program.root.find(*id);
            let cell = self.program.root.find_cell(*id);
            let changed = if let Some(graph) = graph {
                draft.program.root.find(*id) != Some(graph)
            } else if let Some(cell) = cell {
                draft.program.root.find_cell(*id) != Some(cell)
            } else {
                true
            };
            let contains = |owner: &Id| {
                *owner == *id
                    || graph
                        .is_some_and(|g| g.find_cell(*owner).is_some() || g.find(*owner).is_some())
            };
            let state_changed = self
                .parameters
                .iter()
                .chain(draft.parameters.iter())
                .any(|(k, _)| contains(k) && self.parameters.get(k) != draft.parameters.get(k))
                || self
                    .buffers
                    .iter()
                    .chain(draft.buffers.iter())
                    .any(|(k, _)| contains(k) && self.buffers.get(k) != draft.buffers.get(k))
                || self
                    .delays
                    .iter()
                    .chain(draft.delays.iter())
                    .any(|(k, _)| contains(k) && self.delays.get(k) != draft.delays.get(k))
                || self
                    .data
                    .iter()
                    .chain(draft.data.iter())
                    .any(|(k, _)| contains(k) && self.data.get(k) != draft.data.get(k))
                || self
                    .memories
                    .iter()
                    .chain(draft.memories.iter())
                    .any(|(k, _)| contains(k) && self.memories.get(k) != draft.memories.get(k));
            // A definition can be shared: protecting one instance also prevents
            // rewriting its implementation behind the instance's identity.
            let implementation_changed = self.program.code_modules.iter().chain(draft.program.code_modules.iter()).any(|m| {
                let used = crate::inspect::cells(&self.program).iter().any(|info| contains(&info.cell.id)
                    && matches!(info.cell.definition, Definition::Code(i)|Definition::Adaptive(i) if i==m.id));
                used && self.program.code_modules.iter().find(|v|v.id==m.id)!=draft.program.code_modules.iter().find(|v|v.id==m.id)
            });
            if changed || state_changed || implementation_changed || !draft.protected.contains(id) {
                return Err("protected graph identity: no write capability".into());
            }
        }
        let exists = |id: &Id| {
            draft.program.root.find_cell(*id).is_some() || draft.program.root.find(*id).is_some()
        };
        draft
            .parameters
            .retain(|id, _| draft.program.root.find_cell(*id).is_some());
        draft
            .buffers
            .retain(|id, _| draft.program.root.find_cell(*id).is_some());
        draft
            .delays
            .retain(|id, _| draft.program.root.find_cell(*id).is_some());
        draft.data.retain(|id, _| exists(id));
        draft.memories.retain(|id, _| exists(id));
        draft.protected.retain(exists);
        draft.pending.retain(|(id, name), _| {
            draft
                .program
                .root
                .find(*id)
                .is_some_and(|g| g.inputs.iter().any(|p| p.name == *name))
        });
        draft.revision = self.revision.checked_add(1).ok_or("revision exhausted")?;
        draft.program.next_identity = draft.program.next_identity.max(self.program.next_identity);
        draft.validate()?;
        Ok(core::mem::replace(self, draft))
    }
}

/// Bounded associative memory; explicit stores do not enter structural undo history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AssociativeMemory {
    pub capacity: usize,
    pub entries: Vec<(Vec<f64>, Vec<f64>)>,
}
impl AssociativeMemory {
    pub fn store(&mut self, key: Vec<f64>, value: Vec<f64>) -> Result<()> {
        if self.capacity == 0
            || self.capacity > 4096
            || key.is_empty()
            || key.len() > 4096
            || value.len() > 4096
            || !key.iter().chain(&value).all(|v| v.is_finite())
            || self
                .entries
                .first()
                .is_some_and(|(k, v)| k.len() != key.len() || v.len() != value.len())
        {
            return Err("invalid associative memory entry".into());
        }
        if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push((key, value));
        Ok(())
    }
    /// A single modern associative-attention update: softmax(beta Kq)^T V.
    /// Keys/values are supplied explicitly; graph-aware learned memory is future work.
    pub fn associate(&self, query: &[f64], beta: f64) -> Result<Vec<f64>> {
        if !beta.is_finite()
            || beta <= 0.
            || query.is_empty()
            || !query.iter().all(|v| v.is_finite())
        {
            return Err("invalid memory attention query".into());
        }
        if self.entries.is_empty() {
            return Ok(Vec::new());
        }
        let width = self.entries[0].1.len();
        if self
            .entries
            .iter()
            .any(|(k, v)| k.len() != query.len() || v.len() != width)
        {
            return Err("memory attention shape mismatch".into());
        }
        let scores: Vec<f64> = self
            .entries
            .iter()
            .map(|(k, _)| beta * k.iter().zip(query).map(|(a, b)| a * b).sum::<f64>())
            .collect();
        if !scores.iter().all(|v| v.is_finite()) {
            return Err("memory attention numeric overflow".into());
        }
        let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let weights: Vec<f64> = scores.iter().map(|v| libm::exp(v - max)).collect();
        let total = weights.iter().sum::<f64>();
        let mut result = alloc::vec![0.;width];
        for ((_, v), w) in self.entries.iter().zip(weights) {
            for (out, x) in result.iter_mut().zip(v) {
                *out += (w / total) * x;
            }
        }
        if !result.iter().all(|v| v.is_finite()) {
            return Err("memory attention result overflow".into());
        }
        Ok(result)
    }
    /// Squared-distance nearest retrieval. This is not a trained Hopfield network.
    pub fn recall(&self, query: &[f64]) -> Result<Option<&[f64]>> {
        if query.is_empty()
            || !query.iter().all(|v| v.is_finite())
            || self.entries.iter().any(|(k, _)| k.len() != query.len())
        {
            return Err("invalid memory query".into());
        }
        let mut best = None;
        let mut distance = f64::INFINITY;
        for (key, value) in &self.entries {
            let d = key
                .iter()
                .zip(query)
                .map(|(a, b)| (a - b) * (a - b))
                .sum::<f64>();
            if d.is_finite() && d < distance {
                distance = d;
                best = Some(value.as_slice());
            }
        }
        Ok(best)
    }
}

mod mailbox {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &BTreeMap<(Id, String), i64>,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        value.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<BTreeMap<(Id, String), i64>, D::Error> {
        let entries = Vec::<((Id, String), i64)>::deserialize(deserializer)?;
        let mut result = BTreeMap::new();
        for (key, value) in entries {
            if result.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate mailbox key"));
            }
        }
        Ok(result)
    }
}
