use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;

const RELEASE_LIFECYCLE_TEST: &str = "adapter_install_resolves_and_installs_fixture_from_index";
const WEB_TEST_FILTER: &str = "web_";

#[derive(Serialize)]
struct GateResult {
    format: &'static str,
    platform: &'static str,
    result_root: String,
    passed: bool,
    probes: Vec<ProbeResult>,
}

#[derive(Serialize)]
struct ProbeResult {
    name: &'static str,
    safety_classification: &'static str,
    command: Vec<String>,
    exit_code: Option<i32>,
    passed: bool,
    semantic_failures: Vec<String>,
    stdout_log: String,
    stderr_log: String,
    retained_result: Option<String>,
}

struct ProbeCommand {
    name: &'static str,
    safety_classification: &'static str,
    args: Vec<&'static str>,
    expected_tests: usize,
    matrix_root: Option<PathBuf>,
}

#[test]
#[cfg(windows)]
#[ignore = "authoritative completion-grade CLI E2E gate; run explicitly with --ignored --nocapture"]
fn cli_e2e_gate() -> anyhow::Result<()> {
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let gate_root = create_unique_gate_root(&manifest_root)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = manifest_root.join("Cargo.toml");
    let matrix_root = gate_root.join("fresh-project-matrix");

    let probes = vec![
        ProbeCommand {
            name: "fresh-project-schema-migration-source-matrix",
            safety_classification: "isolated-filesystem-and-subprocess",
            args: vec![
                "test",
                "--test",
                "cli_e2e_harness",
                "--",
                "--ignored",
                "--nocapture",
            ],
            expected_tests: 1,
            matrix_root: Some(matrix_root),
        },
        ProbeCommand {
            name: "offline-signed-release-adapter-lifecycle",
            safety_classification: "offline-signed-release-fixture",
            args: vec![
                "test",
                "--test",
                "cli_smoke",
                RELEASE_LIFECYCLE_TEST,
                "--",
                "--exact",
                "--nocapture",
            ],
            expected_tests: 1,
            matrix_root: None,
        },
        ProbeCommand {
            name: "live-loopback-web-safety",
            safety_classification: "live-loopback-network",
            args: vec![
                "test",
                "--test",
                "cli_smoke",
                WEB_TEST_FILTER,
                "--",
                "--nocapture",
            ],
            expected_tests: 5,
            matrix_root: None,
        },
    ];

    let mut results = Vec::with_capacity(probes.len());
    for probe in probes {
        let result = run_probe(&cargo, &manifest_root, &manifest, &gate_root, probe)?;
        println!(
            "CLI E2E gate probe {}: {} (stdout: {}, stderr: {})",
            result.name,
            if result.passed { "passed" } else { "FAILED" },
            result.stdout_log,
            result.stderr_log
        );
        results.push(result);
    }

    let passed = results.iter().all(|probe| probe.passed);
    let result = GateResult {
        format: "ldgr.cli-e2e-gate-result.v1",
        platform: std::env::consts::OS,
        result_root: gate_root.display().to_string(),
        passed,
        probes: results,
    };
    let result_path = gate_root.join("result.json");
    fs::write(&result_path, serde_json::to_vec_pretty(&result)?)
        .with_context(|| format!("failed to write {}", result_path.display()))?;
    println!("retained CLI E2E gate at {}", gate_root.display());

    if !passed {
        bail!(
            "CLI E2E gate failed semantically; inspect {}",
            result_path.display()
        );
    }
    Ok(())
}

#[test]
#[cfg(not(windows))]
#[ignore = "authoritative completion-grade CLI E2E gate requires a Windows CI runner"]
fn cli_e2e_gate() -> anyhow::Result<()> {
    bail!(
        "the authoritative CLI E2E gate requires a Windows CI runner for its PowerShell schema/migration/source matrix"
    )
}

