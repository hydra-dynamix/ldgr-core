use std::ffi::OsString;
use std::process::Command;

use anyhow::bail;
#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use std::path::{Path, PathBuf};

pub(crate) fn command_from_argv(argv: &[OsString]) -> anyhow::Result<Command> {
    let Some(program) = argv.first() else {
        bail!("process argv must not be empty");
    };

    #[cfg(windows)]
    {
        let program_text = program.to_string_lossy();
        if is_portable_shell_program(&program_text) {
            let mut command = Command::new(resolve_windows_bash(Some(Path::new(program)))?);
            command.args(argv[1..].iter().map(windows_shell_arg));
            return Ok(command);
        }
        if matches!(program_text.as_ref(), "true" | "false") {
            let mut command = Command::new(resolve_windows_bash(None)?);
            command
                .arg("-c")
                .arg(format!("{} \"$@\"", program_text))
                .arg(program);
            command.args(argv[1..].iter().map(windows_shell_arg));
            return Ok(command);
        }
        let path = Path::new(program);
        if is_shell_script(path)? {
            let mut command = Command::new(resolve_windows_bash(None)?);
            command.arg(windows_shell_path(path));
            command.args(argv[1..].iter().map(windows_shell_arg));
            return Ok(command);
        }
    }

    let mut command = Command::new(program);
    command.args(&argv[1..]);
    Ok(command)
}

#[cfg(windows)]
fn is_portable_shell_program(program: &str) -> bool {
    let normalized = program.replace('\\', "/").to_ascii_lowercase();
    matches!(
        normalized.rsplit('/').next(),
        Some("sh" | "sh.exe" | "bash" | "bash.exe")
    )
}

#[cfg(windows)]
fn resolve_windows_bash(requested: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(requested) = requested.filter(|path| path.is_absolute() && path.is_file()) {
        return Ok(requested.to_path_buf());
    }
    for key in ["LDGR_CORE_BASH", "LDGR_BASH"] {
        if let Some(configured) = std::env::var_os(key) {
            let configured = PathBuf::from(configured);
            if configured.is_file() {
                return Ok(configured);
            }
            bail!("{key} does not name a file: {}", configured.display());
        }
    }

    let mut candidates = Vec::new();
    for key in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(key) {
            candidates.push(PathBuf::from(root).join("Git/bin/bash.exe"));
        }
    }
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(root).join("Programs/Git/bin/bash.exe"));
    }
    if let Some(git) = where_program("git.exe") {
        if let Some(root) = git.parent().and_then(Path::parent) {
            candidates.push(root.join("bin/bash.exe"));
        }
    }
    if let Some(found) = candidates.into_iter().find(|candidate| candidate.is_file()) {
        return Ok(found);
    }

    bail!("portable shell unavailable; install Git for Windows or set LDGR_CORE_BASH/LDGR_BASH")
}

#[cfg(windows)]
fn where_program(program: &str) -> Option<PathBuf> {
    let output = Command::new("where.exe").arg(program).output().ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(PathBuf::from)
    })?
}

#[cfg(windows)]
fn is_shell_script(path: &Path) -> anyhow::Result<bool> {
    if path.extension().is_some_and(|extension| extension == "sh") {
        return Ok(true);
    }
    if !path.is_file() {
        return Ok(false);
    }
    let content = std::fs::read(path)
        .with_context(|| format!("failed to inspect executable {}", path.display()))?;
    let first_line = content
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    Ok(first_line.starts_with(b"#!")
        && String::from_utf8_lossy(first_line)
            .to_ascii_lowercase()
            .contains("sh"))
}

#[cfg(windows)]
fn windows_shell_arg(value: &OsString) -> OsString {
    let path = Path::new(value);
    if path.is_absolute() {
        return windows_shell_path(path).into();
    }
    value.clone()
}

#[cfg(windows)]
fn windows_shell_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    let bytes = value.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return format!(
            "/{}/{}",
            (bytes[0] as char).to_ascii_lowercase(),
            value[3..].replace('\\', "/")
        );
    }
    value.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::command_from_argv;
    use std::ffi::OsString;

    #[test]
    fn rejects_empty_argv() {
        assert_eq!(
            command_from_argv(&[]).unwrap_err().to_string(),
            "process argv must not be empty"
        );
    }

    #[test]
    fn preserves_native_program_and_arguments() {
        let command = command_from_argv(&[
            OsString::from("ldgr-host-command-fixture"),
            OsString::from("two words"),
        ])
        .unwrap();
        assert_eq!(command.get_program(), "ldgr-host-command-fixture");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["two words"]);
    }

    #[cfg(windows)]
    #[test]
    fn preserves_native_cmd_execution() {
        let mut command = command_from_argv(&[
            OsString::from("cmd"),
            OsString::from("/D"),
            OsString::from("/C"),
            OsString::from("ping -n 2 127.0.0.1 >nul"),
        ])
        .unwrap();
        let started = std::time::Instant::now();
        assert!(command.status().unwrap().success());
        assert!(started.elapsed() >= std::time::Duration::from_millis(500));
    }
}
