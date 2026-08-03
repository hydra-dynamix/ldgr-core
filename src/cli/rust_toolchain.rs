use std::collections::BTreeSet;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use serde_json::{json, Value};

use crate::store::{
    open_store, record_error, resolve_fingerprint, ErrorClass, ErrorRetryability, ErrorSeverity,
    FingerprintRequest, RecordErrorInput, RecoveryOrigin, STRUCTURED_FINGERPRINT_V1,
};

#[derive(Clone, Debug)]
pub(super) struct SourceToolchain {
    pub cargo: PathBuf,
    cargo_home: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
}

impl SourceToolchain {
    pub(super) fn apply(&self, command: &mut Command) {
        if let Some(home) = &self.cargo_home {
            command.env("CARGO_HOME", home);
        }
        if let Some(home) = &self.rustup_home {
            command.env("RUSTUP_HOME", home);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IssueKind {
    InvalidExplicitHome,
    CargoUnavailable,
    ToolchainUnavailable,
    ToolchainAmbiguous,
}

impl IssueKind {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidExplicitHome => "relative-rust-home",
            Self::CargoUnavailable => "cargo-unavailable",
            Self::ToolchainUnavailable => "cargo-toolchain-unavailable",
            Self::ToolchainAmbiguous => "cargo-toolchain-ambiguous",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct DiscoveryIssue {
    kind: IssueKind,
    summary: String,
    attempts: Vec<Attempt>,
    choices: Vec<String>,
    environment: EnvironmentSnapshot,
}

impl DiscoveryIssue {
    fn details(&self) -> Value {
        json!({
            "attempts": self.attempts.iter().map(Attempt::redacted).collect::<Vec<_>>(),
            "fallback_options": self.choices,
        })
    }

    fn environment(&self) -> Value {
        json!({
            "os": env::consts::OS,
            "arch": env::consts::ARCH,
            "family": env::consts::FAMILY,
            "HOME": self.environment.home.state(),
            "USERPROFILE": self.environment.userprofile.state(),
            "CARGO_HOME": self.environment.cargo_home.state(),
            "RUSTUP_HOME": self.environment.rustup_home.state(),
            "PATH": if self.environment.path.is_some() { "present" } else { "missing" },
        })
    }
}

impl fmt::Display for DiscoveryIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.summary)?;
        if !self.choices.is_empty() {
            write!(formatter, ". Recovery options: {}", self.choices.join("; "))?;
        }
        Ok(())
    }
}

impl std::error::Error for DiscoveryIssue {}

#[derive(Clone, Debug, Default)]
struct EnvironmentSnapshot {
    home: EnvironmentPath,
    userprofile: EnvironmentPath,
    cargo_home: EnvironmentPath,
    rustup_home: EnvironmentPath,
    path: Option<std::ffi::OsString>,
}

impl EnvironmentSnapshot {
    fn capture() -> Self {
        Self {
            home: EnvironmentPath::capture("HOME"),
            userprofile: EnvironmentPath::capture("USERPROFILE"),
            cargo_home: EnvironmentPath::capture("CARGO_HOME"),
            rustup_home: EnvironmentPath::capture("RUSTUP_HOME"),
            path: nonempty_var_os("PATH"),
        }
    }
}

#[derive(Clone, Debug, Default)]
enum EnvironmentPath {
    #[default]
    Missing,
    Relative,
    Absolute(PathBuf),
}

impl EnvironmentPath {
    fn capture(name: &str) -> Self {
        match nonempty_var_os(name).map(PathBuf::from) {
            None => Self::Missing,
            Some(path) if path.is_absolute() => Self::Absolute(path),
            Some(_) => Self::Relative,
        }
    }

    fn absolute(&self) -> Option<&Path> {
        match self {
            Self::Absolute(path) => Some(path),
            Self::Missing | Self::Relative => None,
        }
    }

    fn state(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Relative => "relative",
            Self::Absolute(_) => "absolute",
        }
    }

    fn is_relative(&self) -> bool {
        matches!(self, Self::Relative)
    }
}

#[derive(Clone, Debug)]
struct CargoCandidate {
    path: PathBuf,
    origin: String,
    inferred_home: Option<PathBuf>,
    rustup_proxy: bool,
}

