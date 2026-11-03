//! Prepared for the separate verification stage. Do not infer native/SMP
//! correctness from these portable learning-contract tests.
use mira_core::{learning::*, system::System, *};
use mira_lang::learning::{BufferExample, Experiment, TypedProfile};
use std::collections::{BTreeMap, BTreeSet};

const SOURCE: &str = "type Value = tensor[f32,1]\nvalue()->Value = Value.persist()\ntrain()->unit =\n  cells.weights()[0] += cells.input()[0]\n  cells.memory()[0] += 1.0\ninfer()->unit =\n  cells.output()[0] = cells.weights()[0] * cells.input()[0]\n  cells.memory()[0] += 10.0\nloss()->f32 =\n  error = cells.output()[0] - cells.target()[0]\n  error * error\nrun()->unit =\n  value = 0\nspace main:\n  model @100 = run():\n    space internal @101:\n      weights @102 = value()\n      memory @103 = value()\n      input @104 = value()\n      target @105 = value()\n      output @106 = value()\n";
struct Source(Vec<u8>);
impl DataSource for Source {
    fn load(&self, _: Id, max: usize) -> Result<Vec<u8>> {
        if self.0.len() > max {
            return Err("sample too large".into());
        }
        Ok(self.0.clone())
    }
}
fn setup() -> (System, TypedProfile, Registry, Source) {
    let mut system = System::from_program(mira_lang::syntax::parse(SOURCE).unwrap()).unwrap();
    let behavior = |name: &str| {
        system
            .program
            .code_modules
            .iter()
            .find(|m| m.name == name)
            .unwrap()
            .id
    };
    let profile = TypedProfile {
        owner: 100,
        train: behavior("train"),
        infer: behavior("infer"),
        loss: behavior("loss"),
        inputs: BTreeSet::from([104, 105]),
        parameters: BTreeSet::from([102]),
        state: BTreeSet::from([103, 106]),
        scratch_bytes: 1024,
        loss_scale: 1000,
    };
    let ty = system
        .program
        .data_types()
        .iter()
        .find(|t| t.name == "Value")
        .unwrap()
        .id;
    for id in 102..=106 {
        system
            .buffers
            .insert(id, data::Buffer { ty, words: vec![0] });
    }
    let sample = BufferExample {
        inputs: BTreeMap::from([
            (
                104,
                data::Buffer {
                    ty,
                    words: vec![1.0f32.to_bits() as i64],
                },
            ),
            (
                105,
                data::Buffer {
                    ty,
                    words: vec![1.0f32.to_bits() as i64],
                },
            ),
        ]),
    };
    let bytes = serde_json::to_vec(&sample).unwrap();
    let material = Material {
        id: 1,
        payload: 1,
        fingerprint: *blake3::hash(&bytes).as_bytes(),
        source: mira_core::learning::Source::Human {
            owner: "owner".into(),
        },
        license: None,
        purpose: "typed contract fixture".into(),
        allowed: BTreeSet::from([Use::LocalTraining, Use::Evaluation]),
        destinations: BTreeSet::from([system.program.root.id]),
        evidence: Some(Evidence {
            reviewer: "owner".into(),
            reference: "fixture".into(),
            valid_from: 0,
            valid_until: None,
        }),
        parents: vec![],
        transforms: vec![],
        provider_policy: None,
    };
    let registry = Registry {
        materials: BTreeMap::from([(1, material)]),
        policies: vec![],
    };
    (system, profile, registry, Source(bytes))
}
#[test]
fn autonomous_typed_learning_publishes_parameters_without_leaking_recurrent_state() {
    let (mut system, profile, registry, source) = setup();
    let scope = system.program.root.id;
    let loss = profile.loss;
    let mut experiment =
        Experiment::begin_typed(&system, scope, Method::LocalRule, profile).unwrap();
    let mut history = mira_lang::system::History::default();
    let outcome = experiment
        .adapt(
            &mut system,
            &mut history,
            &Default::default(),
            &registry,
            &source,
            &[1],
            &[1],
            1,
            loss,
            0,
        )
        .unwrap();
    assert_eq!(outcome.before.loss, 1000);
    assert_eq!(outcome.after.loss, 0);
    assert!(outcome.receipt.is_some());
    assert_eq!(system.buffers[&102].words, vec![1.0f32.to_bits() as i64]);
    assert_eq!(system.buffers[&103].words, vec![0]);
    assert_eq!(system.buffers[&104].words, vec![0]);
    assert_eq!(system.buffers[&106].words, vec![0]);
    let stored = mira_lang::system::encode(&system).unwrap();
    assert_eq!(mira_lang::system::decode(&stored).unwrap(), system);
}
#[test]
fn undeclared_buffer_writes_do_not_publish_or_change_the_candidate() {
    let (system, mut profile, registry, source) = setup();
    profile.state.remove(&103);
    let mut experiment =
        Experiment::begin_typed(&system, system.program.root.id, Method::LocalRule, profile)
            .unwrap();
    let before = experiment.candidate().clone();
    assert!(
        experiment
            .train(&registry, &source, &[1], 1, 0)
            .unwrap_err()
            .contains("undeclared resource")
    );
    assert_eq!(experiment.candidate(), &before);
}
