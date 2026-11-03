//! Candidate ranking through an ordinary Mira graph. Constraints decide which
//! types are legal; explicit acceptance/correction supplies the learning signal.
//! This development-host adapter does not imply a native editor integration.
use crate::inference::{self, Choice, Report, Target};
use mira_core::{data::Buffer, system::System, *};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
const SOURCE: &str = include_str!("../../../compiler/assist.mira");
const OWNER: Id = 100;
const FEATURES: Id = 102;
const WEIGHTS: Id = 103;
const CHOICE: Id = 104;
const RESPONSE: Id = 105;
fn program() -> Result<&'static Program> {
    static PROGRAM: OnceLock<Result<Program>> = OnceLock::new();
    PROGRAM
        .get_or_init(|| crate::syntax::parse(SOURCE))
        .as_ref()
        .map_err(Clone::clone)
}
pub fn new() -> Result<System> {
    System::from_program(program()?.clone())
}
fn validate(model: &System) -> Result<()> {
    if &model.program != program()? {
        return Err("assistant graph differs from its checked source".into());
    }
    model.validate()?;
    for buffer in model.buffers.values() {
        if buffer.ty != 4
            && buffer
                .words
                .iter()
                .any(|v| !f32::from_bits(*v as u32).is_finite())
        {
            return Err("assistant contains non-finite parameters/state".into());
        }
    }
    Ok(())
}
fn class(ty: Type) -> Result<i64> {
    Ok(match ty {
        Type::I64 => 0,
        Type::F32 => 1,
        Type::Bool => 2,
        Type::Event => 3,
        Type::Unit => 4,
        Type::Addr => 5,
        Type::Data(_) => 6,
        Type::Infer => return Err("cannot rank unresolved type".into()),
    })
}
fn features(program: &Program, target: &Target, choices: usize) -> Result<[f32; 8]> {
    let (id, kind, index, depth) = match target {
        Target::Parameter { definition, index } => (*definition, 0, *index, 0),
        Target::Result { definition } => (*definition, 1, 0, 0),
        Target::Hole {
            definition, path, ..
        } => (*definition, 2, 0, path.len()),
    };
    let d = code::find(&program.code_modules, id).ok_or("assistant target definition missing")?;
    let mut x = [0.0; 8];
    x[0] = 1.0;
    x[1 + kind] = 1.0;
    x[4] = (choices.min(32) as f32) / 32.0;
    x[5] = (d.parameters.len().min(32) as f32) / 32.0;
    x[6] = (index.min(32) as f32) / 32.0;
    x[7] = (depth.min(32) as f32) / 32.0;
    Ok(x)
}
fn report(program: &Program) -> Result<Report> {
    inference::analyze(&mut program.clone(), &[], false)
}
fn inputs(model: &mut System, x: [f32; 8], ty: Type, response: Option<f32>) -> Result<()> {
    model.buffers.insert(
        FEATURES,
        Buffer {
            ty: 2,
            words: x.into_iter().map(|v| v.to_bits() as i64).collect(),
        },
    );
    model.buffers.insert(
        CHOICE,
        Buffer {
            ty: 4,
            words: vec![class(ty)?],
        },
    );
    if let Some(response) = response {
        model.buffers.insert(
            RESPONSE,
            Buffer {
                ty: 5,
                words: vec![response.to_bits() as i64],
            },
        );
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankedType {
    pub choice: Choice,
    pub score: f32,
}
/// Ranking never changes either the source or the model's persistent state.
/// Even the first ranked choice must pass inference::checked before use.
pub fn rank_types(model: &System, source: &Program, target: &Target) -> Result<Vec<RankedType>> {
    validate(model)?;
    let report = report(source)?;
    let required = report
        .requirements
        .iter()
        .find(|r| &r.target == target)
        .ok_or("inference target is already resolved or absent")?;
    let x = features(source, target, required.candidates.len())?;
    let mut draft = model.clone();
    let mut ranked = Vec::new();
    for ty in &required.candidates {
        inputs(&mut draft, x, *ty, None)?;
        let output =
            crate::runtime::execute_behavior_system(&mut draft, OWNER, 10, 1, false, 4096)?;
        let bits = output.terminals.first().ok_or("assistant score missing")?.1;
        let score = f32::from_bits(bits as u32);
        if !score.is_finite() {
            return Err("assistant produced a non-finite score".into());
        }
        ranked.push(RankedType {
            choice: Choice {
                target: target.clone(),
                ty: *ty,
            },
            score,
        });
    }
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.choice.ty.cmp(&b.choice.ty))
    });
    Ok(ranked)
}
/// A compiler acceptance is not a training label. Call this only for an actual
/// accepted/rejected choice or an explicitly supplied example of intended use.
/// System buffers are the model state, saved through the existing System store.
pub fn observe_type(
    model: &mut System,
    source: &Program,
    target: &Target,
    ty: Type,
    accepted: bool,
) -> Result<()> {
    validate(model)?;
    let report = report(source)?;
    let required = report
        .requirements
        .iter()
        .find(|r| &r.target == target)
        .ok_or("inference target missing")?;
    if !required.candidates.contains(&ty) {
        return Err("type observation contradicts deterministic constraints".into());
    }
    let x = features(source, target, required.candidates.len())?;
    let mut draft = model.clone();
    inputs(&mut draft, x, ty, Some(if accepted { 1.0 } else { -1.0 }))?;
    crate::runtime::execute_behavior_system(&mut draft, OWNER, 11, 1, false, 4096)?;
    validate(&draft)?;
    // Keep only persistent learning. Transient inputs never leak into source.
    if let Some(weights) = draft.buffers.get(&WEIGHTS) {
        model.buffers.insert(WEIGHTS, weights.clone());
    }
    Ok(())
}
