//! Recovery-hint dispatch — routes to the sqlite store or legacy tee per `[retriever] mode`.

use crate::core::config::Config;
use crate::core::retriever::{self, RecoveryMode, Stored, MIN_FAILURE_BYTES};

const RECALL_UNAVAILABLE_HINT: &str =
    "[recall unavailable — rtk proxy <command> to bypass filtering]";

fn active_mode() -> RecoveryMode {
    if matches!(std::env::var("RTK_RECALL").ok().as_deref(), Some("0"))
        || matches!(std::env::var("RTK_TEE").ok().as_deref(), Some("0"))
    {
        return RecoveryMode::Disabled;
    }
    Config::load().map(|c| c.retriever.mode).unwrap_or_default()
}

pub fn tee_and_hint(raw: &str, command_slug: &str, exit_code: i32) -> Option<String> {
    if exit_code == 0 || raw.len() < MIN_FAILURE_BYTES {
        return None;
    }
    match active_mode() {
        RecoveryMode::Disabled => Some(RECALL_UNAVAILABLE_HINT.to_string()),
        RecoveryMode::Tee => super::tee_file::tee_and_hint(raw, command_slug)
            .inspect(|_| retriever::record_tee_elision(command_slug)),
        RecoveryMode::Sqlite => {
            match retriever::store(raw.as_bytes(), command_slug, exit_code, 1) {
                Stored::Saved(s) => Some(format!("[full output: rtk recall {}]", s.hash)),
                Stored::Unavailable => Some(RECALL_UNAVAILABLE_HINT.to_string()),
                Stored::Empty => None,
            }
        }
    }
}

pub fn force_tee_hint(content: &str, command_slug: &str) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    match active_mode() {
        RecoveryMode::Disabled => Some(RECALL_UNAVAILABLE_HINT.to_string()),
        RecoveryMode::Tee => super::tee_file::force_tee_hint(content, command_slug)
            .inspect(|_| retriever::record_tee_elision(command_slug)),
        RecoveryMode::Sqlite => match retriever::store(content.as_bytes(), command_slug, 0, 1) {
            Stored::Saved(s) => Some(format!("[full output: rtk recall {}]", s.hash)),
            Stored::Unavailable => Some(RECALL_UNAVAILABLE_HINT.to_string()),
            Stored::Empty => None,
        },
    }
}

pub fn force_tee_tail_hint(
    content: &str,
    command_slug: &str,
    line_offset: usize,
) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    match active_mode() {
        RecoveryMode::Disabled => Some(RECALL_UNAVAILABLE_HINT.to_string()),
        RecoveryMode::Tee => {
            super::tee_file::force_tee_tail_hint(content, command_slug, line_offset)
                .inspect(|_| retriever::record_tee_elision(command_slug))
        }
        RecoveryMode::Sqlite => {
            match retriever::store(content.as_bytes(), command_slug, 0, line_offset) {
                Stored::Saved(s) => Some(format!(
                    "[+{} hidden: rtk recall {}]",
                    s.hidden_lines, s.hash
                )),
                Stored::Unavailable => Some(RECALL_UNAVAILABLE_HINT.to_string()),
                Stored::Empty => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tee_and_hint_skips_success() {
        let big = "x".repeat(1000);
        assert!(tee_and_hint(&big, "cmd", 0).is_none());
    }

    #[test]
    fn test_tee_and_hint_skips_tiny_failure() {
        assert!(tee_and_hint("tiny", "cmd", 1).is_none());
    }

    #[test]
    fn test_force_tee_hint_skips_empty() {
        assert!(force_tee_hint("", "cmd").is_none());
    }

    #[test]
    fn test_force_tee_tail_hint_skips_empty() {
        assert!(force_tee_tail_hint("", "cmd", 5).is_none());
    }
}
