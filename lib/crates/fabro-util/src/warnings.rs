use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// Set of already-emitted warnings (for `warn_user_once!` deduplication).
pub static WARNINGS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Emit a styled `warning: {message}` to stderr.
#[macro_export]
macro_rules! warn_user {
    ($($arg:tt)*) => {{
        let message = format!($($arg)*);
        let style = $crate::console::Style::new().yellow().bold();
        eprintln!("{} {message}", style.apply_to("warning:"));
    }};
}

/// Like [`warn_user!`], but only emits each unique message once per process.
#[macro_export]
macro_rules! warn_user_once {
    ($($arg:tt)*) => {{
        let message = format!($($arg)*);
        let mut set = $crate::WARNINGS.lock().unwrap();
        if set.insert(message.clone()) {
            drop(set);
            $crate::warn_user!("{message}");
        }
    }};
}

#[cfg(test)]
mod tests {
    use crate::WARNINGS;

    #[test]
    fn warn_user_once_deduplicates() {
        let before = WARNINGS.lock().unwrap().len();
        warn_user_once!("dup-test-{}", "alpha");
        let after_first = WARNINGS.lock().unwrap().len();
        warn_user_once!("dup-test-{}", "alpha");
        let after_second = WARNINGS.lock().unwrap().len();
        assert_eq!(after_first, before + 1);
        assert_eq!(
            after_second, after_first,
            "duplicate should not grow the set"
        );
    }

    #[test]
    fn warn_user_once_different_messages() {
        let before = WARNINGS.lock().unwrap().len();
        warn_user_once!("unique-msg-beta-1");
        warn_user_once!("unique-msg-beta-2");
        let after = WARNINGS.lock().unwrap().len();
        assert_eq!(after, before + 2);
    }
}
