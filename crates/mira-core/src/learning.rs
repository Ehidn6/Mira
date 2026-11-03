//! Provenance and purpose-specific authorization for learning materials.
//! Evidence is issued by a trusted owner/ingestor. This registry is not a legal
//! classifier and never infers permission from a provider/model/source label.
use crate::*;
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Use {
    Remember,
    Knowledge,
    ProgramArtifact,
    LocalTraining,
    Evaluation,
    Distillation,
    ProviderCustomization,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Human { owner: String },
    Experience { component: Id },
    Dataset { name: String },
    Simulation { component: Id },
    Cloud { provider: String, model: String },
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub reviewer: String,
    /// Permission/license/contract review reference, not a guessed license name.
    pub reference: String,
    pub valid_from: u64,
    pub valid_until: Option<u64>,
}
impl Evidence {
    fn check(&self, now: u64) -> Result<()> {
        if self.reviewer.is_empty()
            || self.reference.is_empty()
            || now < self.valid_from
            || self
                .valid_until
                .is_some_and(|end| now >= end || end <= self.valid_from)
        {
            return Err("material permission is unknown, expired or not yet effective".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Transform {
    pub component: Id,
    pub revision: u64,
    pub description: String,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Material {
    pub id: Id,
    pub payload: Id,
    pub fingerprint: [u8; 32],
    pub source: Source,
    pub license: Option<String>,
    pub purpose: String,
    pub allowed: BTreeSet<Use>,
    pub destinations: BTreeSet<Id>,
    pub evidence: Option<Evidence>,
    pub parents: Vec<Id>,
    pub transforms: Vec<Transform>,
    /// Exact reviewed policy version for cloud-origin material.
    pub provider_policy: Option<String>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProviderPolicy {
    pub provider: String,
    pub version: String,
    pub allowed: BTreeSet<Use>,
    pub destinations: BTreeSet<Id>,
    pub evidence: Evidence,
}
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    pub materials: BTreeMap<Id, Material>,
    /// Missing provider policies deliberately grant nothing.
    pub policies: Vec<ProviderPolicy>,
}
impl Registry {
    pub fn authorize(&self, id: Id, purpose: Use, destination: Id, now: u64) -> Result<&Material> {
        if self.materials.len() > MAX_ELEMENTS || self.policies.len() > MAX_ELEMENTS {
            return Err("provenance registry budget".into());
        }
        fn walk(
            registry: &Registry,
            id: Id,
            purpose: Use,
            destination: Id,
            now: u64,
            active: &mut BTreeSet<Id>,
            checked: &mut BTreeSet<Id>,
        ) -> Result<()> {
            if checked.contains(&id) {
                return Ok(());
            }
            if active.len() >= MAX_DEPTH || !active.insert(id) {
                return Err("cyclic/deep material provenance".into());
            }
            let m = registry
                .materials
                .get(&id)
                .ok_or("material provenance missing")?;
            if m.id != id
                || id == 0
                || m.payload == 0
                || m.purpose.is_empty()
                || !m.allowed.contains(&purpose)
                || !m.destinations.contains(&destination)
            {
                return Err("material is not authorized for this purpose/destination".into());
            }
            m.evidence
                .as_ref()
                .ok_or("material permission is not reviewed")?
                .check(now)?;
            if let Source::Cloud { provider, model } = &m.source {
                if provider.is_empty() || model.is_empty() {
                    return Err("cloud material has incomplete origin".into());
                }
                let version = m
                    .provider_policy
                    .as_ref()
                    .ok_or("cloud material has no reviewed provider policy")?;
                let mut matching = registry
                    .policies
                    .iter()
                    .filter(|p| p.provider == *provider && p.version == *version);
                let policy = matching.next().ok_or("reviewed provider policy missing")?;
                if matching.next().is_some() {
                    return Err("ambiguous provider policy version".into());
                }
                policy.evidence.check(now)?;
                if !policy.allowed.contains(&purpose) || !policy.destinations.contains(&destination)
                {
                    return Err("provider policy does not authorize this use/destination".into());
                }
            }
            if m.parents.len() > MAX_ELEMENTS || m.transforms.len() > MAX_ELEMENTS {
                return Err("material provenance budget".into());
            }
            // A transformation cannot erase restrictions on an ancestor.
            for parent in &m.parents {
                walk(
                    registry,
                    *parent,
                    purpose,
                    destination,
                    now,
                    active,
                    checked,
                )?;
            }
            active.remove(&id);
            checked.insert(id);
            Ok(())
        }
        walk(
            self,
            id,
            purpose,
            destination,
            now,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
        )?;
        self.materials
            .get(&id)
            .ok_or_else(|| "material missing".into())
    }
}
/// The implementation resolves an authorized immutable universal resource/version.
/// No filesystem paths or cloud response harvesting are implicit in this interface.
pub trait DataSource {
    fn load(&self, payload: Id, max_bytes: usize) -> Result<Vec<u8>>;
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    LocalRule,
    Supervised,
    Continual,
    TeacherStudent,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    pub graph: Id,
    pub port: String,
    pub value: i64,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub cell: Id,
    pub value: i64,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub cell: Id,
    pub port: String,
    pub value: i64,
}
/// Initial scalar sample ABI. Stored as data, never a second program format.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Example {
    pub inputs: Vec<Signal>,
    pub targets: Vec<Target>,
    pub expected: Vec<Expected>,
}

/// Official provider customization is a separate effect from local learning.
/// The service must authorize ProviderCustomization before exporting each material.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CustomizationRequest {
    pub id: Id,
    pub provider: String,
    pub base_model: String,
    pub training_materials: Vec<Id>,
    pub evaluation_materials: Vec<Id>,
    pub limits: crate::intelligence::Limits,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum CustomizationEvent {
    Queued { job: String },
    Running { job: String },
    Completed { job: String, model: String },
    Failed { job: String, reason: String },
    Cancelled { job: String },
}
pub trait CustomizationProvider {
    fn start(&mut self, request: CustomizationRequest) -> Result<String>;
    fn poll(&mut self, job: &str) -> Result<CustomizationEvent>;
    fn cancel(&mut self, job: &str) -> Result<()>;
}
