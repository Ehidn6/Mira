use mira_core::{
    code::{self, Expr},
    Type,
};
use mira_lang::{
    inference::{self, Choice, Target},
    syntax,
};
#[test]
fn types_follow_operations_calls_and_graph_constraints() {
    let p = syntax::parse("scale(x) = x * 2.0\nmain()->f32 = scale(3.0)\n").unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::F32);
    assert_eq!(p.code_modules[0].output, Type::F32);
    let p = syntax::parse("identity(x) = x\nmain()->bool = identity(true)\n").unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::Bool);
    let p=syntax::parse("identity(x) = x\nspace Main:\n  value = 7\n  result = identity()\n  value.out -> result.x\n").unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::I64);
    assert_eq!(
        mira_lang::runtime::execute(&p, 1).unwrap().terminals[0].1,
        7
    );
}
#[test]
fn ambiguous_types_require_intent_and_every_proposal_is_checked() {
    assert!(syntax::parse("identity(x) = x\n")
        .unwrap_err()
        .contains("ambiguous"));
    let mut p = syntax::parse_draft("identity @2(x) = x\nspace Main @1:\n").unwrap();
    let report = inference::analyze(&mut p, &[], false).unwrap();
    let target = Target::Parameter {
        definition: 2,
        index: 0,
    };
    assert!(report
        .requirements
        .iter()
        .any(|r| r.target == target && r.candidates.len() > 1));
    let choice = Choice {
        target,
        ty: Type::Bool,
    };
    inference::analyze(&mut p, &[choice], true).unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::Bool);
    let mut p = syntax::parse_draft("scale @2(x) = x * 2.0\nspace Main @1:\n").unwrap();
    p.code_modules[0].parameters[0].ty = Type::Infer;
    let before = p.clone();
    assert!(inference::analyze(
        &mut p,
        &[Choice {
            target: Target::Parameter {
                definition: 2,
                index: 0
            },
            ty: Type::Bool
        }],
        true
    )
    .is_err());
    assert_eq!(p, before, "rejected proposal changed the program");
}
#[test]
fn holes_have_expected_types_and_cannot_execute_or_compile() {
    let mut p=syntax::parse_draft("calculate @2(value:i64)->i64 = value + ?missing\nspace Main @1:\n  result @3 = calculate()\n").unwrap();
    let report = inference::analyze(&mut p, &[], false).unwrap();
    assert!(report.requirements.iter().any(
        |r| matches!(&r.target,Target::Hole{label,..} if label=="missing")
            && r.candidates == [Type::I64]
    ));
    assert!(p.require_complete().is_err());
    assert!(mira_lang::kernel::compile(&p).is_err());
    assert!(mira_lang::behavior::execute(&p, 2, &[7]).is_err());
    let paths = code::expressions(&p.code_modules[0]).unwrap();
    let path = paths
        .iter()
        .find(|n| matches!(n.expression, Expr::Hole(_)))
        .unwrap()
        .path
        .clone();
    *code::expression_mut(&mut p.code_modules[0], &path).unwrap() = Expr::Int(3);
    let p = syntax::parse(&syntax::format(&p)).unwrap();
    assert_eq!(mira_lang::behavior::execute(&p, 2, &[7]).unwrap(), 10);
}

#[test]
fn examples_share_the_checker_and_hole_filling_gate() {
    let p = syntax::parse(include_str!("../../../language/examples/inference")).unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::F32);
    assert_eq!(p.code_modules[1].parameters[0].ty, Type::I64);
    let mut draft =
        syntax::parse_draft(include_str!("../../../language/examples/incomplete")).unwrap();
    let report = inference::analyze(&mut draft, &[], false).unwrap();
    let target = report
        .requirements
        .iter()
        .find(|r| matches!(r.target, Target::Hole { .. }))
        .unwrap()
        .target
        .clone();
    let original = draft.clone();
    assert!(inference::fill(&draft, &target, Expr::Bool(true), &[]).is_err());
    assert_eq!(draft, original);
    let complete = inference::fill(&draft, &target, Expr::Int(3), &[]).unwrap();
    assert_eq!(
        mira_lang::behavior::execute(&complete, 2, &[7]).unwrap(),
        10
    );
    assert!(inference::fill(&complete, &target, Expr::Int(4), &[])
        .unwrap_err()
        .contains("stale"));
    let p = syntax::parse("run(value:_) =\n  x: _ = value * 2.0\n  x\n").unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::F32);
}

#[test]
fn inferred_tensor_operands_and_ad_reuse_existing_semantics() {
    let source="type Vector = tensor[f32,2]\nsum(values) = tensor.sum(values)\nrun()->f32 = sum(Vector(1.0,2.0))\nspace Main:\n  result = run()\n";
    let p = syntax::parse(source).unwrap();
    assert!(matches!(p.code_modules[0].parameters[0].ty, Type::Data(_)));
    assert_eq!(
        mira_lang::runtime::execute(&p, 1).unwrap().terminals[0].1,
        3.0f32.to_bits() as i64
    );
    let p=syntax::parse("loss(value) = value * value\nrun()->f32 = diff.derivative(loss,3.0)\nspace Main:\n  result = run()\n").unwrap();
    assert_eq!(p.code_modules[0].parameters[0].ty, Type::F32);
    assert_eq!(
        mira_lang::runtime::execute(&p, 1).unwrap().terminals[0].1,
        6.0f32.to_bits() as i64
    );
}
