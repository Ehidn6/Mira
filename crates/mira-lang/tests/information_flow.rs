use mira_core::{
    information::{Content, Reference},
    system::System,
};
use mira_lang::{information, system};
use std::collections::BTreeMap;

fn executions(s: &System) -> Vec<Reference> {
    s.information
        .objects
        .values()
        .filter_map(|object| {
            let r = s.information.latest(object.id).unwrap();
            matches!(
                s.information.get(&r).unwrap().content,
                Content::Execution { .. }
            )
            .then_some(r)
        })
        .collect()
}
const SOURCE: &str = "type Values = tensor[i64,16]\ngenerate()->Values =\n  data = Values()\n  for i in 0..16\n    data[i] = i\n  data\nspace Main:\n  result @100 = generate()\n";

#[test]
fn context_run_results_versions_undo_and_save_form_one_history() {
    let mut s = System::from_program(mira_lang::syntax::parse(SOURCE).unwrap()).unwrap();
    let context = information::create(&mut s, "Investigate output", true).unwrap();
    let source = information::create(&mut s, "Source material", false).unwrap();
    information::relate(&mut s, context.object, source.object, "uses-source").unwrap();
    mira_lang::compilation_cache::clear();
    information::run(&mut s, None, None, &BTreeMap::new()).unwrap();
    let first = executions(&s)[0].clone();
    let (first_program, context_ref) = match &s.information.get(&first).unwrap().content {
        Content::Execution {
            program, context, ..
        } => (program.clone(), context.clone().unwrap()),
        _ => unreachable!(),
    };
    assert_eq!(context_ref, context);
    let exact = information::results_at(&s, &first).unwrap();
    let information::Value::Object(index) = exact.roots[0].2 else {
        panic!("aggregate expected")
    };
    assert_eq!(exact.objects[index].fields.len(), 16);
    assert!(matches!(
        exact.objects[index].fields[15],
        information::Value::Scalar(15)
    ));
    let compiled = mira_lang::compilation_cache::stats();
    let second_context =
        information::revise(&mut s, context.object, context.version, "Revised question").unwrap();
    information::run(&mut s, None, None, &BTreeMap::new()).unwrap();
    assert_eq!(
        mira_lang::compilation_cache::stats().executable_misses,
        compiled.executable_misses,
        "information identity allocation must not recompile unchanged code"
    );
    assert_eq!(s.information.current_context, Some(second_context));
    assert!(
        matches!(&s.information.get(&first).unwrap().content,Content::Execution {context:Some(r),..} if r==&context)
    );
    let before = s.clone();
    assert!(
        information::revise(&mut s, context.object, context.version, "stale overwrite").is_err()
    );
    assert_eq!(s, before);
    let mut history = system::History::default();
    history.commit(&mut s,"Change code",&Default::default(),|draft| {
        mira_lang::behavior::edit_definition(&mut draft.program,100,"generate()->Values =\n  data = Values()\n  for i in 0..16\n    data[i] = i + 100\n  data\n")
    }).unwrap();
    information::run(&mut s, None, None, &BTreeMap::new()).unwrap();
    let third = executions(&s).last().unwrap().clone();
    let third_program = match &s.information.get(&third).unwrap().content {
        Content::Execution { program, .. } => program.clone(),
        _ => unreachable!(),
    };
    assert_eq!(first_program.object, third_program.object);
    assert_eq!(third_program.version, first_program.version + 1);
    let remembered = s.information.clone();
    history.undo(&mut s).unwrap();
    assert_eq!(
        s.information, remembered,
        "structural undo must not erase recorded activity"
    );
    let loaded = system::decode(&system::encode(&s).unwrap()).unwrap();
    assert_eq!(loaded, s);
    let old = information::program_at(&loaded, &first_program).unwrap();
    assert!(!mira_lang::syntax::format(&old).contains("i + 100"));
    let old_input = information::trial_input(&loaded, &first).unwrap();
    let mut trial = old_input;
    let result = mira_lang::runtime::execute_system(&mut trial, 1).unwrap();
    let ty = result.terminal_types[&result.terminals[0].0];
    let mira_core::Type::Data(_) = ty else {
        panic!("tensor expected")
    };
    assert_eq!(result.data.get(result.terminals[0].1, 15).unwrap(), 15);
    let mut corrupt = loaded;
    corrupt
        .information
        .artifacts
        .values_mut()
        .next()
        .unwrap()
        .bytes[0] ^= 1;
    assert!(system::check(&corrupt).is_err());
}

#[test]
fn failure_records_diagnostics_without_committing_computational_writes() {
    let source = "type Weight = tensor[f32,1]\nweight()->Weight = Weight.persist()\nbad()->i64 =\n  cells.memory()[0] = 99.0\n  1 / 0\nspace Main:\n  result @100 = bad():\n    space Internal @101:\n      memory @102 = weight()\n";
    let mut s = System::from_program(mira_lang::syntax::parse(source).unwrap()).unwrap();
    let buffers = s.buffers.clone();
    let tick = s.tick;
    assert!(information::run(&mut s, None, None, &BTreeMap::new()).is_err());
    assert_eq!(s.buffers, buffers);
    assert_eq!(s.tick, tick);
    let r = executions(&s).pop().unwrap();
    assert!(matches!(
        &s.information.get(&r).unwrap().content,
        Content::Execution {
            success: false,
            error: Some(_),
            result: None,
            ..
        }
    ));
    system::check(&s).unwrap();
    assert!(information::results_at(&s, &r).is_err());
}
