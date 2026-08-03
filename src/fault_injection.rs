use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub(crate) const ENVIRONMENT_VARIABLE: &str = "LDGR_TEST_FAULT_INJECTION";
const MARKER_VARIABLE: &str = "LDGR_TEST_FAULT_MARKER";
pub(crate) const EXIT_CODE: i32 = 86;

/// Abruptly terminate the current process at a named durability boundary.
///
/// This intentionally bypasses destructors and ordinary error handling so
/// integration tests exercise the same recovery path as a killed process.
/// The deliberately test-prefixed environment variable keeps the hook inert
/// during normal execution.
pub(crate) fn crash_if(point: &str) {
    if std::env::var(ENVIRONMENT_VARIABLE).as_deref() != Ok(point) {
        return;
    }

    if let Some(path) = std::env::var_os(MARKER_VARIABLE).map(PathBuf::from) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut marker) = fs::File::create(path) {
            let _ = marker.write_all(point.as_bytes());
            let _ = marker.sync_all();
        }
    }
    eprintln!("LDGR deterministic fault injection: {point}");
    std::process::exit(EXIT_CODE);
}
