use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::store::{
    doctor_schema, init_store_with_migration_info, open_store, open_store_with_migration_info,
    MigrationBackupInfo,
};

#[derive(Debug, Clone, Serialize)]
pub struct DatabaseAlignment {
    pub database: PathBuf,
    pub state: String,
    pub compatible: bool,
    pub active_schema_version: Option<i64>,
    pub target_schema_version: i64,
    pub contract_hash: String,
    pub migration: Option<DatabaseMigration>,
    pub issues: Vec<String>,
    pub recovery_command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DatabaseMigration {
    pub from_schema_version: i64,
    pub to_schema_version: i64,
    pub verified_backup: PathBuf,
}

pub(crate) fn align_existing_database(
    db: &Path,
) -> anyhow::Result<(Connection, DatabaseAlignment)> {
    let (connection, migration) =
        open_store_with_migration_info(db).map_err(|error| database_alignment_error(db, error))?;
    let alignment = verified_alignment(db, false, migration.as_ref())?;
    Ok((connection, alignment))
}

pub(crate) fn align_or_initialize_database(
    db: &Path,
    artifact_root: &Path,
) -> anyhow::Result<(Connection, DatabaseAlignment)> {
    let created = !db.exists();
    let migration = init_store_with_migration_info(db, artifact_root)
        .map_err(|error| database_alignment_error(db, error))?;
    let connection = open_store(db).map_err(|error| database_alignment_error(db, error))?;
    let alignment = verified_alignment(db, created, migration.as_ref())?;
    Ok((connection, alignment))
}

fn verified_alignment(
    db: &Path,
    created: bool,
    migration: Option<&MigrationBackupInfo>,
) -> anyhow::Result<DatabaseAlignment> {
    let doctor = doctor_schema(db);
    anyhow::ensure!(
        doctor.readable && doctor.compatible && doctor.pending_migrations.is_empty(),
        "database alignment did not converge for {}: {}",
        db.display(),
        doctor
            .problem
            .as_deref()
            .unwrap_or("schema doctor still reports pending work")
    );
    Ok(DatabaseAlignment {
        database: db.to_path_buf(),
        state: if created {
            "created"
        } else if migration.is_some() {
            "migrated"
        } else {
            "aligned"
        }
        .to_owned(),
        compatible: true,
        active_schema_version: doctor.active_schema_version,
        target_schema_version: doctor.target_schema_version,
        contract_hash: doctor.contract_hash.to_owned(),
        migration: migration.map(|migration| DatabaseMigration {
            from_schema_version: migration.from_schema_version,
            to_schema_version: migration.to_schema_version,
            verified_backup: migration.backup.clone(),
        }),
        issues: Vec::new(),
        recovery_command: doctor.recovery_command,
    })
}

fn database_alignment_error(db: &Path, error: anyhow::Error) -> anyhow::Error {
    let doctor = doctor_schema(db);
    let active = doctor
        .active_schema_version
        .map(|version| version.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let problem = doctor
        .problem
        .as_deref()
        .unwrap_or("database could not be aligned automatically");
    let recovery = doctor
        .recovery_command
        .as_deref()
        .unwrap_or("none available; preserve the database and inspect the doctor report");
    anyhow::anyhow!(
        "database alignment failed for {}: {error:#}\n\
         schema doctor: readable={} compatible={} active={} target={} contract={}\n\
         problem: {}\n\
         recovery: {}\n\
         inspect: ldgr --db \"{}\" schema doctor --json",
        db.display(),
        doctor.readable,
        doctor.compatible,
        active,
        doctor.target_schema_version,
        doctor.contract_hash,
        problem,
        recovery,
        db.display()
    )
}

pub(crate) fn print_migration_notice(alignment: &DatabaseAlignment) {
    let Some(migration) = &alignment.migration else {
        return;
    };
    eprintln!(
        "migration: LDGR Core upgraded schema v{} -> v{}; verified backup: {}",
        migration.from_schema_version,
        migration.to_schema_version,
        migration.verified_backup.display()
    );
}

pub(crate) fn print_database_alignment(alignment: &DatabaseAlignment) {
    println!(
        "database: {} schema-v{} contract={} state={}",
        alignment.database.display(),
        alignment
            .active_schema_version
            .map(|version| version.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        alignment.contract_hash,
        alignment.state
    );
    if let Some(migration) = &alignment.migration {
        println!("database_backup: {}", migration.verified_backup.display());
    }
}
