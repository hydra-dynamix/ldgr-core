use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{bail, Context};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::{load_telemetry_consent, telemetry_kill_switch_active};

pub const EXPERIENCE_DONATION_ENDPOINT: &str = "/donations/experiences/v1";
pub const EXPERIENCE_DONATION_PENDING_DIRECTORY: &str = "experience-donation-pending";
pub const MAX_EXPERIENCE_DONATION_BYTES: usize = 2 * 1024 * 1024;
const EXPERIENCE_DONATION_POLICY_VERSION: u32 = 1;

pub fn validate_experience_donation_payload(payload: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !payload.is_empty() && payload.len() <= MAX_EXPERIENCE_DONATION_BYTES,
        "experience donation payload is outside the v1 byte limit"
    );
    let value: Value = serde_json::from_slice(payload)?;
    anyhow::ensure!(
        value["schema"] == "experience-donation/v1",
        "invalid donation schema"
    );
    anyhow::ensure!(
        value["consent"]["program"] == "experience-donation"
            && value["consent"]["decision"] == "enabled"
            && value["consent"]["policy_version"] == EXPERIENCE_DONATION_POLICY_VERSION,
        "invalid donation consent attestation"
    );
    anyhow::ensure!(
        value["source"]["system"] == "ldgr-core",
        "unsupported donation source"
    );
    anyhow::ensure!(
        value["episode"]["schema"] == "ldgr-work-episode/v1",
        "unsupported donation episode schema"
    );
    let digest = value["episode"]["source_sha256"]
        .as_str()
        .unwrap_or_default();
    anyhow::ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid donation source digest"
    );
    Ok(())
}

pub fn experience_donation_is_eligible(ldgr_home: &Path) -> bool {
    !telemetry_kill_switch_active()
        && load_telemetry_consent(ldgr_home)
            .map(|consent| consent.donation_enabled())
            .unwrap_or(false)
}

pub fn pending_experience_donation_count(ldgr_home: &Path) -> anyhow::Result<usize> {
    let route = ldgr_home
        .join(EXPERIENCE_DONATION_PENDING_DIRECTORY)
        .join("experiences/v1");
    match fs::symlink_metadata(&route) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(fs::read_dir(route)?
                .filter_map(Result::ok)
                .filter(|entry| {
                    fs::symlink_metadata(entry.path()).is_ok_and(|metadata| {
                        metadata.file_type().is_file() && !metadata.file_type().is_symlink()
                    })
                })
                .count())
        }
        Ok(_) => bail!("experience donation route is not a real directory"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

pub fn clear_unsent_experience_donations(ldgr_home: &Path) -> anyhow::Result<()> {
    let pending = ldgr_home.join(EXPERIENCE_DONATION_PENDING_DIRECTORY);
    match fs::symlink_metadata(&pending) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(&pending).with_context(|| {
                format!(
                    "failed to clear unsent experience donations {}",
                    pending.display()
                )
            })
        }
        Ok(_) => bail!(
            "experience donation queue path {} is not a real directory",
            pending.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect experience donation queue {}",
                pending.display()
            )
        }),
    }
}

pub(crate) fn queue_completed_run(
    connection: &Connection,
    ldgr_home: &Path,
    run_id: i64,
) -> anyhow::Result<bool> {
    if !experience_donation_is_eligible(ldgr_home) {
        return Ok(false);
    }
    let episode = build_work_episode(connection, run_id)?;
    let source_bytes = serde_json::to_vec(&episode)?;
    let source_sha256 = hex_digest(&source_bytes);
    let episode_id = format!("WEP-{}", &source_sha256[..32]);
    let envelope = json!({
        "schema": "experience-donation/v1",
        "consent": {
            "program": "experience-donation",
            "decision": "enabled",
            "policy_version": EXPERIENCE_DONATION_POLICY_VERSION,
        },
        "source": {
            "system": "ldgr-core",
            "system_version": env!("CARGO_PKG_VERSION"),
        },
        "episode": {
            "schema": "ldgr-work-episode/v1",
            "episode_id": episode_id,
            "source_sha256": source_sha256,
            "material": episode,
        },
    });
    let payload = serde_json::to_vec(&envelope)?;
    anyhow::ensure!(
        payload.len() <= MAX_EXPERIENCE_DONATION_BYTES,
        "experience donation exceeds the v1 body limit"
    );
    persist_pending(ldgr_home, &payload)?;
    Ok(true)
}

