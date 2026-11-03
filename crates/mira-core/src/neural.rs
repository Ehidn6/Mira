//! Neural policies are parameters of ordinary graph cells, not a second topology.
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    Identity,
    Tanh,
    Sigmoid,
    Relu,
    Gelu,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LearningRule {
    None,
    Gradient,
    Hebbian,
    Oja,
    RewardModulated,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum GraphData {
    Data { type_name: String, bytes: Vec<u8> },
    Text(String),
    None,
}
