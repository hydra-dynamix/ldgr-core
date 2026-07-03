use std::env;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;

/// Parsed options for the conventional `ldgr-<adapter> adapter install` entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInstallCommandOptions {
    /// Exact adapter bundle install directory.
    pub install_root: PathBuf,
    /// Whether the adapter should print only the materialized manifest path.
    pub print_path: bool,
}

/// Result of parsing a conventional adapter install command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterInstallCommand {
    /// The caller should print its adapter-specific help text and return success.
    Help,
    /// The caller should materialize the adapter bundle using these options.
    Install(AdapterInstallCommandOptions),
}

/// Return the default LDGR adapter root (`$LDGR_HOME` or `~/.ldgr`).
pub fn default_adapter_root() -> PathBuf {
    env::var_os("LDGR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".ldgr")
        })
}

/// Parse the shared adapter install flags used by open adapter wrappers.
///
/// This intentionally handles only the small public adapter-wrapper contract:
/// `--adapter-root`, `--install-root`, `--print-path`, and help. Adapter binaries
/// remain responsible for their own help text, bundle materialization, and any
/// adapter-specific validation.
pub fn parse_adapter_install_command<I, S>(
    args: I,
    adapter_install_dir: &str,
) -> Result<AdapterInstallCommand, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let mut install_root = default_adapter_root().join(adapter_install_dir);
    let mut print_path = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--adapter-root") => {
                install_root = next_path(&args, index, "--adapter-root")?.join(adapter_install_dir);
                index += 2;
            }
            Some("--install-root") => {
                install_root = next_path(&args, index, "--install-root")?;
                index += 2;
            }
            Some("--print-path") => {
                print_path = true;
                index += 1;
            }
            Some("--help") | Some("-h") => return Ok(AdapterInstallCommand::Help),
            Some(flag) => return Err(format!("unknown adapter install option `{flag}`")),
            None => return Err("adapter install arguments must be valid UTF-8".to_string()),
        }
    }
    Ok(AdapterInstallCommand::Install(
        AdapterInstallCommandOptions {
            install_root,
            print_path,
        },
    ))
}

fn next_path(args: &[OsString], index: usize, flag: &str) -> Result<PathBuf, String> {
    args.get(index + 1)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} requires a path"))
}

/// Execute the core `ldgr` binary for an adapter pass-through surface and exit
/// with the child status code.
///
/// `LDGR_BIN` can override the executable for tests or embedded distributions.
pub fn pass_through_ldgr_or_exit<I, S>(args: I, empty_message: &str) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    if args.is_empty() {
        return Err(empty_message.to_string());
    }
    let ldgr_bin = env::var_os("LDGR_BIN").unwrap_or_else(|| OsString::from("ldgr"));
    let status = Command::new(&ldgr_bin)
        .args(args.iter().map(AsRef::as_ref))
        .status()
        .map_err(|error| {
            format!(
                "failed to run {}: {error}",
                PathBuf::from(&ldgr_bin).display()
            )
        })?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adapter_root_install_root_and_print_path() {
        let parsed = parse_adapter_install_command(
            ["--adapter-root", "/tmp/adapters", "--print-path"],
            "example",
        )
        .expect("parse adapter root");
        assert_eq!(
            parsed,
            AdapterInstallCommand::Install(AdapterInstallCommandOptions {
                install_root: PathBuf::from("/tmp/adapters/example"),
                print_path: true,
            })
        );

        let parsed = parse_adapter_install_command(["--install-root", "/tmp/exact"], "example")
            .expect("parse install root");
        assert_eq!(
            parsed,
            AdapterInstallCommand::Install(AdapterInstallCommandOptions {
                install_root: PathBuf::from("/tmp/exact"),
                print_path: false,
            })
        );
    }

    #[test]
    fn parses_help_without_requiring_other_options() {
        let parsed = parse_adapter_install_command(["--help"], "example").expect("parse help");
        assert_eq!(parsed, AdapterInstallCommand::Help);
    }

    #[test]
    fn preserves_adapter_install_error_messages() {
        assert_eq!(
            parse_adapter_install_command(["--adapter-root"], "example").unwrap_err(),
            "--adapter-root requires a path"
        );
        assert_eq!(
            parse_adapter_install_command(["--bogus"], "example").unwrap_err(),
            "unknown adapter install option `--bogus`"
        );
    }
}
