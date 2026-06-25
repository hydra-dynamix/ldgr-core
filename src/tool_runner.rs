use anyhow::{bail, Context};

pub fn render_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            if argument.chars().any(char::is_whitespace) {
                let escaped = argument.replace('"', "\\\"");
                format!("\"{escaped}\"")
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_argv_json(raw: &str) -> anyhow::Result<Vec<String>> {
    let argv: Vec<String> = serde_json::from_str(raw).context("argv must be a JSON array")?;
    if argv.is_empty() {
        bail!("argv must not be empty");
    }
    if argv.iter().any(|argument| argument.is_empty()) {
        bail!("argv arguments must not be empty");
    }
    Ok(argv)
}
