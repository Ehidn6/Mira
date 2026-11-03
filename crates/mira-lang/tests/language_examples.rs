use mira_lang::{kernel, runtime, syntax};

#[test]
fn state_example_keeps_the_same_instances_state_across_activations() {
    let p = syntax::parse(include_str!("../../../language/examples/state")).unwrap();
    assert!(!kernel::compile(&p).unwrap().is_empty());
    let mut system = mira_core::system::System::from_program(p).unwrap();
    let first = runtime::execute_system(&mut system, 1).unwrap();
    let second = runtime::execute_system(&mut system, 2).unwrap();
    assert_eq!(first.terminals[0].1, 1.0f32.to_bits() as i64);
    assert_eq!(second.terminals[0].1, 2.0f32.to_bits() as i64);
}

#[test]
fn system_example_produces_a_freestanding_image() {
    let p = syntax::parse(include_str!("../../../language/examples/system")).unwrap();
    assert!(!kernel::compile(&p).unwrap().is_empty());
}

#[test]
fn beginner_graph_example_runs_and_explains_misplaced_arguments() {
    let source = include_str!("../../../language/examples/first-graph");
    let program = syntax::parse(source).unwrap();
    assert_eq!(runtime::execute(&program, 1).unwrap().terminals[0].1, 6);
    let error = syntax::parse("double(value) = value * 2\nspace Main:\n  result = double(3)\n").unwrap_err();
    assert!(error.contains("cell inputs are connected with ->"), "{error}");
}

#[test]
fn interface_graph_uses_the_native_mira_widgets_without_copying_them() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/MiraOS").canonicalize().unwrap();
    // Use precisely the definitions used by the native OS, not fixture copies.
    let mut common = String::from("identities 1000\n");
    for id in [2,440,445,439] {
        let part = mira_lang::source_tree::read_selected(&root,id).unwrap();
        let indent = part.lines().find(|s| !s.trim().is_empty()).unwrap().bytes().take_while(|b| *b == b' ').count();
        for line in part.lines() {
            common.push_str(&line[indent.min(line.len())..]);
            common.push('\n');
        }
    }
    let example = include_str!("../../../language/examples/interface-graph");
    for minimum in [false,true] {
        let mut example = example.to_string();
        if minimum {
            example = example.replace("= -42","= -9223372036854775807 - 1")
                .replace("field[8] == 52 and field[9] == 50 and field[10] == 0", "field[8] == 57 and field[26] == 56 and field[27] == 0");
        }
        let p = syntax::parse(&(common.clone()+&example)).unwrap();
        let mut system = mira_core::system::System::from_program(p.clone()).unwrap();
        for tick in 1..=2 {
            let result = runtime::execute_system(&mut system,tick).unwrap();
            assert_eq!(result.terminals[0].1,1);
        }
        #[cfg(all(target_os="linux",target_arch="x86_64"))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join("../../target").join(format!("interface-graph-{}-{minimum}",std::process::id()));
            std::fs::write(&path,mira_lang::native::compile(&p).unwrap()).unwrap();
            std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = std::process::Command::new(&path).output().unwrap();
            std::fs::remove_file(&path).unwrap();
            assert!(result.status.success());
            assert_eq!(String::from_utf8(result.stdout).unwrap().trim(),"1");
        }
    }
}
