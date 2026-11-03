//! Structural proposals edit the same graph that the compiler and IDE consume.
use crate::{system::*, *};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Edit {
    AddCell {
        space: Id,
        cell: Instance,
    },
    RemoveCell {
        cell: Id,
    },
    Connect {
        space: Id,
        link: Link,
    },
    Disconnect {
        space: Id,
        link: Id,
    },
    SetParameter {
        cell: Id,
        value: i64,
    },
    /// A revision-scoped location in the authoritative code tree. The expected
    /// subtree prevents silently editing a different expression after a change.
    ReplaceExpression {
        definition: Id,
        path: alloc::vec::Vec<usize>,
        expected: code::Expr,
        value: code::Expr,
    },
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    Human,
    Assistant,
    System,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Proposal {
    pub origin: Origin,
    pub explanation: String,
    pub revision: u64,
    pub label: String,
    pub edits: Vec<Edit>,
}
impl Proposal {
    /// Isolation is not authority: candidates still pass compilation/evaluation
    /// and the deterministic System::edit gate before they are published.
    pub fn prepare(&self, base: &System) -> Result<System> {
        if self.label.trim().is_empty()
            || self.origin == Origin::Assistant && self.explanation.trim().is_empty()
        {
            return Err("proposal needs a label and assistant explanation".into());
        }
        if self.revision != base.revision {
            return Err("stale graph proposal".into());
        }
        if self.edits.len() > MAX_ELEMENTS {
            return Err("proposal operation budget".into());
        }
        fn fresh(s: &Space, floor: Id) -> bool {
            s.id >= floor
                && s.links.iter().all(|l| l.id >= floor)
                && s.cells
                    .iter()
                    .all(|c| c.id >= floor && c.space.as_ref().is_none_or(|s| fresh(s, floor)))
        }
        fn remove(s: &mut Space, id: Id) -> bool {
            if let Some(i) = s.cells.iter().position(|c| c.id == id) {
                s.cells.remove(i);
                s.links.retain(|l| l.from.cell != id && l.to.cell != id);
                true
            } else {
                s.cells
                    .iter_mut()
                    .filter_map(|c| c.space.as_mut())
                    .any(|s| remove(s, id))
            }
        }
        let mut draft = base.clone();
        for edit in &self.edits {
            match edit {
                Edit::ReplaceExpression {
                    definition,
                    path,
                    expected,
                    value,
                } => {
                    let module = code::find_mut(&mut draft.program.code_modules, *definition)
                        .ok_or("proposal code definition missing")?;
                    let target = code::expression_mut(module, path)?;
                    if target != expected {
                        return Err("stale code expression proposal".into());
                    }
                    *target = value.clone();
                    // Apply the existing structural bounds to the new tree.
                    code::expressions(module)?;
                }
                Edit::AddCell { space, cell } => {
                    if cell.id < base.program.next_identity
                        || cell
                            .space
                            .as_ref()
                            .is_some_and(|s| !fresh(s, base.program.next_identity))
                    {
                        return Err("proposal cannot reuse a historical identity".into());
                    }
                    draft
                        .program
                        .root
                        .find_mut(*space)
                        .ok_or("proposal graph missing")?
                        .cells
                        .push(cell.clone());
                }
                Edit::RemoveCell { cell } => {
                    if !remove(&mut draft.program.root, *cell) {
                        return Err("proposal cell missing".into());
                    }
                    draft
                        .parameters
                        .retain(|id, _| draft.program.root.find_cell(*id).is_some());
                    draft
                        .buffers
                        .retain(|id, _| draft.program.root.find_cell(*id).is_some());
                    draft
                        .delays
                        .retain(|id, _| draft.program.root.find_cell(*id).is_some());
                    let exists = |id: &Id| {
                        draft.program.root.find_cell(*id).is_some()
                            || draft.program.root.find(*id).is_some()
                    };
                    draft.data.retain(|id, _| exists(id));
                    draft.memories.retain(|id, _| exists(id));
                    draft.protected.retain(exists);
                }
                Edit::Connect { space, link } => {
                    if link.id < base.program.next_identity {
                        return Err("proposal cannot reuse a historical identity".into());
                    }
                    draft
                        .program
                        .root
                        .find_mut(*space)
                        .ok_or("proposal graph missing")?
                        .links
                        .push(link.clone());
                }
                Edit::Disconnect { space, link } => {
                    let s = draft
                        .program
                        .root
                        .find_mut(*space)
                        .ok_or("proposal graph missing")?;
                    if !s.links.iter().any(|l| l.id == *link) {
                        return Err("proposal link missing".into());
                    }
                    s.links.retain(|l| l.id != *link);
                }
                Edit::SetParameter { cell, value } => {
                    draft
                        .program
                        .cell_mut(*cell)
                        .ok_or("proposal cell missing")?
                        .parameter = *value
                }
            }
        }
        draft.program.next_identity = draft.program.next_id()?;
        draft.validate()?;
        Ok(draft)
    }
}
