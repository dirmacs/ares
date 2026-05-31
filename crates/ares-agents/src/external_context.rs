use std::cell::RefCell;

thread_local! {
    static CURRENT_ERUKA_CONTEXT: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn get_current_eruka_context() -> Option<String> {
    CURRENT_ERUKA_CONTEXT.with(|ctx| ctx.borrow().clone())
}

pub fn set_current_eruka_context(context: Option<String>) {
    CURRENT_ERUKA_CONTEXT.with(|ctx| *ctx.borrow_mut() = context);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_none() {
        set_current_eruka_context(None);
        assert_eq!(get_current_eruka_context(), None);
    }

    #[test]
    fn set_and_get_roundtrip() {
        set_current_eruka_context(Some("tenant knowledge".into()));
        assert_eq!(
            get_current_eruka_context().as_deref(),
            Some("tenant knowledge")
        );
        set_current_eruka_context(None);
    }

    #[test]
    fn overwrite_replaces_previous_value() {
        set_current_eruka_context(Some("first".into()));
        set_current_eruka_context(Some("second".into()));
        assert_eq!(get_current_eruka_context().as_deref(), Some("second"));
        set_current_eruka_context(None);
    }

    #[test]
    fn clear_with_none() {
        set_current_eruka_context(Some("temporary".into()));
        set_current_eruka_context(None);
        assert_eq!(get_current_eruka_context(), None);
    }

    #[test]
    fn empty_string_is_preserved() {
        set_current_eruka_context(Some(String::new()));
        assert_eq!(get_current_eruka_context(), Some(String::new()));
        set_current_eruka_context(None);
    }
}
