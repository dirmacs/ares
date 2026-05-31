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
    use super::{get_current_eruka_context, set_current_eruka_context};

    #[test]
    fn eruka_context_defaults_to_none() {
        set_current_eruka_context(None);
        assert_eq!(get_current_eruka_context(), None);
    }

    #[test]
    fn eruka_context_set_and_get_round_trip() {
        set_current_eruka_context(Some("binding-42".into()));
        assert_eq!(
            get_current_eruka_context().as_deref(),
            Some("binding-42")
        );
        set_current_eruka_context(None);
        assert_eq!(get_current_eruka_context(), None);
    }

    #[test]
    fn eruka_context_clear_replaces_previous_value() {
        set_current_eruka_context(Some("first".into()));
        set_current_eruka_context(Some("second".into()));
        assert_eq!(get_current_eruka_context().as_deref(), Some("second"));
    }
}
