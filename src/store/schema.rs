use super::*;
use crate::database_contract::{
    DATABASE_CONTRACT_HASH, GENERATED_CORE_SCHEMA_VERSION, GENERATED_DATABASE_COMPONENTS,
};
use rusqlite::OpenFlags;

pub const CURRENT_SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct SchemaDoctorReport {
    pub database: PathBuf,
    pub readable: bool,
    pub compatible: bool,
    pub active_schema_version: Option<i64>,
    pub target_schema_version: i64,
    pub contract_hash: &'static str,
    pub pending_migrations: Vec<i64>,
    pub components: Vec<SchemaComponentState>,
    pub last_backup: Option<PathBuf>,
    pub recovery_command: Option<String>,
    pub problem: Option<String>,
}

pub fn doctor_schema(db_path: &Path) -> SchemaDoctorReport {
    let mut report = SchemaDoctorReport {
        database: db_path.to_path_buf(),
        readable: false,
        compatible: false,
        active_schema_version: None,
        target_schema_version: CURRENT_SCHEMA_VERSION,
        contract_hash: DATABASE_CONTRACT_HASH,
        pending_migrations: Vec::new(),
        components: Vec::new(),
        last_backup: latest_migration_backup(db_path),
        recovery_command: None,
        problem: None,
    };
    if let Some(backup) = &report.last_backup {
        report.recovery_command =
            Some(format!("cp '{}' '{}'", backup.display(), db_path.display()));
    }
    let connection = match Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(error) => {
            report.problem = Some(format!("failed to open database read-only: {error}"));
            return report;
        }
    };
    report.readable = true;
    match schema_version(&connection) {
        Ok(version) => {
            report.active_schema_version = Some(version);
        }
        Err(error) => {
            report.problem = Some(format!("{error:#}"));
            return report;
        }
    }
    match preflight_schema_migration(&connection) {
        Ok(migration_origin) => {
            report.compatible = true;
            if migration_origin.is_some() {
                report.pending_migrations = vec![CURRENT_SCHEMA_VERSION];
            } else if report.active_schema_version == Some(CURRENT_SCHEMA_VERSION) {
                match list_schema_components(&connection) {
                    Ok(components) => report.components = components,
                    Err(error) => {
                        report.compatible = false;
                        report.problem = Some(format!("{error:#}"));
                    }
                }
            }
        }
        Err(error) => report.problem = Some(format!("{error:#}")),
    }
    report
}

fn latest_migration_backup(db_path: &Path) -> Option<PathBuf> {
    let parent = db_path.parent()?;
    let prefix = format!("{}.backup-schema-v", db_path.file_name()?.to_string_lossy());
    let mut backups = fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                && path.extension().and_then(|value| value.to_str()) == Some("sqlite3")
        })
        .collect::<Vec<_>>();
    backups.sort();
    backups.pop()
}

const SCHEMA_VERSION_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL CHECK (version >= 0),
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT OR IGNORE INTO schema_version (id, version) VALUES (1, 0);
"#;

const SCHEMA_COMPONENT_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_component (
    namespace TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    minimum_core_schema INTEGER NOT NULL CHECK (minimum_core_schema > 0),
    migration_digest TEXT NOT NULL CHECK (migration_digest GLOB 'sha256:*'),
    contract_hash TEXT NOT NULL CHECK (contract_hash GLOB 'sha256:*'),
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK (namespace GLOB '[a-z][a-z0-9-]*')
);
"#;

const CURRENT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS component_ingest (
    id INTEGER PRIMARY KEY,
    component_namespace TEXT NOT NULL REFERENCES schema_component(namespace) ON DELETE RESTRICT,
    source_schema_version INTEGER NOT NULL CHECK (source_schema_version > 0),
    source_contract_hash TEXT NOT NULL CHECK (source_contract_hash GLOB 'sha256:*'),
    idempotency_key TEXT NOT NULL,
    payload_digest TEXT NOT NULL CHECK (payload_digest GLOB 'sha256:*'),
    record_count INTEGER NOT NULL CHECK (record_count >= 0),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(component_namespace, idempotency_key)
);

CREATE TABLE IF NOT EXISTS component_record (
    id INTEGER PRIMARY KEY,
    ingest_id INTEGER NOT NULL REFERENCES component_ingest(id) ON DELETE CASCADE,
    record_kind TEXT NOT NULL,
    record_key TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    source_schema_version INTEGER NOT NULL CHECK (source_schema_version > 0),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(ingest_id, record_kind, record_key)
);

CREATE INDEX IF NOT EXISTS idx_component_ingest_namespace ON component_ingest(component_namespace, created_at);
CREATE INDEX IF NOT EXISTS idx_component_record_ingest ON component_record(ingest_id);

CREATE TABLE IF NOT EXISTS work_item (
    id INTEGER PRIMARY KEY,
    parent_work_item_id INTEGER REFERENCES work_item(id) ON DELETE SET NULL,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'held', 'done', 'canceled')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    priority TEXT,
    program TEXT,
    work_group TEXT,
    acceptance_criteria TEXT,
    hold_kind TEXT CHECK (hold_kind IS NULL OR hold_kind IN ('blocked', 'deferred', 'external-validation')),
    hold_reason TEXT
);

CREATE TABLE IF NOT EXISTS work_dependency (
    work_item_id INTEGER NOT NULL REFERENCES work_item(id) ON DELETE CASCADE,
    depends_on_work_item_id INTEGER NOT NULL REFERENCES work_item(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (work_item_id, depends_on_work_item_id),
    CHECK (work_item_id != depends_on_work_item_id)
);

CREATE TABLE IF NOT EXISTS run (
    id INTEGER PRIMARY KEY,
    work_item_id INTEGER NOT NULL REFERENCES work_item(id) ON DELETE CASCADE,
    command TEXT,
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'success', 'failed', 'partial')),
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT,
    notes TEXT
);

CREATE TABLE IF NOT EXISTS observation (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS global_observation (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('observation', 'notification')),
    body TEXT NOT NULL,
    source TEXT,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'cleared')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS artifact (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    description TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS prompt (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    role TEXT NOT NULL,
    body TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'retired')),
    current_version INTEGER NOT NULL DEFAULT 1,
    current_version_id INTEGER REFERENCES prompt_version(id) ON DELETE SET NULL,
    source_path TEXT,
    description TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS prompt_version (
    id INTEGER PRIMARY KEY,
    prompt_id INTEGER NOT NULL REFERENCES prompt(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    role TEXT NOT NULL,
    body TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    source_path TEXT,
    description TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(prompt_id, version)
);

CREATE TABLE IF NOT EXISTS prompt_bundle (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'sealed', 'retired')),
    manifest_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(manifest_json)),
    bundle_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS prompt_bundle_item (
    id INTEGER PRIMARY KEY,
    bundle_id INTEGER NOT NULL REFERENCES prompt_bundle(id) ON DELETE CASCADE,
    prompt_id INTEGER NOT NULL REFERENCES prompt(id) ON DELETE RESTRICT,
    prompt_version_id INTEGER NOT NULL REFERENCES prompt_version(id) ON DELETE RESTRICT,
    prompt_slug TEXT NOT NULL,
    prompt_role TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    UNIQUE(bundle_id, prompt_slug)
);

