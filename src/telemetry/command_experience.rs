//! Privacy projection and local consolidation for command-experience/v1.
//!
//! Raw command text is accepted only at the local classification boundary and
//! is never written to the telemetry projection. Durable projection state is a
//! finite numerical construction plus bucketed counters for the current release
//! window. No project, installation, session, run, timestamp, or content value
//! is present in a queued or transmitted payload.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::buffer::{queue_committed_terminal_sequence, QueuedTerminalSequence};
use super::transition::{
    NormalizedTerminal, NumericalProtocol, StateCode, CANCELLED, COMPLETED_INCONCLUSIVE,
    COMPLETED_NEGATIVE, COMPLETED_POSITIVE, OPERATIONAL_FAILURE, PENDING,
};

pub const COMMAND_EXPERIENCE_STORE_FILE: &str = "telemetry-constructions-v1.json";
pub const COMMAND_EXPERIENCE_SCHEMA_VERSION: u32 = 1;
pub const RELEASE_SUPPORT_THRESHOLD: u32 = 5;
pub const RELEASE_WINDOW_DAYS: u64 = 7;
pub const MAX_RELEASES_PER_WINDOW: u32 = 20;
pub const SEPARATOR: StateCode = 9;

const ACTION_MIN: StateCode = 10;
const ACTION_MAX: StateCode = 19;
const OBJECT_MIN: StateCode = 20;
const OBJECT_MAX: StateCode = 29;
const CONDITION_MIN: StateCode = 30;
const CONDITION_MAX: StateCode = 34;
const MACRO_MIN: StateCode = 40;
const MACRO_MAX: StateCode = 51;
const TOOL_MIN: StateCode = 60;
const TOOL_MAX: StateCode = 68;
const ARTIFACT_ROLE_MIN: StateCode = 70;
const ARTIFACT_ROLE_MAX: StateCode = 75;
const VALIDATION_MIN: StateCode = 80;
const VALIDATION_MAX: StateCode = 83;
const ARTIFACT_COUNT_MIN: StateCode = 90;
const ARTIFACT_COUNT_MAX: StateCode = 94;
const SUPPORT_MIN: StateCode = 100;
const SUPPORT_MAX: StateCode = 103;
const SUCCESS_RATE_MIN: StateCode = 110;
const SUCCESS_RATE_MAX: StateCode = 114;
const RESIDUAL_RATIO_MIN: StateCode = 120;
const RESIDUAL_RATIO_MAX: StateCode = 123;
const CONTEXT_REDUCTION_MIN: StateCode = 130;
const CONTEXT_REDUCTION_MAX: StateCode = 134;
const RETRIEVAL_BASIN_MIN: StateCode = 140;
const RETRIEVAL_BASIN_MAX: StateCode = 144;

const COMMAND_EXPERIENCE_STATES: &[StateCode] = &[
    PENDING,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
    SEPARATOR,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
    20,
    21,
    22,
    23,
    24,
    25,
    26,
    27,
    28,
    29,
    30,
    31,
    32,
    33,
    34,
    40,
    41,
    42,
    43,
    44,
    45,
    46,
    47,
    48,
    49,
    50,
    51,
    60,
    61,
    62,
    63,
    64,
    65,
    66,
    67,
    68,
    70,
    71,
    72,
    73,
    74,
    75,
    80,
    81,
    82,
    83,
    90,
    91,
    92,
    93,
    94,
    100,
    101,
    102,
    103,
    110,
    111,
    112,
    113,
    114,
    120,
    121,
    122,
    123,
    130,
    131,
    132,
    133,
    134,
    140,
    141,
    142,
    143,
    144,
];

pub const COMMAND_EXPERIENCE_V1: NumericalProtocol = NumericalProtocol::command_experience_v1(
    "/sequences/command-experience/v1",
    COMMAND_EXPERIENCE_STATES,
    48,
);

