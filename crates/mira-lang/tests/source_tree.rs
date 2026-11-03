use mira_lang::source_tree::{self, SourceTree};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
const SOURCE: &str = include_str!("../../../language/examples/recursive-source");
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mira-source-tree-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn recursive_storage_is_the_z_tree_with_contents_inside_their_parent() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let dir = Directory::new();
    let path = dir.0.join("Main");
    source_tree::write(&path, &tree).unwrap();
    assert!(path.is_file());
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("  first @10 = helper():\n    space Inside @11:\n      value @12 = 9"));
    assert!(!stored.contains("\"offset\""));
    assert!(!stored.contains("\"element\""));
    assert_eq!(stored, tree.assembled().unwrap());
    let reopened = source_tree::read(&path).unwrap();
    assert_eq!(reopened, tree);
    assert_eq!(mira_lang::load(&path).unwrap(), mira_lang::syntax::parse(SOURCE).unwrap());
    let rows = source_tree::catalog(&path).unwrap();
    assert_eq!(rows.iter().find(|r| r.id == 12).unwrap().path, "/Main/first/Inside/value");
    assert_eq!(rows.iter().find(|r| r.id == 12).unwrap().parent, Some(11));
    assert_eq!(rows.iter().filter(|r| r.name == "helper").count(), 1);
    let first_behavior = reopened.behavior("first").unwrap();
    let second_behavior = reopened.behavior("second").unwrap();
    assert_eq!(first_behavior.entry.id, second_behavior.entry.id);
    assert_eq!(first_behavior.entry.name, "helper");
    assert!(first_behavior.source.contains("x + 1"));
    assert!(reopened.assembled().unwrap().contains("learning note"));
}

#[test]
fn entering_objects_uses_names_and_keeps_children_out_of_the_working_context() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let inside = tree.enter("Main/first/Inside").unwrap();
    assert_eq!(inside.entry.id, 11);
    assert_eq!(inside.children.len(), 1);
    assert_eq!(inside.children[0].name, "value");
    assert!(inside.source.contains("space Inside:"));
    assert!(inside.source.contains("enter value"));
    assert!(!inside.source.contains('@'));
    assert!(!inside.source.contains("= 9"));
    assert_eq!(tree.resolve("/Main/first/Inside/value").unwrap(), 12);
    assert_eq!(tree.resolve("first/Inside/value").unwrap(), 12);
    assert_eq!(tree.resolve("value").unwrap(), 12);
    assert_eq!(tree.resolve("12").unwrap(), 12);
    assert!(tree.resolve("first/value").is_err()); // Cannot skip Z.
    let mut repeated = tree.clone();
    repeated.replace(20, "  second @20 = helper():\n    space Inside @200:\n      value @201 = 4\n").unwrap();
    assert!(repeated.resolve("value").unwrap_err().contains("ambiguous"));
    assert_eq!(repeated.resolve("second/Inside/value").unwrap(), 201);
}

#[test]
fn edit_reparents_the_stored_subtree_only_when_program_z_changes() {
    let mut tree = SourceTree::from_source(SOURCE).unwrap();
    tree.replace(10, "  first @10 = helper():\n    space Inside @11:\n      box @200 = graph():\n        space Deeper @201:\n          value @12 = 9\n").unwrap();
    assert_eq!(tree.resolve("first/Inside/box/Deeper/value").unwrap(), 12);
    assert!(tree.resolve("first/Inside/value").is_err());
    let program = tree.validate().unwrap();
    assert!(program.root.find(201).unwrap().cell(12).is_ok());
    let dir = Directory::new();
    let path = dir.0.join("Main");
    source_tree::write(&path, &tree).unwrap();
    let reopened = source_tree::read(&path).unwrap();
    assert_eq!(reopened, tree);
    assert_eq!(reopened.validate().unwrap(), program);
    // A metadata-only reparenting cannot override the actual source declaration.
    let mut corrupt = tree.clone();
    corrupt.root.children.iter_mut().find(|c| c.element.id == 10).unwrap().element.name = "fake".into();
    assert!(source_tree::write(&path, &corrupt).is_err());
    assert_eq!(source_tree::read(&path).unwrap(), tree);
}

#[test]
fn targeted_edits_and_failures_preserve_identity_state_and_storage() {
    let mut tree = SourceTree::from_source(SOURCE).unwrap();
    let id = tree.resolve("helper").unwrap();
    tree.replace(id, "helper(x:i64)->i64 = x + 2\n").unwrap();
    assert_eq!(tree.validate().unwrap().code_modules[0].id, id);
    let run = mira_lang::runtime::execute(&tree.validate().unwrap(), 1).unwrap();
    assert!(run.terminals.iter().any(|(name,value)| name == "Main/second" && *value == 9));
    let before = tree.clone();
    assert!(tree.replace(id, "helper(x:i64)->i64 = missing\n").is_err());
    assert!(tree.replace(id, "helper(x:i64)->i64 = x\nextra @99()->i64 = 1\n").is_err());
    assert_eq!(tree, before);
    let dir = Directory::new();
    let path = dir.0.join("Main");
    source_tree::write(&path, &tree).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut corrupt = tree.clone();
    corrupt.root.children[0].offset = usize::MAX;
    assert!(source_tree::write(&path, &corrupt).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    tree.replace(id, "helper(x:i64)->i64 = x + 3\n").unwrap();
    source_tree::write(&path, &tree).unwrap();
    assert_eq!(source_tree::read(&path).unwrap(), tree);
}

#[test]
fn duplicate_identity_symlinks_foreign_files_and_old_layouts_are_rejected() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let mut corrupt = tree.clone();
    corrupt.root.children.push(corrupt.root.children[0].clone());
    assert!(corrupt.validate().is_err());
    let dir = Directory::new();
    let foreign = dir.0.join("foreign");
    std::fs::write(&foreign, "important").unwrap();
    assert!(source_tree::write(&foreign, &tree).is_err());
    assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "important");
    let old = dir.0.join("old");
    std::fs::write(&old, r#"{"format":"mira-source-layout-v3","tree":{},"files":[]}"#).unwrap();
    assert!(source_tree::read(&old).is_err());
    #[cfg(unix)] {
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&foreign, &link).unwrap();
        assert!(source_tree::read(&link).is_err());
        assert!(source_tree::write(&link, &tree).is_err());
    }
}

