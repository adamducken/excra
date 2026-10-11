#[test]
fn workflow_uses_stable_rust() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    assert!(workflow.contains("toolchain: stable"));
    assert!(!workflow.contains("nightly"));
}

#[test]
fn workflow_pins_actions_and_uses_the_committed_lockfile() {
    let workflow = include_str!("../.github/workflows/ci.yml");

    assert!(workflow.contains("\npermissions:\n  contents: read\n"));
    assert!(!workflow.contains("cargo generate-lockfile"));

    for action in workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- uses: "))
    {
        let revision = action
            .split_once('@')
            .expect("action has a revision")
            .1
            .split_whitespace()
            .next()
            .expect("action revision is not empty");
        assert_eq!(revision.len(), 40, "action is not pinned: {action}");
        assert!(
            revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "action is not pinned: {action}"
        );
    }

    assert!(include_str!("../Cargo.lock").starts_with("# This file is automatically @generated"));
}

#[test]
fn workflow_selects_cargo_llvm_cov_for_the_generic_installer() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    let mut lines = workflow.lines();

    lines
        .find(|line| {
            line.trim_start()
                .starts_with("- uses: taiki-e/install-action@")
        })
        .expect("workflow uses taiki-e/install-action");
    assert_eq!(lines.next(), Some("        with:"));
    assert_eq!(lines.next(), Some("          tool: cargo-llvm-cov"));
}
