use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::catalog::CoreReleaseMetadata;

pub const CORE_INSTALLATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub const CORE_INSTALLATION_RECEIPT_FILE: &str = "core-installation-receipt.json";
pub const LAUNCHER_COMPATIBILITY_SCHEMA: &str = "ldgr.launcher-compatibility.v1";
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoreInstallerKind {
    Official,
    PackageManager,
    LegacyAdopted,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorePackageManager {
    Cargo,
    Homebrew,
    Winget,
    Scoop,
    Nix,
    Snap,
    System,
}

impl CorePackageManager {
    pub fn update_command(self) -> Option<String> {
        match self {
            Self::Cargo => Some("cargo install --locked --force ldgr-core".into()),
            Self::Homebrew => Some("brew upgrade ldgr".into()),
            Self::Winget => Some("winget upgrade LDGR.LDGR".into()),
            Self::Scoop => Some("scoop update ldgr".into()),
            Self::Nix => Some("update LDGR through the Nix profile".into()),
            Self::Snap => Some("sudo snap refresh ldgr".into()),
            Self::System => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreArchiveProvenance {
    pub url: String,
    pub sha256: String,
    pub signing_key_id: String,
    pub platform: String,
    pub release_commit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreInstallationReceipt {
    pub schema_version: u32,
    pub installer_kind: CoreInstallerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<CorePackageManager>,
    pub core_version: String,
    pub agentctl_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<CoreArchiveProvenance>,
    pub install_root: PathBuf,
    pub core_binary_path: PathBuf,
    pub agentctl_binary_path: PathBuf,
    pub core_binary_sha256: String,
    pub agentctl_binary_sha256: String,
    pub compatibility_schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_successful_plan_id: Option<String>,
    pub installed_at_unix_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityEvidence {
    pub core_version: String,
    pub agentctl_version: String,
    pub compatibility_schema: String,
}

pub trait CompatibilityProbe {
    fn probe(&self, core: &Path, agentctl: &Path) -> anyhow::Result<CompatibilityEvidence>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCompatibilityProbe;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityReport {
    schema: String,
    compatible: bool,
    core_version: String,
    core_executable: PathBuf,
    agentctl_version: String,
    agentctl_requirement: String,
    error_recovery_schema: u32,
}

impl CompatibilityProbe for ProcessCompatibilityProbe {
    fn probe(&self, core: &Path, agentctl: &Path) -> anyhow::Result<CompatibilityEvidence> {
        let output = Command::new(core)
            .arg("--version")
            .output()
            .with_context(|| format!("failed to run {} --version", core.display()))?;
        ensure!(
            output.status.success(),
            "{} --version failed with {}",
            core.display(),
            output.status
        );
        let text = String::from_utf8(output.stdout).context("Core --version is not UTF-8")?;
        let core_version = text
            .trim()
            .strip_prefix("ldgr ")
            .context("Core --version must report ldgr followed by a version")?
            .to_owned();
        Version::parse(&core_version).context("Core version is not semantic")?;
        let output = Command::new(agentctl)
            .arg("--version")
            .output()
            .with_context(|| format!("failed to run {} --version", agentctl.display()))?;
        ensure!(
            output.status.success(),
            "{} --version failed with {}",
            agentctl.display(),
            output.status
        );
        let text = String::from_utf8(output.stdout).context("agentctl --version is not UTF-8")?;
        let agentctl_version = text
            .trim()
            .strip_prefix("agentctl ")
            .context("agentctl --version must report agentctl followed by a version")?
            .to_owned();
        Version::parse(&agentctl_version).context("agentctl version is not semantic")?;
        let output = Command::new(core)
            .args([
                "compatibility",
                "--agentctl-version",
                &agentctl_version,
                "--json",
            ])
            .output()
            .with_context(|| format!("failed to run {} compatibility", core.display()))?;
        ensure!(
            output.status.success(),
            "paired compatibility validation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let report: CompatibilityReport = serde_json::from_slice(&output.stdout)
            .context("Core compatibility output is not schema-v1 JSON")?;
        ensure!(
            report.compatible,
            "paired Core/agentctl report is incompatible"
        );
        ensure!(
            report.schema == LAUNCHER_COMPATIBILITY_SCHEMA,
            "unsupported compatibility schema {}",
            report.schema
        );
        ensure!(
            canonical_regular_file(&report.core_executable, "reported Core")?
                == canonical_regular_file(core, "current Core")?,
            "compatibility report executable does not match current_exe"
        );
        ensure!(
            report.agentctl_version == agentctl_version,
            "compatibility report changed agentctl version"
        );
        ensure!(
            report.core_version == core_version,
            "compatibility report changed Core version"
        );
        ensure!(
            !report.agentctl_requirement.is_empty() && report.error_recovery_schema > 0,
            "compatibility report omitted recovery metadata"
        );
        Version::parse(&report.core_version).context("Core version is not semantic")?;
        Ok(CompatibilityEvidence {
            core_version,
            agentctl_version,
            compatibility_schema: report.schema,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyAdoptionConsent {
    InteractivePending,
    InteractiveConfirmed,
    NonInteractive { yes: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyAdoptionAuthorization {
    ConfirmationRequired,
    Approved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyAdoptionCandidate {
    pub install_root: PathBuf,
    pub core_binary_path: PathBuf,
    pub agentctl_binary_path: PathBuf,
    pub evidence: CompatibilityEvidence,
    pub authorization: LegacyAdoptionAuthorization,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreInstallationOwnership {
    OfficialInstall(CoreInstallationReceipt),
    PackageManagerCheckOnly {
        managed_by: CorePackageManager,
        update_command: Option<String>,
        reason: String,
    },
    LegacyAdoption(LegacyAdoptionCandidate),
    Unmanaged {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct OwnershipContext {
    pub home: PathBuf,
    pub current_exe: PathBuf,
    pub receipt_path: PathBuf,
    pub cargo_home: Option<PathBuf>,
    pub platform: String,
    pub adoption_consent: LegacyAdoptionConsent,
}

#[derive(Clone, Debug)]
pub struct OfficialReceiptInput {
    pub home: PathBuf,
    pub current_exe: PathBuf,
    pub agentctl_binary: PathBuf,
    pub release_metadata_path: PathBuf,
    pub archive_url: String,
    pub archive_sha256: String,
    pub signing_key_id: String,
    pub previous_successful_plan_id: Option<String>,
}

pub fn core_installation_receipt_path(home: &Path) -> PathBuf {
    home.join(".ldgr").join(CORE_INSTALLATION_RECEIPT_FILE)
}

pub fn resolve_current_core_installation(
    home: &Path,
    consent: LegacyAdoptionConsent,
) -> anyhow::Result<CoreInstallationOwnership> {
    let current_exe = std::env::current_exe().context("failed to resolve current_exe")?;
    let receipt_path = core_installation_receipt_path(home);
    resolve_core_installation_ownership(
        &OwnershipContext {
            home: home.to_path_buf(),
            current_exe,
            receipt_path,
            cargo_home: std::env::var_os("CARGO_HOME").map(PathBuf::from),
            platform: current_platform().to_owned(),
            adoption_consent: consent,
        },
        &ProcessCompatibilityProbe,
    )
}

pub fn resolve_core_installation_ownership(
    context: &OwnershipContext,
    probe: &dyn CompatibilityProbe,
) -> anyhow::Result<CoreInstallationOwnership> {
    ensure!(
        supported_platform(&context.platform),
        "unsupported platform {}",
        context.platform
    );
    let current_exe = canonical_regular_file(&context.current_exe, "current_exe")?;
    let install_root = current_exe
        .parent()
        .context("current_exe has no installation directory")?
        .to_path_buf();
    if let Some(manager) =
        recognized_manager_root(&install_root, &context.home, context.cargo_home.as_deref())?
    {
        return Ok(CoreInstallationOwnership::PackageManagerCheckOnly {
            managed_by: manager,
            update_command: manager.update_command(),
            reason: "current_exe is inside a recognized package-manager or system root".into(),
        });
    }
    if context.receipt_path.exists() {
        let receipt = read_core_installation_receipt(&context.receipt_path)?;
        validate_receipt(&receipt)?;
        ensure_receipt_matches_current_exe(&receipt, &current_exe)?;
        if let Some(manager) = receipt.managed_by {
            return Ok(CoreInstallationOwnership::PackageManagerCheckOnly {
                managed_by: manager,
                update_command: manager.update_command(),
                reason: "installation receipt declares package-manager ownership".into(),
            });
        }
        ensure!(
            receipt.installer_kind == CoreInstallerKind::Official,
            "only an official receipt establishes self-update ownership"
        );
        ensure_receipt_binary_digests(&receipt)?;
        let evidence = probe.probe(&receipt.core_binary_path, &receipt.agentctl_binary_path)?;
        ensure!(
            evidence.core_version == receipt.core_version
                && evidence.agentctl_version == receipt.agentctl_version
                && evidence.compatibility_schema == receipt.compatibility_schema,
            "live compatibility evidence does not match the receipt"
        );
        return Ok(CoreInstallationOwnership::OfficialInstall(receipt));
    }
    let canonical_home = canonical_directory(&context.home, "user home")?;
    if !path_within(&install_root, &canonical_home) {
        return Ok(CoreInstallationOwnership::Unmanaged {
            reason: "current_exe is outside the canonical user home".into(),
        });
    }
    if !directory_user_owned_and_writable(&install_root)? {
        return Ok(CoreInstallationOwnership::Unmanaged {
            reason: "current_exe directory is not user-owned and writable".into(),
        });
    }
    let agentctl = match canonical_regular_file(
        &install_root.join(agentctl_binary_name(&context.platform)),
        "sibling agentctl",
    ) {
        Ok(path) => path,
        Err(error) => {
            return Ok(CoreInstallationOwnership::Unmanaged {
                reason: format!("safe legacy adoption proof failed: {error:#}"),
            })
        }
    };
    ensure!(
        agentctl.parent() == Some(install_root.as_path()),
        "sibling agentctl escapes current_exe directory"
    );
    let evidence = match probe.probe(&current_exe, &agentctl) {
        Ok(value) => value,
        Err(error) => {
            return Ok(CoreInstallationOwnership::Unmanaged {
                reason: format!("safe legacy compatibility proof failed: {error:#}"),
            })
        }
    };
    let authorization = match context.adoption_consent {
        LegacyAdoptionConsent::InteractivePending => {
            LegacyAdoptionAuthorization::ConfirmationRequired
        }
        LegacyAdoptionConsent::InteractiveConfirmed
        | LegacyAdoptionConsent::NonInteractive { yes: true } => {
            LegacyAdoptionAuthorization::Approved
        }
        LegacyAdoptionConsent::NonInteractive { yes: false } => {
            return Ok(CoreInstallationOwnership::Unmanaged {
                reason: "non-interactive legacy adoption requires --yes".into(),
            });
        }
    };
    Ok(CoreInstallationOwnership::LegacyAdoption(
        LegacyAdoptionCandidate {
            install_root,
            core_binary_path: current_exe,
            agentctl_binary_path: agentctl,
            evidence,
            authorization,
        },
    ))
}

pub fn write_official_installation_receipt(
    input: &OfficialReceiptInput,
) -> anyhow::Result<CoreInstallationReceipt> {
    let current_exe = canonical_regular_file(&input.current_exe, "current_exe")?;
    let agentctl = canonical_regular_file(&input.agentctl_binary, "paired agentctl")?;
    let install_root = current_exe
        .parent()
        .context("current_exe has no installation directory")?
        .to_path_buf();
    ensure!(
        agentctl.parent() == Some(install_root.as_path()),
        "paired agentctl is not a sibling of current_exe"
    );
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    ensure!(
        recognized_manager_root(&install_root, &input.home, cargo_home.as_deref())?.is_none(),
        "refusing official ownership inside a package-manager or system root"
    );
    let metadata_text = read_bounded_utf8(&input.release_metadata_path, "release metadata")?;
    let metadata: CoreReleaseMetadata =
        serde_json::from_str(&metadata_text).context("release metadata is not schema-v1 JSON")?;
    ensure!(
        metadata.schema_version == 1,
        "unsupported release metadata schema"
    );
    ensure!(
        metadata.package == "ldgr-core" && metadata.binary == "ldgr",
        "release metadata identifies a different package"
    );
    ensure!(
        metadata.platform == current_platform(),
        "release metadata platform does not match this executable"
    );
    ensure!(
        metadata.launcher_compatibility_schema == LAUNCHER_COMPATIBILITY_SCHEMA,
        "release metadata compatibility schema is unsupported"
    );
    ensure!(
        metadata.version == env!("CARGO_PKG_VERSION"),
        "release metadata Core version does not match installed binary"
    );
    let evidence = ProcessCompatibilityProbe.probe(&current_exe, &agentctl)?;
    ensure!(
        evidence.core_version == metadata.version
            && evidence.agentctl_version == metadata.agentctl_version
            && evidence.compatibility_schema == metadata.launcher_compatibility_schema,
        "paired validation does not match release metadata"
    );
    let receipt = CoreInstallationReceipt {
        schema_version: CORE_INSTALLATION_RECEIPT_SCHEMA_VERSION,
        installer_kind: CoreInstallerKind::Official,
        managed_by: None,
        core_version: evidence.core_version,
        agentctl_version: evidence.agentctl_version,
        archive: Some(CoreArchiveProvenance {
            url: input.archive_url.clone(),
            sha256: input.archive_sha256.clone(),
            signing_key_id: input.signing_key_id.clone(),
            platform: metadata.platform,
            release_commit: metadata.commit,
        }),
        install_root,
        core_binary_path: current_exe,
        agentctl_binary_path: agentctl,
        core_binary_sha256: digest_file(&input.current_exe)?,
        agentctl_binary_sha256: digest_file(&input.agentctl_binary)?,
        compatibility_schema: metadata.launcher_compatibility_schema,
        previous_successful_plan_id: input.previous_successful_plan_id.clone(),
        installed_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_secs(),
    };
    validate_receipt(&receipt)?;
    atomic_write_receipt(&core_installation_receipt_path(&input.home), &receipt)?;
    Ok(receipt)
}

pub fn read_core_installation_receipt(path: &Path) -> anyhow::Result<CoreInstallationReceipt> {
    reject_link_or_reparse(path, "Core installation receipt")?;
    let text = read_bounded_utf8(path, "Core installation receipt")?;
    serde_json::from_str(&text).context("Core installation receipt is not schema-v1 JSON")
}

pub fn validate_receipt(receipt: &CoreInstallationReceipt) -> anyhow::Result<()> {
    ensure!(
        receipt.schema_version == CORE_INSTALLATION_RECEIPT_SCHEMA_VERSION,
        "unsupported Core installation receipt schema {}",
        receipt.schema_version
    );
    Version::parse(&receipt.core_version).context("receipt Core version is not semantic")?;
    Version::parse(&receipt.agentctl_version)
        .context("receipt agentctl version is not semantic")?;
    ensure!(
        receipt.install_root.is_absolute(),
        "receipt install_root must be absolute"
    );
    ensure!(
        receipt.core_binary_path.is_absolute() && receipt.agentctl_binary_path.is_absolute(),
        "receipt binary paths must be absolute"
    );
    ensure!(
        receipt.core_binary_path.parent() == Some(receipt.install_root.as_path()),
        "receipt Core binary must be a direct child of install_root"
    );
    ensure!(
        receipt.agentctl_binary_path.parent() == Some(receipt.install_root.as_path()),
        "receipt agentctl binary must be a direct child of install_root"
    );
    ensure!(
        valid_sha256(&receipt.core_binary_sha256) && valid_sha256(&receipt.agentctl_binary_sha256),
        "receipt binary digests must be canonical SHA-256"
    );
    ensure!(
        receipt.compatibility_schema == LAUNCHER_COMPATIBILITY_SCHEMA,
        "receipt compatibility schema is unsupported"
    );
    if let Some(plan_id) = &receipt.previous_successful_plan_id {
        ensure!(
            valid_sha256(plan_id),
            "previous plan id must be a SHA-256 digest"
        );
    }
    match receipt.installer_kind {
        CoreInstallerKind::Official => {
            ensure!(
                receipt.managed_by.is_none(),
                "official receipt cannot declare managed_by"
            );
            let archive = receipt
                .archive
                .as_ref()
                .context("official receipt requires archive provenance")?;
            ensure!(
                archive.url.starts_with("https://") || archive.url.starts_with("file://"),
                "official archive URL must use HTTPS or file"
            );
            ensure!(
                valid_sha256(&archive.sha256),
                "receipt archive SHA-256 is invalid"
            );
            ensure!(
                !archive.signing_key_id.trim().is_empty()
                    && !archive.release_commit.trim().is_empty(),
                "archive signing key and release commit are required"
            );
            ensure!(
                supported_platform(&archive.platform),
                "receipt archive platform is unsupported"
            );
        }
        CoreInstallerKind::PackageManager => ensure!(
            receipt.managed_by.is_some(),
            "package-manager receipt must declare managed_by"
        ),
        CoreInstallerKind::LegacyAdopted => ensure!(
            receipt.managed_by.is_none(),
            "legacy-adopted receipt cannot declare managed_by"
        ),
    }
    Ok(())
}

fn ensure_receipt_matches_current_exe(
    receipt: &CoreInstallationReceipt,
    current_exe: &Path,
) -> anyhow::Result<()> {
    let root = canonical_directory(&receipt.install_root, "receipt install_root")?;
    let core = canonical_regular_file(&receipt.core_binary_path, "receipt Core")?;
    let agentctl = canonical_regular_file(&receipt.agentctl_binary_path, "receipt agentctl")?;
    ensure!(
        core == current_exe,
        "receipt Core binary does not match current_exe"
    );
    ensure!(
        core.parent() == Some(root.as_path()) && agentctl.parent() == Some(root.as_path()),
        "receipt binary path escapes canonical install_root"
    );
    Ok(())
}

fn ensure_receipt_binary_digests(receipt: &CoreInstallationReceipt) -> anyhow::Result<()> {
    ensure!(
        digest_file(&receipt.core_binary_path)? == receipt.core_binary_sha256,
        "receipt-owned Core binary was modified"
    );
    ensure!(
        digest_file(&receipt.agentctl_binary_path)? == receipt.agentctl_binary_sha256,
        "receipt-owned agentctl binary was modified"
    );
    Ok(())
}

fn recognized_manager_root(
    install_root: &Path,
    home: &Path,
    cargo_home: Option<&Path>,
) -> anyhow::Result<Option<CorePackageManager>> {
    let install_root = normalize_existing_or_absolute(install_root)?;
    let home = normalize_existing_or_absolute(home)?;
    let cargo_root = cargo_home
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"))
        .join("bin");
    if path_eq(&install_root, &normalize_existing_or_absolute(&cargo_root)?) {
        return Ok(Some(CorePackageManager::Cargo));
    }
    #[cfg(windows)]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "SystemRoot"] {
            if let Some(root) = std::env::var_os(variable).map(PathBuf::from) {
                if path_within(&install_root, &normalize_existing_or_absolute(&root)?) {
                    return Ok(Some(CorePackageManager::System));
                }
            }
        }
        if path_has_components(&install_root, &["Microsoft", "WinGet", "Packages"]) {
            return Ok(Some(CorePackageManager::Winget));
        }
        if path_has_components(&install_root, &["scoop", "apps"]) {
            return Ok(Some(CorePackageManager::Scoop));
        }
    }
    #[cfg(unix)]
    {
        for root in ["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/usr/local/bin"] {
            if path_within(&install_root, Path::new(root)) {
                return Ok(Some(CorePackageManager::System));
            }
        }
        if path_within(&install_root, Path::new("/opt/homebrew"))
            || path_has_components(&install_root, &["Homebrew", "Cellar"])
        {
            return Ok(Some(CorePackageManager::Homebrew));
        }
        if path_within(&install_root, Path::new("/nix/store")) {
            return Ok(Some(CorePackageManager::Nix));
        }
        if path_within(&install_root, Path::new("/snap")) {
            return Ok(Some(CorePackageManager::Snap));
        }
    }
    Ok(None)
}

fn path_has_components(path: &Path, needle: &[&str]) -> bool {
    let components = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let needle = needle
        .iter()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>();
    components
        .windows(needle.len())
        .any(|window| window == needle)
}

fn directory_user_owned_and_writable(path: &Path) -> anyhow::Result<bool> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().readonly() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return Ok(metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o200 != 0);
    }
    #[cfg(not(unix))]
    Ok(true)
}

fn current_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        _ => "unsupported",
    }
}

fn supported_platform(value: &str) -> bool {
    matches!(
        value,
        "windows-x86_64" | "linux-x86_64" | "linux-aarch64" | "macos-x86_64" | "macos-aarch64"
    )
}

fn agentctl_binary_name(platform: &str) -> &'static str {
    if platform.starts_with("windows-") {
        "agentctl.exe"
    } else {
        "agentctl"
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest_file(path: &Path) -> anyhow::Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_bounded_utf8(path: &Path, label: &str) -> anyhow::Result<String> {
    let metadata = fs::metadata(path).with_context(|| format!("failed to inspect {label}"))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_RECEIPT_BYTES,
        "{label} must be a bounded regular file"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECEIPT_BYTES,
        "{label} exceeds 64 KiB"
    );
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

fn canonical_regular_file(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
    reject_link_or_reparse(path, label)?;
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(metadata.is_file(), "{label} must be a regular file");
    fs::canonicalize(path).with_context(|| format!("failed to canonicalize {label}"))
}

fn canonical_directory(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
    reject_link_or_reparse(path, label)?;
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(metadata.is_dir(), "{label} must be a directory");
    fs::canonicalize(path).with_context(|| format!("failed to canonicalize {label}"))
}

fn reject_link_or_reparse(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "{label} must not be a symbolic link"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "{label} must not be a reparse point"
        );
    }
    Ok(())
}

fn normalize_existing_or_absolute(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .with_context(|| format!("failed to canonicalize {}", path.display()));
    }
    ensure!(
        path.is_absolute(),
        "ownership boundary must be absolute: {}",
        path.display()
    );
    Ok(path.to_path_buf())
}

#[cfg(windows)]
fn path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn path_eq(left: &Path, right: &Path) -> bool {
    left == right
}

fn path_within(path: &Path, root: &Path) -> bool {
    path_eq(path, root) || path.strip_prefix(root).is_ok()
}

fn atomic_write_receipt(path: &Path, receipt: &CoreInstallationReceipt) -> anyhow::Result<()> {
    let parent = path.parent().context("receipt path has no parent")?;
    fs::create_dir_all(parent)?;
    reject_existing_parent_links(parent)?;
    if path.exists() {
        reject_link_or_reparse(path, "existing Core installation receipt")?;
    }
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() as u64 <= MAX_RECEIPT_BYTES,
        "receipt exceeds 64 KiB"
    );
    let temporary = parent.join(format!(
        ".core-installation-receipt.tmp-{}",
        std::process::id()
    ));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        atomic_replace(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn reject_existing_parent_links(path: &Path) -> anyhow::Result<()> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            reject_link_or_reparse(candidate, "receipt parent")?;
        }
        current = candidate.parent();
    }
    Ok(())
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::rename(source, destination)
        .context("failed to atomically replace Core installation receipt")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedProbe(CompatibilityEvidence);
    impl CompatibilityProbe for FixedProbe {
        fn probe(&self, _core: &Path, _agentctl: &Path) -> anyhow::Result<CompatibilityEvidence> {
            Ok(self.0.clone())
        }
    }

    fn evidence() -> CompatibilityEvidence {
        CompatibilityEvidence {
            core_version: env!("CARGO_PKG_VERSION").into(),
            agentctl_version: "0.1.2".into(),
            compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA.into(),
        }
    }

    fn core_name() -> &'static str {
        if cfg!(windows) {
            "ldgr.exe"
        } else {
            "ldgr"
        }
    }

    fn isolated_pair() -> anyhow::Result<(tempfile::TempDir, PathBuf, PathBuf, PathBuf)> {
        let home = tempfile::tempdir()?;
        let bin = home.path().join("tools/bin");
        fs::create_dir_all(&bin)?;
        let core = bin.join(core_name());
        let agentctl = bin.join(agentctl_binary_name(current_platform()));
        fs::write(&core, b"core")?;
        fs::write(&agentctl, b"agentctl")?;
        Ok((home, bin, core, agentctl))
    }

    fn context(home: &Path, core: &Path, consent: LegacyAdoptionConsent) -> OwnershipContext {
        OwnershipContext {
            home: home.to_path_buf(),
            current_exe: core.to_path_buf(),
            receipt_path: core_installation_receipt_path(home),
            cargo_home: None,
            platform: current_platform().into(),
            adoption_consent: consent,
        }
    }

    #[test]
    fn safe_legacy_adoption_requires_confirmation_or_yes() -> anyhow::Result<()> {
        let (home, bin, core, _) = isolated_pair()?;
        let pending = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::InteractivePending,
        );
        let CoreInstallationOwnership::LegacyAdoption(candidate) =
            resolve_core_installation_ownership(&pending, &FixedProbe(evidence()))?
        else {
            panic!("expected legacy adoption candidate");
        };
        assert_eq!(candidate.install_root, bin.canonicalize()?);
        assert_eq!(
            candidate.authorization,
            LegacyAdoptionAuthorization::ConfirmationRequired
        );

        let denied = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::NonInteractive { yes: false },
        );
        assert!(matches!(
            resolve_core_installation_ownership(&denied, &FixedProbe(evidence()))?,
            CoreInstallationOwnership::Unmanaged { reason }
                if reason.contains("requires --yes")
        ));
        let approved = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::NonInteractive { yes: true },
        );
        assert!(matches!(
            resolve_core_installation_ownership(&approved, &FixedProbe(evidence()))?,
            CoreInstallationOwnership::LegacyAdoption(value)
                if value.authorization == LegacyAdoptionAuthorization::Approved
        ));
        Ok(())
    }

    #[test]
    fn current_exe_outside_isolated_home_is_unmanaged() -> anyhow::Result<()> {
        let (_outside, _, core, _) = isolated_pair()?;
        let home = tempfile::tempdir()?;
        let context = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::InteractivePending,
        );
        assert!(matches!(
            resolve_core_installation_ownership(&context, &FixedProbe(evidence()))?,
            CoreInstallationOwnership::Unmanaged { reason } if reason.contains("outside")
        ));
        Ok(())
    }

    #[test]
    fn isolated_cargo_bin_is_always_check_only() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let cargo_home = home.path().join("cargo-home");
        let bin = cargo_home.join("bin");
        fs::create_dir_all(&bin)?;
        let core = bin.join(core_name());
        fs::write(&core, b"core")?;
        let mut context = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::NonInteractive { yes: true },
        );
        context.cargo_home = Some(cargo_home);
        assert!(matches!(
            resolve_core_installation_ownership(&context, &FixedProbe(evidence()))?,
            CoreInstallationOwnership::PackageManagerCheckOnly {
                managed_by: CorePackageManager::Cargo,
                ..
            }
        ));
        Ok(())
    }

    fn official_receipt(
        bin: &Path,
        core: &Path,
        agentctl: &Path,
    ) -> anyhow::Result<CoreInstallationReceipt> {
        Ok(CoreInstallationReceipt {
            schema_version: 1,
            installer_kind: CoreInstallerKind::Official,
            managed_by: None,
            core_version: evidence().core_version,
            agentctl_version: evidence().agentctl_version,
            archive: Some(CoreArchiveProvenance {
                url: "https://example.test/core.tgz".into(),
                sha256: "a".repeat(64),
                signing_key_id: "test-key".into(),
                platform: current_platform().into(),
                release_commit: "commit".into(),
            }),
            install_root: bin.canonicalize()?,
            core_binary_path: core.canonicalize()?,
            agentctl_binary_path: agentctl.canonicalize()?,
            core_binary_sha256: digest_file(core)?,
            agentctl_binary_sha256: digest_file(agentctl)?,
            compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA.into(),
            previous_successful_plan_id: None,
            installed_at_unix_seconds: 1,
        })
    }

    #[test]
    fn official_receipt_is_atomic_and_binds_current_exe_and_digests() -> anyhow::Result<()> {
        let (home, bin, core, agentctl) = isolated_pair()?;
        let receipt_path = core_installation_receipt_path(home.path());
        fs::create_dir_all(receipt_path.parent().unwrap())?;
        let receipt = official_receipt(&bin, &core, &agentctl)?;
        atomic_write_receipt(&receipt_path, &receipt)?;
        let context = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::InteractivePending,
        );
        assert!(matches!(
            resolve_core_installation_ownership(&context, &FixedProbe(evidence()))?,
            CoreInstallationOwnership::OfficialInstall(_)
        ));
        assert!(
            fs::read_dir(receipt_path.parent().unwrap())?.all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-"))
        );
        fs::write(&agentctl, b"modified")?;
        assert!(
            resolve_core_installation_ownership(&context, &FixedProbe(evidence()))
                .unwrap_err()
                .to_string()
                .contains("modified")
        );
        Ok(())
    }

    #[test]
    fn receipt_for_a_different_current_exe_is_rejected() -> anyhow::Result<()> {
        let (home, bin, core, agentctl) = isolated_pair()?;
        let other = bin.join(if cfg!(windows) { "other.exe" } else { "other" });
        fs::write(&other, b"other")?;
        let receipt_path = core_installation_receipt_path(home.path());
        fs::create_dir_all(receipt_path.parent().unwrap())?;
        atomic_write_receipt(&receipt_path, &official_receipt(&bin, &other, &agentctl)?)?;
        let context = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::InteractivePending,
        );
        assert!(
            resolve_core_installation_ownership(&context, &FixedProbe(evidence()))
                .unwrap_err()
                .to_string()
                .contains("current_exe")
        );
        Ok(())
    }

    #[test]
    fn official_installers_record_only_after_pair_validation() {
        for script in [
            include_str!("../../scripts/install.ps1"),
            include_str!("../../scripts/install.sh"),
        ] {
            let version = script.rfind("--version").expect("version validation");
            let compatibility = script
                .rfind("compatibility --agentctl-version")
                .expect("compatibility validation");
            let record = script
                .rfind("__record-core-installation")
                .expect("receipt writer");
            assert!(version < compatibility && compatibility < record);
            assert!(script.contains("--release-metadata"));
            assert!(script.contains("--archive-sha256"));
            assert!(script.contains("--signing-key-id"));
        }
    }

    #[test]
    fn declared_package_manager_receipt_is_check_only() -> anyhow::Result<()> {
        let (home, bin, core, agentctl) = isolated_pair()?;
        let mut receipt = official_receipt(&bin, &core, &agentctl)?;
        receipt.installer_kind = CoreInstallerKind::PackageManager;
        receipt.managed_by = Some(CorePackageManager::Cargo);
        receipt.archive = None;
        let receipt_path = core_installation_receipt_path(home.path());
        fs::create_dir_all(receipt_path.parent().unwrap())?;
        atomic_write_receipt(&receipt_path, &receipt)?;
        let context = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::InteractivePending,
        );
        assert!(matches!(
            resolve_core_installation_ownership(&context, &FixedProbe(evidence()))?,
            CoreInstallationOwnership::PackageManagerCheckOnly {
                managed_by: CorePackageManager::Cargo,
                ..
            }
        ));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn windows_oriented_package_roots_are_check_only() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let winget = home
            .path()
            .join("AppData/Local/Microsoft/WinGet/Packages/ldgr");
        let scoop = home.path().join("scoop/apps/ldgr/current");
        assert_eq!(
            recognized_manager_root(&winget, home.path(), None)?,
            Some(CorePackageManager::Winget)
        );
        assert_eq!(
            recognized_manager_root(&scoop, home.path(), None)?,
            Some(CorePackageManager::Scoop)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unix_system_roots_and_isolated_symlinks_are_rejected() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;
        assert_eq!(
            recognized_manager_root(
                Path::new("/usr/local/bin"),
                Path::new("/tmp/isolated-home"),
                None
            )?,
            Some(CorePackageManager::System)
        );
        assert_eq!(
            recognized_manager_root(
                Path::new("/opt/homebrew/bin"),
                Path::new("/tmp/isolated-home"),
                None
            )?,
            Some(CorePackageManager::Homebrew)
        );
        let (home, bin, core, agentctl) = isolated_pair()?;
        let link = bin.join("linked-agentctl");
        symlink(&agentctl, &link)?;
        let receipt = official_receipt(&bin, &core, &link)?;
        let receipt_path = core_installation_receipt_path(home.path());
        fs::create_dir_all(receipt_path.parent().unwrap())?;
        atomic_write_receipt(&receipt_path, &receipt)?;
        let context = context(
            home.path(),
            &core,
            LegacyAdoptionConsent::InteractivePending,
        );
        assert!(resolve_core_installation_ownership(&context, &FixedProbe(evidence())).is_err());
        Ok(())
    }
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    ensure!(
        ok != 0,
        "failed to atomically replace Core installation receipt: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