#[derive(Clone, Debug)]
struct RustupHomeCandidate {
    path: PathBuf,
    origin: String,
}

#[derive(Clone, Debug)]
struct Attempt {
    cargo_origin: String,
    rustup_origin: Option<String>,
    outcome: String,
}

impl Attempt {
    fn redacted(&self) -> Value {
        json!({
            "cargo": self.cargo_origin,
            "rustup_home": self.rustup_origin,
            "outcome": self.outcome,
        })
    }
}

#[derive(Clone, Debug)]
struct ProbeOutcome {
    success: bool,
    summary: String,
}

pub(super) fn resolve_source_adapter_toolchain(
    argv: &[String],
) -> Result<Option<SourceToolchain>, Box<DiscoveryIssue>> {
    if !is_source_cargo_runner(argv) || !cfg!(windows) {
        return Ok(None);
    }
    let environment = EnvironmentSnapshot::capture();
    discover_windows_with(
        &environment,
        |path| path.is_file(),
        |path| path.is_dir(),
        probe_rustup_home,
        probe_cargo,
    )
    .map(Some)
}

fn is_source_cargo_runner(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    let file_name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    matches!(
        file_name.to_ascii_lowercase().as_str(),
        "cargo" | "cargo.exe"
    ) && argv.get(1).is_some_and(|argument| argument == "run")
        && argv.iter().any(|argument| argument == "--manifest-path")
        && argv.iter().any(|argument| argument == "--target-dir")
}

