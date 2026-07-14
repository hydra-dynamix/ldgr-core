use std::collections::BTreeSet;

use anyhow::{bail, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::in_write_transaction;
use crate::database_contract::parse_and_validate_adapter_contract;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentIngestRecord {
    pub kind: String,
    pub key: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentIngestResult {
    pub ingest_id: i64,
    pub duplicate: bool,
    pub record_count: usize,
}

/// Explicit adapter-owned conversion for one supported legacy payload contract.
/// Core remains the only code allowed to commit the converted records.
pub trait ComponentIngestionTransform {
    fn component_namespace(&self) -> &str;
    fn accepts(&self, source_schema_version: i64, source_contract_hash: &str) -> bool;
    fn transform(
        &self,
        source_schema_version: i64,
        records: Vec<ComponentIngestRecord>,
    ) -> anyhow::Result<Vec<ComponentIngestRecord>>;
}

pub fn ingest_component_records(
    connection: &Connection,
    adapter_contract_json: &str,
    source_schema_version: i64,
    source_contract_hash: &str,
    idempotency_key: &str,
    records: Vec<ComponentIngestRecord>,
    legacy_transform: Option<&dyn ComponentIngestionTransform>,
) -> anyhow::Result<ComponentIngestResult> {
    let contract = parse_and_validate_adapter_contract(adapter_contract_json)?;
    anyhow::ensure!(
        source_schema_version > 0,
        "source schema version must be positive"
    );
    validate_token("source contract hash", source_contract_hash, 200)?;
    anyhow::ensure!(
        source_contract_hash.starts_with("sha256:"),
        "source contract hash must be a sha256 fingerprint"
    );
    validate_token("idempotency key", idempotency_key, 500)?;

    let current_version = contract.component.schema_version;
    let records = if source_schema_version == current_version {
        anyhow::ensure!(
            source_contract_hash == contract.contract_hash,
            "current payload contract hash does not match the active Core contract"
        );
        records
    } else if source_schema_version > current_version {
        bail!(
            "future {} payload schema v{} is not supported; active component schema is v{}",
            contract.component.namespace,
            source_schema_version,
            current_version
        );
    } else {
        let transform = legacy_transform
            .context("legacy payload requires an explicitly registered ingestion transform")?;
        anyhow::ensure!(
            transform.component_namespace() == contract.component.namespace,
            "legacy transform namespace does not match the adapter contract"
        );
        anyhow::ensure!(
            transform.accepts(source_schema_version, source_contract_hash),
            "legacy payload version or contract hash is not registered"
        );
        transform.transform(source_schema_version, records)?
    };

    validate_records(&records)?;
    let canonical = serde_json::to_vec(&records).context("failed to encode ingestion payload")?;
    let payload_digest = format!("sha256:{:x}", Sha256::digest(&canonical));
    let namespace = contract.component.namespace;
    let record_count = records.len();

    in_write_transaction(connection, |connection| {
        let existing = connection
            .query_row(
                "SELECT id, source_schema_version, source_contract_hash, payload_digest, record_count
                 FROM component_ingest
                 WHERE component_namespace = ?1 AND idempotency_key = ?2",
                params![namespace, idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((ingest_id, version, hash, digest, count)) = existing {
            anyhow::ensure!(
                version == source_schema_version
                    && hash == source_contract_hash
                    && digest == payload_digest
                    && count == record_count as i64,
                "idempotency key already identifies a different ingestion payload"
            );
            return Ok(ComponentIngestResult {
                ingest_id,
                duplicate: true,
                record_count,
            });
        }

        connection.execute(
            "INSERT INTO component_ingest
             (component_namespace, source_schema_version, source_contract_hash,
              idempotency_key, payload_digest, record_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                namespace,
                source_schema_version,
                source_contract_hash,
                idempotency_key,
                payload_digest,
                record_count as i64
            ],
        )?;
        let ingest_id = connection.last_insert_rowid();
        for record in &records {
            connection.execute(
                "INSERT INTO component_record
                 (ingest_id, record_kind, record_key, payload_json, source_schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    ingest_id,
                    record.kind,
                    record.key,
                    serde_json::to_string(&record.payload)?,
                    source_schema_version
                ],
            )?;
        }
        Ok(ComponentIngestResult {
            ingest_id,
            duplicate: false,
            record_count,
        })
    })
}

fn validate_token(label: &str, value: &str, maximum: usize) -> anyhow::Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{label} cannot be empty");
    anyhow::ensure!(value.len() <= maximum, "{label} exceeds {maximum} bytes");
    anyhow::ensure!(!value.contains('\0'), "{label} contains a null byte");
    Ok(())
}

fn validate_records(records: &[ComponentIngestRecord]) -> anyhow::Result<()> {
    anyhow::ensure!(!records.is_empty(), "ingestion payload cannot be empty");
    let mut identities = BTreeSet::new();
    for record in records {
        validate_token("record kind", &record.kind, 200)?;
        validate_token("record key", &record.key, 500)?;
        anyhow::ensure!(
            identities.insert((&record.kind, &record.key)),
            "duplicate record identity {}:{}",
            record.kind,
            record.key
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database_contract::{generated_adapter_contract_json, DATABASE_CONTRACT_HASH};
    use crate::store::open_store;
    use tempfile::TempDir;

    fn record(value: &str) -> ComponentIngestRecord {
        ComponentIngestRecord {
            kind: "finding".into(),
            key: "one".into(),
            payload: serde_json::json!({"value": value}),
        }
    }

    struct Legacy;
    impl ComponentIngestionTransform for Legacy {
        fn component_namespace(&self) -> &str {
            "research"
        }
        fn accepts(&self, version: i64, hash: &str) -> bool {
            version == 3 && hash == "sha256:legacy-research-v3"
        }
        fn transform(
            &self,
            _: i64,
            mut records: Vec<ComponentIngestRecord>,
        ) -> anyhow::Result<Vec<ComponentIngestRecord>> {
            for record in &mut records {
                record.payload["transformed"] = true.into();
            }
            Ok(records)
        }
    }

    #[test]
    fn current_ingestion_retains_provenance_and_retries_idempotently() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ledger.db"))?;
        let contract = generated_adapter_contract_json("example")?;
        let first = ingest_component_records(
            &connection,
            &contract,
            1,
            DATABASE_CONTRACT_HASH,
            "request-1",
            vec![record("a")],
            None,
        )?;
        let retry = ingest_component_records(
            &connection,
            &contract,
            1,
            DATABASE_CONTRACT_HASH,
            "request-1",
            vec![record("a")],
            None,
        )?;
        assert!(!first.duplicate);
        assert!(retry.duplicate);
        assert_eq!(first.ingest_id, retry.ingest_id);
        let provenance: (String, i64, String) = connection.query_row(
            "SELECT component_namespace, source_schema_version, source_contract_hash FROM component_ingest WHERE id=?1",
            [first.ingest_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            provenance,
            ("example".into(), 1, DATABASE_CONTRACT_HASH.into())
        );
        assert!(ingest_component_records(
            &connection,
            &contract,
            1,
            DATABASE_CONTRACT_HASH,
            "request-1",
            vec![record("different")],
            None
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn legacy_requires_registered_transform_and_bad_inputs_leave_no_writes() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ledger.db"))?;
        let contract = generated_adapter_contract_json("research")?;
        assert!(ingest_component_records(
            &connection,
            &contract,
            0,
            "sha256:bad",
            "bad-zero",
            vec![record("a")],
            None
        )
        .is_err());
        assert!(ingest_component_records(
            &connection,
            &contract,
            5,
            "sha256:future",
            "bad-future",
            vec![record("a")],
            None
        )
        .is_err());
        assert!(ingest_component_records(
            &connection,
            &contract,
            4,
            "sha256:wrong",
            "bad-hash",
            vec![record("a")],
            None
        )
        .is_err());
        assert!(ingest_component_records(
            &connection,
            &contract,
            3,
            "sha256:legacy-research-v3",
            "legacy",
            vec![record("a")],
            None
        )
        .is_err());
        let accepted = ingest_component_records(
            &connection,
            &contract,
            3,
            "sha256:legacy-research-v3",
            "legacy",
            vec![record("a")],
            Some(&Legacy),
        )?;
        assert_eq!(accepted.record_count, 1);
        let count: i64 =
            connection.query_row("SELECT count(*) FROM component_ingest", [], |row| {
                row.get(0)
            })?;
        assert_eq!(count, 1);
        Ok(())
    }
}
