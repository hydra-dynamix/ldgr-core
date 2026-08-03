use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use super::{get_error, get_error_occurrence, ErrorOccurrence, ErrorRecord, ErrorRelation};

pub const DEFAULT_ERROR_CONTEXT_LIMIT: usize = 5;
pub const MAX_ERROR_CONTEXT_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ErrorContextBounds {
    pub prior_occurrences: usize,
    pub relations: usize,
    pub related_work_items: usize,
    pub related_runs: usize,
    pub dispositions: usize,
    pub decisions: usize,
    pub artifacts: usize,
    pub validations: usize,
    pub environment_differences: usize,
}

impl ErrorContextBounds {
    pub fn uniform(limit: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            limit <= MAX_ERROR_CONTEXT_LIMIT,
            "context limit must not exceed {MAX_ERROR_CONTEXT_LIMIT}"
        );
        Ok(Self {
            prior_occurrences: limit,
            relations: limit,
            related_work_items: limit,
            related_runs: limit,
            dispositions: limit,
            decisions: limit,
            artifacts: limit,
            validations: limit,
            environment_differences: limit,
        })
    }
}

impl Default for ErrorContextBounds {
    fn default() -> Self {
        Self::uniform(DEFAULT_ERROR_CONTEXT_LIMIT).expect("default error context limit is valid")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextSection {
    pub available: usize,
    pub included: usize,
    pub truncated: bool,
}

impl ContextSection {
    fn new(available: usize, included: usize) -> Self {
        Self {
            available,
            included,
            truncated: available > included,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextOccurrence {
    pub occurrence_id: String,
    pub operation_id: String,
    pub attempt_id: String,
    pub class: String,
    pub domain: String,
    pub code: String,
    pub severity: String,
    pub retryability: String,
    pub source: String,
    pub summary: String,
    pub details: Value,
    pub environment: Value,
    pub observed_at: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedWorkContext {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub hold_kind: Option<String>,
    pub hold_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedRunContext {
    pub id: i64,
    pub work_item_id: i64,
    pub work_slug: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorDispositionContext {
    pub id: i64,
    pub occurrence_id: String,
    pub disposition: String,
    pub actor: String,
    pub source: String,
    pub rationale: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedDecisionContext {
    pub id: i64,
    pub work_item_id: i64,
    pub work_slug: String,
    pub outcome: String,
    pub rationale: String,
    pub next_work_slug: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedArtifactContext {
    pub id: i64,
    pub run_id: i64,
    pub kind: String,
    pub path: String,
    pub description: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedValidationContext {
    pub id: i64,
    pub run_id: i64,
    pub outcome: String,
    pub command: Option<String>,
    pub rationale: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvironmentDifference {
    pub prior_occurrence_id: String,
    pub key: String,
    pub prior: Option<Value>,
    pub current: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct ContextRedaction {
    pub sensitive_values: usize,
    pub home_paths: usize,
    pub truncated_values: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorContextPacket {
    pub format: &'static str,
    pub schema_version: u32,
    pub error: ErrorRecord,
    pub repeated: bool,
    pub current_occurrence_id: String,
    pub prior_occurrences: Vec<ContextOccurrence>,
    pub relations: Vec<ErrorRelation>,
    pub related_work_items: Vec<RelatedWorkContext>,
    pub related_runs: Vec<RelatedRunContext>,
    pub dispositions: Vec<ErrorDispositionContext>,
    pub decisions: Vec<RelatedDecisionContext>,
    pub artifacts: Vec<RelatedArtifactContext>,
    pub validations: Vec<RelatedValidationContext>,
    pub environment_differences: Vec<EnvironmentDifference>,
    pub bounds: ErrorContextBounds,
    pub sections: BTreeMap<String, ContextSection>,
    pub redaction: ContextRedaction,
}

pub fn error_context_packet(
    connection: &Connection,
    error_id: i64,
    occurrence_id: Option<&str>,
    bounds: ErrorContextBounds,
) -> anyhow::Result<ErrorContextPacket> {
    let error = get_error(connection, error_id)?;
    let current = match occurrence_id {
        Some(occurrence_id) => {
            let occurrence = get_error_occurrence(connection, occurrence_id)?;
            anyhow::ensure!(
                occurrence.error_id == error_id,
                "occurrence {occurrence_id} does not belong to error {error_id}"
            );
            occurrence
        }
        None => get_error_occurrence(connection, &error.latest_occurrence_id)?,
    };
    let mut sections = BTreeMap::new();
    let mut redaction = ContextRedaction::default();

    let prior_available = count_prior_occurrences(connection, &current)?;
    let prior = prior_occurrences(connection, &current, bounds.prior_occurrences)?;
    let prior_occurrences = prior
        .iter()
        .map(|occurrence| context_occurrence(occurrence, &mut redaction))
        .collect::<Vec<_>>();
    sections.insert(
        "prior_occurrences".to_owned(),
        ContextSection::new(prior_available, prior_occurrences.len()),
    );

    let relation_available: usize = connection.query_row(
        "SELECT COUNT(*) FROM error_relation WHERE error_id=?1",
        params![error_id],
        |row| row.get(0),
    )?;
    let mut relations = list_relations(connection, error_id, bounds.relations)?;
    sections.insert(
        "relations".to_owned(),
        ContextSection::new(relation_available, relations.len()),
    );

    let mut work_ids = BTreeSet::new();
    let mut run_ids = BTreeSet::new();
    let mut decision_ids = BTreeSet::new();
    let mut artifact_ids = BTreeSet::new();
    let mut validation_ids = BTreeSet::new();
    collect_relation_ids(
        connection,
        &relations,
        &mut work_ids,
        &mut run_ids,
        &mut decision_ids,
        &mut artifact_ids,
        &mut validation_ids,
    )?;
    for relation in &mut relations {
        relation.source = redact_text(&relation.source, &mut redaction, 256);
    }

    let mut related_runs = BTreeMap::new();
    for run_id in run_ids.clone() {
        if let Some(run) = load_run(connection, run_id)? {
            work_ids.insert(run.work_item_id);
            related_runs.insert(run.id, run);
        }
    }
    for work_id in work_ids.clone() {
        for run in latest_runs_for_work(connection, work_id, bounds.related_runs)? {
            run_ids.insert(run.id);
            related_runs.entry(run.id).or_insert(run);
        }
    }
    let related_run_available = count_related_runs(connection, &work_ids, &run_ids)?;
    let mut related_runs = related_runs
        .into_values()
        .map(|mut run| {
            run.notes = run
                .notes
                .map(|value| redact_text(&value, &mut redaction, 1024));
            run
        })
        .collect::<Vec<_>>();
    related_runs.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    related_runs.truncate(bounds.related_runs);
    let related_runs_truncated = related_run_available > related_runs.len();
    sections.insert(
        "related_runs".to_owned(),
        derived_section(
            related_run_available,
            related_runs.len(),
            relation_available > relations.len(),
        ),
    );

    let mut related_work_items = Vec::new();
    for work_id in work_ids.iter().rev() {
        if related_work_items.len() == bounds.related_work_items {
            break;
        }
        if let Some(mut work) = load_work(connection, *work_id)? {
            work.title = redact_text(&work.title, &mut redaction, 512);
            work.hold_reason = work
                .hold_reason
                .map(|value| redact_text(&value, &mut redaction, 1024));
            related_work_items.push(work);
        }
    }
    sections.insert(
        "related_work_items".to_owned(),
        derived_section(
            work_ids.len(),
            related_work_items.len(),
            relation_available > relations.len(),
        ),
    );

    let disposition_available: usize = connection.query_row(
        "SELECT COUNT(*) FROM error_disposition WHERE error_id=?1",
        params![error_id],
        |row| row.get(0),
    )?;
    let dispositions =
        load_dispositions(connection, error_id, bounds.dispositions, &mut redaction)?;
    sections.insert(
        "dispositions".to_owned(),
        ContextSection::new(disposition_available, dispositions.len()),
    );

    let (decisions, decision_available) = load_decisions(
        connection,
        &work_ids,
        &decision_ids,
        bounds.decisions,
        &mut redaction,
    )?;
    sections.insert(
        "decisions".to_owned(),
        derived_section(
            decision_available,
            decisions.len(),
            relation_available > relations.len(),
        ),
    );

    let all_run_ids = related_runs
        .iter()
        .map(|run| run.id)
        .collect::<BTreeSet<_>>();
    let (artifacts, artifact_available) = load_artifacts(
        connection,
        &all_run_ids,
        &artifact_ids,
        bounds.artifacts,
        &mut redaction,
    )?;
    sections.insert(
        "artifacts".to_owned(),
        derived_section(
            artifact_available,
            artifacts.len(),
            relation_available > relations.len() || related_runs_truncated,
        ),
    );

    let (validations, validation_available) = load_validations(
        connection,
        &all_run_ids,
        &validation_ids,
        bounds.validations,
        &mut redaction,
    )?;
    sections.insert(
        "validations".to_owned(),
        derived_section(
            validation_available,
            validations.len(),
            relation_available > relations.len() || related_runs_truncated,
        ),
    );

    let mut differences = environment_differences(&current, &prior, &mut redaction);
    let difference_available = differences.len();
    differences.truncate(bounds.environment_differences);
    sections.insert(
        "environment_differences".to_owned(),
        derived_section(
            difference_available,
            differences.len(),
            prior_available > prior.len(),
        ),
    );

    Ok(ErrorContextPacket {
        format: "ldgr-error-context",
        schema_version: 1,
        repeated: error.occurrence_count > 1,
        error,
        current_occurrence_id: current.occurrence_id,
        prior_occurrences,
        relations,
        related_work_items,
        related_runs,
        dispositions,
        decisions,
        artifacts,
        validations,
        environment_differences: differences,
        bounds,
        sections,
        redaction,
    })
}

fn derived_section(
    available: usize,
    included: usize,
    dependency_truncated: bool,
) -> ContextSection {
    let mut section = ContextSection::new(available, included);
    section.truncated |= dependency_truncated;
    section
}

fn count_prior_occurrences(
    connection: &Connection,
    current: &ErrorOccurrence,
) -> anyhow::Result<usize> {
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM error_occurrence
         WHERE error_id=?1 AND (
            julianday(observed_at) < julianday(?2) OR
            (julianday(observed_at) = julianday(?2) AND julianday(recorded_at) < julianday(?3)) OR
            (julianday(observed_at) = julianday(?2) AND julianday(recorded_at) = julianday(?3) AND occurrence_id < ?4)
         )",
        params![
            current.error_id,
            current.observed_at,
            current.recorded_at,
            current.occurrence_id
        ],
        |row| row.get(0),
    )?)
}

fn prior_occurrences(
    connection: &Connection,
    current: &ErrorOccurrence,
    limit: usize,
) -> anyhow::Result<Vec<ErrorOccurrence>> {
    let mut statement = connection.prepare(
        "SELECT occurrence_id, error_id, producer, idempotency_key, operation_id, attempt_id,
                class, domain, code, severity, retryability, source, summary, details_json,
                environment_json, observed_at, recorded_at, recovery_origin, payload_digest
         FROM error_occurrence
         WHERE error_id=?1 AND (
            julianday(observed_at) < julianday(?2) OR
            (julianday(observed_at) = julianday(?2) AND julianday(recorded_at) < julianday(?3)) OR
            (julianday(observed_at) = julianday(?2) AND julianday(recorded_at) = julianday(?3) AND occurrence_id < ?4)
         )
         ORDER BY julianday(observed_at) DESC, julianday(recorded_at) DESC, occurrence_id DESC LIMIT ?5",
    )?;
    let result = statement
        .query_map(
            params![
                current.error_id,
                current.observed_at,
                current.recorded_at,
                current.occurrence_id,
                limit
            ],
            occurrence_from_context_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into);
    result
}

fn occurrence_from_context_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ErrorOccurrence> {
    let parse_json = |column: &str| -> rusqlite::Result<Value> {
        serde_json::from_str(&row.get::<_, String>(column)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    };
    Ok(ErrorOccurrence {
        occurrence_id: row.get("occurrence_id")?,
        error_id: row.get("error_id")?,
        producer: row.get("producer")?,
        idempotency_key: row.get("idempotency_key")?,
        operation_id: row.get("operation_id")?,
        attempt_id: row.get("attempt_id")?,
        class: parse_context_enum(row, "class")?,
        domain: row.get("domain")?,
        code: row.get("code")?,
        severity: parse_context_enum(row, "severity")?,
        retryability: parse_context_enum(row, "retryability")?,
        source: row.get("source")?,
        summary: row.get("summary")?,
        details: parse_json("details_json")?,
        environment: parse_json("environment_json")?,
        observed_at: row.get("observed_at")?,
        recorded_at: row.get("recorded_at")?,
        recovery_origin: parse_context_enum(row, "recovery_origin")?,
        payload_digest: row.get("payload_digest")?,
    })
}

fn parse_context_enum<T>(row: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    row.get::<_, String>(column)?
        .parse()
        .map_err(|error: T::Err| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error.to_string(),
                )),
            )
        })
}

fn list_relations(
    connection: &Connection,
    error_id: i64,
    limit: usize,
) -> anyhow::Result<Vec<ErrorRelation>> {
    let mut statement = connection.prepare(
        "SELECT id, error_id, occurrence_id, relation_kind, entity_type, entity_id, source, created_at
         FROM error_relation WHERE error_id=?1
         ORDER BY created_at DESC, id DESC LIMIT ?2",
    )?;
    let result = statement
        .query_map(params![error_id, limit], |row| {
            Ok(ErrorRelation {
                id: row.get("id")?,
                error_id: row.get("error_id")?,
                occurrence_id: row.get("occurrence_id")?,
                relation_kind: row.get("relation_kind")?,
                entity_type: row.get("entity_type")?,
                entity_id: row.get("entity_id")?,
                source: row.get("source")?,
                created_at: row.get("created_at")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into);
    result
}

#[allow(clippy::too_many_arguments)]
fn collect_relation_ids(
    connection: &Connection,
    relations: &[ErrorRelation],
    work_ids: &mut BTreeSet<i64>,
    run_ids: &mut BTreeSet<i64>,
    decision_ids: &mut BTreeSet<i64>,
    artifact_ids: &mut BTreeSet<i64>,
    validation_ids: &mut BTreeSet<i64>,
) -> anyhow::Result<()> {
    for relation in relations {
        let Ok(id) = relation.entity_id.parse::<i64>() else {
            continue;
        };
        match relation.entity_type.as_str() {
            "work_item" => {
                work_ids.insert(id);
            }
            "run" => {
                run_ids.insert(id);
            }
            "decision" => {
                decision_ids.insert(id);
                if let Some(work_id) = connection
                    .query_row(
                        "SELECT work_item_id FROM decision WHERE id=?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()?
                {
                    work_ids.insert(work_id);
                }
            }
            "artifact" => {
                artifact_ids.insert(id);
                if let Some(run_id) = connection
                    .query_row(
                        "SELECT run_id FROM artifact WHERE id=?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()?
                {
                    run_ids.insert(run_id);
                }
            }
            "validation" => {
                validation_ids.insert(id);
                if let Some(run_id) = connection
                    .query_row(
                        "SELECT entity_id FROM event_log
                         WHERE id=?1 AND entity_type='run' AND event_type='validation'",
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()?
                {
                    run_ids.insert(run_id);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn load_work(connection: &Connection, work_id: i64) -> anyhow::Result<Option<RelatedWorkContext>> {
    Ok(connection
        .query_row(
            "SELECT id, slug, title, status, hold_kind, hold_reason
             FROM work_item WHERE id=?1",
            params![work_id],
            |row| {
                Ok(RelatedWorkContext {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    title: row.get(2)?,
                    status: row.get(3)?,
                    hold_kind: row.get(4)?,
                    hold_reason: row.get(5)?,
                })
            },
        )
        .optional()?)
}

fn load_run(connection: &Connection, run_id: i64) -> anyhow::Result<Option<RelatedRunContext>> {
    Ok(connection
        .query_row(
            "SELECT run.id, run.work_item_id, work_item.slug, run.status, run.started_at,
                    run.finished_at, run.notes
             FROM run JOIN work_item ON work_item.id=run.work_item_id
             WHERE run.id=?1",
            params![run_id],
            run_context_from_row,
        )
        .optional()?)
}

fn latest_runs_for_work(
    connection: &Connection,
    work_id: i64,
    limit: usize,
) -> anyhow::Result<Vec<RelatedRunContext>> {
    let mut statement = connection.prepare(
        "SELECT run.id, run.work_item_id, work_item.slug, run.status, run.started_at,
                run.finished_at, run.notes
         FROM run JOIN work_item ON work_item.id=run.work_item_id
         WHERE run.work_item_id=?1 ORDER BY run.started_at DESC, run.id DESC LIMIT ?2",
    )?;
    let result = statement
        .query_map(params![work_id, limit], run_context_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into);
    result
}

fn run_context_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RelatedRunContext> {
    Ok(RelatedRunContext {
        id: row.get(0)?,
        work_item_id: row.get(1)?,
        work_slug: row.get(2)?,
        status: row.get(3)?,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        notes: row.get(6)?,
    })
}

fn count_related_runs(
    connection: &Connection,
    work_ids: &BTreeSet<i64>,
    explicit_run_ids: &BTreeSet<i64>,
) -> anyhow::Result<usize> {
    let mut ids = explicit_run_ids.clone();
    for work_id in work_ids {
        let mut statement = connection.prepare("SELECT id FROM run WHERE work_item_id=?1")?;
        for row in statement.query_map(params![work_id], |row| row.get(0))? {
            ids.insert(row?);
        }
    }
    Ok(ids.len())
}

fn load_dispositions(
    connection: &Connection,
    error_id: i64,
    limit: usize,
    redaction: &mut ContextRedaction,
) -> anyhow::Result<Vec<ErrorDispositionContext>> {
    let mut statement = connection.prepare(
        "SELECT id, occurrence_id, disposition, actor, source, rationale, created_at
         FROM error_disposition WHERE error_id=?1
         ORDER BY created_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![error_id, limit], |row| {
            Ok(ErrorDispositionContext {
                id: row.get(0)?,
                occurrence_id: row.get(1)?,
                disposition: row.get(2)?,
                actor: row.get(3)?,
                source: row.get(4)?,
                rationale: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|mut disposition| {
            disposition.actor = redact_text(&disposition.actor, redaction, 128);
            disposition.source = redact_text(&disposition.source, redaction, 256);
            disposition.rationale = redact_text(&disposition.rationale, redaction, 1024);
            disposition
        })
        .collect())
}

fn load_decisions(
    connection: &Connection,
    work_ids: &BTreeSet<i64>,
    explicit_ids: &BTreeSet<i64>,
    limit: usize,
    redaction: &mut ContextRedaction,
) -> anyhow::Result<(Vec<RelatedDecisionContext>, usize)> {
    let mut ids = explicit_ids.clone();
    for work_id in work_ids {
        let mut statement = connection.prepare("SELECT id FROM decision WHERE work_item_id=?1")?;
        for row in statement.query_map(params![work_id], |row| row.get(0))? {
            ids.insert(row?);
        }
    }
    let available = ids.len();
    let mut result = Vec::new();
    for id in ids.iter().rev().take(limit) {
        if let Some(mut decision) = connection
            .query_row(
                "SELECT decision.id, decision.work_item_id, work_item.slug, decision.outcome,
                        decision.rationale, next.slug, decision.created_at
                 FROM decision
                 JOIN work_item ON work_item.id=decision.work_item_id
                 LEFT JOIN work_item next ON next.id=decision.next_work_item_id
                 WHERE decision.id=?1",
                params![id],
                |row| {
                    Ok(RelatedDecisionContext {
                        id: row.get(0)?,
                        work_item_id: row.get(1)?,
                        work_slug: row.get(2)?,
                        outcome: row.get(3)?,
                        rationale: row.get(4)?,
                        next_work_slug: row.get(5)?,
                        created_at: row.get(6)?,
                    })
                },
            )
            .optional()?
        {
            decision.rationale = redact_text(&decision.rationale, redaction, 1024);
            result.push(decision);
        }
    }
    Ok((result, available))
}

fn load_artifacts(
    connection: &Connection,
    run_ids: &BTreeSet<i64>,
    explicit_ids: &BTreeSet<i64>,
    limit: usize,
    redaction: &mut ContextRedaction,
) -> anyhow::Result<(Vec<RelatedArtifactContext>, usize)> {
    let mut ids = explicit_ids.clone();
    for run_id in run_ids {
        let mut statement = connection.prepare("SELECT id FROM artifact WHERE run_id=?1")?;
        for row in statement.query_map(params![run_id], |row| row.get(0))? {
            ids.insert(row?);
        }
    }
    let available = ids.len();
    let mut result = Vec::new();
    for id in ids.iter().rev().take(limit) {
        if let Some(mut artifact) = connection
            .query_row(
                "SELECT id, run_id, kind, path, description, created_at
                 FROM artifact WHERE id=?1",
                params![id],
                |row| {
                    Ok(RelatedArtifactContext {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        kind: row.get(2)?,
                        path: row.get(3)?,
                        description: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()?
        {
            artifact.path = redact_text(&artifact.path, redaction, 512);
            artifact.description = redact_text(&artifact.description, redaction, 1024);
            result.push(artifact);
        }
    }
    Ok((result, available))
}

fn load_validations(
    connection: &Connection,
    run_ids: &BTreeSet<i64>,
    explicit_ids: &BTreeSet<i64>,
    limit: usize,
    redaction: &mut ContextRedaction,
) -> anyhow::Result<(Vec<RelatedValidationContext>, usize)> {
    let mut ids = explicit_ids.clone();
    for run_id in run_ids {
        let mut statement = connection.prepare(
            "SELECT id FROM event_log
             WHERE entity_type='run' AND event_type='validation' AND entity_id=?1",
        )?;
        for row in statement.query_map(params![run_id], |row| row.get(0))? {
            ids.insert(row?);
        }
    }
    let available = ids.len();
    let mut result = Vec::new();
    for id in ids.iter().rev().take(limit) {
        let row = connection
            .query_row(
                "SELECT id, entity_id, payload_json, created_at FROM event_log
                 WHERE id=?1 AND entity_type='run' AND event_type='validation'",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, run_id, payload, created_at)) = row {
            let payload: Value = serde_json::from_str(&payload)?;
            result.push(RelatedValidationContext {
                id,
                run_id,
                outcome: payload
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                command: payload
                    .get("command")
                    .and_then(Value::as_str)
                    .map(|value| redact_text(value, redaction, 1024)),
                rationale: payload
                    .get("rationale")
                    .and_then(Value::as_str)
                    .map(|value| redact_text(value, redaction, 1024)),
                created_at,
            });
        }
    }
    Ok((result, available))
}

fn environment_differences(
    current: &ErrorOccurrence,
    prior: &[ErrorOccurrence],
    redaction: &mut ContextRedaction,
) -> Vec<EnvironmentDifference> {
    const MEANINGFUL_KEYS: &[&str] = &[
        "arch",
        "cargo_home",
        "ci",
        "container",
        "executable",
        "family",
        "harness",
        "home_available",
        "os",
        "runtime",
        "rustup_home",
        "shell",
        "toolchain",
        "wsl",
    ];
    let current_environment = current.environment.as_object();
    let mut differences = Vec::new();
    for occurrence in prior {
        let prior_environment = occurrence.environment.as_object();
        for key in MEANINGFUL_KEYS {
            let before = prior_environment.and_then(|values| values.get(*key));
            let after = current_environment.and_then(|values| values.get(*key));
            if before != after {
                differences.push(EnvironmentDifference {
                    prior_occurrence_id: occurrence.occurrence_id.clone(),
                    key: (*key).to_owned(),
                    prior: before.map(|value| sanitize_json(value, Some(key), redaction, 0)),
                    current: after.map(|value| sanitize_json(value, Some(key), redaction, 0)),
                });
            }
        }
    }
    differences
}

fn context_occurrence(
    occurrence: &ErrorOccurrence,
    redaction: &mut ContextRedaction,
) -> ContextOccurrence {
    ContextOccurrence {
        occurrence_id: occurrence.occurrence_id.clone(),
        operation_id: occurrence.operation_id.clone(),
        attempt_id: occurrence.attempt_id.clone(),
        class: occurrence.class.to_string(),
        domain: occurrence.domain.clone(),
        code: occurrence.code.clone(),
        severity: occurrence.severity.to_string(),
        retryability: occurrence.retryability.to_string(),
        source: redact_text(&occurrence.source, redaction, 256),
        summary: redact_text(&occurrence.summary, redaction, 1024),
        details: sanitize_json(&occurrence.details, None, redaction, 0),
        environment: sanitize_json(&occurrence.environment, None, redaction, 0),
        observed_at: occurrence.observed_at.clone(),
        recorded_at: occurrence.recorded_at.clone(),
    }
}

fn sanitize_json(
    value: &Value,
    key: Option<&str>,
    redaction: &mut ContextRedaction,
    depth: usize,
) -> Value {
    if key.is_some_and(is_sensitive_key) {
        redaction.sensitive_values += 1;
        return Value::String("<redacted:sensitive-key>".to_owned());
    }
    if depth >= 4 {
        redaction.truncated_values += 1;
        return Value::String("<truncated:max-depth>".to_owned());
    }
    match value {
        Value::Object(values) => {
            let mut output = serde_json::Map::new();
            for (index, (key, value)) in values.iter().enumerate() {
                if index == 32 {
                    output.insert(
                        "_truncated".to_owned(),
                        Value::String("object exceeds 32 fields".to_owned()),
                    );
                    redaction.truncated_values += 1;
                    break;
                }
                output.insert(
                    key.clone(),
                    sanitize_json(value, Some(key), redaction, depth + 1),
                );
            }
            Value::Object(output)
        }
        Value::Array(values) => {
            let mut output = values
                .iter()
                .take(16)
                .map(|value| sanitize_json(value, key, redaction, depth + 1))
                .collect::<Vec<_>>();
            if values.len() > output.len() {
                output.push(Value::String("<truncated:array>".to_owned()));
                redaction.truncated_values += 1;
            }
            Value::Array(output)
        }
        Value::String(value) => Value::String(redact_text(value, redaction, 256)),
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "authorization",
        "connection_string",
        "cookie",
        "credential",
        "password",
        "private_key",
        "secret",
        "token",
    ]
    .iter()
    .any(|candidate| key.contains(candidate))
}

pub(super) fn redact_text(value: &str, redaction: &mut ContextRedaction, max_len: usize) -> String {
    let lowered = value.to_ascii_lowercase();
    if [
        "authorization:",
        "password=",
        "secret=",
        "token=",
        "private key",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        redaction.sensitive_values += 1;
        return "<redacted:sensitive-value>".to_owned();
    }
    let mut result = redact_home_path(value);
    if result != value {
        redaction.home_paths += 1;
    }
    if result.len() > max_len {
        let mut boundary = max_len;
        while !result.is_char_boundary(boundary) {
            boundary -= 1;
        }
        result.truncate(boundary);
        result.push_str("<truncated>");
        redaction.truncated_values += 1;
    }
    result
}

fn redact_home_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    for marker in ["/Users/", "/home/"] {
        if let Some(start) = normalized.find(marker) {
            let username_start = start + marker.len();
            let suffix_start = normalized[username_start..]
                .find('/')
                .map(|index| username_start + index)
                .unwrap_or(normalized.len());
            return format!(
                "{}$HOME{}",
                &normalized[..start],
                &normalized[suffix_start..]
            );
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        link_error, record_error, transition_error, ErrorClass, ErrorRetryability, ErrorSeverity,
        ErrorState, RecordErrorInput, RecoveryOrigin,
    };

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    #[test]
    fn recurrence_packet_is_bounded_redacted_and_links_causal_evidence() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        crate::store::ensure_schema(&connection)?;
        connection.execute(
            "INSERT INTO work_item (slug, title, description, status)
             VALUES ('context-work', 'Context work', 'test', 'running')",
            [],
        )?;
        let work_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO run (work_item_id, command, status) VALUES (?1, 'test', 'running')",
            params![work_id],
        )?;
        let run_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO artifact (run_id, kind, path, description)
             VALUES (?1, 'report', '/home/alice/private/report.md', 'evidence')",
            params![run_id],
        )?;
        connection.execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES ('run', ?1, 'validation', '{\"outcome\":\"failed\",\"command\":\"token=secret\",\"rationale\":\"known failure\"}')",
            params![run_id],
        )?;
        connection.execute(
            "INSERT INTO decision (work_item_id, outcome, rationale)
             VALUES (?1, 'continue', 'password=secret')",
            params![work_id],
        )?;

        let fingerprint = digest('a');
        for index in 0..3 {
            let occurrence_id = format!("occurrence-{index}");
            let key = format!("key-{index}");
            let details = serde_json::json!({
                "token": "do-not-return",
                "path": "/home/alice/project",
            });
            let environment = serde_json::json!({
                "os": if index == 2 { "windows" } else { "linux" },
                "secret_token": "do-not-return",
            });
            let result = record_error(
                &connection,
                &RecordErrorInput {
                    occurrence_id: &occurrence_id,
                    producer: "test",
                    idempotency_key: &key,
                    operation_id: "operation",
                    attempt_id: &format!("attempt-{index}"),
                    fingerprint_version: "structured-v1",
                    fingerprint: &fingerprint,
                    fingerprint_inputs: None,
                    fingerprint_provenance: None,
                    class: ErrorClass::InfrastructureError,
                    domain: "test.context",
                    code: "repeat",
                    severity: ErrorSeverity::Error,
                    retryability: ErrorRetryability::AfterChange,
                    source: "test",
                    summary: "same cause",
                    details: &details,
                    environment: &environment,
                    observed_at: &format!("2026-07-31T00:00:0{index}Z"),
                    recovery_origin: RecoveryOrigin::Database,
                },
            )?;
            if index == 0 {
                link_error(
                    &connection,
                    result.error.id,
                    Some(&occurrence_id),
                    "affected",
                    "run",
                    &run_id.to_string(),
                    "test",
                )?;
                transition_error(
                    &connection,
                    result.error.id,
                    ErrorState::Resolved,
                    "operator",
                    "test",
                    "verified workaround",
                )?;
            }
        }

        let packet = error_context_packet(
            &connection,
            1,
            Some("occurrence-2"),
            ErrorContextBounds::uniform(1)?,
        )?;
        assert!(packet.repeated);
        assert_eq!(packet.prior_occurrences.len(), 1);
        assert!(packet.sections["prior_occurrences"].truncated);
        assert_eq!(packet.related_runs[0].id, run_id);
        assert_eq!(packet.related_work_items[0].id, work_id);
        assert_eq!(packet.dispositions.len(), 1);
        assert_eq!(packet.decisions.len(), 1);
        assert_eq!(packet.artifacts.len(), 1);
        assert_eq!(packet.validations.len(), 1);
        assert!(!packet.environment_differences.is_empty());
        let encoded = serde_json::to_string(&packet)?;
        assert!(!encoded.contains("do-not-return"));
        assert!(!encoded.contains("/home/alice"));
        assert!(!encoded.contains("token=secret"));
        assert!(!encoded.contains("password=secret"));
        assert!(packet.redaction.sensitive_values >= 3);
        Ok(())
    }
}
