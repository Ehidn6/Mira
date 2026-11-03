use mira_core::{code::*, system::System, transform::*};
use mira_lang::{
    metaprogram::{self, Invocation},
    syntax,
};
#[test]
fn mira_inspects_and_rewrites_the_authoritative_code_tree() {
    let program = syntax::parse(include_str!("../../../language/examples/code")).unwrap();
    let result = mira_lang::runtime::execute(&program, 1).unwrap();
    assert_eq!(
        result
            .terminals
            .iter()
            .find(|(n, _)| n.ends_with("/output"))
            .unwrap()
            .1,
        8
    );
    assert_eq!(
        result
            .terminals
            .iter()
            .find(|(n, _)| n.ends_with("/inspector"))
            .unwrap()
            .1,
        1
    );
    assert_eq!(mira_lang::behavior::execute(&program,20,&[5]).unwrap(),6);
    assert_eq!(syntax::parse(&syntax::format(&program)).unwrap(), program);
    assert!(!mira_lang::native::compile(&program).unwrap().is_empty());
}
#[test]
fn incorrect_or_capturing_expansions_publish_nothing() {
    for body in [
        "node = meta.quote(2,0)\n  expression = meta.boolean(1)\n  meta.replace(node,expression)",
        "node = meta.quote(2,0)\n  other = meta.parameter(3,0)\n  meta.replace(node,other)",
        "node = meta.quote(2,0)\n  other = meta.quote(2,2)\n  meta.replace(other,node)",
    ] {
        let source=format!("run @2(x:i64)->i64 = x + 1\nother @3(x:i64)->i64 = x\nmacro change()->unit =\n  {}\nspace Main @1:\n",body.replace("\n  ","\n").replace('\n',"\n  "));
        let mut program = syntax::parse(&source).unwrap_or_else(|e| panic!("{e}\n{source}"));
        let before = program.clone();
        assert!(metaprogram::expand(
            &mut program,
            &[Invocation {
                name: "change".into(),
                args: vec![]
            }]
        )
        .is_err());
        assert_eq!(program, before);
    }
}
#[test]
fn scoped_parameters_and_stale_expression_guards() {
    let source="run @2(x:i64)->i64 = 0\nmacro change()->unit =\n  node = meta.quote(2,0)\n  input = meta.parameter(2,0)\n  one = meta.integer(1)\n  sum = meta.binary(0,input,one)\n  meta.replace(node,sum)\nspace Main @1:\nexpand change()\n";
    let program = syntax::parse(source).unwrap();
    assert_eq!(mira_lang::behavior::execute(&program, 2, &[4]).unwrap(), 5);
    let base = System::from_program(program).unwrap();
    let proposal = Proposal {
        origin: Origin::System,
        explanation: "".into(),
        revision: base.revision,
        label: "stale subtree".into(),
        edits: vec![Edit::ReplaceExpression {
            definition: 2,
            path: vec![0, 0],
            expected: Expr::Int(0),
            value: Expr::Int(5),
        }],
    };
    assert!(proposal.prepare(&base).unwrap_err().contains("stale code"));
}

#[test]
fn mira_macro_fills_a_hole_before_final_typechecking() {
    let source="run @2(value) -> i64 = ?body\nmacro complete()->unit =\n  target = meta.quote(2,0)\n  parameter = meta.parameter(2,0)\n  one = meta.integer(1)\n  body = meta.binary(0,parameter,one)\n  meta.replace(target,body)\nspace Main @1:\nexpand complete()\n";
    let program = syntax::parse(source).unwrap();
    assert_eq!(
        program.code_modules[0].parameters[0].ty,
        mira_core::Type::I64
    );
    assert_eq!(mira_lang::behavior::execute(&program, 2, &[4]).unwrap(), 5);
    let invalid = source.replace("meta.integer(1)", "meta.boolean(1)");
    assert!(syntax::parse(&invalid).is_err());
}
