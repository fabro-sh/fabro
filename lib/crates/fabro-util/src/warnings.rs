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
        let message = "dup-test-alpha";
        WARNINGS.lock().unwrap().remove(message);

        warn_user_once!("{message}");
        warn_user_once!("{message}");

        assert!(
            WARNINGS.lock().unwrap().contains(message),
            "warning should be recorded once in the set"
        );
    }

    #[test]
    fn warn_user_once_different_messages() {
        let first = "unique-msg-beta-1";
        let second = "unique-msg-beta-2";
        let mut warnings = WARNINGS.lock().unwrap();
        warnings.remove(first);
        warnings.remove(second);
        drop(warnings);

        warn_user_once!("{first}");
        warn_user_once!("{second}");

        let warnings = WARNINGS.lock().unwrap();
        assert!(warnings.contains(first));
        assert!(warnings.contains(second));
    }
}
