use std::ffi::OsStr;

use crate::harness_config::{UpdateCheck, UpdateConfig};

pub const NO_UPDATE_CHECK_ENV: &str = "LDGR_NO_UPDATE_CHECK";
pub const CI_ENV: &str = "CI";

/// Returns whether this process opted out of automatic update discovery.
/// Explicit update commands do not consult this startup-only override.
pub fn process_update_check_disabled() -> bool {
    no_update_check_value_disables(std::env::var_os(NO_UPDATE_CHECK_ENV).as_deref())
}

/// Resolves persisted startup policy with the immediate process override.
pub fn automatic_update_checks_enabled(config: &UpdateConfig) -> bool {
    automatic_update_checks_enabled_for(config, std::env::var_os(NO_UPDATE_CHECK_ENV).as_deref())
}

/// Returns whether update notices should be hidden for this process.
/// CI only suppresses notices; it does not disable explicit update checks.
pub fn update_notices_suppressed_by_ci() -> bool {
    ci_value_suppresses(std::env::var_os(CI_ENV).as_deref())
}

/// Applies both the persisted notification preference and the process CI guard.
pub fn update_notices_enabled(config: &UpdateConfig) -> bool {
    update_notices_enabled_for(config, std::env::var_os(CI_ENV).as_deref())
}

fn automatic_update_checks_enabled_for(config: &UpdateConfig, value: Option<&OsStr>) -> bool {
    config.check == UpdateCheck::Startup && !no_update_check_value_disables(value)
}

fn update_notices_enabled_for(config: &UpdateConfig, value: Option<&OsStr>) -> bool {
    config.notify && !ci_value_suppresses(value)
}

fn no_update_check_value_disables(value: Option<&OsStr>) -> bool {
    value
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.trim() == "1")
}

fn ci_value_suppresses(value: Option<&OsStr>) -> bool {
    value
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use crate::harness_config::{UpdateCheck, UpdateConfig};

    use super::{
        automatic_update_checks_enabled_for, ci_value_suppresses, no_update_check_value_disables,
        update_notices_enabled_for,
    };

    #[test]
    fn no_update_check_accepts_only_the_documented_process_override() {
        assert!(no_update_check_value_disables(Some(OsStr::new("1"))));
        assert!(no_update_check_value_disables(Some(OsStr::new(" 1 "))));
        for value in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("0")),
            Some(OsStr::new("true")),
        ] {
            assert!(!no_update_check_value_disables(value));
        }
    }

    #[test]
    fn ci_true_suppresses_notices_without_becoming_a_general_truthy_flag() {
        assert!(ci_value_suppresses(Some(OsStr::new("true"))));
        assert!(ci_value_suppresses(Some(OsStr::new(" TRUE "))));
        for value in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("0")),
            Some(OsStr::new("1")),
        ] {
            assert!(!ci_value_suppresses(value));
        }
    }

    #[test]
    fn process_overrides_compose_with_persisted_update_preferences() {
        let mut config = UpdateConfig::default();
        assert!(automatic_update_checks_enabled_for(&config, None));
        assert!(!automatic_update_checks_enabled_for(
            &config,
            Some(OsStr::new("1"))
        ));
        config.check = UpdateCheck::Never;
        assert!(!automatic_update_checks_enabled_for(&config, None));

        assert!(update_notices_enabled_for(&config, None));
        assert!(!update_notices_enabled_for(
            &config,
            Some(OsStr::new("true"))
        ));
        config.notify = false;
        assert!(!update_notices_enabled_for(&config, None));
    }
}
