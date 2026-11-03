//! Passive information contracts; no executable graph or authority is implied.
use mira_core::information::{
    Artifact, Content, Origin, PROGRAM_FORMAT, Reference, STATE_FORMAT, Store,
};
fn human(inputs: Vec<Reference>) -> Origin {
    Origin {
        producer: "human".into(),
        inputs,
    }
}
#[test]
fn pinned_versions_survive_revision_and_relations_are_regular_objects() {
    let mut store = Store::default();
    let first = store
        .create(10, Content::Text("first context".into()), human(vec![]))
        .unwrap();
    let second = store
        .revise(
            10,
            1,
            Content::Text("second context".into()),
            human(vec![first.clone()]),
        )
        .unwrap();
    assert_eq!(
        store.get(&first).unwrap().content,
        Content::Text("first context".into())
    );
    assert_eq!(store.latest(10).unwrap(), second);
    let relation = store
        .create(
            11,
            Content::Relation {
                from: first.clone(),
                to: second.clone(),
                kind: "revised-into".into(),
            },
            human(vec![first]),
        )
        .unwrap();
    assert!(matches!(
        store.get(&relation).unwrap().content,
        Content::Relation { .. }
    ));
    store.validate().unwrap();
    let encoded = serde_json::to_vec(&store).unwrap();
    assert_eq!(serde_json::from_slice::<Store>(&encoded).unwrap(), store);
}
#[test]
fn invalid_revision_and_missing_refs_leave_the_store_unchanged() {
    let mut store = Store::default();
    let first = store
        .create(10, Content::Text("context".into()), human(vec![]))
        .unwrap();
    let before = store.clone();
    assert!(
        store
            .revise(10, 2, Content::Text("stale".into()), human(vec![]))
            .is_err()
    );
    assert!(
        store
            .create(
                11,
                Content::Relation {
                    from: first,
                    to: Reference {
                        object: 99,
                        version: 1
                    },
                    kind: "missing".into()
                },
                human(vec![])
            )
            .is_err()
    );
    assert_eq!(store, before);
}
#[test]
fn executions_pin_program_context_and_state_artifact() {
    let mut store = Store::default();
    let program_digest = [1; 32];
    let state_digest = [2; 32];
    store.artifacts.insert(
        program_digest,
        Artifact {
            format: PROGRAM_FORMAT.into(),
            bytes: vec![1],
        },
    );
    store.artifacts.insert(
        state_digest,
        Artifact {
            format: STATE_FORMAT.into(),
            bytes: vec![2],
        },
    );
    store.artifacts.insert(
        [3; 32],
        Artifact {
            format: mira_core::information::RESULT_FORMAT.into(),
            bytes: vec![3],
        },
    );
    let program = store
        .create(
            10,
            Content::Program {
                artifact: program_digest,
            },
            human(vec![]),
        )
        .unwrap();
    store.program = Some(program.object);
    let context = store
        .create(11, Content::Text("task".into()), human(vec![]))
        .unwrap();
    store.current_context = Some(context.clone());
    let execution = store
        .create(
            12,
            Content::Execution {
                program: program.clone(),
                context: Some(context.clone()),
                target: Some(100),
                tick: 1,
                success: true,
                input_state: state_digest,
                result: Some([3; 32]),
                outputs: vec![],
                error: None,
            },
            Origin {
                producer: "mira.runtime".into(),
                inputs: vec![program, context],
            },
        )
        .unwrap();
    assert!(matches!(
        store.get(&execution).unwrap().content,
        Content::Execution { success: true, .. }
    ));
    let encoded = serde_json::to_vec(&store).unwrap();
    assert_eq!(serde_json::from_slice::<Store>(&encoded).unwrap(), store);
    let mut rewritten = store.clone();
    rewritten.objects.get_mut(&10).unwrap().versions[0]
        .origin
        .producer = "rewritten".into();
    assert!(rewritten.extends(&store).is_err());
}
#[test]
fn information_ids_share_the_program_identity_domain() {
    let program = mira_lang::syntax::parse("space Main:\n").unwrap();
    let root = program.root.id;
    let mut system = mira_core::system::System::from_program(program).unwrap();
    system
        .information
        .create(root, Content::Text("collision".into()), human(vec![]))
        .unwrap();
    assert!(system.validate().is_err());
    system.information = Store::default();
    let id = system.allocate_id().unwrap();
    system
        .information
        .create(id, Content::Text("valid".into()), human(vec![]))
        .unwrap();
    system.validate().unwrap();
}
#[test]
fn duplicate_artifact_keys_are_not_silently_overwritten() {
    let digest = serde_json::to_string(&[7u8; 32]).unwrap();
    let artifact = r#"{"format":"mira.state/1","bytes":[1]}"#;
    let json = format!(
        "{{\"objects\":{{}},\"artifacts\":[[{digest},{artifact}],[{digest},{artifact}]],\"current_context\":null,\"program\":null}}"
    );
    assert!(serde_json::from_str::<Store>(&json).is_err());
}
