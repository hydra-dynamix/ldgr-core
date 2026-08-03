use assert_cmd::Command;

#[test]
fn compatibility_report_accepts_paired_agentctl() {
    let output = Command::cargo_bin("ldgr")
        .expect("ldgr")
        .args(["compatibility", "--agentctl-version", "0.1.2", "--json"])
        .output()
        .expect("compatibility");
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("compatibility JSON");
    assert_eq!(report["schema"], "ldgr.launcher-compatibility.v1");
    assert_eq!(report["compatible"], true);
    assert_eq!(report["core_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["agentctl_requirement"], ">=0.1.2, <0.2.0");
    assert_eq!(report["error_recovery_schema"], 1);
}

#[test]
fn compatibility_report_rejects_old_agentctl() {
    Command::cargo_bin("ldgr")
        .expect("ldgr")
        .args(["compatibility", "--agentctl-version", "0.1.1", "--json"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("\"compatible\": false"))
        .stderr(predicates::str::contains(
            "install the paired release bundle",
        ));
}