macro_rules! coded_enum {
    ($name:ident { $($variant:ident = $code:literal => $label:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        #[repr(u16)]
        pub enum $name { $($variant = $code),+ }

        impl $name {
            pub const fn code(self) -> StateCode { self as StateCode }
            pub const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
}

coded_enum!(ActionClass {
    Fix = 10 => "FIX", Build = 11 => "BUILD", Test = 12 => "TEST",
    Research = 13 => "RESEARCH", Migrate = 14 => "MIGRATE", Review = 15 => "REVIEW",
    Document = 16 => "DOCUMENT", Operate = 17 => "OPERATE", Secure = 18 => "SECURE",
    Other = 19 => "OTHER"
});
coded_enum!(ObjectClass {
    Code = 20 => "CODE", Config = 21 => "CONFIG", Schema = 22 => "SCHEMA",
    Data = 23 => "DATA", Docs = 24 => "DOCS", Tests = 25 => "TESTS",
    Dependency = 26 => "DEPENDENCY", Infrastructure = 27 => "INFRASTRUCTURE",
    Model = 28 => "MODEL", Other = 29 => "OTHER"
});
coded_enum!(ConditionFlag {
    Production = 30 => "PROD", Urgent = 31 => "URGENT", Security = 32 => "SECURITY",
    Migration = 33 => "MIGRATION", Recovery = 34 => "RECOVERY"
});
coded_enum!(MacroEvent {
    Inspect = 40 => "INSPECT", Diagnose = 41 => "DIAGNOSE", Plan = 42 => "PLAN",
    Edit = 43 => "EDIT", Execute = 44 => "EXECUTE", Validate = 45 => "VALIDATE",
    Deploy = 46 => "DEPLOY", Verify = 47 => "VERIFY", Research = 48 => "RESEARCH",
    Review = 49 => "REVIEW", Migrate = 50 => "MIGRATE", Recover = 51 => "RECOVER"
});
coded_enum!(ToolClass {
    Vcs = 60 => "VCS", TestRunner = 61 => "TEST_RUNNER",
    PackageManager = 62 => "PACKAGE_MANAGER", BuildSystem = 63 => "BUILD_SYSTEM",
    NetworkClient = 64 => "NETWORK_CLIENT", Database = 65 => "DATABASE",
    Container = 66 => "CONTAINER", Orchestrator = 67 => "ORCHESTRATOR",
    RemoteShell = 68 => "REMOTE_SHELL"
});
coded_enum!(ArtifactRole {
    Source = 70 => "SOURCE", Test = 71 => "TEST", Config = 72 => "CONFIG",
    Data = 73 => "DATA", Report = 74 => "REPORT", Other = 75 => "OTHER"
});
coded_enum!(ValidationBucket {
    None = 80 => "NONE", SomePass = 81 => "SOME_PASS", AllPass = 82 => "ALL_PASS",
    AnyFail = 83 => "ANY_FAIL"
});
coded_enum!(ArtifactCountBucket {
    Zero = 90 => "0", One = 91 => "1", TwoThree = 92 => "2_3",
    FourSeven = 93 => "4_7", EightPlus = 94 => "8_PLUS"
});
coded_enum!(ContextReductionBucket {
    NotObserved = 130 => "NOT_OBSERVED", None = 131 => "NONE", Low = 132 => "LOW",
    Medium = 133 => "MEDIUM", High = 134 => "HIGH"
});
coded_enum!(RetrievalBasinBucket {
    NotObserved = 140 => "NOT_OBSERVED", None = 141 => "NONE", Narrow = 142 => "NARROW",
    Moderate = 143 => "MODERATE", Broad = 144 => "BROAD"
});

pub const fn is_action_code(code: StateCode) -> bool {
    code >= ACTION_MIN && code <= ACTION_MAX
}
pub const fn is_object_code(code: StateCode) -> bool {
    code >= OBJECT_MIN && code <= OBJECT_MAX
}
pub const fn is_condition_code(code: StateCode) -> bool {
    code >= CONDITION_MIN && code <= CONDITION_MAX
}
pub const fn is_macro_event_code(code: StateCode) -> bool {
    code >= MACRO_MIN && code <= MACRO_MAX
}
pub const fn is_tool_class_code(code: StateCode) -> bool {
    code >= TOOL_MIN && code <= TOOL_MAX
}
pub const fn is_artifact_role_code(code: StateCode) -> bool {
    code >= ARTIFACT_ROLE_MIN && code <= ARTIFACT_ROLE_MAX
}
pub const fn is_validation_code(code: StateCode) -> bool {
    code >= VALIDATION_MIN && code <= VALIDATION_MAX
}
pub const fn is_artifact_count_code(code: StateCode) -> bool {
    code >= ARTIFACT_COUNT_MIN && code <= ARTIFACT_COUNT_MAX
}
pub const fn is_support_code(code: StateCode) -> bool {
    code >= SUPPORT_MIN && code <= SUPPORT_MAX
}
pub const fn is_success_rate_code(code: StateCode) -> bool {
    code >= SUCCESS_RATE_MIN && code <= SUCCESS_RATE_MAX
}
pub const fn is_residual_ratio_code(code: StateCode) -> bool {
    code >= RESIDUAL_RATIO_MIN && code <= RESIDUAL_RATIO_MAX
}
pub const fn is_context_reduction_code(code: StateCode) -> bool {
    code >= CONTEXT_REDUCTION_MIN && code <= CONTEXT_REDUCTION_MAX
}
pub const fn is_retrieval_basin_code(code: StateCode) -> bool {
    code >= RETRIEVAL_BASIN_MIN && code <= RETRIEVAL_BASIN_MAX
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateEpisodeProjection {
    pub action: ActionClass,
    pub object: ObjectClass,
    pub conditions: Vec<ConditionFlag>,
    pub macro_events: Vec<MacroEvent>,
    pub tool_classes: Vec<ToolClass>,
    pub artifact_roles: Vec<ArtifactRole>,
    pub validation: ValidationBucket,
    pub artifact_count: ArtifactCountBucket,
    pub context_reduction: ContextReductionBucket,
    pub retrieval_basin: RetrievalBasinBucket,
    pub terminal: NormalizedTerminal,
}

impl PrivateEpisodeProjection {
    pub fn from_local_inputs(
        raw_input: &str,
        validation: ValidationBucket,
        artifact_roles: impl IntoIterator<Item = ArtifactRole>,
        artifact_count: usize,
        terminal: NormalizedTerminal,
    ) -> Self {
        let normalized = tokenize(raw_input);
        let artifact_roles = artifact_roles.into_iter().collect::<BTreeSet<_>>();
        Self {
            action: classify_action(&normalized),
            object: classify_object(&normalized),
            conditions: classify_conditions(&normalized),
            macro_events: classify_macros(&normalized, validation),
            tool_classes: classify_tools(&normalized),
            artifact_roles: artifact_roles.into_iter().collect(),
            validation,
            artifact_count: artifact_count_bucket(artifact_count),
            context_reduction: ContextReductionBucket::NotObserved,
            retrieval_basin: RetrievalBasinBucket::NotObserved,
            terminal,
        }
    }

    fn construction_shape(&self) -> Vec<StateCode> {
        let mut states = vec![PENDING, self.action.code(), self.object.code()];
        states.extend(self.conditions.iter().map(|value| value.code()));
        states.push(SEPARATOR);
        states.extend(self.macro_events.iter().map(|value| value.code()));
        states.extend(self.tool_classes.iter().map(|value| value.code()));
        states.extend(self.artifact_roles.iter().map(|value| value.code()));
        states.push(SEPARATOR);
        states.push(self.validation.code());
        states.push(self.artifact_count.code());
        states
    }
}

fn tokenize(raw: &str) -> Vec<String> {
    raw.split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn contains_any(tokens: &[String], candidates: &[&str]) -> bool {
    tokens
        .iter()
        .any(|token| candidates.contains(&token.as_str()))
}

fn classify_action(tokens: &[String]) -> ActionClass {
    let classes = [
        (ActionClass::Fix, &["fix", "repair", "debug"] as &[_]),
        (ActionClass::Build, &["build", "create", "implement", "add"]),
        (ActionClass::Test, &["test", "validate", "verify"]),
        (
            ActionClass::Research,
            &["research", "investigate", "inspect", "analyze"],
        ),
        (ActionClass::Migrate, &["migrate", "migration", "upgrade"]),
        (ActionClass::Review, &["review", "audit"]),
        (ActionClass::Document, &["document", "docs", "write"]),
        (
            ActionClass::Operate,
            &["deploy", "release", "publish", "operate"],
        ),
        (ActionClass::Secure, &["secure", "harden", "security"]),
    ];
    classes
        .into_iter()
        .find(|(_, words)| contains_any(tokens, words))
        .map(|value| value.0)
        .unwrap_or(ActionClass::Other)
}

fn classify_object(tokens: &[String]) -> ObjectClass {
    let classes = [
        (
            ObjectClass::Schema,
            &["schema", "database", "sql", "migration"] as &[_],
        ),
        (
            ObjectClass::Config,
            &["config", "configuration", "setting", "settings"],
        ),
        (
            ObjectClass::Tests,
            &["test", "tests", "fixture", "fixtures"],
        ),
        (
            ObjectClass::Docs,
            &["doc", "docs", "documentation", "readme"],
        ),
        (ObjectClass::Data, &["data", "dataset", "csv", "json"]),
        (
            ObjectClass::Dependency,
            &["dependency", "dependencies", "package"],
        ),
        (
            ObjectClass::Infrastructure,
            &[
                "infra",
                "infrastructure",
                "deploy",
                "docker",
                "site",
                "server",
            ],
        ),
        (ObjectClass::Model, &["model", "inference", "prompt"]),
        (
            ObjectClass::Code,
            &["code", "source", "rust", "typescript", "python", "api"],
        ),
    ];
    classes
        .into_iter()
        .find(|(_, words)| contains_any(tokens, words))
        .map(|value| value.0)
        .unwrap_or(ObjectClass::Other)
}

fn classify_conditions(tokens: &[String]) -> Vec<ConditionFlag> {
    let classes = [
        (
            ConditionFlag::Production,
            &["prod", "production", "live"] as &[_],
        ),
        (ConditionFlag::Urgent, &["urgent", "critical", "hotfix"]),
        (
            ConditionFlag::Security,
            &["security", "secure", "privacy", "vulnerability"],
        ),
        (
            ConditionFlag::Migration,
            &["migrate", "migration", "upgrade"],
        ),
        (
            ConditionFlag::Recovery,
            &["recover", "recovery", "restore", "incident"],
        ),
    ];
    classes
        .into_iter()
        .filter(|(_, words)| contains_any(tokens, words))
        .map(|value| value.0)
        .collect()
}

fn classify_macros(tokens: &[String], validation: ValidationBucket) -> Vec<MacroEvent> {
    let mut events = Vec::new();
    for token in tokens {
        let event = match token.as_str() {
            "inspect" | "read" | "list" | "find" => Some(MacroEvent::Inspect),
            "diagnose" | "debug" | "fix" | "repair" => Some(MacroEvent::Diagnose),
            "plan" | "design" => Some(MacroEvent::Plan),
            "edit" | "patch" | "write" | "implement" | "add" | "create" => Some(MacroEvent::Edit),
            "run" | "execute" | "build" => Some(MacroEvent::Execute),
            "test" | "validate" => Some(MacroEvent::Validate),
            "deploy" | "publish" | "release" => Some(MacroEvent::Deploy),
            "verify" | "check" => Some(MacroEvent::Verify),
            "research" | "investigate" | "analyze" => Some(MacroEvent::Research),
            "review" | "audit" => Some(MacroEvent::Review),
            "migrate" | "migration" | "upgrade" => Some(MacroEvent::Migrate),
            "recover" | "recovery" | "restore" => Some(MacroEvent::Recover),
            _ => None,
        };
        if let Some(event) = event {
            if events.last() != Some(&event) && events.len() < 12 {
                events.push(event);
            }
        }
    }
    if events.is_empty() {
        events.push(MacroEvent::Execute);
    }
    if validation != ValidationBucket::None && !events.contains(&MacroEvent::Validate) {
        events.push(MacroEvent::Validate);
    }
    events
}

fn classify_tools(tokens: &[String]) -> Vec<ToolClass> {
    let classes = [
        (ToolClass::Vcs, &["git", "gh"] as &[_]),
        (
            ToolClass::TestRunner,
            &["pytest", "jest", "vitest", "cargo-test"],
        ),
        (
            ToolClass::PackageManager,
            &["uv", "pip", "npm", "pnpm", "yarn", "cargo"],
        ),
        (
            ToolClass::BuildSystem,
            &["make", "cmake", "gradle", "maven"],
        ),
        (ToolClass::NetworkClient, &["curl", "wget", "http"]),
        (ToolClass::Database, &["sqlite", "psql", "mysql"]),
        (ToolClass::Container, &["docker", "podman"]),
        (
            ToolClass::Orchestrator,
            &["kubectl", "helm", "terraform", "ansible"],
        ),
        (ToolClass::RemoteShell, &["ssh", "scp", "rsync"]),
    ];
    classes
        .into_iter()
        .filter(|(_, words)| contains_any(tokens, words))
        .map(|value| value.0)
        .collect()
}

pub fn artifact_count_bucket(count: usize) -> ArtifactCountBucket {
    match count {
        0 => ArtifactCountBucket::Zero,
        1 => ArtifactCountBucket::One,
        2..=3 => ArtifactCountBucket::TwoThree,
        4..=7 => ArtifactCountBucket::FourSeven,
        _ => ArtifactCountBucket::EightPlus,
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalCounts {
    positive: u32,
    negative: u32,
    inconclusive: u32,
    failure: u32,
    cancelled: u32,
}

impl TerminalCounts {
    fn increment(&mut self, terminal: NormalizedTerminal) {
        match terminal {
            NormalizedTerminal::CompletedPositive => self.positive += 1,
            NormalizedTerminal::CompletedNegative => self.negative += 1,
            NormalizedTerminal::CompletedInconclusive => self.inconclusive += 1,
            NormalizedTerminal::OperationalFailure => self.failure += 1,
            NormalizedTerminal::Cancelled => self.cancelled += 1,
        }
    }
    fn total(&self) -> u32 {
        self.positive + self.negative + self.inconclusive + self.failure + self.cancelled
    }
    fn terminal_code(&self) -> StateCode {
        let ranked = [
            (self.positive, COMPLETED_POSITIVE),
            (self.negative, COMPLETED_NEGATIVE),
            (self.inconclusive, COMPLETED_INCONCLUSIVE),
            (self.failure, OPERATIONAL_FAILURE),
            (self.cancelled, CANCELLED),
        ];
        ranked
            .into_iter()
            .max_by_key(|(count, _)| *count)
            .map(|(_, code)| code)
            .unwrap_or(COMPLETED_INCONCLUSIVE)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConstructionAggregate {
    shape: Vec<StateCode>,
    terminals: TerminalCounts,
    released: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConstructionStore {
    schema_version: u32,
    release_window: u64,
    contributions_released: u32,
    constructions: BTreeMap<String, ConstructionAggregate>,
}

impl ConstructionStore {
    fn empty(release_window: u64) -> Self {
        Self {
            schema_version: COMMAND_EXPERIENCE_SCHEMA_VERSION,
            release_window,
            contributions_released: 0,
            constructions: BTreeMap::new(),
        }
    }
}

pub fn construction_store_path(ldgr_home: &Path) -> PathBuf {
    ldgr_home.join(COMMAND_EXPERIENCE_STORE_FILE)
}

pub fn record_private_episode(
    ldgr_home: &Path,
    episode: &PrivateEpisodeProjection,
) -> anyhow::Result<()> {
    if !super::anonymous_collection_is_eligible(ldgr_home) {
        return Ok(());
    }
    let window = current_release_window()?;
    let mut store = load_store(ldgr_home, window)?;
    let shape = episode.construction_shape();
    validate_shape(&shape)?;
    let key = shape
        .iter()
        .map(StateCode::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let aggregate = store
        .constructions
        .entry(key)
        .or_insert_with(|| ConstructionAggregate {
            shape,
            terminals: TerminalCounts::default(),
            released: false,
        });
    aggregate.terminals.increment(episode.terminal);
    save_store(ldgr_home, &store)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConstructionReleaseReport {
    pub eligible: usize,
    pub queued: usize,
    pub suppressed_rare: usize,
    pub suppressed_cap: usize,
}

pub fn release_eligible_constructions(
    ldgr_home: &Path,
) -> anyhow::Result<ConstructionReleaseReport> {
    let mut report = ConstructionReleaseReport::default();
    if !super::anonymous_collection_is_eligible(ldgr_home) {
        return Ok(report);
    }
    let window = current_release_window()?;
    let mut store = load_store(ldgr_home, window)?;
    for aggregate in store.constructions.values_mut() {
        if aggregate.released {
            continue;
        }
        let support = aggregate.terminals.total();
        if support < RELEASE_SUPPORT_THRESHOLD {
            report.suppressed_rare += 1;
            continue;
        }
        report.eligible += 1;
        if store.contributions_released >= MAX_RELEASES_PER_WINDOW {
            report.suppressed_cap += 1;
            continue;
        }
        let states = released_sequence(aggregate)?;
        if queue_committed_terminal_sequence(ldgr_home, &COMMAND_EXPERIENCE_V1, &states)?
            == QueuedTerminalSequence::Queued
        {
            aggregate.released = true;
            store.contributions_released += 1;
            report.queued += 1;
        }
    }
    save_store(ldgr_home, &store)?;
    Ok(report)
}

fn released_sequence(aggregate: &ConstructionAggregate) -> anyhow::Result<Vec<StateCode>> {
    let total = aggregate.terminals.total();
    ensure!(
        total >= RELEASE_SUPPORT_THRESHOLD,
        "construction support is below the release threshold"
    );
    let mut states = aggregate.shape.clone();
    states.push(match total {
        0..=3 => 100,
        4..=7 => 101,
        8..=15 => 102,
        _ => 103,
    });
    states.push(ratio_bucket(aggregate.terminals.positive, total, 110, true));
    let residual = total.saturating_sub(aggregate.terminals.positive);
    states.push(ratio_bucket(residual, total, 120, false));
    states.push(ContextReductionBucket::NotObserved.code());
    states.push(RetrievalBasinBucket::NotObserved.code());
    states.push(aggregate.terminals.terminal_code());
    validate_command_experience_sequence(&states)?;
    Ok(states)
}

fn ratio_bucket(
    numerator: u32,
    denominator: u32,
    base: StateCode,
    distinguish_all: bool,
) -> StateCode {
    if numerator == 0 {
        return base;
    }
    if distinguish_all && numerator == denominator {
        return base + 4;
    }
    let percentage = numerator.saturating_mul(100) / denominator.max(1);
    if percentage <= 25 {
        base + 1
    } else if percentage <= 74 {
        base + 2
    } else {
        base + 3
    }
}

fn current_release_window() -> anyhow::Result<u64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    Ok(seconds / (RELEASE_WINDOW_DAYS * 24 * 60 * 60))
}

fn load_store(ldgr_home: &Path, release_window: u64) -> anyhow::Result<ConstructionStore> {
    let path = construction_store_path(ldgr_home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(ConstructionStore::empty(release_window))
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read command-experience store {}", path.display())
            })
        }
    };
    let store: ConstructionStore = serde_json::from_str(&text).with_context(|| {
        format!(
            "failed to parse command-experience store {}",
            path.display()
        )
    })?;
    ensure!(
        store.schema_version == COMMAND_EXPERIENCE_SCHEMA_VERSION,
        "unsupported command-experience store schema_version {}",
        store.schema_version
    );
    if store.release_window != release_window {
        return Ok(ConstructionStore::empty(release_window));
    }
    for aggregate in store.constructions.values() {
        validate_shape(&aggregate.shape)?;
    }
    Ok(store)
}

fn save_store(ldgr_home: &Path, store: &ConstructionStore) -> anyhow::Result<()> {
    fs::create_dir_all(ldgr_home)
        .with_context(|| format!("failed to create LDGR home {}", ldgr_home.display()))?;
    let destination = construction_store_path(ldgr_home);
    let mut temporary = NamedTempFile::new_in(ldgr_home)
        .context("failed to create temporary command-experience store")?;
    serde_json::to_writer_pretty(&mut temporary, store)
        .context("failed to serialize command-experience store")?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to atomically replace {}", destination.display()))?;
    Ok(())
}

fn validate_shape(states: &[StateCode]) -> anyhow::Result<()> {
    ensure!(
        states.len() >= 8,
        "command-experience construction shape is incomplete"
    );
    ensure!(
        states.first() == Some(&PENDING),
        "command-experience construction must begin with 0"
    );
    ensure!(
        states.iter().filter(|state| **state == SEPARATOR).count() == 2,
        "command-experience construction must contain exactly two separators"
    );
    let mut candidate = states.to_vec();
    candidate.extend([101, 112, 121, 130, 140, COMPLETED_POSITIVE]);
    validate_command_experience_sequence(&candidate)
}

pub fn validate_command_experience_sequence(states: &[StateCode]) -> anyhow::Result<()> {
    ensure!(
        (14..=48).contains(&states.len()),
        "command-experience sequence length is outside the v1 grammar"
    );
    ensure!(
        states.first() == Some(&PENDING),
        "command-experience sequence must begin with 0"
    );
    ensure!(
        is_action_code(states[1]),
        "command-experience action code is missing"
    );
    ensure!(
        is_object_code(states[2]),
        "command-experience object code is missing"
    );
    let separators = states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| (*state == SEPARATOR).then_some(index))
        .collect::<Vec<_>>();
    ensure!(
        separators.len() == 2,
        "command-experience sequence must contain exactly two separators"
    );
    let first = separators[0];
    let second = separators[1];
    ensure!(
        first >= 3 && second > first + 1,
        "command-experience sections are empty or out of order"
    );
    ensure!(
        states[3..first]
            .iter()
            .all(|state| is_condition_code(*state)),
        "command-experience condition section contains a non-condition code"
    );
    ensure!(
        strictly_increasing(&states[3..first]),
        "command-experience condition flags must be unique and canonical"
    );

    let execution = &states[first + 1..second];
    let macro_end = execution
        .iter()
        .position(|state| !is_macro_event_code(*state))
        .unwrap_or(execution.len());
    ensure!(
        macro_end > 0,
        "command-experience execution section requires a macro event"
    );
    ensure!(
        execution[..macro_end]
            .windows(2)
            .all(|pair| pair[0] != pair[1]),
        "command-experience macro events cannot repeat adjacently"
    );
    let tool_end = execution[macro_end..]
        .iter()
        .position(|state| !is_tool_class_code(*state))
        .map(|offset| macro_end + offset)
        .unwrap_or(execution.len());
    ensure!(
        strictly_increasing(&execution[macro_end..tool_end]),
        "command-experience tool classes must be unique and canonical"
    );
    ensure!(
        execution[tool_end..]
            .iter()
            .all(|state| is_artifact_role_code(*state)),
        "command-experience execution section contains an undeclared category order"
    );
    ensure!(
        strictly_increasing(&execution[tool_end..]),
        "command-experience artifact roles must be unique and canonical"
    );

    let tail = &states[second + 1..];
    ensure!(
        tail.len() == 8,
        "command-experience outcome/statistics tail must contain exactly eight codes"
    );
    ensure!(
        is_validation_code(tail[0]),
        "command-experience validation bucket is missing"
    );
    ensure!(
        is_artifact_count_code(tail[1]),
        "command-experience artifact-count bucket is missing"
    );
    ensure!(
        is_support_code(tail[2]),
        "command-experience support bucket is missing"
    );
    ensure!(
        is_success_rate_code(tail[3]),
        "command-experience success-rate bucket is missing"
    );
    ensure!(
        is_residual_ratio_code(tail[4]),
        "command-experience residual-ratio bucket is missing"
    );
    ensure!(
        is_context_reduction_code(tail[5]),
        "command-experience context-reduction bucket is missing"
    );
    ensure!(
        is_retrieval_basin_code(tail[6]),
        "command-experience retrieval-basin bucket is missing"
    );
    ensure!(
        NormalizedTerminal::try_from(tail[7]).is_ok(),
        "command-experience terminal is missing"
    );
    Ok(())
}

fn strictly_increasing(states: &[StateCode]) -> bool {
    states.windows(2).all(|pair| pair[0] < pair[1])
}

pub fn decode_command_experience(states: &[StateCode]) -> anyhow::Result<String> {
    validate_command_experience_sequence(states)?;
    Ok(states
        .iter()
        .map(|code| label_for_code(*code))
        .collect::<Vec<_>>()
        .join(" -> "))
}

fn label_for_code(code: StateCode) -> &'static str {
    match code {
        0 => "START",
        3 => "COMPLETED_POSITIVE",
        4 => "COMPLETED_NEGATIVE",
        5 => "COMPLETED_INCONCLUSIVE",
        6 => "OPERATIONAL_FAILURE",
        7 => "CANCELLED",
        9 => "|",
        10 => "FIX",
        11 => "BUILD",
        12 => "TEST",
        13 => "RESEARCH",
        14 => "MIGRATE",
        15 => "REVIEW",
        16 => "DOCUMENT",
        17 => "OPERATE",
        18 => "SECURE",
        19 => "OTHER_ACTION",
        20 => "CODE",
        21 => "CONFIG",
        22 => "SCHEMA",
        23 => "DATA",
        24 => "DOCS",
        25 => "TESTS",
        26 => "DEPENDENCY",
        27 => "INFRASTRUCTURE",
        28 => "MODEL",
        29 => "OTHER_OBJECT",
        30 => "PROD",
        31 => "URGENT",
        32 => "SECURITY",
        33 => "MIGRATION",
        34 => "RECOVERY",
        40 => "INSPECT",
        41 => "DIAGNOSE",
        42 => "PLAN",
        43 => "EDIT",
        44 => "EXECUTE",
        45 => "VALIDATE",
        46 => "DEPLOY",
        47 => "VERIFY",
        48 => "RESEARCH_STEP",
        49 => "REVIEW_STEP",
        50 => "MIGRATE_STEP",
        51 => "RECOVER",
        60 => "VCS",
        61 => "TEST_RUNNER",
        62 => "PACKAGE_MANAGER",
        63 => "BUILD_SYSTEM",
        64 => "NETWORK_CLIENT",
        65 => "DATABASE",
        66 => "CONTAINER",
        67 => "ORCHESTRATOR",
        68 => "REMOTE_SHELL",
        70 => "SOURCE",
        71 => "TEST_ARTIFACT",
        72 => "CONFIG_ARTIFACT",
        73 => "DATA_ARTIFACT",
        74 => "REPORT",
        75 => "OTHER_ARTIFACT",
        80 => "VALIDATION_NONE",
        81 => "SOME_PASS",
        82 => "ALL_PASS",
        83 => "ANY_FAIL",
        90 => "ARTIFACTS_0",
        91 => "ARTIFACTS_1",
        92 => "ARTIFACTS_2_3",
        93 => "ARTIFACTS_4_7",
        94 => "ARTIFACTS_8_PLUS",
        100 => "SUPPORT_1_3",
        101 => "SUPPORT_4_7",
        102 => "SUPPORT_8_15",
        103 => "SUPPORT_16_PLUS",
        110 => "SUCCESS_NONE",
        111 => "SUCCESS_LOW",
        112 => "SUCCESS_MEDIUM",
        113 => "SUCCESS_HIGH",
        114 => "SUCCESS_ALL",
        120 => "RESIDUAL_NONE",
        121 => "RESIDUAL_LOW",
        122 => "RESIDUAL_MEDIUM",
        123 => "RESIDUAL_HIGH",
        130 => "CONTEXT_NOT_OBSERVED",
        131 => "CONTEXT_NONE",
        132 => "CONTEXT_LOW",
        133 => "CONTEXT_MEDIUM",
        134 => "CONTEXT_HIGH",
        140 => "RETRIEVAL_NOT_OBSERVED",
        141 => "RETRIEVAL_NONE",
        142 => "RETRIEVAL_NARROW",
        143 => "RETRIEVAL_MODERATE",
        144 => "RETRIEVAL_BROAD",
        _ => "UNDECLARED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{save_telemetry_consent, TelemetryConsent, TelemetryConsentDecision};

    fn enable(home: &Path) -> anyhow::Result<()> {
        save_telemetry_consent(
            home,
            &TelemetryConsent::current(TelemetryConsentDecision::Enabled),
        )?;
        Ok(())
    }

    #[test]
    fn local_classifier_emits_only_finite_codes_and_discards_raw_input() -> anyhow::Result<()> {
        let episode = PrivateEpisodeProjection::from_local_inputs(
            "fix production Rust code with git and cargo; validate report",
            ValidationBucket::AllPass,
            [ArtifactRole::Report],
            1,
            NormalizedTerminal::CompletedPositive,
        );
        assert_eq!(episode.action, ActionClass::Fix);
        assert_eq!(episode.object, ObjectClass::Code);
        assert_eq!(episode.conditions, vec![ConditionFlag::Production]);
        assert!(episode.tool_classes.contains(&ToolClass::Vcs));
        assert!(episode.tool_classes.contains(&ToolClass::PackageManager));
        let shape = episode.construction_shape();
        assert!(!String::from_utf8_lossy(&serde_json::to_vec(&shape)?).contains("production"));
        validate_shape(&shape)?;
        Ok(())
    }

    #[test]
    fn grammar_rejects_wrong_sections_duplicates_and_arbitrary_codes() {
        let valid = [
            0, 10, 20, 30, 9, 40, 45, 60, 74, 9, 82, 91, 101, 114, 120, 130, 140, 3,
        ];
        assert!(validate_command_experience_sequence(&valid).is_ok());
        for invalid in [
            vec![
                0, 10, 20, 30, 30, 9, 40, 9, 82, 91, 101, 114, 120, 130, 140, 3,
            ],
            vec![0, 10, 20, 9, 60, 40, 9, 82, 91, 101, 114, 120, 130, 140, 3],
            vec![0, 10, 20, 9, 40, 999, 9, 82, 91, 101, 114, 120, 130, 140, 3],
        ] {
            assert!(validate_command_experience_sequence(&invalid).is_err());
        }
    }

    #[test]
    fn rare_constructions_stay_local_and_release_once_at_k_five() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        let episode = PrivateEpisodeProjection::from_local_inputs(
            "fix code inspect edit validate",
            ValidationBucket::AllPass,
            [ArtifactRole::Source, ArtifactRole::Test],
            2,
            NormalizedTerminal::CompletedPositive,
        );
        for _ in 0..4 {
            record_private_episode(home.path(), &episode)?;
        }
        let rare = release_eligible_constructions(home.path())?;
        assert_eq!(rare.queued, 0);
        assert_eq!(rare.suppressed_rare, 1);
        record_private_episode(home.path(), &episode)?;
        let released = release_eligible_constructions(home.path())?;
        assert_eq!(released.queued, 1);
        let again = release_eligible_constructions(home.path())?;
        assert_eq!(again.queued, 0);
        let queued = fs::read_dir(home.path().join("telemetry-pending/command-experience/v1"))?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(queued.len(), 1);
        let payload = fs::read(queued[0].path())?;
        assert!(!String::from_utf8_lossy(&payload).contains("fix"));
        Ok(())
    }
}
