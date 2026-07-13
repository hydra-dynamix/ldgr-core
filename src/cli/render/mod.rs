pub(crate) mod brief_context;
pub(crate) mod context;
pub(crate) mod status;
pub(crate) mod text;

use serde::Serialize;

pub(crate) fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Emit `value` as pretty JSON when `json` is set, otherwise via the plain-text printer.
pub(crate) fn emit<T: Serialize>(
    json: bool,
    value: &T,
    text: impl FnOnce(&T),
) -> anyhow::Result<()> {
    if json {
        print_json(value)
    } else {
        text(value);
        Ok(())
    }
}

pub(crate) fn display_optional_id(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_owned())
}

pub(crate) fn display_exit_code(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}