fn create_unique_gate_root(manifest_root: &Path) -> anyhow::Result<PathBuf> {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let parent = std::env::var_os("LDGR_CLI_E2E_GATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_root.join("target").join("cli-e2e-gate"));
    fs::create_dir_all(&parent)?;
    let gate_root = parent.join(format!("run-{}-{unique}", std::process::id()));
    fs::create_dir(&gate_root)
        .with_context(|| format!("failed to create unique gate root {}", gate_root.display()))?;
    Ok(gate_root)
}

fn run_probe(
    cargo: &std::ffi::OsStr,
    manifest_root: &Path,
    manifest: &Path,
    gate_root: &Path,
    probe: ProbeCommand,
) -> anyhow::Result<ProbeResult> {
    let stdout_path = gate_root.join(format!("{}.stdout.log", probe.name));
    let stderr_path = gate_root.join(format!("{}.stderr.log", probe.name));
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let mut command = Command::new(cargo);
    command
        .current_dir(manifest_root)
        .arg("--color")
        .arg("never")
        .arg(probe.args[0])
        .arg("--manifest-path")
        .arg(manifest)
        .args(&probe.args[1..])
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(matrix_root) = &probe.matrix_root {
        command.env("LDGR_CLI_E2E_MATRIX_ROOT", matrix_root);
    }

    let status = command.status();
    let stdout_text = fs::read_to_string(&stdout_path).unwrap_or_default();
    let stderr_text = fs::read_to_string(&stderr_path).unwrap_or_default();
    let mut semantic_failures = Vec::new();
    let exit_status = match status {
        Ok(status) => {
            if !status.success() {
                semantic_failures.push(format!("cargo test exited with {status}"));
            }
            Some(status)
        }
        Err(error) => {
            semantic_failures.push(format!("failed to launch cargo test: {error}"));
            None
        }
    };

    validate_test_summary(
        &stdout_text,
        &stderr_text,
        probe.expected_tests,
        &mut semantic_failures,
    );

    let retained_result = if let Some(matrix_root) = &probe.matrix_root {
        let result_path = matrix_root.join("matrix").join("result.json");
        validate_matrix_result(&result_path, &mut semantic_failures);
        Some(result_path.display().to_string())
    } else {
        None
    };

    Ok(ProbeResult {
        name: probe.name,
        safety_classification: probe.safety_classification,
        command: rendered_command(cargo, manifest, &probe.args),
        exit_code: exit_status.as_ref().and_then(ExitStatus::code),
        passed: exit_status.is_some_and(|status| status.success()) && semantic_failures.is_empty(),
        semantic_failures,
        stdout_log: stdout_path.display().to_string(),
        stderr_log: stderr_path.display().to_string(),
        retained_result,
    })
}

fn validate_test_summary(
    stdout: &str,
    stderr: &str,
    expected_tests: usize,
    failures: &mut Vec<String>,
) {
    let summary = format!("test result: ok. {expected_tests} passed; 0 failed;");
    if !stdout
        .lines()
        .chain(stderr.lines())
        .any(|line| line.contains(&summary))
    {
        failures.push(format!(
            "missing successful cargo test summary for exactly {expected_tests} selected test(s)"
        ));
    }
}

fn validate_matrix_result(path: &Path, failures: &mut Vec<String>) {
    let body = match fs::read(path) {
        Ok(body) => body,
        Err(error) => {
            failures.push(format!(
                "missing retained matrix result {}: {error}",
                path.display()
            ));
            return;
        }
    };
    let json_body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&body);
    let result: Value = match serde_json::from_slice(json_body) {
        Ok(result) => result,
        Err(error) => {
            failures.push(format!("invalid matrix result JSON: {error}"));
            return;
        }
    };
    if result["format"] != "ldgr.cli-e2e-result.v1" {
        failures.push("matrix result has an unexpected format".to_owned());
    }
    let total = result["total_cases"].as_u64().unwrap_or(0);
    let passed = result["passed_cases"].as_u64().unwrap_or(0);
    let failed = result["failed_cases"].as_u64().unwrap_or(u64::MAX);
    if total == 0 || passed != total || failed != 0 {
        failures.push(format!(
            "matrix semantic result is not clean: total={total} passed={passed} failed={failed}"
        ));
    }
    if result["safety_classification_cases"].as_u64().unwrap_or(0) == 0 {
        failures.push("matrix lost its independent safety classifications".to_owned());
    }
    if result["fatal_error"]
        .as_str()
        .is_some_and(|error| !error.is_empty())
    {
        failures.push(format!(
            "matrix reported fatal_error={}",
            result["fatal_error"]
        ));
    }
}

fn rendered_command(cargo: &std::ffi::OsStr, manifest: &Path, args: &[&str]) -> Vec<String> {
    let mut rendered = vec![
        cargo.to_string_lossy().into_owned(),
        "--color".to_owned(),
        "never".to_owned(),
        args[0].to_owned(),
        "--manifest-path".to_owned(),
        manifest.display().to_string(),
    ];
    rendered.extend(args[1..].iter().map(|arg| (*arg).to_owned()));
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_summary_requires_the_exact_selected_test_count() {
        let mut failures = Vec::new();
        validate_test_summary(
            "test result: ok. 5 passed; 0 failed; 0 ignored",
            "",
            5,
            &mut failures,
        );
        assert!(failures.is_empty());

        validate_test_summary(
            "test result: ok. 0 passed; 0 failed; 5 filtered out",
            "",
            5,
            &mut failures,
        );
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn matrix_result_requires_clean_semantics_and_safety_classifications() {
        let directory = tempfile::TempDir::new().unwrap();
        let result_path = directory.path().join("result.json");
        let clean_result = serde_json::json!({
            "format": "ldgr.cli-e2e-result.v1",
            "total_cases": 3,
            "passed_cases": 3,
            "failed_cases": 0,
            "safety_classification_cases": 1,
            "fatal_error": ""
        });
        fs::write(
            &result_path,
            [
                b"\xEF\xBB\xBF".as_slice(),
                clean_result.to_string().as_bytes(),
            ]
            .concat(),
        )
        .unwrap();
        let mut failures = Vec::new();
        validate_matrix_result(&result_path, &mut failures);
        assert!(failures.is_empty());

        fs::write(
            &result_path,
            serde_json::json!({
                "format": "ldgr.cli-e2e-result.v1",
                "total_cases": 3,
                "passed_cases": 3,
                "failed_cases": 0,
                "safety_classification_cases": 0,
                "fatal_error": ""
            })
            .to_string(),
        )
        .unwrap();
        validate_matrix_result(&result_path, &mut failures);
        assert_eq!(
            failures,
            ["matrix lost its independent safety classifications"]
        );
    }
}
