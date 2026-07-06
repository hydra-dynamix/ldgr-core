use super::helpers::{in_write_transaction, record_event};
use super::types::Prompt;
use anyhow::{bail, Context};
use rusqlite::{params, Connection, OptionalExtension};

pub fn stable_content_hash(content: &str) -> String {
    format!("fnv1a64:{:016x}", fnv1a64(content.as_bytes()))
}

pub fn create_prompt(
    connection: &Connection,
    slug: &str,
    role: &str,
    body: &str,
    source_path: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<Prompt> {
    validate_slug(slug)?;
    validate_non_empty("role", role)?;
    validate_non_empty("body", body)?;
    in_write_transaction(connection, |connection| {
        let hash = stable_content_hash(body);
        connection.execute(
            "INSERT INTO prompt (slug, role, body, content_hash, status, source_path, description) VALUES (?1, ?2, ?3, ?4, 'draft', ?5, ?6)",
            params![slug, role, body, hash, source_path, description],
        ).with_context(|| format!("failed to create prompt {slug}"))?;
        let prompt_id = connection.last_insert_rowid();
        insert_prompt_version(
            connection,
            NewPromptVersion {
                prompt_id,
                version: 1,
                role,
                body,
                content_hash: &hash,
                source_path,
                description,
            },
        )?;
        let version_id = connection.last_insert_rowid();
        connection.execute(
            "UPDATE prompt SET current_version_id = ?1 WHERE id = ?2",
            params![version_id, prompt_id],
        )?;
        record_event(connection, "prompt", prompt_id, "create", "{}")?;
        get_prompt(connection, slug)?.context("created prompt disappeared")
    })
}

pub fn update_prompt(
    connection: &Connection,
    slug: &str,
    body: &str,
    source_path: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<Prompt> {
    validate_non_empty("body", body)?;
    in_write_transaction(connection, |connection| {
        let existing =
            get_prompt(connection, slug)?.with_context(|| format!("unknown prompt {slug}"))?;
        let next_version = existing.current_version + 1;
        let hash = stable_content_hash(body);
        let source_path = source_path.or(existing.source_path.as_deref());
        let description = description.or(existing.description.as_deref());
        insert_prompt_version(
            connection,
            NewPromptVersion {
                prompt_id: existing.id,
                version: next_version,
                role: &existing.role,
                body,
                content_hash: &hash,
                source_path,
                description,
            },
        )?;
        let version_id = connection.last_insert_rowid();
        connection.execute(
            "UPDATE prompt SET body = ?1, content_hash = ?2, source_path = ?3, description = ?4, current_version = ?5, current_version_id = ?6, updated_at = datetime('now') WHERE id = ?7",
            params![body, hash, source_path, description, next_version, version_id, existing.id],
        )?;
        record_event(connection, "prompt", existing.id, "update", "{}")?;
        get_prompt(connection, slug)?.context("updated prompt disappeared")
    })
}

pub fn set_prompt_status(
    connection: &Connection,
    slug: &str,
    status: &str,
) -> anyhow::Result<Prompt> {
    if !matches!(status, "draft" | "active" | "retired") {
        bail!("prompt status must be draft, active, or retired");
    }
    in_write_transaction(connection, |connection| {
        let prompt =
            get_prompt(connection, slug)?.with_context(|| format!("unknown prompt {slug}"))?;
        connection.execute(
            "UPDATE prompt SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status, prompt.id],
        )?;
        record_event(connection, "prompt", prompt.id, status, "{}")?;
        get_prompt(connection, slug)?.context("prompt disappeared after status update")
    })
}

pub fn get_prompt(connection: &Connection, slug: &str) -> anyhow::Result<Option<Prompt>> {
    connection
        .query_row(
            "SELECT id, slug, role, body, content_hash, status, current_version, current_version_id, source_path, description, created_at, updated_at FROM prompt WHERE slug = ?1",
            params![slug],
            Prompt::from_row,
        )
        .optional()
        .context("failed to read prompt")
}

pub fn list_prompts(connection: &Connection, status: Option<&str>) -> anyhow::Result<Vec<Prompt>> {
    let sql = match status {
        Some(_) => "SELECT id, slug, role, body, content_hash, status, current_version, current_version_id, source_path, description, created_at, updated_at FROM prompt WHERE status = ?1 ORDER BY slug",
        None => "SELECT id, slug, role, body, content_hash, status, current_version, current_version_id, source_path, description, created_at, updated_at FROM prompt ORDER BY slug",
    };
    let mut statement = connection.prepare(sql).context("failed to list prompts")?;
    let rows = match status {
        Some(status) => statement.query_map(params![status], Prompt::from_row)?,
        None => statement.query_map([], Prompt::from_row)?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read prompts")
}

pub fn active_prompt(connection: &Connection, slug: &str) -> anyhow::Result<Prompt> {
    let prompt = get_prompt(connection, slug)?.with_context(|| format!("unknown prompt {slug}"))?;
    if prompt.status != "active" {
        bail!(
            "prompt {slug} is {}; activate it before loop use",
            prompt.status
        );
    }
    Ok(prompt)
}

struct NewPromptVersion<'a> {
    prompt_id: i64,
    version: i64,
    role: &'a str,
    body: &'a str,
    content_hash: &'a str,
    source_path: Option<&'a str>,
    description: Option<&'a str>,
}

fn insert_prompt_version(
    connection: &Connection,
    version: NewPromptVersion<'_>,
) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO prompt_version (prompt_id, version, role, body, content_hash, source_path, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            version.prompt_id,
            version.version,
            version.role,
            version.body,
            version.content_hash,
            version.source_path,
            version.description
        ],
    )?;
    Ok(())
}