fn discover_windows_with<F, D, R, P>(
    environment: &EnvironmentSnapshot,
    mut is_file: F,
    mut is_dir: D,
    mut rustup_metadata: R,
    mut cargo_probe: P,
) -> Result<SourceToolchain, Box<DiscoveryIssue>>
where
    F: FnMut(&Path) -> bool,
    D: FnMut(&Path) -> bool,
    R: FnMut(&Path) -> Option<PathBuf>,
    P: FnMut(&Path, Option<&Path>, Option<&Path>) -> ProbeOutcome,
{
    let relative_explicit = [
        ("CARGO_HOME", &environment.cargo_home),
        ("RUSTUP_HOME", &environment.rustup_home),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.is_relative().then_some(name))
    .collect::<Vec<_>>();
    if !relative_explicit.is_empty() {
        return Err(Box::new(DiscoveryIssue {
            kind: IssueKind::InvalidExplicitHome,
            summary: format!(
                "{} must be absolute for deterministic source-adapter execution",
                relative_explicit.join(" and ")
            ),
            attempts: Vec::new(),
            choices: vec![format!(
                "set {} to an existing absolute directory or remove it so LDGR can discover platform defaults",
                relative_explicit.join(" and ")
            )],
            environment: environment.clone(),
        }));
    }

    let cargo_candidates = cargo_candidates(environment, &mut is_file);
    if cargo_candidates.is_empty() {
        return Err(Box::new(DiscoveryIssue {
            kind: IssueKind::CargoUnavailable,
            summary: "LDGR could not find cargo before source-adapter spawn".to_string(),
            attempts: Vec::new(),
            choices: vec![
                "add cargo.exe to PATH".to_string(),
                "set CARGO_HOME to an absolute Cargo home containing bin/cargo.exe".to_string(),
                "install Rustup in USERPROFILE/.cargo (the Windows platform default)".to_string(),
            ],
            environment: environment.clone(),
        }));
    }

    // First reproduce the command's inherited environment. A successful explicit/current
    // configuration is authoritative and must not be rewritten.
    let current = &cargo_candidates[0];
    let current_probe = cargo_probe(&current.path, None, None);
    let mut attempts = vec![Attempt {
        cargo_origin: current.origin.clone(),
        rustup_origin: None,
        outcome: current_probe.summary.clone(),
    }];
    if current_probe.success {
        return Ok(SourceToolchain {
            cargo: current.path.clone(),
            cargo_home: None,
            rustup_home: None,
        });
    }

    let rustup_homes = rustup_home_candidates(
        environment,
        &cargo_candidates,
        &mut is_file,
        &mut is_dir,
        &mut rustup_metadata,
    );
    let explicit_cargo_home = environment.cargo_home.absolute().map(Path::to_path_buf);
    let explicit_rustup_home = environment.rustup_home.absolute().map(Path::to_path_buf);
    let mut successes = Vec::<(SourceToolchain, String, Option<String>)>::new();

    for cargo in &cargo_candidates {
        let cargo_home = explicit_cargo_home
            .clone()
            .or_else(|| cargo.inferred_home.clone());
        if cargo.rustup_proxy {
            let homes = if let Some(home) = &explicit_rustup_home {
                vec![RustupHomeCandidate {
                    path: home.clone(),
                    origin: "RUSTUP_HOME".to_string(),
                }]
            } else {
                rustup_homes.clone()
            };
            for rustup in homes {
                let outcome = cargo_probe(&cargo.path, cargo_home.as_deref(), Some(&rustup.path));
                attempts.push(Attempt {
                    cargo_origin: cargo.origin.clone(),
                    rustup_origin: Some(rustup.origin.clone()),
                    outcome: outcome.summary.clone(),
                });
                if outcome.success {
                    successes.push((
                        SourceToolchain {
                            cargo: cargo.path.clone(),
                            cargo_home: cargo_home.clone(),
                            rustup_home: Some(rustup.path),
                        },
                        cargo.origin.clone(),
                        Some(rustup.origin),
                    ));
                }
            }
        } else {
            let outcome = cargo_probe(&cargo.path, cargo_home.as_deref(), None);
            attempts.push(Attempt {
                cargo_origin: cargo.origin.clone(),
                rustup_origin: None,
                outcome: outcome.summary.clone(),
            });
            if outcome.success {
                successes.push((
                    SourceToolchain {
                        cargo: cargo.path.clone(),
                        cargo_home,
                        rustup_home: None,
                    },
                    cargo.origin.clone(),
                    None,
                ));
            }
        }
    }

    deduplicate_successes(&mut successes);
    match successes.len() {
        1 => Ok(successes.remove(0).0),
        0 => Err(Box::new(DiscoveryIssue {
            kind: IssueKind::ToolchainUnavailable,
            summary: "cargo was discovered, but no usable Cargo/Rustup toolchain configuration passed `cargo --version` before source-adapter spawn".to_string(),
            attempts,
            choices: unavailable_choices(environment, &cargo_candidates, &rustup_homes),
            environment: environment.clone(),
        })),
        _ => Err(Box::new(DiscoveryIssue {
            kind: IssueKind::ToolchainAmbiguous,
            summary: "multiple Cargo/Rustup fallback configurations are usable; LDGR will not choose one implicitly".to_string(),
            attempts,
            choices: successes
                .iter()
                .map(|(_, cargo, rustup)| {
                    format!(
                        "select cargo from {cargo}{} with explicit absolute CARGO_HOME/RUSTUP_HOME",
                        rustup
                            .as_ref()
                            .map(|origin| format!(" and Rustup home from {origin}"))
                            .unwrap_or_default()
                    )
                })
                .collect(),
            environment: environment.clone(),
        })),
    }
}

fn cargo_candidates<F>(environment: &EnvironmentSnapshot, is_file: &mut F) -> Vec<CargoCandidate>
where
    F: FnMut(&Path) -> bool,
{
    let mut raw = Vec::<(PathBuf, String, Option<PathBuf>)>::new();
    if let Some(path) = find_on_path("cargo", environment.path.as_deref(), is_file) {
        raw.push((path, "PATH".to_string(), None));
    }
    if let Some(home) = environment.cargo_home.absolute() {
        raw.push((
            home.join("bin/cargo.exe"),
            "CARGO_HOME/bin".to_string(),
            Some(home.to_path_buf()),
        ));
    }
    for (origin, profile) in [
        ("USERPROFILE/.cargo/bin", &environment.userprofile),
        ("HOME/.cargo/bin", &environment.home),
    ] {
        if let Some(profile) = profile.absolute() {
            let home = profile.join(".cargo");
            raw.push((home.join("bin/cargo.exe"), origin.to_string(), Some(home)));
        }
    }

    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for (path, origin, inferred_home) in raw {
        if !is_file(&path) || !seen.insert(normalized_identity(&path)) {
            continue;
        }
        let rustup_proxy = path
            .parent()
            .is_some_and(|parent| is_file(&parent.join("rustup.exe")));
        let inferred_home = inferred_home.or_else(|| {
            rustup_proxy
                .then(|| path.parent())
                .flatten()
                .filter(|parent| {
                    parent
                        .file_name()
                        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
                })
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });
        candidates.push(CargoCandidate {
            path,
            origin,
            inferred_home,
            rustup_proxy,
        });
    }
    candidates
}