fn build_work_episode(connection: &Connection, run_id: i64) -> anyhow::Result<Value> {
    let (work_item_id, started_at, completed_at, run) = connection
        .query_row(
            "SELECT run.work_item_id, run.started_at, run.finished_at,
                    json_object(
                      'id', run.id,
                      'work_item_id', run.work_item_id,
                      'command', run.command,
                      'status', run.status,
                      'started_at', run.started_at,
                      'completed_at', run.finished_at,
                      'notes', run.notes
                    )
             FROM run
             WHERE run.id = ?1 AND run.finished_at IS NOT NULL",
            params![run_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .context("failed to read completed run for experience donation")?;
    let work: String = connection.query_row(
        "SELECT json_object(
           'id', id,
           'parent_work_item_id', parent_work_item_id,
           'slug', slug,
           'title', title,
           'description', description,
           'acceptance_criteria', acceptance_criteria,
           'program', program,
           'work_group', work_group,
           'priority', priority,
           'status', status,
           'created_at', created_at,
           'updated_at', updated_at
         )
         FROM work_item WHERE id = ?1",
        params![work_item_id],
        |row| row.get(0),
    )?;
    let project_id = connection
        .query_row(
            "SELECT project_id FROM project_identity LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "unknown".to_owned());

    let start_event_id: i64 = connection.query_row(
        "SELECT MIN(id) FROM event_log
         WHERE entity_type = 'run' AND entity_id = ?1 AND event_type = 'start'",
        params![run_id],
        |row| row.get(0),
    )?;
    let finish_event_id: i64 = connection.query_row(
        "SELECT MAX(id) FROM event_log
         WHERE entity_type = 'run' AND entity_id = ?1 AND event_type = 'finish'",
        params![run_id],
        |row| row.get(0),
    )?;

    let events = query_json_rows(
        connection,
        "SELECT json_object(
           'id', id,
           'entity_type', entity_type,
           'entity_id', entity_id,
           'event_type', event_type,
           'payload', json(payload_json),
           'created_at', created_at
         )
         FROM event_log
         WHERE id BETWEEN ?1 AND ?2
           AND ((entity_type = 'run' AND entity_id = ?3)
             OR (entity_type = 'work_item' AND entity_id = ?4))
         ORDER BY id",
        params![start_event_id, finish_event_id, run_id, work_item_id],
    )?;
    let observations = query_json_rows(
        connection,
        "SELECT json_object('id', id, 'body', body, 'created_at', created_at)
         FROM observation WHERE run_id = ?1 ORDER BY id",
        params![run_id],
    )?;
    let artifacts = query_json_rows(
        connection,
        "SELECT json_object(
           'id', id, 'kind', kind, 'path', path,
           'description', description, 'created_at', created_at
         )
         FROM artifact WHERE run_id = ?1 ORDER BY id",
        params![run_id],
    )?;
    let decisions = query_json_rows(
        connection,
        "SELECT json_object(
           'id', id, 'outcome', outcome, 'rationale', rationale,
           'next_work_item_id', next_work_item_id, 'created_at', created_at
         )
         FROM decision
         WHERE work_item_id = ?1 AND created_at >= ?2
         ORDER BY id",
        params![work_item_id, started_at],
    )?;
    let errors = query_json_rows(
        connection,
        "SELECT DISTINCT json_object(
           'id', er.id, 'class', er.class, 'domain', er.domain, 'code', er.code,
           'severity', er.severity, 'state', er.state, 'summary', eo.summary,
           'created_at', eo.recorded_at
         )
         FROM error_record er
         JOIN error_occurrence eo ON eo.error_id = er.id
         JOIN error_relation rel ON rel.error_id = er.id
         WHERE eo.recorded_at >= ?1 AND eo.recorded_at <= ?2
           AND ((rel.entity_type = 'run' AND rel.entity_id = CAST(?3 AS TEXT))
             OR (rel.entity_type = 'work_item' AND rel.entity_id = CAST(?4 AS TEXT)))
         ORDER BY eo.recorded_at, er.id",
        params![started_at, completed_at, run_id, work_item_id],
    )?;

    Ok(json!({
        "source_schema_version": crate::store::CURRENT_SCHEMA_VERSION,
        "source_schema_fingerprint": crate::database_contract::DATABASE_CONTRACT_HASH
            .strip_prefix("sha256:")
            .unwrap_or(crate::database_contract::DATABASE_CONTRACT_HASH),
        "project_id": project_id,
        "work_item": parse_json(&work)?,
        "run": parse_json(&run)?,
        "events": events,
        "observations": observations,
        "artifacts": artifacts,
        "decisions": decisions,
        "errors": errors,
        "terminal_status": parse_json(&run)?["status"],
    }))
}

fn query_json_rows<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> anyhow::Result<Vec<Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(parameters, |row| row.get::<_, String>(0))?;
    rows.map(|row| parse_json(&row?)).collect()
}

fn parse_json(value: &str) -> anyhow::Result<Value> {
    serde_json::from_str(value).context("failed to parse donated ledger JSON")
}

fn persist_pending(ldgr_home: &Path, payload: &[u8]) -> anyhow::Result<()> {
    let pending_root = ldgr_home.join(EXPERIENCE_DONATION_PENDING_DIRECTORY);
    ensure_real_directory(&pending_root)?;
    let experience_root = pending_root.join("experiences");
    ensure_real_directory(&experience_root)?;
    let destination = experience_root.join("v1");
    ensure_real_directory(&destination)?;
    let mut pending = NamedTempFile::new_in(&destination)
        .context("failed to create pending experience donation")?;
    pending
        .write_all(payload)
        .context("failed to write pending experience donation")?;
    pending
        .flush()
        .context("failed to flush pending experience donation")?;
    pending
        .as_file()
        .sync_all()
        .context("failed to sync pending experience donation")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        pending
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    pending
        .keep()
        .map_err(|error| error.error)
        .context("failed to preserve pending experience donation")?;
    sync_directory(&destination)
}

fn ensure_real_directory(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!(
            "experience donation queue path is not a real directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(path)
            .with_context(|| format!("failed to create donation queue {}", path.display()))?,
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> anyhow::Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn donation(source: &str, episode_schema: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": "experience-donation/v1",
            "consent": {
                "program": "experience-donation",
                "decision": "enabled",
                "policy_version": 1
            },
            "source": {"system": source, "system_version": "0.1.18"},
            "episode": {
                "schema": episode_schema,
                "source_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }))
        .expect("fixture must serialize")
    }

    #[test]
    fn donation_boundary_accepts_only_core_ledger_episodes() {
        assert!(validate_experience_donation_payload(&donation(
            "ldgr-core",
            "ldgr-work-episode/v1"
        ))
        .is_ok());
        assert!(validate_experience_donation_payload(&donation(
            "pi-ldgr-memory",
            "pi-harness-work-episode/v3"
        ))
        .is_err());
        assert!(validate_experience_donation_payload(&donation(
            "pi-ldgr-memory",
            "ldgr-work-episode/v1"
        ))
        .is_err());
    }
}