fn validate_slug(slug: &str) -> anyhow::Result<()> {
    validate_non_empty("slug", slug)?;
    if !slug
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        bail!("slug may only contain ASCII letters, numbers, '.', '-', and '_'");
    }
    Ok(())
}

fn validate_non_empty(label: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::super::schema::ensure_schema;
    use super::*;

    fn prompt_store_connection() -> anyhow::Result<Connection> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        ensure_schema(&connection)?;
        Ok(connection)
    }

    fn count_rows(connection: &Connection, table: &str) -> anyhow::Result<i64> {
        let sql = format!("SELECT count(*) FROM {table}");
        connection
            .query_row(&sql, [], |row| row.get(0))
            .with_context(|| format!("failed to count {table} rows"))
    }

    #[test]
    fn create_prompt_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let connection = prompt_store_connection()?;
        connection.execute("DROP TABLE event_log", [])?;

        let result = create_prompt(
            &connection,
            "surface",
            "surface-loop",
            "prompt body",
            None,
            None,
        );

        assert!(result.is_err());
        assert_eq!(count_rows(&connection, "prompt")?, 0);
        assert_eq!(count_rows(&connection, "prompt_version")?, 0);

        Ok(())
    }

    #[test]
    fn update_prompt_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let connection = prompt_store_connection()?;
        create_prompt(
            &connection,
            "surface",
            "surface-loop",
            "prompt v1",
            None,
            None,
        )?;
        connection.execute("DROP TABLE event_log", [])?;

        let result = update_prompt(&connection, "surface", "prompt v2", None, None);

        assert!(result.is_err());
        let prompt = get_prompt(&connection, "surface")?.context("missing prompt")?;
        assert_eq!(prompt.body, "prompt v1");
        assert_eq!(prompt.current_version, 1);
        assert_eq!(count_rows(&connection, "prompt_version")?, 1);

        Ok(())
    }

    #[test]
    fn set_prompt_status_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let connection = prompt_store_connection()?;
        create_prompt(
            &connection,
            "surface",
            "surface-loop",
            "prompt body",
            None,
            None,
        )?;
        connection.execute("DROP TABLE event_log", [])?;

        let result = set_prompt_status(&connection, "surface", "active");

        assert!(result.is_err());
        let prompt = get_prompt(&connection, "surface")?.context("missing prompt")?;
        assert_eq!(prompt.status, "draft");

        Ok(())
    }
}