fn rustup_home_candidates<F, D, R>(
    environment: &EnvironmentSnapshot,
    cargo: &[CargoCandidate],
    is_file: &mut F,
    is_dir: &mut D,
    rustup_metadata: &mut R,
) -> Vec<RustupHomeCandidate>
where
    F: FnMut(&Path) -> bool,
    D: FnMut(&Path) -> bool,
    R: FnMut(&Path) -> Option<PathBuf>,
{
    if environment.rustup_home.absolute().is_some() {
        return Vec::new();
    }
    let mut raw = Vec::new();
    let mut rustup_executables = cargo
        .iter()
        .filter_map(|candidate| {
            candidate
                .path
                .parent()
                .map(|parent| parent.join("rustup.exe"))
        })
        .filter(|path| is_file(path))
        .collect::<Vec<_>>();
    if let Some(path) = find_on_path("rustup", environment.path.as_deref(), is_file) {
        rustup_executables.push(path);
    }
    for rustup in rustup_executables {
        if let Some(home) =
            rustup_metadata(&rustup).filter(|path| path.is_absolute() && is_dir(path))
        {
            raw.push(RustupHomeCandidate {
                path: home,
                origin: "rustup show home".to_string(),
            });
        }
    }
    for (origin, profile) in [
        ("USERPROFILE/.rustup", &environment.userprofile),
        ("HOME/.rustup", &environment.home),
    ] {
        if let Some(profile) = profile.absolute() {
            let path = profile.join(".rustup");
            if is_dir(&path) {
                raw.push(RustupHomeCandidate {
                    path,
                    origin: origin.to_string(),
                });
            }
        }
    }
    let mut seen = BTreeSet::new();
    raw.into_iter()
        .filter(|candidate| seen.insert(normalized_identity(&candidate.path)))
        .collect()
}

fn find_on_path<F>(
    program: &str,
    path: Option<&std::ffi::OsStr>,
    is_file: &mut F,
) -> Option<PathBuf>
where
    F: FnMut(&Path) -> bool,
{
    let path = path?;
    env::split_paths(path).find_map(|directory| {
        ["exe", ""].into_iter().find_map(|extension| {
            let file = if extension.is_empty() {
                directory.join(program)
            } else {
                directory.join(format!("{program}.{extension}"))
            };
            is_file(&file).then_some(file)
        })
    })
}

