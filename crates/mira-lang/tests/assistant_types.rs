use mira_core::{system::System, Type};
use mira_lang::{
    assist,
    inference::{self, Choice, Target},
    syntax,
};
#[test]
fn mira_learns_an_ambiguous_type_preference_and_keeps_it_after_restore() {
    let p = syntax::parse_draft("identity @2(value) = value\nspace Main @1:\n").unwrap();
    let target = Target::Parameter {
        definition: 2,
        index: 0,
    };
    let mut model = assist::new().unwrap();
    let before = assist::rank_types(&model, &p, &target).unwrap();
    assert!(before.iter().all(|r| r.score == 0.0));
    assist::observe_type(&mut model, &p, &target, Type::Bool, true).unwrap();
    let ranked = assist::rank_types(&model, &p, &target).unwrap();
    assert_eq!(ranked[0].choice.ty, Type::Bool);
    assert!(ranked[0].score > 0.0);
    let checked = inference::checked(&p, &[ranked[0].choice.clone()]).unwrap();
    assert_eq!(checked.code_modules[0].parameters[0].ty, Type::Bool);
    assert_eq!(mira_lang::behavior::execute(&checked, 2, &[1]).unwrap(), 1);
    let bytes = mira_lang::system::encode(&model).unwrap();
    let restored: System = mira_lang::system::decode(&bytes).unwrap();
    let after = assist::rank_types(&restored, &p, &target).unwrap();
    assert_eq!(after[0].score, ranked[0].score);
    assert_eq!(restored, model, "ranking mutated persistent state");
    let mut native = model.program.clone();
    native.root.cells[0].name = "kernel".into();
    native.root.cells[0].definition = mira_core::Definition::Code(10);
    assert!(!mira_lang::kernel::compile(&native).unwrap().is_empty());
}
#[test]
fn training_cannot_override_constraints_and_confidence_is_not_proof() {
    let mut p = syntax::parse_draft("double @2(value) = value * 2.0\nspace Main @1:\n").unwrap();
    // Reopen the source annotation for an explicit proposal, not a new type model.
    p.code_modules[0].parameters[0].ty = Type::Infer;
    let target = Target::Parameter {
        definition: 2,
        index: 0,
    };
    let mut model = assist::new().unwrap();
    let before = model.clone();
    assert!(assist::observe_type(&mut model, &p, &target, Type::Bool, true).is_err());
    assert_eq!(model, before);
    assert!(inference::checked(
        &p,
        &[Choice {
            target,
            ty: Type::Bool
        }]
    )
    .is_err());
}
