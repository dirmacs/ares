//! Memory management — re-exported from the `ares-memory` crate.

pub use ares_memory::*;

#[cfg(test)]
mod tests {
    use super::{DEFAULT_HISTORY_WINDOW, MAX_FACTS_IN_PROMPT, MAX_PREFERENCES_IN_PROMPT};

    #[test]
    fn reexports_memory_constants() {
        assert_eq!(DEFAULT_HISTORY_WINDOW, 10);
        assert_eq!(MAX_FACTS_IN_PROMPT, 20);
        assert_eq!(MAX_PREFERENCES_IN_PROMPT, 10);
    }
}