CREATE TABLE IF NOT EXISTS decision (
    id INTEGER PRIMARY KEY,
    work_item_id INTEGER NOT NULL REFERENCES work_item(id) ON DELETE CASCADE,
    outcome TEXT NOT NULL CHECK (outcome IN ('continue', 'stop', 'inconclusive')),
    rationale TEXT NOT NULL,
    next_work_item_id INTEGER REFERENCES work_item(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS event_log (
    id INTEGER PRIMARY KEY,
    entity_type TEXT NOT NULL,
    entity_id INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload_json)),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS loop_intervention (
    id INTEGER PRIMARY KEY,
    action TEXT NOT NULL CHECK (action IN ('pause', 'stop', 'steer')),
    reason TEXT NOT NULL,
    instruction TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'applied', 'cleared')),
    requested_by TEXT,
    applied_run_id INTEGER REFERENCES run(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CORE_TABLES: &[&str] = &[
    "schema_version",
    "schema_component",
    "component_ingest",
    "component_record",
    "work_item",
    "work_dependency",
    "run",
    "observation",
    "global_observation",
    "artifact",
    "prompt",
    "prompt_version",
    "prompt_bundle",
    "prompt_bundle_item",
    "decision",
    "event_log",
    "loop_intervention",
];

#[derive(Debug, PartialEq, Eq)]
struct ColumnSchema {
    name: &'static str,
    type_name: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct ForeignKeySchema {
    from_column: &'static str,
    target_table: &'static str,
    target_column: &'static str,
    on_delete: &'static str,
}

struct TableSchema {
    name: &'static str,
    columns: &'static [ColumnSchema],
    foreign_keys: &'static [ForeignKeySchema],
    required_sql: &'static [&'static str],
}

const SCHEMA_VERSION_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("version", "INTEGER", true, None, false),
    column("applied_at", "TEXT", true, Some("datetime('now')"), false),
];
const SCHEMA_COMPONENT_COLUMNS: &[ColumnSchema] = &[
    column("namespace", "TEXT", false, None, true),
    column("schema_version", "INTEGER", true, None, false),
    column("minimum_core_schema", "INTEGER", true, None, false),
    column("migration_digest", "TEXT", true, None, false),
    column("contract_hash", "TEXT", true, None, false),
    column("applied_at", "TEXT", true, Some("datetime('now')"), false),
];
const COMPONENT_INGEST_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("component_namespace", "TEXT", true, None, false),
    column("source_schema_version", "INTEGER", true, None, false),
    column("source_contract_hash", "TEXT", true, None, false),
    column("idempotency_key", "TEXT", true, None, false),
    column("payload_digest", "TEXT", true, None, false),
    column("record_count", "INTEGER", true, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const COMPONENT_RECORD_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("ingest_id", "INTEGER", true, None, false),
    column("record_kind", "TEXT", true, None, false),
    column("record_key", "TEXT", true, None, false),
    column("payload_json", "TEXT", true, None, false),
    column("source_schema_version", "INTEGER", true, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const WORK_ITEM_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("parent_work_item_id", "INTEGER", false, None, false),
    column("slug", "TEXT", true, None, false),
    column("title", "TEXT", true, None, false),
    column("description", "TEXT", true, None, false),
    column("status", "TEXT", true, Some("'pending'"), false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
    column("updated_at", "TEXT", true, Some("datetime('now')"), false),
    column("priority", "TEXT", false, None, false),
    column("program", "TEXT", false, None, false),
    column("work_group", "TEXT", false, None, false),
    column("acceptance_criteria", "TEXT", false, None, false),
    column("hold_kind", "TEXT", false, None, false),
    column("hold_reason", "TEXT", false, None, false),
];
const V1_WORK_ITEM_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("parent_work_item_id", "INTEGER", false, None, false),
    column("slug", "TEXT", true, None, false),
    column("title", "TEXT", true, None, false),
    column("description", "TEXT", true, None, false),
    column("status", "TEXT", true, Some("'pending'"), false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
    column("updated_at", "TEXT", true, Some("datetime('now')"), false),
];
const WORK_DEPENDENCY_COLUMNS: &[ColumnSchema] = &[
    column("work_item_id", "INTEGER", true, None, true),
    column("depends_on_work_item_id", "INTEGER", true, None, true),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const RUN_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("work_item_id", "INTEGER", true, None, false),
    column("command", "TEXT", false, None, false),
    column("status", "TEXT", true, Some("'running'"), false),
    column("started_at", "TEXT", true, Some("datetime('now')"), false),
    column("finished_at", "TEXT", false, None, false),
    column("notes", "TEXT", false, None, false),
];
const OBSERVATION_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("run_id", "INTEGER", true, None, false),
    column("body", "TEXT", true, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const GLOBAL_OBSERVATION_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("kind", "TEXT", true, None, false),
    column("body", "TEXT", true, None, false),
    column("source", "TEXT", false, None, false),
    column("status", "TEXT", true, Some("'active'"), false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
    column("updated_at", "TEXT", true, Some("datetime('now')"), false),
];
const ARTIFACT_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("run_id", "INTEGER", true, None, false),
    column("kind", "TEXT", true, None, false),
    column("path", "TEXT", true, None, false),
    column("description", "TEXT", true, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const PROMPT_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("slug", "TEXT", true, None, false),
    column("role", "TEXT", true, None, false),
    column("body", "TEXT", true, None, false),
    column("content_hash", "TEXT", true, None, false),
    column("status", "TEXT", true, Some("'draft'"), false),
    column("current_version", "INTEGER", true, Some("1"), false),
    column("current_version_id", "INTEGER", false, None, false),
    column("source_path", "TEXT", false, None, false),
    column("description", "TEXT", false, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
    column("updated_at", "TEXT", true, Some("datetime('now')"), false),
];
const PROMPT_VERSION_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("prompt_id", "INTEGER", true, None, false),
    column("version", "INTEGER", true, None, false),
    column("role", "TEXT", true, None, false),
    column("body", "TEXT", true, None, false),
    column("content_hash", "TEXT", true, None, false),
    column("source_path", "TEXT", false, None, false),
    column("description", "TEXT", false, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const PROMPT_BUNDLE_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("slug", "TEXT", true, None, false),
    column("status", "TEXT", true, Some("'draft'"), false),
    column("manifest_json", "TEXT", true, Some("'{}'"), false),
    column("bundle_hash", "TEXT", true, Some("''"), false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const PROMPT_BUNDLE_ITEM_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("bundle_id", "INTEGER", true, None, false),
    column("prompt_id", "INTEGER", true, None, false),
    column("prompt_version_id", "INTEGER", true, None, false),
    column("prompt_slug", "TEXT", true, None, false),
    column("prompt_role", "TEXT", true, None, false),
    column("prompt_version", "INTEGER", true, None, false),
    column("content_hash", "TEXT", true, None, false),
];
const DECISION_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("work_item_id", "INTEGER", true, None, false),
    column("outcome", "TEXT", true, None, false),
    column("rationale", "TEXT", true, None, false),
    column("next_work_item_id", "INTEGER", false, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const EVENT_LOG_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("entity_type", "TEXT", true, None, false),
    column("entity_id", "INTEGER", true, None, false),
    column("event_type", "TEXT", true, None, false),
    column("payload_json", "TEXT", true, Some("'{}'"), false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
];
const LOOP_INTERVENTION_COLUMNS: &[ColumnSchema] = &[
    column("id", "INTEGER", false, None, true),
    column("action", "TEXT", true, None, false),
    column("reason", "TEXT", true, None, false),
    column("instruction", "TEXT", false, None, false),
    column("status", "TEXT", true, Some("'pending'"), false),
    column("requested_by", "TEXT", false, None, false),
    column("applied_run_id", "INTEGER", false, None, false),
    column("created_at", "TEXT", true, Some("datetime('now')"), false),
    column("updated_at", "TEXT", true, Some("datetime('now')"), false),
];

const WORK_ITEM_FOREIGN_KEYS: &[ForeignKeySchema] = &[foreign_key(
    "parent_work_item_id",
    "work_item",
    "id",
    "SET NULL",
)];
const COMPONENT_INGEST_FOREIGN_KEYS: &[ForeignKeySchema] = &[foreign_key(
    "component_namespace",
    "schema_component",
    "namespace",
    "RESTRICT",
)];
const COMPONENT_RECORD_FOREIGN_KEYS: &[ForeignKeySchema] = &[foreign_key(
    "ingest_id",
    "component_ingest",
    "id",
    "CASCADE",
)];
const WORK_DEPENDENCY_FOREIGN_KEYS: &[ForeignKeySchema] = &[
    foreign_key("work_item_id", "work_item", "id", "CASCADE"),
    foreign_key("depends_on_work_item_id", "work_item", "id", "RESTRICT"),
];
const RUN_FOREIGN_KEYS: &[ForeignKeySchema] =
    &[foreign_key("work_item_id", "work_item", "id", "CASCADE")];
const OBSERVATION_FOREIGN_KEYS: &[ForeignKeySchema] =
    &[foreign_key("run_id", "run", "id", "CASCADE")];
const ARTIFACT_FOREIGN_KEYS: &[ForeignKeySchema] = &[foreign_key("run_id", "run", "id", "CASCADE")];
const PROMPT_FOREIGN_KEYS: &[ForeignKeySchema] = &[foreign_key(
    "current_version_id",
    "prompt_version",
    "id",
    "SET NULL",
)];
const PROMPT_VERSION_FOREIGN_KEYS: &[ForeignKeySchema] =
    &[foreign_key("prompt_id", "prompt", "id", "CASCADE")];
const PROMPT_BUNDLE_ITEM_FOREIGN_KEYS: &[ForeignKeySchema] = &[
    foreign_key("bundle_id", "prompt_bundle", "id", "CASCADE"),
    foreign_key("prompt_id", "prompt", "id", "RESTRICT"),
    foreign_key("prompt_version_id", "prompt_version", "id", "RESTRICT"),
];
const DECISION_FOREIGN_KEYS: &[ForeignKeySchema] = &[
    foreign_key("work_item_id", "work_item", "id", "CASCADE"),
    foreign_key("next_work_item_id", "work_item", "id", "SET NULL"),
];
const LOOP_INTERVENTION_FOREIGN_KEYS: &[ForeignKeySchema] =
    &[foreign_key("applied_run_id", "run", "id", "SET NULL")];

const NO_FOREIGN_KEYS: &[ForeignKeySchema] = &[];

const SCHEMA_VERSION_REQUIRED_SQL: &[&str] = &["CHECK (id = 1)", "CHECK (version >= 0)"];
const SCHEMA_COMPONENT_REQUIRED_SQL: &[&str] = &[
    "CHECK (schema_version > 0)",
    "CHECK (minimum_core_schema > 0)",
    "CHECK (migration_digest GLOB 'sha256:*')",
    "CHECK (contract_hash GLOB 'sha256:*')",
    "CHECK (namespace GLOB '[a-z][a-z0-9-]*')",
];
const COMPONENT_INGEST_REQUIRED_SQL: &[&str] = &[
    "CHECK (source_schema_version > 0)",
    "CHECK (source_contract_hash GLOB 'sha256:*')",
    "CHECK (payload_digest GLOB 'sha256:*')",
    "CHECK (record_count >= 0)",
    "UNIQUE(component_namespace, idempotency_key)",
];
const COMPONENT_RECORD_REQUIRED_SQL: &[&str] = &[
    "CHECK (json_valid(payload_json))",
    "CHECK (source_schema_version > 0)",
    "UNIQUE(ingest_id, record_kind, record_key)",
];
const WORK_ITEM_REQUIRED_SQL: &[&str] = &[
    "slug TEXT NOT NULL UNIQUE",
    "CHECK (status IN ('pending', 'running', 'held', 'done', 'canceled'))",
    "CHECK (hold_kind IS NULL OR hold_kind IN ('blocked', 'deferred', 'external-validation'))",
];
const WORK_DEPENDENCY_REQUIRED_SQL: &[&str] = &[
    "PRIMARY KEY (work_item_id, depends_on_work_item_id)",
    "CHECK (work_item_id != depends_on_work_item_id)",
];
const RUN_REQUIRED_SQL: &[&str] =
    &["CHECK (status IN ('running', 'success', 'failed', 'partial'))"];
const GLOBAL_OBSERVATION_REQUIRED_SQL: &[&str] = &[
    "CHECK (kind IN ('observation', 'notification'))",
    "CHECK (status IN ('active', 'cleared'))",
];
const ARTIFACT_REQUIRED_SQL: &[&str] = &[];
const PROMPT_REQUIRED_SQL: &[&str] = &[
    "slug TEXT NOT NULL UNIQUE",
    "CHECK (status IN ('draft', 'active', 'retired'))",
];
const PROMPT_VERSION_REQUIRED_SQL: &[&str] = &["UNIQUE(prompt_id, version)"];
const PROMPT_BUNDLE_REQUIRED_SQL: &[&str] = &[
    "slug TEXT NOT NULL UNIQUE",
    "CHECK (status IN ('draft', 'sealed', 'retired'))",
    "CHECK (json_valid(manifest_json))",
];
const PROMPT_BUNDLE_ITEM_REQUIRED_SQL: &[&str] = &["UNIQUE(bundle_id, prompt_slug)"];
const DECISION_REQUIRED_SQL: &[&str] = &["CHECK (outcome IN ('continue', 'stop', 'inconclusive'))"];
const EVENT_LOG_REQUIRED_SQL: &[&str] = &["CHECK (json_valid(payload_json))"];
const LOOP_INTERVENTION_REQUIRED_SQL: &[&str] = &[
    "CHECK (action IN ('pause', 'stop', 'steer'))",
    "CHECK (status IN ('pending', 'applied', 'cleared'))",
];
const NO_REQUIRED_SQL: &[&str] = &[];

const EXPECTED_SCHEMA: &[TableSchema] = &[
    table(
        "schema_version",
        SCHEMA_VERSION_COLUMNS,
        NO_FOREIGN_KEYS,
        SCHEMA_VERSION_REQUIRED_SQL,
    ),
    table(
        "schema_component",
        SCHEMA_COMPONENT_COLUMNS,
        NO_FOREIGN_KEYS,
        SCHEMA_COMPONENT_REQUIRED_SQL,
    ),
    table(
        "component_ingest",
        COMPONENT_INGEST_COLUMNS,
        COMPONENT_INGEST_FOREIGN_KEYS,
        COMPONENT_INGEST_REQUIRED_SQL,
    ),
    table(
        "component_record",
        COMPONENT_RECORD_COLUMNS,
        COMPONENT_RECORD_FOREIGN_KEYS,
        COMPONENT_RECORD_REQUIRED_SQL,
    ),
    table(
        "work_item",
        WORK_ITEM_COLUMNS,
        WORK_ITEM_FOREIGN_KEYS,
        WORK_ITEM_REQUIRED_SQL,
    ),
    table(
        "work_dependency",
        WORK_DEPENDENCY_COLUMNS,
        WORK_DEPENDENCY_FOREIGN_KEYS,
        WORK_DEPENDENCY_REQUIRED_SQL,
    ),
    table("run", RUN_COLUMNS, RUN_FOREIGN_KEYS, RUN_REQUIRED_SQL),
    table(
        "observation",
        OBSERVATION_COLUMNS,
        OBSERVATION_FOREIGN_KEYS,
        NO_REQUIRED_SQL,
    ),
    table(
        "global_observation",
        GLOBAL_OBSERVATION_COLUMNS,
        NO_FOREIGN_KEYS,
        GLOBAL_OBSERVATION_REQUIRED_SQL,
    ),
    table(
        "artifact",
        ARTIFACT_COLUMNS,
        ARTIFACT_FOREIGN_KEYS,
        ARTIFACT_REQUIRED_SQL,
    ),
    table(
        "prompt",
        PROMPT_COLUMNS,
        PROMPT_FOREIGN_KEYS,
        PROMPT_REQUIRED_SQL,
    ),
    table(
        "prompt_version",
        PROMPT_VERSION_COLUMNS,
        PROMPT_VERSION_FOREIGN_KEYS,
        PROMPT_VERSION_REQUIRED_SQL,
    ),
    table(
        "prompt_bundle",
        PROMPT_BUNDLE_COLUMNS,
        NO_FOREIGN_KEYS,
        PROMPT_BUNDLE_REQUIRED_SQL,
    ),
    table(
        "prompt_bundle_item",
        PROMPT_BUNDLE_ITEM_COLUMNS,
        PROMPT_BUNDLE_ITEM_FOREIGN_KEYS,
        PROMPT_BUNDLE_ITEM_REQUIRED_SQL,
    ),
    table(
        "decision",
        DECISION_COLUMNS,
        DECISION_FOREIGN_KEYS,
        DECISION_REQUIRED_SQL,
    ),
    table(
        "event_log",
        EVENT_LOG_COLUMNS,
        NO_FOREIGN_KEYS,
        EVENT_LOG_REQUIRED_SQL,
    ),
    table(
        "loop_intervention",
        LOOP_INTERVENTION_COLUMNS,
        LOOP_INTERVENTION_FOREIGN_KEYS,
        LOOP_INTERVENTION_REQUIRED_SQL,
    ),
];

const fn column(
    name: &'static str,
    type_name: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key: bool,
) -> ColumnSchema {
    ColumnSchema {
        name,
        type_name,
        not_null,
        default_value,
        primary_key,
    }
}

const fn foreign_key(
    from_column: &'static str,
    target_table: &'static str,
    target_column: &'static str,
    on_delete: &'static str,
) -> ForeignKeySchema {
    ForeignKeySchema {
        from_column,
        target_table,
        target_column,
        on_delete,
    }
}

const fn table(
    name: &'static str,
    columns: &'static [ColumnSchema],
    foreign_keys: &'static [ForeignKeySchema],
    required_sql: &'static [&'static str],
) -> TableSchema {
    TableSchema {
        name,
        columns,
        foreign_keys,
        required_sql,
    }
}

const SCHEMA_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_work_item_status ON work_item(status);
CREATE INDEX IF NOT EXISTS idx_work_item_parent ON work_item(parent_work_item_id);
CREATE INDEX IF NOT EXISTS idx_work_item_priority_program ON work_item(priority, program, status);
CREATE INDEX IF NOT EXISTS idx_work_dependency_depends_on ON work_dependency(depends_on_work_item_id);
CREATE INDEX IF NOT EXISTS idx_run_work_item ON run(work_item_id);
CREATE INDEX IF NOT EXISTS idx_run_status ON run(status);
CREATE INDEX IF NOT EXISTS idx_observation_run ON observation(run_id);
CREATE INDEX IF NOT EXISTS idx_global_observation_status_kind ON global_observation(status, kind);
CREATE INDEX IF NOT EXISTS idx_artifact_run ON artifact(run_id);
CREATE INDEX IF NOT EXISTS idx_prompt_status ON prompt(status);
CREATE INDEX IF NOT EXISTS idx_prompt_version_prompt ON prompt_version(prompt_id);
CREATE INDEX IF NOT EXISTS idx_prompt_bundle_status ON prompt_bundle(status);
CREATE INDEX IF NOT EXISTS idx_prompt_bundle_item_bundle ON prompt_bundle_item(bundle_id);
CREATE INDEX IF NOT EXISTS idx_decision_work_item ON decision(work_item_id);
CREATE INDEX IF NOT EXISTS idx_event_log_entity ON event_log(entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_loop_intervention_status ON loop_intervention(status);

CREATE TRIGGER IF NOT EXISTS trg_work_dependency_no_cycle
BEFORE INSERT ON work_dependency
WHEN NEW.work_item_id = NEW.depends_on_work_item_id OR EXISTS (
    WITH RECURSIVE ancestors(id) AS (
        SELECT depends_on_work_item_id
        FROM work_dependency
        WHERE work_item_id = NEW.depends_on_work_item_id
        UNION
        SELECT dependency.depends_on_work_item_id
        FROM work_dependency AS dependency
        JOIN ancestors ON dependency.work_item_id = ancestors.id
    )
    SELECT 1 FROM ancestors WHERE id = NEW.work_item_id
)
BEGIN
    SELECT RAISE(ABORT, 'work dependency cycle');
END;
"#;

pub(crate) fn ensure_schema(connection: &Connection) -> anyhow::Result<()> {
    let existing_tables = application_table_names(connection)?;
    if existing_tables.is_empty() {
        create_current_schema(connection)?;
        return Ok(());
    }

    if !table_exists(connection, "schema_version")? {
        return Err(incompatible_schema_error("missing schema_version table"));
    }

    preflight_schema_migration(connection)?;
    apply_pending_schema_migrations(connection, None)?;
    let version = schema_version(connection)?;
    if version != CURRENT_SCHEMA_VERSION {
        return Err(incompatible_schema_error(format!(
            "schema version {version} does not match required version {CURRENT_SCHEMA_VERSION}"
        )));
    }
    if !current_schema_matches(connection)? {
        return Err(incompatible_schema_error(
            "schema shape does not match the current core schema",
        ));
    }
    validate_component_catalog(connection)?;

    connection
        .execute_batch(CURRENT_SCHEMA)
        .context("failed to ensure current schema")?;
    connection
        .execute_batch(SCHEMA_INDEXES)
        .context("failed to ensure schema indexes")?;
    Ok(())
}

pub(crate) fn preflight_schema_migration(connection: &Connection) -> anyhow::Result<Option<i64>> {
    let existing_tables = application_table_names(connection)?;
    if existing_tables.is_empty() {
        return Ok(None);
    }
    if !table_exists(connection, "schema_version")? {
        return Err(incompatible_schema_error("missing schema_version table"));
    }
    let version = schema_version(connection)?;
    match version {
        1 if version_1_schema_matches(connection)? => Ok(Some(1)),
        1 => Err(incompatible_schema_error(
            "schema version 1 shape is not eligible for migration",
        )),
        2 if current_schema_matches(connection)? => {
            validate_component_catalog(connection)?;
            Ok(None)
        }
        2 if version_2_schema_matches(connection)? => Ok(Some(2)),
        2 => Err(incompatible_schema_error(
            "schema version 2 shape is not eligible for migration",
        )),
        3 if version_3_schema_matches(connection)? => Ok(Some(3)),
        3 => Err(incompatible_schema_error(
            "obsolete schema version 3 shape is not eligible for normalization to schema v2",
        )),
        4 if current_schema_matches(connection)? => Ok(Some(4)),
        4 => Err(incompatible_schema_error(
            "obsolete schema version 4 shape is not eligible for normalization to schema v2",
        )),
        _ => Err(incompatible_schema_error(format!(
            "schema version {version} does not match required version {CURRENT_SCHEMA_VERSION}"
        ))),
    }
}

fn apply_pending_schema_migrations(
    connection: &Connection,
    fail_after_version: Option<i64>,
) -> anyhow::Result<()> {
    let migration_origin = preflight_schema_migration(connection)?;
    if migration_origin.is_none() {
        return Ok(());
    }
    in_migration_transaction(connection, |connection| {
        let version = schema_version(connection)?;
        if version == 1 {
            migrate_v1_to_v2(connection)?;
            if fail_after_version == Some(CURRENT_SCHEMA_VERSION) {
                anyhow::bail!("injected migration failure after schema v{CURRENT_SCHEMA_VERSION}");
            }
        }
        apply_current_v2_contract(connection)?;
        anyhow::ensure!(
            current_schema_matches(connection)?,
            "normalized schema shape does not match ldgr-core schema v{CURRENT_SCHEMA_VERSION}"
        );
        validate_component_catalog(connection)?;
        validate_foreign_keys(connection)
    })
}

fn migrate_v1_to_v2(connection: &Connection) -> anyhow::Result<()> {
    if !version_1_schema_matches(connection)? {
        return Err(incompatible_schema_error(
            "schema version 1 shape is not eligible for the v2 migration",
        ));
    }
    connection
        .execute_batch(
                r#"
                ALTER TABLE work_item ADD COLUMN priority TEXT;
                ALTER TABLE work_item ADD COLUMN program TEXT;
                ALTER TABLE work_item ADD COLUMN work_group TEXT;
                ALTER TABLE work_item ADD COLUMN acceptance_criteria TEXT;
                ALTER TABLE work_item ADD COLUMN hold_kind TEXT CHECK (hold_kind IS NULL OR hold_kind IN ('blocked', 'deferred', 'external-validation'));
                ALTER TABLE work_item ADD COLUMN hold_reason TEXT;
                CREATE TABLE work_dependency (
                    work_item_id INTEGER NOT NULL REFERENCES work_item(id) ON DELETE CASCADE,
                    depends_on_work_item_id INTEGER NOT NULL REFERENCES work_item(id) ON DELETE RESTRICT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    PRIMARY KEY (work_item_id, depends_on_work_item_id),
                    CHECK (work_item_id != depends_on_work_item_id)
                );
                "#,
            )
        .context("failed to migrate ldgr schema from v1 to v2")?;
    connection
        .execute_batch(SCHEMA_INDEXES)
        .context("failed to create v2 schema indexes and dependency guard")?;
    set_schema_version(connection, 2)
}

fn apply_current_v2_contract(connection: &Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(SCHEMA_COMPONENT_TABLE)
        .context("failed to create the v2 schema component catalog")?;
    connection
        .execute_batch(CURRENT_SCHEMA)
        .context("failed to create the v2 component ingestion ledger")?;
    seed_generated_component_catalog(connection)?;
    connection
        .execute_batch(SCHEMA_INDEXES)
        .context("failed to create v2 schema indexes")?;
    set_schema_version(connection, CURRENT_SCHEMA_VERSION)
}

fn validate_foreign_keys(connection: &Connection) -> anyhow::Result<()> {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .context("failed to prepare foreign-key validation")?;
    let violations = statement
        .query_map([], |_| Ok(()))
        .context("failed to run foreign-key validation")?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        violations.is_empty(),
        "foreign-key validation found {} violation(s)",
        violations.len()
    );
    Ok(())
}

fn version_2_schema_matches(connection: &Connection) -> anyhow::Result<bool> {
    let table_names = application_table_names(connection)?;
    let v2_tables = CORE_TABLES
        .iter()
        .copied()
        .filter(|name| {
            !matches!(
                *name,
                "schema_component" | "component_ingest" | "component_record"
            )
        })
        .collect::<Vec<_>>();
    if table_names.len() != v2_tables.len()
        || table_names
            .iter()
            .any(|name| !v2_tables.contains(&name.as_str()))
    {
        return Ok(false);
    }
    for table_schema in EXPECTED_SCHEMA {
        if matches!(
            table_schema.name,
            "schema_component" | "component_ingest" | "component_record"
        ) {
            continue;
        }
        if !table_matches_schema(connection, table_schema)? {
            return Ok(false);
        }
    }
    work_item_accepts_held_status(connection)
}

fn version_3_schema_matches(connection: &Connection) -> anyhow::Result<bool> {
    let table_names = application_table_names(connection)?;
    let v3_tables = CORE_TABLES
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "component_ingest" | "component_record"))
        .collect::<Vec<_>>();
    if table_names.len() != v3_tables.len()
        || table_names
            .iter()
            .any(|name| !v3_tables.contains(&name.as_str()))
    {
        return Ok(false);
    }
    for table_schema in EXPECTED_SCHEMA {
        if matches!(table_schema.name, "component_ingest" | "component_record") {
            continue;
        }
        if !table_matches_schema(connection, table_schema)? {
            return Ok(false);
        }
    }
    work_item_accepts_held_status(connection)
}

fn version_1_schema_matches(connection: &Connection) -> anyhow::Result<bool> {
    let table_names = application_table_names(connection)?;
    let v1_tables = CORE_TABLES
        .iter()
        .copied()
        .filter(|name| {
            !matches!(
                *name,
                "work_dependency" | "schema_component" | "component_ingest" | "component_record"
            )
        })
        .collect::<Vec<_>>();
    if table_names.len() != v1_tables.len()
        || table_names
            .iter()
            .any(|name| !v1_tables.contains(&name.as_str()))
    {
        return Ok(false);
    }
    let v1_work_item = TableSchema {
        name: "work_item",
        columns: V1_WORK_ITEM_COLUMNS,
        foreign_keys: WORK_ITEM_FOREIGN_KEYS,
        required_sql: &[
            "slug TEXT NOT NULL UNIQUE",
            "CHECK (status IN ('pending', 'running', 'held', 'done', 'canceled'))",
        ],
    };
    if !table_matches_schema(connection, &v1_work_item)? {
        return Ok(false);
    }
    for table_schema in EXPECTED_SCHEMA {
        if matches!(
            table_schema.name,
            "work_item"
                | "work_dependency"
                | "schema_component"
                | "component_ingest"
                | "component_record"
        ) {
            continue;
        }
        if !table_matches_schema(connection, table_schema)? {
            return Ok(false);
        }
    }
    work_item_accepts_held_status(connection)
}

fn create_current_schema(connection: &Connection) -> anyhow::Result<()> {
    anyhow::ensure!(
        CURRENT_SCHEMA_VERSION == GENERATED_CORE_SCHEMA_VERSION,
        "generated database contract is stale: Core schema is {} but contract is {}",
        CURRENT_SCHEMA_VERSION,
        GENERATED_CORE_SCHEMA_VERSION
    );
    connection
        .execute_batch(SCHEMA_VERSION_TABLE)
        .context("failed to create schema version table")?;
    connection
        .execute_batch(SCHEMA_COMPONENT_TABLE)
        .context("failed to create schema component catalog")?;
    connection
        .execute_batch(CURRENT_SCHEMA)
        .context("failed to create current schema")?;
    connection
        .execute_batch(SCHEMA_INDEXES)
        .context("failed to create schema indexes")?;
    seed_generated_component_catalog(connection)?;
    set_schema_version(connection, CURRENT_SCHEMA_VERSION)
}

fn incompatible_schema_error(reason: impl fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "incompatible ldgr database schema: {reason}. This database cannot be migrated automatically to ldgr-core schema v{CURRENT_SCHEMA_VERSION} without risking data loss."
    )
}

fn application_table_names(connection: &Connection) -> anyhow::Result<Vec<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name DESC",
        )
        .context("failed to prepare schema reset table query")?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .context("failed to query schema reset tables")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read schema reset tables")
}

