//! Чистые решения без ввода-вывода: когда проверять и что предлагать.

use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;

/// Фоновая проверка — не чаще раза в 12 часов.
pub const CHECK_INTERVAL_SECS: u64 = 12 * 60 * 60;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn should_check(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        None => true,
        // Часы перевели назад — не ждём «будущего», проверяем.
        Some(t) if t > now => true,
        Some(t) => now - t >= CHECK_INTERVAL_SECS,
    }
}

/// `candidate` строго новее `current` по semver. Пре-релизы предлагаются
/// только тем, кто сам сидит на пре-релизе. Невалидные версии — `false`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    let (Ok(cur), Ok(cand)) = (Version::parse(current), Version::parse(candidate)) else {
        return false;
    };
    if !cand.pre.is_empty() && cur.pre.is_empty() {
        return false;
    }
    cand > cur
}

/// Предлагать ли версию: новее текущей и не пропущена пользователем
/// (ручная проверка пропуск игнорирует).
pub fn should_offer(current: &str, candidate: &str, skipped: Option<&str>, manual: bool) -> bool {
    is_newer(current, candidate) && (manual || skipped != Some(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_checks() {
        assert!(should_check(None, 1_000));
    }

    #[test]
    fn respects_interval_boundary() {
        let t = 1_000_000;
        assert!(!should_check(Some(t), t + CHECK_INTERVAL_SECS - 1));
        assert!(should_check(Some(t), t + CHECK_INTERVAL_SECS));
    }

    #[test]
    fn clock_moved_back_triggers_check() {
        assert!(should_check(Some(2_000_000), 1_000_000));
    }

    #[test]
    fn newer_versions() {
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.2.0", "1.1.9"));
        assert!(!is_newer("1.0.0", "мусор"));
        assert!(!is_newer("мусор", "1.0.0"));
    }

    #[test]
    fn prereleases_only_for_prerelease_users() {
        assert!(!is_newer("1.0.0", "1.1.0-beta.1"));
        assert!(is_newer("1.1.0-beta.1", "1.1.0-beta.2"));
        assert!(is_newer("1.1.0-beta.2", "1.1.0"));
    }

    #[test]
    fn skipped_version_is_not_offered_but_newer_is() {
        assert!(!should_offer("1.0.0", "1.1.0", Some("1.1.0"), false));
        assert!(should_offer("1.0.0", "1.1.1", Some("1.1.0"), false));
        assert!(should_offer("1.0.0", "1.1.0", None, false));
    }

    #[test]
    fn manual_check_ignores_skip_but_not_version_order() {
        assert!(should_offer("1.0.0", "1.1.0", Some("1.1.0"), true));
        assert!(!should_offer("1.1.0", "1.0.0", None, true));
    }
}
