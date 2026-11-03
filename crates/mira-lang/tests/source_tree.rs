use mira_lang::source_tree::{self, SourceTree};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
const SOURCE: &str = "// project note\nidentities 100\nhelper(x:i64)->i64 = x + 1 // learning note\nspace Main @1:\n  first @10 = helper():\n    space Inside @11:\n      value @12 = 9\n  second @20 = helper()\n  input @30 = 7\n  input.out -> first.x\n  input.out -> second.x\n";
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
fn recursive_storage_matches_z_and_keeps_shared_definition_once() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    assert_eq!(
        tree.validate().unwrap(),
        mira_lang::syntax::parse(SOURCE).unwrap()
    );
    let rows = tree.list().unwrap();
    assert_eq!(rows.iter().find(|r| r.id == 11).unwrap().parent, Some(10));
    assert_eq!(rows.iter().find(|r| r.id == 12).unwrap().parent, Some(11));
    assert_eq!(rows.iter().filter(|r| r.name == "helper").count(), 1);
    assert!(tree.assembled().unwrap().contains("// learning note"));
    let dir = Directory::new();
    let compound = dir.0.join("without-extension");
    source_tree::write(&compound, &tree).unwrap();
    assert_eq!(source_tree::read(&compound).unwrap(), tree);
    let expanded = dir.0.join("project");
    source_tree::write_expanded(&expanded, &tree).unwrap();
    assert!(std::fs::read_dir(&expanded).unwrap().count() < rows.len());
    assert_eq!(
        source_tree::read_selected(&expanded, 12).unwrap(),
        tree.read(12).unwrap()
    );
    assert_eq!(source_tree::read(&expanded).unwrap(), tree);
    assert_eq!(
        mira_lang::load(&expanded).unwrap(),
        tree.validate().unwrap()
    );
    assert!(
        !std::fs::read_to_string(expanded.join("index"))
            .unwrap()
            .contains("learning note")
    );
}
#[test]
fn targeted_edits_preserve_identity_and_failed_changes_leave_storage_intact() {
    let mut tree = SourceTree::from_source(SOURCE).unwrap();
    let id = tree
        .list()
        .unwrap()
        .iter()
        .find(|r| r.name == "helper")
        .unwrap()
        .id;
    tree.replace(id, "helper(x:i64)->i64 = x + 2\n").unwrap();
    assert_eq!(tree.validate().unwrap().code_modules[0].id, id);
    let activation = mira_lang::runtime::execute(&tree.validate().unwrap(), 1).unwrap();
    assert!(
        activation
            .terminals
            .iter()
            .any(|(name, value)| name == "Main/second" && *value == 9)
    );
    let before = tree.clone();
    assert!(tree.replace(id, "helper(x:i64)->i64 = missing\n").is_err());
    assert_eq!(tree, before);
    assert!(
        tree.replace(id, "helper(x:i64)->i64 = x\nextra @99()->i64 = 1\n")
            .is_err()
    );
    assert_eq!(tree, before);
    let dir = Directory::new();
    let expanded = dir.0.join("project");
    source_tree::write_expanded(&expanded, &tree).unwrap();
    let mut corrupt = tree.clone();
    corrupt.root.children[0].offset = usize::MAX;
    assert!(source_tree::write_expanded(&expanded, &corrupt).is_err());
    assert_eq!(source_tree::read(&expanded).unwrap(), tree);
    tree.replace(id, "helper(x:i64)->i64 = x + 3\n").unwrap();
    source_tree::write_expanded(&expanded, &tree).unwrap();
    assert_eq!(source_tree::read(&expanded).unwrap(), tree);
    assert!(
        source_tree::write_grouped(
            &expanded,
            &tree,
            &[source_tree::SourceGroup {
                name: "incomplete".into(),
                ids: vec![id]
            }]
        )
        .is_err()
    );
    assert_eq!(source_tree::read(&expanded).unwrap(), tree);
}
#[test]
fn duplicate_id_and_symlink_projection_are_rejected() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let mut corrupt = tree.clone();
    corrupt.root.children.push(corrupt.root.children[0].clone());
    assert!(corrupt.validate().is_err());
    #[cfg(unix)]
    {
        let dir = Directory::new();
        let expanded = dir.0.join("project");
        source_tree::write_expanded(&expanded, &tree).unwrap();
        let content = expanded.join("program");
        std::fs::remove_file(&content).unwrap();
        std::os::unix::fs::symlink(dir.0.join("elsewhere"), content).unwrap();
        assert!(source_tree::read(&expanded).is_err());
    }
}

#[test]
fn selective_reads_do_not_open_unrelated_source_bodies() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let helper = tree
        .list()
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "helper")
        .unwrap()
        .id;
    let dir = Directory::new();
    let expanded = dir.0.join("project");
    let ids = tree
        .list()
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .filter(|id| *id != helper)
        .collect();
    let groups = vec![
        source_tree::SourceGroup {
            name: "helper".into(),
            ids: vec![helper],
        },
        source_tree::SourceGroup {
            name: "other".into(),
            ids,
        },
    ];
    source_tree::write_grouped(&expanded, &tree, &groups).unwrap();
    let packed = dir.0.join("packed");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_mira"))
        .args([
            "source",
            expanded.to_str().unwrap(),
            "pack",
            packed.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(source_tree::source_files(&packed).unwrap().len(), 2);
    assert_eq!(source_tree::read(&packed).unwrap(), tree);
    let reopened = dir.0.join("reopened");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_mira"))
        .args([
            "source",
            packed.to_str().unwrap(),
            "split",
            reopened.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(source_tree::source_files(&reopened).unwrap().len(), 2);
    assert_eq!(source_tree::read(&reopened).unwrap(), tree);
    std::fs::remove_file(expanded.join("other")).unwrap();
    assert!(
        source_tree::catalog(&expanded)
            .unwrap()
            .iter()
            .any(|entry| entry.id == 20)
    );
    assert!(
        source_tree::read_selected(&expanded, helper)
            .unwrap()
            .contains("learning note")
    );
    assert!(source_tree::read(&expanded).is_err());
}

#[test]
fn cli_writes_an_indented_contained_file_without_parsing_it_as_a_program() {
    let tree = SourceTree::from_source(SOURCE).unwrap();
    let dir = Directory::new();
    let expanded = dir.0.join("project");
    source_tree::write_expanded(&expanded, &tree).unwrap();
    let fragment = dir.0.join("fragment");
    std::fs::write(&fragment, "      value @12 = 11\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mira"))
        .args([
            "source",
            expanded.to_str().unwrap(),
            "write",
            "12",
            fragment.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        mira_lang::load(&expanded)
            .unwrap()
            .root
            .find(11)
            .unwrap()
            .cell(12)
            .unwrap()
            .parameter,
        11
    );
}

#[test]
fn recursive_z_roundtrip_is_not_limited_to_the_json_defaults() {
    let mut source = String::from("space Main:\n");
    let mut depth = 1;
    for index in 0..23 {
        source.push_str(&format!(
            "{}box = graph():\n{}space Inner{index}:\n",
            "  ".repeat(depth),
            "  ".repeat(depth + 1)
        ));
        depth += 2;
    }
    source.push_str(&format!("{}value = 7\n", "  ".repeat(depth)));
    let tree = SourceTree::from_source(&source).unwrap();
    let dir = Directory::new();
    let path = dir.0.join("recursive");
    source_tree::write(&path, &tree).unwrap();
    assert_eq!(source_tree::read(&path).unwrap(), tree);
}