fn quoted_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn current_schema_matches(connection: &Connection) -> anyhow::Result<bool> {
    let table_names = application_table_names(connection)?;
    for table_name in &table_names {
        if !CORE_TABLES.contains(&table_name.as_str()) {
            return Ok(false);
        }
    }
    for table_name in CORE_TABLES {
        if !table_exists(connection, table_name)? {
            return Ok(false);
        }
    }
    for table_schema in EXPECTED_SCHEMA {
        if !table_matches_schema(connection, table_schema)? {
            return Ok(false);
        }
    }
    work_item_accepts_held_status(connection)
}

#[derive(Debug, PartialEq, Eq)]
struct ActualColumnSchema {
    name: String,
    type_name: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key: bool,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ActualForeignKeySchema {
    from_column: String,
    target_table: String,
    target_column: String,
    on_delete: String,
}

fn table_exists(connection: &Connection, table: &str) -> anyhow::Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .with_context(|| format!("failed to inspect table {table}"))
}

fn table_matches_schema(
    connection: &Connection,
    table_schema: &TableSchema,
) -> anyhow::Result<bool> {
    if table_columns(connection, table_schema.name)? != expected_columns(table_schema.columns) {
        return Ok(false);
    }
    if table_foreign_keys(connection, table_schema.name)?
        != expected_foreign_keys(table_schema.foreign_keys)
    {
        return Ok(false);
    }

    let table_sql = table_create_sql(connection, table_schema.name)?;
    let normalized_sql = normalize_sql(&table_sql);
    Ok(table_schema
        .required_sql
        .iter()
        .all(|required| normalized_sql.contains(&normalize_sql(required))))
}