fn probe_rustup_home(rustup: &Path) -> Option<PathBuf> {
    let output = Command::new(rustup).args(["show", "home"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn probe_cargo(
    cargo: &Path,
    cargo_home: Option<&Path>,
    rustup_home: Option<&Path>,
) -> ProbeOutcome {
    let mut command = Command::new(cargo);
    command.arg("--version");
    if let Some(home) = cargo_home {
        command.env("CARGO_HOME", home);
    }
    if let Some(home) = rustup_home {
        command.env("RUSTUP_HOME", home);
    }
    match command.output() {
        Ok(output) if output.status.success() => ProbeOutcome {
            success: true,
            summary: "passed cargo --version".to_string(),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ProbeOutcome {
                success: false,
                summary: classify_probe_failure(&stderr),
            }
        }
        Err(error) => ProbeOutcome {
            success: false,
            summary: format!("could not start cargo probe ({})", error.kind()),
        },
    }
}

fn classify_probe_failure(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("rustup") && lower.contains("toolchain") {
        "cargo proxy could not select an installed Rustup toolchain".to_string()
    } else if lower.contains("toolchain") {
        "cargo could not select a toolchain".to_string()
    } else {
        "cargo --version returned a non-zero status".to_string()
    }
}

fn deduplicate_successes(successes: &mut Vec<(SourceToolchain, String, Option<String>)>) {
    let mut seen = BTreeSet::new();
    successes.retain(|(toolchain, _, _)| {
        seen.insert((
            normalized_identity(&toolchain.cargo),
            toolchain.cargo_home.as_deref().map(normalized_identity),
            toolchain.rustup_home.as_deref().map(normalized_identity),
        ))
    });
}

fn unavailable_choices(
    environment: &EnvironmentSnapshot,
    cargo: &[CargoCandidate],
    rustup: &[RustupHomeCandidate],
) -> Vec<String> {
    let mut choices = Vec::new();
    if !cargo.is_empty() {
        choices.push(format!(
            "set CARGO_HOME to the absolute home for a discovered cargo candidate ({})",
            cargo
                .iter()
                .map(|candidate| candidate.origin.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !rustup.is_empty() {
        choices.push(format!(
            "set RUSTUP_HOME explicitly to one discovered installed-toolchain home ({})",
            rustup
                .iter()
                .map(|candidate| candidate.origin.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if environment.rustup_home.absolute().is_none() {
        choices.push(
            "install a Rustup toolchain or set RUSTUP_HOME to its existing absolute home"
                .to_string(),
        );
    }
    choices.push(
        "use a standalone cargo executable on PATH if Rustup is intentionally not used".to_string(),
    );
    choices
}

fn normalized_identity(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn nonempty_var_os(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

pub(super) fn record_discovery_error(
    db_path: &Path,
    adapter_slug: &str,
    issue: &DiscoveryIssue,
) -> anyhow::Result<i64> {
    let connection = open_store(db_path)?;
    let fingerprint = resolve_fingerprint(FingerprintRequest {
        version: STRUCTURED_FINGERPRINT_V1,
        supplied_fingerprint: None,
        override_rationale: None,
        split_key: None,
        split_rationale: None,
        class: ErrorClass::InfrastructureError,
        domain: "ldgr.adapter.source-runtime",
        code: issue.kind.code(),
        boundary: Some("toolchain-discovery"),
        component: Some("ldgr-core"),
        subject: Some(adapter_slug),
    })?;
    let occurrence_id = uuid_v7()?;
    let operation_id = uuid_v7()?;
    let attempt_id = uuid_v7()?;
    let idempotency_key = format!("{attempt_id}:{}", issue.kind.code());
    let observed_at = observed_at();
    let details = issue.details();
    let environment = issue.environment();
    let result = record_error(
        &connection,
        &RecordErrorInput {
            occurrence_id: &occurrence_id,
            producer: "ldgr-core",
            idempotency_key: &idempotency_key,
            operation_id: &operation_id,
            attempt_id: &attempt_id,
            fingerprint_version: &fingerprint.version,
            fingerprint: &fingerprint.fingerprint,
            fingerprint_inputs: Some(&fingerprint.inputs),
            fingerprint_provenance: Some(&fingerprint.provenance),
            class: ErrorClass::InfrastructureError,
            domain: "ldgr.adapter.source-runtime",
            code: issue.kind.code(),
            severity: ErrorSeverity::Error,
            retryability: ErrorRetryability::AfterChange,
            source: "ldgr-core:adapter-pre-spawn",
            summary: &issue.summary,
            details: &details,
            environment: &environment,
            observed_at: &observed_at,
            recovery_origin: RecoveryOrigin::Database,
        },
    )?;
    Ok(result.error.id)
}

fn uuid_v7() -> anyhow::Result<String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis() as u64;
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("generating toolchain discovery identity: {error}"))?;
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().expect("four bytes")),
        u16::from_be_bytes(bytes[4..6].try_into().expect("two bytes")),
        u16::from_be_bytes(bytes[6..8].try_into().expect("two bytes")),
        u16::from_be_bytes(bytes[8..10].try_into().expect("two bytes")),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    ))
}

fn observed_at() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        (second_of_day % 3_600) / 60,
        second_of_day % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{get_error, init_store, ErrorClass};

    fn absolute(path: impl Into<PathBuf>) -> EnvironmentPath {
        EnvironmentPath::Absolute(path.into())
    }

    fn windows_environment(root: &Path) -> EnvironmentSnapshot {
        EnvironmentSnapshot {
            home: absolute(root.join("harness-home")),
            userprofile: absolute(root.join("profile")),
            cargo_home: EnvironmentPath::Missing,
            rustup_home: EnvironmentPath::Missing,
            path: Some(env::join_paths([root.join("profile/.cargo/bin")]).unwrap()),
        }
    }

    fn create(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"fixture").unwrap();
    }

    #[test]
    fn source_runner_detection_does_not_touch_normal_adapter_commands() {
        assert!(is_source_cargo_runner(&[
            "cargo".into(),
            "run".into(),
            "--manifest-path".into(),
            "workspace/Cargo.toml".into(),
            "--target-dir".into(),
            "target".into(),
        ]));
        assert!(!is_source_cargo_runner(&[
            "cargo".into(),
            "metadata".into()
        ]));
        assert!(!is_source_cargo_runner(&["adapter.exe".into()]));
    }

    #[cfg(windows)]
    #[test]
    fn split_harness_home_discovers_windows_profile_rustup() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let environment = windows_environment(root.path());
        let cargo = root.path().join("profile/.cargo/bin/cargo.exe");
        let rustup = root.path().join("profile/.cargo/bin/rustup.exe");
        let rustup_home = root.path().join("profile/.rustup");
        create(&cargo);
        create(&rustup);
        fs::create_dir_all(&rustup_home)?;

        let resolved = discover_windows_with(
            &environment,
            |path| path.is_file(),
            |path| path.is_dir(),
            |_| None,
            |_, _, selected_rustup| ProbeOutcome {
                success: selected_rustup == Some(rustup_home.as_path()),
                summary: if selected_rustup == Some(rustup_home.as_path()) {
                    "passed cargo --version".into()
                } else {
                    "cargo proxy could not select an installed Rustup toolchain".into()
                },
            },
        )?;

        assert_eq!(resolved.cargo, cargo);
        assert_eq!(
            resolved.cargo_home.as_deref(),
            Some(root.path().join("profile/.cargo").as_path())
        );
        assert_eq!(resolved.rustup_home.as_deref(), Some(rustup_home.as_path()));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn rustup_metadata_recovers_when_profile_homes_are_missing() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let cargo = root.path().join("portable/bin/cargo.exe");
        let rustup = root.path().join("portable/bin/rustup.exe");
        let rustup_home = root.path().join("portable/toolchains");
        create(&cargo);
        create(&rustup);
        fs::create_dir_all(&rustup_home)?;
        let environment = EnvironmentSnapshot {
            path: Some(env::join_paths([cargo.parent().unwrap()])?),
            ..Default::default()
        };

        let resolved = discover_windows_with(
            &environment,
            |path| path.is_file(),
            |path| path.is_dir(),
            |candidate| (candidate == rustup).then(|| rustup_home.clone()),
            |_, _, selected_rustup| ProbeOutcome {
                success: selected_rustup == Some(rustup_home.as_path()),
                summary: "probe".into(),
            },
        )?;

        assert_eq!(resolved.cargo, cargo);
        assert_eq!(resolved.rustup_home.as_deref(), Some(rustup_home.as_path()));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn valid_explicit_split_homes_are_preserved() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let cargo_home = root.path().join("cargo");
        let rustup_home = root.path().join("toolchains");
        let cargo = cargo_home.join("bin/cargo.exe");
        create(&cargo);
        create(&cargo_home.join("bin/rustup.exe"));
        fs::create_dir_all(&rustup_home)?;
        let environment = EnvironmentSnapshot {
            cargo_home: absolute(&cargo_home),
            rustup_home: absolute(&rustup_home),
            ..Default::default()
        };

        let resolved = discover_windows_with(
            &environment,
            |path| path.is_file(),
            |path| path.is_dir(),
            |_| None,
            |_, cargo_override, rustup_override| ProbeOutcome {
                success: cargo_override == Some(cargo_home.as_path())
                    && rustup_override == Some(rustup_home.as_path()),
                summary: "probe".into(),
            },
        )?;

        assert_eq!(resolved.cargo_home.as_deref(), Some(cargo_home.as_path()));
        assert_eq!(resolved.rustup_home.as_deref(), Some(rustup_home.as_path()));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn standalone_cargo_succeeds_without_rustup() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let cargo = root.path().join("standalone/cargo.exe");
        create(&cargo);
        let environment = EnvironmentSnapshot {
            path: Some(env::join_paths([cargo.parent().unwrap()])?),
            ..Default::default()
        };

        let resolved = discover_windows_with(
            &environment,
            |path| path.is_file(),
            |path| path.is_dir(),
            |_| None,
            |_, cargo_home, rustup_home| ProbeOutcome {
                success: cargo_home.is_none() && rustup_home.is_none(),
                summary: "passed cargo --version".into(),
            },
        )?;

        assert_eq!(resolved.cargo, cargo);
        assert!(resolved.rustup_home.is_none());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn relative_explicit_homes_fail_without_derived_paths() {
        let environment = EnvironmentSnapshot {
            cargo_home: EnvironmentPath::Relative,
            ..Default::default()
        };
        let issue = discover_windows_with(
            &environment,
            |_| false,
            |_| false,
            |_| None,
            |_, _, _| unreachable!(),
        )
        .unwrap_err();
        assert_eq!(issue.kind, IssueKind::InvalidExplicitHome);
        assert!(issue.to_string().contains("must be absolute"));
        assert!(!issue.to_string().contains(r"\.cargo"));
    }

    #[cfg(windows)]
    #[test]
    fn multiple_working_rustup_homes_are_reported_as_ambiguous() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let mut environment = windows_environment(root.path());
        environment.home = absolute(root.path().join("alternate"));
        let cargo = root.path().join("profile/.cargo/bin/cargo.exe");
        create(&cargo);
        create(&root.path().join("profile/.cargo/bin/rustup.exe"));
        fs::create_dir_all(root.path().join("profile/.rustup"))?;
        fs::create_dir_all(root.path().join("alternate/.rustup"))?;

        let issue = discover_windows_with(
            &environment,
            |path| path.is_file(),
            |path| path.is_dir(),
            |_| None,
            |_, _, rustup_home| ProbeOutcome {
                success: rustup_home.is_some(),
                summary: "probe".into(),
            },
        )
        .unwrap_err();

        assert_eq!(issue.kind, IssueKind::ToolchainAmbiguous);
        assert_eq!(issue.choices.len(), 2);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn missing_cargo_returns_actionable_redacted_options() {
        let root = tempfile::tempdir().unwrap();
        let environment = windows_environment(root.path());
        let issue = discover_windows_with(
            &environment,
            |_| false,
            |_| false,
            |_| None,
            |_, _, _| unreachable!(),
        )
        .unwrap_err();

        assert_eq!(issue.kind, IssueKind::CargoUnavailable);
        assert!(issue.to_string().contains("add cargo.exe to PATH"));
        assert!(!issue
            .to_string()
            .contains(&root.path().display().to_string()));
        assert_eq!(issue.environment()["HOME"], "absolute");
    }

    #[cfg(windows)]
    #[test]
    fn discovery_failure_is_a_redacted_first_class_infrastructure_error() -> anyhow::Result<()> {
        let project = tempfile::tempdir()?;
        let db = project.path().join(".ldgr/ldgr.db");
        let artifacts = project.path().join(".ldgr/artifacts");
        init_store(&db, &artifacts)?;
        let environment = windows_environment(project.path());
        let issue = discover_windows_with(
            &environment,
            |_| false,
            |_| false,
            |_| None,
            |_, _, _| unreachable!(),
        )
        .unwrap_err();

        let error_id = record_discovery_error(&db, "example", &issue)?;
        let connection = open_store(&db)?;
        let error = get_error(&connection, error_id)?;
        assert_eq!(error.class, ErrorClass::InfrastructureError);
        assert_eq!(error.domain, "ldgr.adapter.source-runtime");
        assert_eq!(error.code, "cargo-unavailable");
        let stored: String = connection.query_row(
            "SELECT details_json || environment_json FROM error_occurrence WHERE error_id=?1",
            [error_id],
            |row| row.get(0),
        )?;
        assert!(!stored.contains(&project.path().display().to_string()));
        assert!(stored.contains("add cargo.exe to PATH"));
        Ok(())
    }
}
