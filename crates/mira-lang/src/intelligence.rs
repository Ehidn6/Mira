//! Local tool boundary shared by future cloud and native Mira intelligence drivers.
//! Dispatch has immutable access to the system and can only return a staged proposal.
use crate::context;
use mira_core::{
    intelligence::ToolCall,
    system::System,
    transform::{Edit, Origin, Proposal},
    *,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Created by the trusted owner/service, never deserialized from model arguments.
pub struct Grant {
    pub graphs: BTreeSet<Id>,
    pub revision: u64,
    pub inspect: bool,
    pub propose: bool,
    pub max_calls: usize,
    pub max_argument_bytes: usize,
    pub max_reply_bytes: usize,
}
pub struct ToolSession {
    grant: Grant,
    seen: BTreeSet<String>,
    remaining: usize,
}
#[derive(Serialize)]
pub struct Reply {
    pub call_id: String,
    pub output: serde_json::Value,
    /// Host side only. The UI decides whether/when to stage this for human review.
    #[serde(skip_serializing)]
    pub proposal: Option<Proposal>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorArgs {
    behavior: Id,
    revision: u64,
    known: Option<String>,
}
impl ToolSession {
    pub fn new(grant: Grant) -> Result<Self> {
        if grant.graphs.is_empty()
            || grant.max_calls == 0
            || grant.max_calls > 1024
            || grant.max_argument_bytes == 0
            || grant.max_argument_bytes > 1024 * 1024
            || grant.max_reply_bytes == 0
            || grant.max_reply_bytes > 1024 * 1024
        {
            return Err("invalid intelligence tool grant".into());
        }
        let remaining = grant.max_calls;
        Ok(Self {
            grant,
            remaining,
            seen: BTreeSet::new(),
        })
    }
    pub fn remaining(&self) -> usize {
        self.remaining
    }
    fn graph(&self, id: Id) -> Result<()> {
        if !self.grant.graphs.contains(&id) {
            return Err("graph is outside intelligence tool grant".into());
        }
        Ok(())
    }
    pub fn dispatch(&mut self, system: &System, call: &ToolCall) -> Result<Reply> {
        if call.id.is_empty()
            || call.id.len() > 256
            || call.name.len() > 128
            || call.arguments.len() > self.grant.max_argument_bytes
        {
            return Err("invalid or oversized intelligence tool call".into());
        }
        if self.seen.contains(&call.id) {
            return Err("duplicate intelligence tool call identity".into());
        }
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("intelligence tool call budget exhausted")?;
        self.seen.insert(call.id.clone());
        if system.revision != self.grant.revision {
            return Err("intelligence context changed; owner must issue a fresh grant".into());
        }
        let mut proposal = None;
        let output = match call.name.as_str() {
            "mira_project_context" => {
                if !self.grant.inspect {
                    return Err("graph inspection is not granted".into());
                }
                let mut request: context::Request =
                    serde_json::from_slice(&call.arguments).map_err(|e| e.to_string())?;
                self.graph(request.space.unwrap_or(system.program.root.id))?;
                if request.revision.is_some_and(|r| r != self.grant.revision) {
                    return Err("tool request uses another revision".into());
                }
                request.revision = Some(self.grant.revision);
                serde_json::to_value(context::page(system, &request)?).map_err(|e| e.to_string())?
            }
            "mira_behavior_context" => {
                if !self.grant.inspect {
                    return Err("behavior inspection is not granted".into());
                }
                let args: BehaviorArgs =
                    serde_json::from_slice(&call.arguments).map_err(|e| e.to_string())?;
                self.graph(system.program.root.id)?;
                serde_json::to_value(context::behavior(
                    system,
                    args.behavior,
                    args.revision,
                    args.known.as_deref(),
                )?)
                .map_err(|e| e.to_string())?
            }
            "mira_propose_graph" => {
                if !self.grant.propose {
                    return Err("graph proposals are not granted".into());
                }
                let mut p: Proposal =
                    serde_json::from_slice(&call.arguments).map_err(|e| e.to_string())?;
                p.origin = Origin::Assistant;
                for edit in &p.edits {
                    let graph = match edit {
                        Edit::ReplaceExpression { .. } => system.program.root.id,
                        Edit::AddCell { space, .. }
                        | Edit::Connect { space, .. }
                        | Edit::Disconnect { space, .. } => *space,
                        Edit::RemoveCell { cell } | Edit::SetParameter { cell, .. } => {
                            mira_core::inspect::cells(&system.program)
                                .into_iter()
                                .find(|info| info.cell.id == *cell)
                                .ok_or("proposal cell missing")?
                                .space
                                .id
                        }
                    };
                    self.graph(graph)?;
                }
                let candidate = p.prepare(system)?;
                crate::system::check(&candidate)?;
                let output = serde_json::json!({"status":"proposed","label":p.label,"revision":p.revision,"operations":p.edits.len(),"requires_human_review":true});
                proposal = Some(p);
                output
            }
            // No accept/execute/shell/vault tool exists in this interface.
            _ => return Err("unknown or forbidden intelligence tool".into()),
        };
        if serde_json::to_vec(&output)
            .map_err(|e| e.to_string())?
            .len()
            > self.grant.max_reply_bytes
        {
            return Err("intelligence tool reply exceeds the grant".into());
        }
        Ok(Reply {
            call_id: call.id.clone(),
            output,
            proposal,
        })
    }
}