fn expected_columns(columns: &[ColumnSchema]) -> Vec<ActualColumnSchema> {
    columns
        .iter()
        .map(|column| ActualColumnSchema {
            name: column.name.to_string(),
            type_name: normalize_type_name(column.type_name),
            not_null: column.not_null,
            default_value: column.default_value.map(normalize_default_value),
            primary_key: column.primary_key,
        })
        .collect()
}

fn expected_foreign_keys(foreign_keys: &[ForeignKeySchema]) -> Vec<ActualForeignKeySchema> {
    let mut foreign_keys = foreign_keys
        .iter()
        .map(|foreign_key| ActualForeignKeySchema {
            from_column: foreign_key.from_column.to_string(),
            target_table: foreign_key.target_table.to_string(),
            target_column: foreign_key.target_column.to_string(),
            on_delete: foreign_key.on_delete.to_string(),
        })
        .collect::<Vec<_>>();
    foreign_keys.sort();
    foreign_keys
}

fn table_columns(
    connection: &Connection,
    table_name: &str,
) -> anyhow::Result<Vec<ActualColumnSchema>> {
    let sql = format!("PRAGMA table_info({})", quoted_identifier(table_name));
    let mut statement = connection
        .prepare(&sql)
        .with_context(|| format!("failed to inspect columns for table {table_name}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(ActualColumnSchema {
                name: row.get("name")?,
                type_name: normalize_type_name(row.get::<_, String>("type")?.as_str()),
                not_null: row.get::<_, i64>("notnull")? != 0,
                default_value: row
                    .get::<_, Option<String>>("dflt_value")?
                    .map(|value| normalize_default_value(&value)),
                primary_key: row.get::<_, i64>("pk")? != 0,
            })
        })
        .with_context(|| format!("failed to query columns for table {table_name}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("failed to read columns for table {table_name}"))
}

fn table_foreign_keys(
    connection: &Connection,
    table_name: &str,
) -> anyhow::Result<Vec<ActualForeignKeySchema>> {
    let sql = format!("PRAGMA foreign_key_list({})", quoted_identifier(table_name));
    let mut statement = connection
        .prepare(&sql)
        .with_context(|| format!("failed to inspect foreign keys for table {table_name}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(ActualForeignKeySchema {
                from_column: row.get("from")?,
                target_table: row.get("table")?,
                target_column: row.get("to")?,
                on_delete: row.get("on_delete")?,
            })
        })
        .with_context(|| format!("failed to query foreign keys for table {table_name}"))?;
    let mut foreign_keys = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("failed to read foreign keys for table {table_name}"))?;
    foreign_keys.sort();
    Ok(foreign_keys)
}

