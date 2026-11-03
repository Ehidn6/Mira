//! Provider-neutral intelligence service contracts. Local graph execution imports
//! none of these services and never requires credentials, network or an accelerator.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Document,
    Other(String),
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub provider: String,
    /// Catalog identity, not a language keyword or a fixed latest-model alias.
    pub id: String,
    pub inputs: BTreeSet<Modality>,
    pub outputs: BTreeSet<Modality>,
    pub streaming: bool,
    pub tools: bool,
    pub context_tokens: Option<u64>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    /// Data attached to an authorized graph identity, not a host path or an arbitrary URL.
    Resource {
        resource: Id,
        modality: Modality,
        media_type: String,
    },
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
    pub tool_call: Option<String>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// UTF-8 JSON Schema. The provider adapter translates its supported subset.
    pub schema: Vec<u8>,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    LocalHistory,
    ProviderHistory,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Continuation {
    pub provider: String,
    /// Preserve complete provider output items/phase/encrypted reasoning or a
    /// remote continuation reference. Never reinterpret them as Mira authority.
    pub opaque: Vec<u8>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub max_total_tokens: u64,
    pub max_tool_calls: u32,
    pub max_payload_bytes: usize,
    /// Optional ceiling in millionths of the configured currency, no baked-in price.
    pub max_cost_microunits: Option<u64>,
    pub currency: Option<String>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: Id,
    pub task: Id,
    pub provider: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub continuation: Option<Continuation>,
    pub retention: Retention,
    pub limits: Limits,
}
impl Request {
    /// Transport preflight still must count/tokenize inputs and reserve budget.
    pub fn validate(&self, model: &Model, resources: &BTreeSet<Id>) -> Result<()> {
        if self.id == 0
            || self.task == 0
            || self.provider != model.provider
            || self.model != model.id
            || self.model.is_empty()
        {
            return Err("invalid intelligence request/model identity".into());
        }
        let l = &self.limits;
        if l.max_input_tokens == 0
            || l.max_output_tokens == 0
            || l.max_payload_bytes == 0
            || l.max_total_tokens < l.max_output_tokens
            || model.context_tokens.is_some_and(|n| {
                l.max_input_tokens
                    .checked_add(l.max_output_tokens)
                    .is_none_or(|v| v > n)
            })
        {
            return Err("invalid intelligence request limits".into());
        }
        if self
            .continuation
            .as_ref()
            .is_some_and(|c| c.provider != self.provider)
        {
            return Err("continuation belongs to another provider".into());
        }
        if !self.tools.is_empty() && (!model.tools || l.max_tool_calls == 0) {
            return Err("model or request does not permit tools".into());
        }
        if l.max_cost_microunits.is_some() && l.currency.as_ref().is_none_or(|s| s.is_empty()) {
            return Err("cost ceiling needs an explicit currency".into());
        }
        let mut bytes = 0usize;
        let mut add = |n: usize| -> Result<()> {
            bytes = bytes
                .checked_add(n)
                .ok_or("intelligence payload overflow")?;
            if bytes > l.max_payload_bytes {
                return Err("intelligence payload limit".into());
            }
            Ok(())
        };
        if let Some(c) = &self.continuation {
            add(c.opaque.len())?;
        }
        let mut names = BTreeSet::new();
        for t in &self.tools {
            if t.name.is_empty() || !names.insert(&t.name) {
                return Err("invalid or duplicate tool name".into());
            }
            add(t.name.len())?;
            add(t.description.len())?;
            if core::str::from_utf8(&t.schema).is_err() || t.schema.is_empty() {
                return Err("tool schema must be UTF-8 JSON".into());
            }
            add(t.schema.len())?;
        }
        for message in &self.messages {
            if message.role == Role::Tool
                && message.tool_call.as_ref().is_none_or(|id| id.is_empty())
            {
                return Err("tool output requires a call identity".into());
            }
            if let Some(id) = &message.tool_call {
                add(id.len())?;
            }
            for part in &message.content {
                match part {
                    Content::Text(text) => {
                        if !model.inputs.contains(&Modality::Text) {
                            return Err("model does not support text input".into());
                        }
                        add(text.len())?;
                    }
                    Content::Resource {
                        resource,
                        modality,
                        media_type,
                    } => {
                        if !resources.contains(resource) {
                            return Err("intelligence resource is outside the grant".into());
                        }
                        if !model.inputs.contains(modality) {
                            return Err("model does not support resource modality".into());
                        }
                        if media_type.is_empty() {
                            return Err("resource media type missing".into());
                        }
                        add(media_type.len())?;
                    }
                }
            }
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Cached input and reasoning are subsets, not extra tokens to double-count.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub cost_microunits: Option<u64>,
    pub currency: Option<String>,
    pub price_revision: Option<String>,
}
impl Usage {
    pub fn total_tokens(&self) -> Result<u64> {
        if self.cached_input_tokens > self.input_tokens
            || self.reasoning_output_tokens > self.output_tokens
        {
            return Err("invalid token usage subsets".into());
        }
        self.input_tokens
            .checked_add(self.output_tokens)
            .ok_or_else(|| "token accounting overflow".into())
    }
    pub fn check(&self, limits: &Limits) -> Result<()> {
        if self.input_tokens > limits.max_input_tokens
            || self.output_tokens > limits.max_output_tokens
            || self.total_tokens()? > limits.max_total_tokens
        {
            return Err("intelligence token limit exceeded".into());
        }
        if let Some(cap) = limits.max_cost_microunits {
            let cost = self
                .cost_microunits
                .ok_or("cost ceiling requires a priced reservation/usage record")?;
            if self.currency.as_ref().is_none_or(|s| s.is_empty())
                || self.currency != limits.currency
                || self.price_revision.as_ref().is_none_or(|s| s.is_empty())
                || cost > cap
            {
                return Err("unpriced or over-budget intelligence usage".into());
            }
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Vec<u8>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started {
        request: Id,
        provider_response: String,
    },
    TextDelta {
        request: Id,
        item: String,
        text: String,
    },
    /// Emitted only after complete bounded arguments have been assembled.
    ToolReady {
        request: Id,
        call: ToolCall,
    },
    Usage {
        request: Id,
        usage: Usage,
    },
    Completed {
        request: Id,
        continuation: Option<Continuation>,
    },
    Failed {
        request: Id,
        code: String,
        message: String,
    },
    Cancelled {
        request: Id,
    },
    Resource {
        request: Id,
        resource: Id,
        modality: Modality,
        media_type: String,
    },
}
/// An event-driven driver can be hosted in MiraOS or the temporary Linux host.
/// No provider implementation, network transport or key store is implied here.
pub trait Provider {
    fn models(&mut self) -> Result<Vec<Model>>;
    fn start(&mut self, request: Request) -> Result<()>;
    fn poll(&mut self, request: Id, limit: usize) -> Result<Vec<Event>>;
    fn cancel(&mut self, request: Id) -> Result<()>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRef(pub String);
/// A capability to use a key. Implementations must avoid persisting/logging secret
/// bytes and must enforce the owner/provider binding. A reference is not a key.
pub trait CredentialVault {
    fn use_secret(
        &mut self,
        reference: &CredentialRef,
        provider: &str,
        consumer: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()>;
}
/// Session data belongs to the intelligence component, never the graph edit log.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: Id,
    pub task: Id,
    pub messages: Vec<Message>,
    pub continuation: Option<Continuation>,
    pub usage: Vec<Usage>,
}