#[test]
fn cli_enters_and_edits_a_contained_object_by_its_z_path() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let dir = Directory::new();
    let path = dir.0.join("Main");
    source_tree::write(&path, &tree).unwrap();
    let fragment = dir.0.join("fragment");
    std::fs::write(&fragment, "      value @12 = 11\n").unwrap();
    let call = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_mira"))
            .args(["source", path.to_str().unwrap()]).args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        output.stdout
    };
    let entered: serde_json::Value = serde_json::from_slice(&call(&["enter", "first/Inside", "--json"])).unwrap();
    assert_eq!(entered["children"][0]["path"], "/Main/first/Inside/value");
    call(&["write", "first/Inside/value", fragment.to_str().unwrap()]);
    assert_eq!(mira_lang::load(&path).unwrap().root.find(11).unwrap().cell(12).unwrap().parameter, 11);
    assert!(String::from_utf8(call(&["read", "first/Inside/value"])).unwrap().contains("= 11"));
    let rejected = std::process::Command::new(env!("CARGO_BIN_EXE_mira"))
        .args(["source", path.to_str().unwrap(), "regroup", "unused"]).output().unwrap();
    assert!(!rejected.status.success());
}

#[test]
fn recursive_z_roundtrip_is_not_limited_to_the_json_defaults() {
    let mut source = String::from("space Main:\n");
    let mut depth = 1;
    for index in 0..23 {
        source.push_str(&format!("{}box = graph():\n{}space Inner{index}:\n", "  ".repeat(depth), "  ".repeat(depth + 1)));
        depth += 2;
    }
    source.push_str(&format!("{}value = 7\n", "  ".repeat(depth)));
    let tree = SourceTree::from_source(&source).unwrap();
    let dir = Directory::new();
    let path = dir.0.join("recursive");
    source_tree::write(&path, &tree).unwrap();
    assert_eq!(source_tree::read(&path).unwrap(), tree);
}

#[test]
fn component_contains_its_own_behavior_and_z_without_a_separate_boot_object() {
    let source = include_str!("../../../language/examples/owned-source");
    let tree = SourceTree::from_source(source).unwrap();
    let program = tree.validate().unwrap();
    assert_eq!(program.root.cell(10).unwrap().definition, mira_core::Definition::Code(11));
    assert_eq!(mira_core::code::find(&program.code_modules,11).unwrap().owner, Some(10));
    assert_eq!(tree.resolve("kernel/Runtime/value").unwrap(),14);
    assert_eq!(tree.resolve("kernel/adjust").unwrap(),12);
    assert!(tree.get(11).is_none()); // Implementation identity is not another file.
    let view = tree.enter("kernel").unwrap();
    assert!(view.source.contains("kernel = code ()->i64 ="));
    assert!(view.source.contains("cells.value() + adjust(1)"));
    assert_eq!(view.children.len(),2);
    assert_eq!(tree.behavior("kernel").unwrap().entry.id,10);
    assert_eq!(mira_lang::syntax::parse(&mira_lang::syntax::format(&program)).unwrap(),program);
    assert_eq!(mira_lang::runtime::execute(&program,1).unwrap().terminals[0].1,10);
    assert!(!mira_lang::kernel::compile(&program).unwrap().is_empty());
    let dir = Directory::new();
    let path = dir.0.join("Main");
    source_tree::write(&path,&tree).unwrap();
    let mut reopened = source_tree::read(&path).unwrap();
    reopened.replace(14,"      value @14 = 20\n").unwrap();
    source_tree::write(&path,&reopened).unwrap();
    let loaded = mira_lang::load(&path).unwrap();
    assert_eq!(mira_lang::runtime::execute(&loaded,1).unwrap().terminals[0].1,23);
    let mut corrupted = loaded.clone();
    mira_core::code::find_mut(&mut corrupted.code_modules,11).unwrap().owner = Some(14);
    assert!(corrupted.validate().is_err());
    let before = reopened.clone();
    let replacement = reopened.read(10).unwrap().replace("code @11", "code @2");
    assert!(reopened.replace(10,&replacement).unwrap_err().contains("high-water"));
    assert_eq!(reopened,before);
    let mut placed = program.clone();
    placed.root.cells[0].position = [11.0,12.0,13.0];
    assert_eq!(mira_lang::syntax::parse(&mira_lang::syntax::format(&placed)).unwrap(),placed);
    let implicit = source.replace(" @10", "").replace(" @11", "").replace(" @12", "").replace(" @13", "").replace(" @14", "").replace(" @1", "");
    let implicit_tree = SourceTree::from_source(&implicit).unwrap();
    assert!(implicit_tree.resolve("kernel/Runtime/value").is_ok());
    assert_eq!(mira_lang::runtime::execute(&implicit_tree.validate().unwrap(),1).unwrap().terminals[0].1,10);
}
