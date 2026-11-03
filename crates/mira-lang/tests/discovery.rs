//! One experiment uses the actual MiraOS graph, learning and checkpoint codec.
#[allow(dead_code)]
mod common;
use mira_lang::{source_tree, syntax, uefi};

#[test]
#[ignore = "requires QEMU: shared native learner, rule change and checkpoint round-trip"]
fn native_discovery_uses_shared_learning_and_records_predictions_before_results() {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/MiraOS");
    let mut tree = source_tree::read(&project.canonicalize().unwrap()).unwrap();
    fn replace_boot(node: &mut source_tree::SourceElement) -> bool {
        if node.id == 100 {
            let fixture = include_str!("fixtures/discovery-boot");
            let body = fixture.split_once('\n').unwrap().1;
            node.content = "  kernel @100 = code @164 ()->unit =\n".to_owned()
                + &body.lines().map(|line| format!("  {line}\n")).collect::<String>();
            for child in &mut node.children { child.offset = node.content.len(); }
            return true;
        }
        node.children
            .iter_mut()
            .any(|child| replace_boot(&mut child.element))
    }
    assert!(replace_boot(&mut tree.root));
    let program = syntax::parse(&tree.assembled().unwrap()).unwrap();
    let image = uefi::compile(&program).unwrap();
    let esp = common::work().join("discovery-esp");
    uefi::write_esp(&image, &esp).unwrap();
    let output = common::qemu_timed(&esp, true, "discovery", 30);
    assert!(output.ends_with("D!"), "{output}");
    // Firmware diagnostics precede the experiment. Only its four final words
    // before D! belong to the metric protocol.
    let mut metrics: Vec<f32> = output
        .split_whitespace()
        .rev()
        .skip(1)
        .take(4)
        .map(|word| f32::from_bits(u32::from_str_radix(word, 16).unwrap()))
        .collect();
    metrics.reverse();
    assert_eq!(metrics.len(), 4, "{output}");
    assert!(
        metrics
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0 && *v <= 4.0)
    );
    eprintln!(
        "world squared error: phase0 first16={} recentEWMA={}; changed rules first16={} recentEWMA={}",
        metrics[0], metrics[1], metrics[2], metrics[3]
    );
}
