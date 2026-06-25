use anyhow::{bail, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::helpers::{in_write_transaction, record_event};
use super::types::{Prompt, PromptBundle, PromptBundleItem, PromptVersion};

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

pub fn create_bundle(
    connection: &Connection,
    slug: &str,
    prompt_slugs: &[String],
) -> anyhow::Result<PromptBundle> {
    validate_slug(slug)?;
    if prompt_slugs.is_empty() {
        bail!("bundle create requires at least one --prompt");
    }
    in_write_transaction(connection, |connection| {
        let prompts = prompt_slugs
            .iter()
            .map(|prompt_slug| {
                let prompt = active_prompt(connection, prompt_slug)?;
                let version_id = prompt
                    .current_version_id
                    .with_context(|| format!("prompt {} has no current version", prompt.slug))?;
                Ok((prompt, version_id))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        connection.execute(
            "INSERT INTO prompt_bundle (slug, status, manifest_json, bundle_hash) VALUES (?1, 'draft', '{}', '')",
            params![slug],
        )?;
        let bundle_id = connection.last_insert_rowid();
        for (prompt, version_id) in &prompts {
            connection.execute(
                "INSERT INTO prompt_bundle_item (bundle_id, prompt_id, prompt_version_id, prompt_slug, prompt_role, prompt_version, content_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![bundle_id, prompt.id, version_id, prompt.slug, prompt.role, prompt.current_version, prompt.content_hash],
            )?;
        }
        record_event(connection, "prompt_bundle", bundle_id, "create", "{}")?;
        get_bundle(connection, slug)?.context("created bundle disappeared")
    })
}

pub fn seal_bundle(connection: &Connection, slug: &str) -> anyhow::Result<PromptBundle> {
    in_write_transaction(connection, |connection| {
        let bundle =
            get_bundle(connection, slug)?.with_context(|| format!("unknown bundle {slug}"))?;
        if bundle.status == "sealed" {
            return Ok(bundle);
        }
        if bundle.status != "draft" {
            bail!("only draft bundles can be sealed");
        }
        let items = list_bundle_items(connection, bundle.id)?;
        if items.is_empty() {
            bail!("cannot seal empty bundle {slug}");
        }
        let manifest = BundleManifest {
            slug: bundle.slug.clone(),
            prompts: items.clone(),
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        let bundle_hash = stable_content_hash(&manifest_json);
        connection.execute(
            "UPDATE prompt_bundle SET status = 'sealed', manifest_json = ?1, bundle_hash = ?2 WHERE id = ?3",
            params![manifest_json, bundle_hash, bundle.id],
        )?;
        record_event(connection, "prompt_bundle", bundle.id, "seal", "{}")?;
        get_bundle(connection, slug)?.context("sealed bundle disappeared")
    })
}

pub fn get_bundle(connection: &Connection, slug: &str) -> anyhow::Result<Option<PromptBundle>> {
    connection
        .query_row(
            "SELECT id, slug, status, manifest_json, bundle_hash, created_at FROM prompt_bundle WHERE slug = ?1",
            params![slug],
            PromptBundle::from_row,
        )
        .optional()
        .context("failed to read bundle")
}

pub fn sealed_bundle(connection: &Connection, slug: &str) -> anyhow::Result<PromptBundle> {
    let bundle = get_bundle(connection, slug)?.with_context(|| format!("unknown bundle {slug}"))?;
    if bundle.status != "sealed" {
        bail!(
            "bundle {slug} is {}; seal it before loop use",
            bundle.status
        );
    }
    Ok(bundle)
}

pub fn list_bundle_items(
    connection: &Connection,
    bundle_id: i64,
) -> anyhow::Result<Vec<PromptBundleItem>> {
    let mut statement = connection.prepare(
        "SELECT id, bundle_id, prompt_id, prompt_version_id, prompt_slug, prompt_role, prompt_version, content_hash FROM prompt_bundle_item WHERE bundle_id = ?1 ORDER BY id",
    )?;
    let items = statement
        .query_map(params![bundle_id], PromptBundleItem::from_row)?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to list bundle items")?;
    Ok(items)
}

pub fn bundled_prompt_version(
    connection: &Connection,
    bundle_id: i64,
    role: Option<&str>,
) -> anyhow::Result<(PromptBundleItem, PromptVersion)> {
    let items = list_bundle_items(connection, bundle_id)?;
    let item = if let Some(role) = role {
        items
            .into_iter()
            .find(|item| item.prompt_role == role)
            .with_context(|| format!("bundle does not contain prompt role {role}"))?
    } else if items.len() == 1 {
        items.into_iter().next().expect("one item")
    } else {
        bail!("bundle contains multiple prompts; pass --prompt-role");
    };
    let version = connection.query_row(
        "SELECT id, prompt_id, version, role, body, content_hash, source_path, description, created_at FROM prompt_version WHERE id = ?1",
        params![item.prompt_version_id],
        PromptVersion::from_row,
    )?;
    Ok((item, version))
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

#[derive(Serialize)]
struct BundleManifest {
    slug: String,
    prompts: Vec<PromptBundleItem>,
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

    #[test]
    fn create_bundle_prevalidates_prompt_references_before_inserting_bundle() -> anyhow::Result<()>
    {
        let connection = prompt_store_connection()?;
        create_prompt(
            &connection,
            "surface",
            "surface-loop",
            "prompt body",
            None,
            None,
        )?;
        set_prompt_status(&connection, "surface", "active")?;

        let result = create_bundle(
            &connection,
            "cleanroom",
            &["surface".to_owned(), "missing".to_owned()],
        );

        assert!(result.is_err());
        assert_eq!(count_rows(&connection, "prompt_bundle")?, 0);
        assert_eq!(count_rows(&connection, "prompt_bundle_item")?, 0);

        Ok(())
    }

    #[test]
    fn create_bundle_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let connection = prompt_store_connection()?;
        create_prompt(
            &connection,
            "surface",
            "surface-loop",
            "prompt body",
            None,
            None,
        )?;
        set_prompt_status(&connection, "surface", "active")?;
        connection.execute("DROP TABLE event_log", [])?;

        let result = create_bundle(&connection, "cleanroom", &["surface".to_owned()]);

        assert!(result.is_err());
        assert_eq!(count_rows(&connection, "prompt_bundle")?, 0);
        assert_eq!(count_rows(&connection, "prompt_bundle_item")?, 0);

        Ok(())
    }

    #[test]
    fn seal_bundle_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let connection = prompt_store_connection()?;
        create_prompt(
            &connection,
            "surface",
            "surface-loop",
            "prompt body",
            None,
            None,
        )?;
        set_prompt_status(&connection, "surface", "active")?;
        create_bundle(&connection, "cleanroom", &["surface".to_owned()])?;
        connection.execute("DROP TABLE event_log", [])?;

        let result = seal_bundle(&connection, "cleanroom");

        assert!(result.is_err());
        let bundle = get_bundle(&connection, "cleanroom")?.context("missing bundle")?;
        assert_eq!(bundle.status, "draft");
        assert_eq!(bundle.manifest_json, "{}");
        assert_eq!(bundle.bundle_hash, "");
        assert_eq!(count_rows(&connection, "prompt_bundle_item")?, 1);

        Ok(())
    }
}
