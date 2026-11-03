//! Passive, immutable-version information attached to the computational system.
//! References pin exact versions. Relations and executions are ordinary objects;
//! neither creates a second component tree or grants mutation authority.
use crate::*;
use serde::{Deserialize, Serialize};
pub type Digest = [u8; 32];
pub const PROGRAM_FORMAT: &str = "mira.program/1";
pub const STATE_FORMAT: &str = "mira.state/1";
pub const RESULT_FORMAT: &str = "mira.result/1";
pub const MAX_VERSIONS: usize = 256;
pub const MAX_OBJECTS: usize = 1024;
pub const MAX_TOTAL_VERSIONS: usize = 4096;
const MAX_TEXT: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reference {
    pub object: Id,
    pub version: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub producer: String,
    pub inputs: Vec<Reference>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub label: String,
    pub ty: String,
    pub value: String,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Program {
        artifact: Digest,
    },
    Execution {
        program: Reference,
        context: Option<Reference>,
        target: Option<Id>,
        tick: u64,
        success: bool,
        input_state: Digest,
        result: Option<Digest>,
        outputs: Vec<Observation>,
        error: Option<String>,
    },
    Relation {
        from: Reference,
        to: Reference,
        kind: String,
    },
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub number: u64,
    pub content: Content,
    pub origin: Origin,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub id: Id,
    pub versions: Vec<Version>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub format: String,
    pub bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Store {
    pub objects: BTreeMap<Id, Object>,
    #[serde(with = "artifact_entries")]
    pub artifacts: BTreeMap<Digest, Artifact>,
    pub current_context: Option<Reference>,
    pub program: Option<Id>,
}
impl Store {
    pub fn get(&self, reference: &Reference) -> Result<&Version> {
        let index = reference
            .version
            .checked_sub(1)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("information version must be positive")?;
        let version = self
            .objects
            .get(&reference.object)
            .and_then(|object| object.versions.get(index))
            .ok_or("information reference is missing")?;
        if version.number != reference.version {
            return Err("information version sequence is invalid".into());
        }
        Ok(version)
    }
    pub fn latest(&self, id: Id) -> Result<Reference> {
        let version = self
            .objects
            .get(&id)
            .and_then(|object| object.versions.last())
            .ok_or("information object is missing")?;
        Ok(Reference {
            object: id,
            version: version.number,
        })
    }
    pub fn create(&mut self, id: Id, content: Content, origin: Origin) -> Result<Reference> {
        if id == 0 || self.objects.contains_key(&id) {
            return Err("information identity is zero or occupied".into());
        }
        let mut draft = self.clone();
        draft.objects.insert(
            id,
            Object {
                id,
                versions: alloc::vec![Version {
                    number: 1,
                    content,
                    origin
                }],
            },
        );
        draft.validate()?;
        *self = draft;
        Ok(Reference {
            object: id,
            version: 1,
        })
    }
    pub fn revise(
        &mut self,
        id: Id,
        expected_version: u64,
        content: Content,
        origin: Origin,
    ) -> Result<Reference> {
        if self.latest(id)?.version != expected_version {
            return Err("information version changed; revision is stale".into());
        }
        let number = expected_version
            .checked_add(1)
            .ok_or("information version exhausted")?;
        let mut draft = self.clone();
        draft
            .objects
            .get_mut(&id)
            .ok_or("information object missing")?
            .versions
            .push(Version {
                number,
                content,
                origin,
            });
        draft.validate()?;
        *self = draft;
        Ok(Reference {
            object: id,
            version: number,
        })
    }
    /// Structural edits may append information, never rewrite or erase a
    /// previously committed version or change an artifact under the same digest.
    pub fn extends(&self, previous: &Self) -> Result<()> {
        for (id, object) in &previous.objects {
            if self.objects.get(id).is_none_or(|next| {
                next.id != object.id || !next.versions.starts_with(&object.versions)
            }) {
                return Err("committed information versions cannot be rewritten or removed".into());
            }
        }
        for (digest, artifact) in &previous.artifacts {
            if self.artifacts.get(digest) != Some(artifact) {
                return Err(
                    "committed information artifacts cannot be rewritten or removed".into(),
                );
            }
        }
        Ok(())
    }
    fn artifact(&self, digest: &Digest, format: &str) -> Result<()> {
        if self
            .artifacts
            .get(digest)
            .is_none_or(|artifact| artifact.format != format)
        {
            return Err(format!(
                "information artifact is missing or has the wrong format: {format}"
            ));
        }
        Ok(())
    }
    /// Core checks structure, bounds and pinned references. The host additionally
    /// verifies artifact digests; a digest is not an authorization capability.
    pub fn validate(&self) -> Result<()> {
        if self.objects.len() > MAX_OBJECTS || self.artifacts.len() > MAX_OBJECTS {
            return Err("information object/artifact budget exceeded".into());
        }
        let mut bytes = 0usize;
        let mut add = |size: usize| -> Result<()> {
            bytes = bytes.checked_add(size).ok_or("information size overflow")?;
            if bytes > MAX_TOTAL_BYTES {
                return Err("information byte budget exceeded".into());
            }
            Ok(())
        };
        for artifact in self.artifacts.values() {
            if !matches!(
                artifact.format.as_str(),
                PROGRAM_FORMAT | STATE_FORMAT | RESULT_FORMAT
            ) || artifact.bytes.is_empty()
            {
                return Err("unsupported or empty information artifact".into());
            }
            add(artifact.bytes.len())?;
        }
        let mut versions = 0usize;
        for (id, object) in &self.objects {
            if *id == 0
                || object.id != *id
                || object.versions.is_empty()
                || object.versions.len() > MAX_VERSIONS
            {
                return Err("invalid information identity or version budget".into());
            }
            versions = versions
                .checked_add(object.versions.len())
                .ok_or("information version overflow")?;
            if versions > MAX_TOTAL_VERSIONS {
                return Err("information total version budget exceeded".into());
            }
            for (index, version) in object.versions.iter().enumerate() {
                if version.number != (index as u64) + 1 {
                    return Err("information versions must form a consecutive sequence".into());
                }
                if version.origin.producer.trim().is_empty()
                    || version.origin.producer.len() > 1024
                    || version.origin.inputs.len() > 256
                {
                    return Err("invalid information provenance".into());
                }
                add(128 + version.origin.producer.len() + version.origin.inputs.len() * 16)?;
                for input in &version.origin.inputs {
                    self.get(input)?;
                    if input.object == *id && input.version >= version.number {
                        return Err("information provenance cannot depend on its own current/future version".into());
                    }
                }
                match &version.content {
                    Content::Text(text) => {
                        if text.len() > MAX_TEXT {
                            return Err("information text budget exceeded".into());
                        }
                        add(text.len())?;
                    }
                    Content::Program { artifact } => self.artifact(artifact, PROGRAM_FORMAT)?,
                    Content::Relation { from, to, kind } => {
                        self.get(from)?;
                        self.get(to)?;
                        if kind.trim().is_empty() || kind.len() > 128 {
                            return Err("invalid information relation kind".into());
                        }
                        add(kind.len())?;
                    }
                    Content::Execution {
                        program,
                        context,
                        target,
                        success,
                        input_state,
                        result,
                        outputs,
                        error,
                        ..
                    } => {
                        if !matches!(self.get(program)?.content, Content::Program { .. }) {
                            return Err(
                                "execution program reference is not a program version".into()
                            );
                        }
                        if let Some(context) = context {
                            self.get(context)?;
                        }
                        if target == &Some(0)
                            || *success && error.is_some()
                            || !*success && error.as_ref().is_none_or(|e| e.trim().is_empty())
                        {
                            return Err("invalid information execution result".into());
                        }
                        self.artifact(input_state, STATE_FORMAT)?;
                        if *success != result.is_some() {
                            return Err(
                                "execution success requires an exact result artifact".into()
                            );
                        }
                        if let Some(result) = result {
                            self.artifact(result, RESULT_FORMAT)?;
                        }
                        if outputs.len() > 4096 {
                            return Err("information observation budget exceeded".into());
                        }
                        for output in outputs {
                            if output.label.is_empty()
                                || output.label.len() > 4096
                                || output.ty.is_empty()
                                || output.ty.len() > 1024
                                || output.value.len() > MAX_TEXT
                            {
                                return Err("invalid information observation".into());
                            }
                            add(output.label.len())?;
                            add(output.ty.len())?;
                            add(output.value.len())?;
                        }
                        if let Some(error) = error {
                            if error.len() > MAX_TEXT {
                                return Err("information error budget exceeded".into());
                            }
                            add(error.len())?;
                        }
                    }
                }
            }
        }
        // Relations may form cycles; provenance may not. Every version is a
        // separate vertex, so revising an object from its previous version is
        // ordinary acyclic history rather than a false self-cycle.
        let mut pending = BTreeMap::<Reference, usize>::new();
        let mut dependents = BTreeMap::<Reference, Vec<Reference>>::new();
        for (id, object) in &self.objects {
            for version in &object.versions {
                let current = Reference {
                    object: *id,
                    version: version.number,
                };
                pending.insert(current.clone(), version.origin.inputs.len());
                for input in &version.origin.inputs {
                    dependents
                        .entry(input.clone())
                        .or_default()
                        .push(current.clone());
                }
            }
        }
        let mut ready: Vec<_> = pending
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(reference, _)| reference.clone())
            .collect();
        let mut processed = 0usize;
        while let Some(reference) = ready.pop() {
            processed += 1;
            if let Some(children) = dependents.get(&reference) {
                for child in children {
                    let count = pending
                        .get_mut(child)
                        .ok_or("information provenance dependency missing")?;
                    *count = count
                        .checked_sub(1)
                        .ok_or("information provenance count invalid")?;
                    if *count == 0 {
                        ready.push(child.clone());
                    }
                }
            }
        }
        if processed != versions {
            return Err("information provenance contains a cycle".into());
        }
        if let Some(context) = &self.current_context {
            self.get(context)?;
        }
        if let Some(id) = self.program {
            if !matches!(
                self.get(&self.latest(id)?)?.content,
                Content::Program { .. }
            ) {
                return Err(
                    "current information program does not identify a program object".into(),
                );
            }
        }
        Ok(())
    }
}
mod artifact_entries {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        map: &BTreeMap<Digest, Artifact>,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<BTreeMap<Digest, Artifact>, D::Error> {
        let entries = Vec::<(Digest, Artifact)>::deserialize(deserializer)?;
        let mut map = BTreeMap::new();
        for (digest, artifact) in entries {
            if map.insert(digest, artifact).is_some() {
                return Err(serde::de::Error::custom(
                    "duplicate information artifact digest",
                ));
            }
        }
        Ok(map)
    }
}
