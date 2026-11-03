use mira_core::system::System;
use mira_lang::{compilation_cache, syntax, system};
use std::collections::BTreeMap;

fn system() -> System {
    System::from_program(syntax::parse("value() -> i64 = 7\nspace main:\n  a = value()\n").unwrap())
        .unwrap()
}

#[test]
fn ticks_and_visual_moves_reuse_code_but_body_changes_invalidate() {
    compilation_cache::clear();
    let mut s = system();
    let first = system::step(&mut s, &BTreeMap::new()).unwrap();
    let initial = compilation_cache::stats();
    assert!(initial.executable_misses > 0);
    s.program.root.position = [12.0, 3.0, 0.0];
    s.program.root.cells[0].position = [-2.0, 5.0, 0.0];
    let second = system::step(&mut s, &BTreeMap::new()).unwrap();
    assert_eq!(first.terminals, second.terminals);
    let reused = compilation_cache::stats();
    assert_eq!(reused.executable_misses, initial.executable_misses);
    assert!(reused.executable_hits > initial.executable_hits);
    assert_eq!(reused.validation_misses, initial.validation_misses);
    assert!(reused.validation_hits > initial.validation_hits);
    let cell = s.program.root.cells[0].id;
    mira_lang::behavior::edit_definition(&mut s.program, cell, "value() -> i64 = 9\n").unwrap();
    let changed = system::step(&mut s, &BTreeMap::new()).unwrap();
    assert!(compilation_cache::stats().executable_misses > reused.executable_misses);
    assert_ne!(second.terminals, changed.terminals);
}

#[test]
fn unchanged_procedures_reuse_numeric_lowering_after_an_unrelated_edit() {
    compilation_cache::clear();
    let mut s = System::from_program(syntax::parse(
        "add(x:f32) -> f32 = x + 2.0\nleft() -> f32 = add(1.0)\nright() -> f32 = 3.0 * 4.0\nspace main:\n  a = left()\n  b = right()\n"
    ).unwrap()).unwrap();
    system::step(&mut s, &BTreeMap::new()).unwrap();
    let before = compilation_cache::stats();
    let cell = s
        .program
        .root
        .cells
        .iter()
        .find(|c| c.name == "b")
        .unwrap()
        .id;
    mira_lang::behavior::edit_definition(&mut s.program, cell, "right() -> f32 = 5.0 * 4.0\n")
        .unwrap();
    system::step(&mut s, &BTreeMap::new()).unwrap();
    let after = compilation_cache::stats();
    assert!(after.executable_misses > before.executable_misses);
    assert!(after.procedure_hits > before.procedure_hits);
    assert!(after.procedure_misses > before.procedure_misses);
}

#[test]
fn executable_retention_is_bounded() {
    compilation_cache::clear();
    for n in 0..20 {
        let mut s = system();
        s.program.code_modules[0].body = vec![mira_core::code::Statement::Expr(
            mira_core::code::Expr::Int(n),
        )];
        system::step(&mut s, &BTreeMap::new()).unwrap();
    }
    let stats = compilation_cache::stats();
    assert!(stats.executable_entries <= 8);
    assert!(stats.validation_entries <= 8);
    assert!(stats.validation_bytes <= 16 * 1024 * 1024);
    assert!(stats.executable_bytes <= 16 * 1024 * 1024);
    assert!(stats.procedure_entries <= 256);
    assert!(stats.procedure_bytes <= 8 * 1024 * 1024);
}

#[test]
fn readiness_cache_does_not_hide_invalid_runtime_buffers() {
    compilation_cache::clear();
    let mut s = system();
    system::step(&mut s, &BTreeMap::new()).unwrap();
    // The source is unchanged, but a buffer on a scalar-output owner is invalid.
    s.buffers.insert(
        s.program.root.cells[0].id,
        mira_core::data::Buffer {
            ty: u64::MAX,
            words: vec![0],
        },
    );
    assert!(system::step(&mut s, &BTreeMap::new()).is_err());
}
