use std::path::PathBuf;
use libmemlog::mode_contract::*;

#[test]
fn test_parse_driver_mode_0660() {
    let src = "    .mode = 0660,\n    .fops = &memlog_fops,";
    assert_eq!(parse_driver_mode(src).unwrap(), 0o660);
}

#[test]
fn test_parse_udev_rule_mode_0660() {
    let src = r#"KERNEL=="memlog", GROUP="memlog", MODE="0660""#;
    assert_eq!(parse_udev_rule_mode(src).unwrap(), 0o660);
}

#[test]
fn test_parse_udev_rule_mode_0640() {
    let src = r#"KERNEL=="memlog", GROUP="memlog", MODE="0640""#;
    assert_eq!(parse_udev_rule_mode(src).unwrap(), 0o640);
}

#[test]
fn test_contract_pass_all_agree_0660() {
    let driver_mode = parse_driver_mode("    .mode = 0660,").unwrap();
    let readme_mode = parse_udev_rule_mode(r#"KERNEL=="memlog", GROUP="memlog", MODE="0660""#).unwrap();
    let rule_mode = parse_udev_rule_mode(r#"KERNEL=="memlog", GROUP="memlog", MODE="0660""#).unwrap();
    assert_eq!(driver_mode, 0o660);
    assert_eq!(readme_mode, 0o660);
    assert_eq!(rule_mode, 0o660);
    // all agree and have group-write
    assert_eq!(driver_mode, readme_mode);
    assert_eq!(readme_mode, rule_mode);
    assert!(rule_mode & 0o020 != 0, "group-write required");
}

#[test]
fn test_contract_fail_udev_rule_0640_vs_0660() {
    // reproduces the 2026-06-16 bug: udev rule says 0640, others say 0660
    let driver_mode = parse_driver_mode("    .mode = 0660,").unwrap();
    let rule_mode = parse_udev_rule_mode(r#"KERNEL=="memlog", GROUP="memlog", MODE="0640""#).unwrap();
    assert_ne!(driver_mode, rule_mode, "contract violation: driver=0660 but udev rule=0640");
}

#[test]
fn test_contract_fail_consistent_but_no_group_write() {
    // all three agree on 0640 — consistent but wrong
    let mode = parse_udev_rule_mode(r#"KERNEL=="memlog", GROUP="memlog", MODE="0640""#).unwrap();
    assert_eq!(mode, 0o640);
    assert!(mode & 0o020 == 0, "expected 0640 lacks group-write");
    // this IS the failure — group-write sanity floor catches it
}

#[test]
fn test_env_override_resolution() {
    // MEMLOG_UDEV_RULE_PATH env override takes priority
    let tmp = std::env::temp_dir().join("test-memlog.rules");
    std::fs::write(&tmp, r#"KERNEL=="memlog", GROUP="memlog", MODE="0660""#).unwrap();
    std::env::set_var("MEMLOG_UDEV_RULE_PATH", tmp.to_str().unwrap());
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (path, source) = resolve_udev_rule_path(&repo_root).unwrap();
    assert_eq!(source, "env:MEMLOG_UDEV_RULE_PATH");
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(parse_udev_rule_mode(&content).unwrap(), 0o660);
    std::env::remove_var("MEMLOG_UDEV_RULE_PATH");
}

/// Live integration test — reads the real driver/memlog.c, README.md, and udev rule.
/// Marked `#[ignore]` because the cloud build box doesn't have the wintermute-kernel
/// udev rule file; run explicitly with `cargo test -- --ignored test_live_sources_agree`
/// on the real machine to expose the 0640 vs 0660 skew bug.
#[test]
#[ignore]
fn test_live_sources_agree() {
    // integration test: exercises the real files on this machine
    // CARGO_MANIFEST_DIR resolves correctly on both local and cloud build hosts
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let driver_src = std::fs::read_to_string(repo_root.join("driver/memlog.c"))
        .expect("driver/memlog.c not found");
    let driver_mode = parse_driver_mode(&driver_src)
        .expect("could not parse driver mode");

    let readme_src = std::fs::read_to_string(repo_root.join("README.md"))
        .expect("README.md not found");
    let readme_mode = parse_udev_rule_mode(&readme_src)
        .expect("could not parse README mode");

    let (rule_path, source) = resolve_udev_rule_path(&repo_root)
        .expect("could not resolve udev rule path");
    eprintln!("Using udev rule source: {} at {:?}", source, rule_path);
    let rule_src = std::fs::read_to_string(&rule_path)
        .expect("could not read udev rule file");
    let rule_mode = parse_udev_rule_mode(&rule_src)
        .expect("could not parse udev rule mode");

    eprintln!(
        "driver: 0{:o}, readme: 0{:o}, udev rule ({}): 0{:o}",
        driver_mode, readme_mode, source, rule_mode
    );

    assert_eq!(
        driver_mode, readme_mode,
        "driver mode 0{:o} != README mode 0{:o}",
        driver_mode, readme_mode
    );
    assert_eq!(
        readme_mode, rule_mode,
        "README mode 0{:o} != udev rule ({}) mode 0{:o}",
        readme_mode, source, rule_mode
    );
    assert!(
        rule_mode & 0o020 != 0,
        "agreed mode 0{:o} lacks group-write (sanity floor)",
        rule_mode
    );
}