fn table_create_sql(connection: &Connection, table_name: &str) -> anyhow::Result<String> {
    connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table_name],
            |row| row.get(0),
        )
        .with_context(|| format!("failed to inspect table SQL for {table_name}"))
}

fn normalize_type_name(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn normalize_default_value(value: &str) -> String {
    let mut value = value.trim();
    while value.len() >= 2 && value.starts_with('(') && value.ends_with(')') {
        value = value[1..value.len() - 1].trim();
    }
    value.to_string()
}

fn normalize_sql(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn work_item_accepts_held_status(connection: &Connection) -> anyhow::Result<bool> {
    let sql = normalize_sql(&table_create_sql(connection, "work_item")?);
    Ok(sql.contains("'held'"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaComponentState {
    pub namespace: String,
    pub schema_version: i64,
    pub minimum_core_schema: i64,
    pub migration_digest: String,
    pub contract_hash: String,
    pub applied_at: String,
}

pub fn list_schema_components(
    connection: &Connection,
) -> anyhow::Result<Vec<SchemaComponentState>> {
    let mut statement = connection
        .prepare(
            "SELECT namespace, schema_version, minimum_core_schema, migration_digest,
                    contract_hash, applied_at
             FROM schema_component
             ORDER BY namespace",
        )
        .context("failed to prepare schema component query")?;
    let rows = statement
        .query_map([], |row| {
            Ok(SchemaComponentState {
                namespace: row.get(0)?,
                schema_version: row.get(1)?,
                minimum_core_schema: row.get(2)?,
                migration_digest: row.get(3)?,
                contract_hash: row.get(4)?,
                applied_at: row.get(5)?,
            })
        })
        .context("failed to query schema component catalog")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read schema component catalog")
}

fn seed_generated_component_catalog(connection: &Connection) -> anyhow::Result<()> {
    for component in GENERATED_DATABASE_COMPONENTS {
        connection
            .execute(
                "INSERT INTO schema_component (
                    namespace, schema_version, minimum_core_schema, migration_digest, contract_hash
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(namespace) DO UPDATE SET
                    schema_version = excluded.schema_version,
                    minimum_core_schema = excluded.minimum_core_schema,
                    migration_digest = excluded.migration_digest,
                    contract_hash = excluded.contract_hash,
                    applied_at = datetime('now')",
                params![
                    component.namespace,
                    component.schema_version,
                    component.minimum_core_schema,
                    component.migration_digest,
                    DATABASE_CONTRACT_HASH,
                ],
            )
            .with_context(|| {
                format!(
                    "failed to register generated schema component {}",
                    component.namespace
                )
            })?;
    }
    Ok(())
}

fn validate_component_catalog(connection: &Connection) -> anyhow::Result<()> {
    anyhow::ensure!(
        CURRENT_SCHEMA_VERSION == GENERATED_CORE_SCHEMA_VERSION,
        "generated database contract is stale: Core schema is {} but contract is {}",
        CURRENT_SCHEMA_VERSION,
        GENERATED_CORE_SCHEMA_VERSION
    );
    let actual = list_schema_components(connection)?;
    anyhow::ensure!(
        actual.len() == GENERATED_DATABASE_COMPONENTS.len(),
        "schema component catalog has {} entries; expected {}",
        actual.len(),
        GENERATED_DATABASE_COMPONENTS.len()
    );
    for generated in GENERATED_DATABASE_COMPONENTS {
        let component = actual
            .iter()
            .find(|component| component.namespace == generated.namespace)
            .with_context(|| {
                format!(
                    "schema component catalog is missing {}",
                    generated.namespace
                )
            })?;
        anyhow::ensure!(
            component.schema_version == generated.schema_version,
            "schema component {} version {} does not match generated version {}",
            generated.namespace,
            component.schema_version,
            generated.schema_version
        );
        anyhow::ensure!(
            component.minimum_core_schema == generated.minimum_core_schema,
            "schema component {} minimum Core schema {} does not match generated value {}",
            generated.namespace,
            component.minimum_core_schema,
            generated.minimum_core_schema
        );
        anyhow::ensure!(
            component.migration_digest == generated.migration_digest,
            "schema component {} migration digest does not match the generated contract",
            generated.namespace
        );
        anyhow::ensure!(
            component.contract_hash == DATABASE_CONTRACT_HASH,
            "schema component {} contract hash does not match the active contract",
            generated.namespace
        );
    }
    let core = actual
        .iter()
        .find(|component| component.namespace == "core")
        .context("schema component catalog is missing core")?;
    anyhow::ensure!(
        core.schema_version == schema_version(connection)?,
        "core component version does not match schema_version"
    );
    Ok(())
}

pub fn current_schema_version(connection: &Connection) -> anyhow::Result<i64> {
    connection
        .query_row(
            "SELECT version FROM schema_version WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .context("failed to read schema version")
}

fn schema_version(connection: &Connection) -> anyhow::Result<i64> {
    current_schema_version(connection)
}

fn set_schema_version(connection: &Connection, version: i64) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE schema_version SET version = ?1, applied_at = datetime('now') WHERE id = 1",
            params![version],
        )
        .with_context(|| format!("failed to record schema version {version}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fresh_schema_contains_only_core_tables() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        assert_eq!(schema_version(&connection)?, CURRENT_SCHEMA_VERSION);
        for table in [
            "schema_component",
            "component_ingest",
            "component_record",
            "work_item",
            "work_dependency",
            "run",
            "observation",
            "global_observation",
            "artifact",
            "prompt",
            "prompt_version",
            "prompt_bundle",
            "prompt_bundle_item",
            "decision",
            "event_log",
            "loop_intervention",
        ] {
            assert!(table_exists(&connection, table)?, "missing {table}");
        }
        assert_eq!(
            list_schema_components(&connection)?.len(),
            GENERATED_DATABASE_COMPONENTS.len()
        );
        for table in [
            "issue",
            "fact",
            "expectation",
            "validation_result",
            "failure",
            "blocker",
            "milestone",
            "target_profile",
            "adapter_profile",
            "tool",
            "tool_execution",
            "skill_invocation",
        ] {
            assert!(
                !table_exists(&connection, table)?,
                "advanced table {table} exists"
            );
        }
        Ok(())
    }

    #[test]
    fn incompatible_unknown_database_is_rejected_without_mutation() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            connection.execute_batch(include_str!(
                "../../tests/fixtures/schema/incompatible-v1-with-adapter-table.sql"
            ))?;
        }

        let error = open_store(&db_path).unwrap_err();

        let message = format!("{error:#}");
        assert!(
            message.contains("incompatible ldgr database schema"),
            "{message}"
        );
        assert!(
            message.contains("cannot be migrated automatically"),
            "{message}"
        );
        let connection = Connection::open(&db_path)?;
        assert!(table_exists(&connection, "adapter_unregistered_record")?);
        assert_eq!(schema_version(&connection)?, 1);
        let preserved: String = connection.query_row(
            "SELECT slug FROM work_item WHERE slug = 'preserved-old-work'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(preserved, "preserved-old-work");
        Ok(())
    }

    #[test]
    fn version_2_database_with_missing_core_constraint_is_rejected() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            rebuild_work_item_without_status_check(&connection)?;
            assert_eq!(schema_version(&connection)?, CURRENT_SCHEMA_VERSION);
            assert_core_tables_exist(&connection)?;
        }

        assert_schema_shape_rejected(&db_path)
    }

    #[test]
    fn version_2_database_with_missing_core_foreign_key_is_rejected() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            rebuild_run_without_work_item_foreign_key(&connection)?;
            assert_eq!(schema_version(&connection)?, CURRENT_SCHEMA_VERSION);
            assert_core_tables_exist(&connection)?;
        }

        assert_schema_shape_rejected(&db_path)
    }

    #[test]
    fn released_v1_database_migrates_to_current_without_losing_ledger_data() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            downgrade_test_schema_to_v1(&connection)?;
            connection.execute(
                "INSERT INTO work_item (slug, title, description, status)
                 VALUES ('preserved', 'Preserved', 'Released v1 data', 'done')",
                [],
            )?;
            let work_item_id = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO run (work_item_id, command, status, finished_at, notes)
                 VALUES (?1, 'test', 'success', datetime('now'), 'preserve me')",
                params![work_item_id],
            )?;
            let run_id = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO observation (run_id, body) VALUES (?1, 'durable evidence')",
                params![run_id],
            )?;
            assert_eq!(schema_version(&connection)?, 1);
        }

        let connection = open_store(&db_path)?;
        assert_eq!(schema_version(&connection)?, CURRENT_SCHEMA_VERSION);
        assert!(table_exists(&connection, "work_dependency")?);
        let item = require_work_item_by_slug(&connection, "preserved")?;
        assert_eq!(item.status, WorkItemStatus::Done);
        assert_eq!(item.priority, None);
        assert_eq!(
            list_observations(&connection, None, 10)?[0].body,
            "durable evidence"
        );
        Ok(())
    }

    #[test]
    fn released_v2_database_gains_complete_component_catalog() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            downgrade_test_schema_to_v2(&connection)?;
            assert_eq!(schema_version(&connection)?, 2);
            assert!(!table_exists(&connection, "schema_component")?);
        }

        let connection = open_store(&db_path)?;
        assert_eq!(schema_version(&connection)?, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            list_schema_components(&connection)?.len(),
            GENERATED_DATABASE_COMPONENTS.len()
        );
        validate_component_catalog(&connection)?;
        let backups = fs::read_dir(temp.path())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.contains(&format!("backup-schema-v2-to-v{}", CURRENT_SCHEMA_VERSION))
                    })
                    && path.extension().and_then(|value| value.to_str()) == Some("sqlite3")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1, "expected one verified migration backup");
        let backup_connection = Connection::open(&backups[0])?;
        assert_eq!(schema_version(&backup_connection)?, 2);
        assert!(!table_exists(&backup_connection, "schema_component")?);
        let backup_metadata: MigrationBackupInfo =
            serde_json::from_slice(&fs::read(backups[0].with_extension("json"))?)?;
        assert_eq!(backup_metadata.from_schema_version, 2);
        assert_eq!(backup_metadata.to_schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(backup_metadata.contract_hash, DATABASE_CONTRACT_HASH);
        drop(connection);
        let doctor = doctor_schema(&db_path);
        assert!(doctor.compatible);
        assert_eq!(doctor.last_backup.as_deref(), Some(backups[0].as_path()));
        assert!(doctor
            .recovery_command
            .as_deref()
            .is_some_and(|command| command.contains("cp '")));
        Ok(())
    }

    #[test]
    fn schema_doctor_reports_pending_migrations_without_mutation() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            downgrade_test_schema_to_v2(&connection)?;
        }
        let report = doctor_schema(&db_path);
        assert!(report.readable);
        assert!(report.compatible);
        assert_eq!(report.active_schema_version, Some(2));
        assert_eq!(report.pending_migrations, vec![2]);
        assert!(report.components.is_empty());
        assert!(report.last_backup.is_none());
        let connection = Connection::open(&db_path)?;
        assert_eq!(schema_version(&connection)?, 2);
        assert!(!table_exists(&connection, "schema_component")?);
        Ok(())
    }

    #[test]
    fn multi_step_migration_rolls_back_to_v1_after_injected_failure() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        let connection = Connection::open(&db_path)?;
        create_current_schema(&connection)?;
        downgrade_test_schema_to_v1(&connection)?;
        connection.execute(
            "INSERT INTO work_item (slug, title, description) VALUES ('survivor', 'Survivor', 'Synthetic')",
            [],
        )?;

        let error = apply_pending_schema_migrations(&connection, Some(2)).unwrap_err();
        assert!(format!("{error:#}").contains("injected migration failure"));
        assert_eq!(schema_version(&connection)?, 1);
        assert!(!table_exists(&connection, "work_dependency")?);
        assert!(!table_exists(&connection, "schema_component")?);
        let survivor: String = connection.query_row(
            "SELECT slug FROM work_item WHERE slug = 'survivor'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(survivor, "survivor");
        Ok(())
    }

    #[test]
    fn catalog_hash_mismatch_is_rejected_without_repair() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            connection.execute(
                "UPDATE schema_component SET contract_hash = 'sha256:tampered' WHERE namespace = 'research'",
                [],
            )?;
        }

        let error = open_store(&db_path).unwrap_err();
        assert!(
            format!("{error:#}").contains("contract hash does not match"),
            "{error:#}"
        );
        let connection = Connection::open(&db_path)?;
        let hash: String = connection.query_row(
            "SELECT contract_hash FROM schema_component WHERE namespace = 'research'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(hash, "sha256:tampered");
        Ok(())
    }

    #[test]
    fn withdrawn_v4_database_is_normalized_to_v2_without_losing_ingestion() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            connection.execute(
                "INSERT INTO component_ingest (
                    component_namespace, source_schema_version, source_contract_hash,
                    idempotency_key, payload_digest, record_count
                 ) VALUES ('research', 4, 'sha256:withdrawn-v4', 'preserved',
                           'sha256:preserved', 0)",
                [],
            )?;
            connection.execute(
                "UPDATE schema_component
                 SET minimum_core_schema = 4, contract_hash = 'sha256:withdrawn-v4'",
                [],
            )?;
            connection.execute(
                "UPDATE schema_component SET schema_version = 4 WHERE namespace = 'core'",
                [],
            )?;
            set_schema_version(&connection, 4)?;
        }

        let connection = open_store(&db_path)?;
        assert_eq!(schema_version(&connection)?, 2);
        validate_component_catalog(&connection)?;
        let preserved: i64 = connection.query_row(
            "SELECT COUNT(*) FROM component_ingest WHERE idempotency_key = 'preserved'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(preserved, 1);
        let backups = fs::read_dir(temp.path())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("backup-schema-v4-to-v2"))
                    && path.extension().and_then(|value| value.to_str()) == Some("sqlite3")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1, "expected one verified v4-to-v2 backup");
        let backup_connection = Connection::open(&backups[0])?;
        assert_eq!(schema_version(&backup_connection)?, 4);
        Ok(())
    }

    #[test]
    fn malformed_withdrawn_v4_catalog_rolls_back_normalization() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        {
            let connection = Connection::open(&db_path)?;
            create_current_schema(&connection)?;
            connection.execute(
                "INSERT INTO schema_component (
                    namespace, schema_version, minimum_core_schema, migration_digest, contract_hash
                 ) VALUES ('unknown-adapter', 1, 2, 'sha256:unknown', 'sha256:withdrawn-v4')",
                [],
            )?;
            connection.execute(
                "UPDATE schema_component SET contract_hash = 'sha256:withdrawn-v4'",
                [],
            )?;
            set_schema_version(&connection, 4)?;
        }

        let error = open_store(&db_path).unwrap_err();
        assert!(
            format!("{error:#}").contains("expected"),
            "unexpected error: {error:#}"
        );
        let connection = Connection::open(&db_path)?;
        assert_eq!(schema_version(&connection)?, 4);
        let unknown_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM schema_component WHERE namespace = 'unknown-adapter'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(unknown_count, 1);
        Ok(())
    }

    fn assert_core_tables_exist(connection: &Connection) -> anyhow::Result<()> {
        for table_name in CORE_TABLES {
            assert!(
                table_exists(connection, table_name)?,
                "missing {table_name}"
            );
        }
        Ok(())
    }

    fn assert_schema_shape_rejected(db_path: &Path) -> anyhow::Result<()> {
        let error = open_store(db_path).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("incompatible ldgr database schema")
                && message.contains("cannot be migrated automatically"),
            "{message}"
        );
        Ok(())
    }

    fn downgrade_test_schema_to_v1(connection: &Connection) -> anyhow::Result<()> {
        connection.execute_batch(
            r#"
            DROP TABLE component_record;
            DROP TABLE component_ingest;
            DROP TABLE schema_component;
            DROP TRIGGER IF EXISTS trg_work_dependency_no_cycle;
            DROP INDEX IF EXISTS idx_work_dependency_depends_on;
            DROP INDEX IF EXISTS idx_work_item_priority_program;
            DROP TABLE work_dependency;
            ALTER TABLE work_item DROP COLUMN hold_reason;
            ALTER TABLE work_item DROP COLUMN hold_kind;
            ALTER TABLE work_item DROP COLUMN acceptance_criteria;
            ALTER TABLE work_item DROP COLUMN work_group;
            ALTER TABLE work_item DROP COLUMN program;
            ALTER TABLE work_item DROP COLUMN priority;
            UPDATE schema_version SET version = 1 WHERE id = 1;
            "#,
        )?;
        Ok(())
    }

    fn downgrade_test_schema_to_v2(connection: &Connection) -> anyhow::Result<()> {
        connection.execute_batch(
            r#"
            DROP TABLE component_record;
            DROP TABLE component_ingest;
            DROP TABLE schema_component;
            UPDATE schema_version SET version = 2 WHERE id = 1;
            "#,
        )?;
        Ok(())
    }

    fn rebuild_work_item_without_status_check(connection: &Connection) -> anyhow::Result<()> {
        connection
            .execute_batch(
                r#"
                PRAGMA foreign_keys = OFF;
                ALTER TABLE work_item RENAME TO work_item_old;
                CREATE TABLE work_item (
                    id INTEGER PRIMARY KEY,
                    parent_work_item_id INTEGER REFERENCES work_item(id) ON DELETE SET NULL,
                    slug TEXT NOT NULL UNIQUE,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO work_item
                    (id, parent_work_item_id, slug, title, description, status, created_at, updated_at)
                SELECT id, parent_work_item_id, slug, title, description, status, created_at, updated_at
                FROM work_item_old;
                DROP TABLE work_item_old;
                PRAGMA foreign_keys = ON;
                "#,
            )
            .context("failed to rebuild work_item without status check")
    }

    fn rebuild_run_without_work_item_foreign_key(connection: &Connection) -> anyhow::Result<()> {
        connection
            .execute_batch(
                r#"
                PRAGMA foreign_keys = OFF;
                ALTER TABLE run RENAME TO run_old;
                CREATE TABLE run (
                    id INTEGER PRIMARY KEY,
                    work_item_id INTEGER NOT NULL,
                    command TEXT,
                    status TEXT NOT NULL DEFAULT 'running'
                        CHECK (status IN ('running', 'success', 'failed', 'partial')),
                    started_at TEXT NOT NULL DEFAULT (datetime('now')),
                    finished_at TEXT,
                    notes TEXT
                );
                INSERT INTO run
                    (id, work_item_id, command, status, started_at, finished_at, notes)
                SELECT id, work_item_id, command, status, started_at, finished_at, notes
                FROM run_old;
                DROP TABLE run_old;
                PRAGMA foreign_keys = ON;
                "#,
            )
            .context("failed to rebuild run without work_item foreign key")
    }
}
