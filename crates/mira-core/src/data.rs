//! Portable data layouts. Aggregate values are mutable references for one activation.
use crate::{code::Parameter, *};
pub const MAX_WORDS: usize = 1_048_576;
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Record(Vec<Parameter>),
    Tensor { element: Type, shape: Vec<usize> },
    Array { element: Type, length: usize },
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub id: Id,
    pub name: String,
    pub kind: Kind,
}
impl Definition {
    pub fn len(&self) -> usize {
        match &self.kind {
            Kind::Record(fields) => fields.len(),
            Kind::Tensor { shape, .. } => shape
                .iter()
                .try_fold(1usize, |a, b| a.checked_mul(*b))
                .unwrap_or(usize::MAX),
            Kind::Array { length, .. } => *length,
        }
    }
    pub fn member_types(&self) -> Vec<Type> {
        match &self.kind {
            Kind::Record(fields) => fields.iter().map(|p| p.ty).collect(),
            Kind::Array { element, length } => alloc::vec![*element;*length],
            Kind::Tensor { element, .. } => alloc::vec![*element;self.len()],
        }
    }
    pub fn field(&self, name: &str) -> Option<(usize, Type)> {
        match &self.kind {
            Kind::Record(fields) => fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.name == name)
                .map(|(i, f)| (i, f.ty)),
            _ => None,
        }
    }
}
pub fn find(types: &[Definition], id: Id) -> Result<&Definition> {
    types
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("data type #{id} missing"))
}

/// Serialized owning storage. No activation-local handles can enter a buffer.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    pub ty: Id,
    pub words: Vec<i64>,
}
impl Buffer {
    pub fn validate(&self, types: &[Definition]) -> Result<()> {
        let definition = find(types, self.ty)?;
        if self.words.len() > MAX_WORDS || self.words.len() != definition.len() {
            return Err("persistent buffer type/shape mismatch".into());
        }
        let members = definition.member_types();
        if members
            .iter()
            .any(|ty| !matches!(ty, Type::I64 | Type::F32 | Type::Bool))
        {
            return Err("persistent buffer type/shape mismatch".into());
        }
        for (element, value) in members.iter().zip(&self.words) {
            match element {
                Type::Bool if !matches!(value, 0 | 1) => {
                    return Err("invalid persistent boolean".into());
                }
                Type::F32 => {
                    crate::vm::word_f32(*value)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}
