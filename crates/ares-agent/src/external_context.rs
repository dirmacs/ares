use cordis::Service;

/// Request-scoped Eruka (or other external) context injected via Cordis intercept.
pub struct ExternalContext(pub String);

impl Service for ExternalContext {}

#[cfg(test)]
mod tests {
    use super::ExternalContext;
    use cordis::Context;

    #[test]
    fn eruka_context_defaults_to_none() {
        let ctx = Context::new_root();
        assert!(ctx.get::<ExternalContext>().is_none());
    }

    #[test]
    fn eruka_context_set_and_get_round_trip() {
        let ctx = Context::new_root();
        ctx.provide(ExternalContext("binding-42".into()));
        assert_eq!(
            ctx.get::<ExternalContext>()
                .as_deref()
                .map(|e| e.0.as_str()),
            Some("binding-42")
        );
    }

    #[test]
    fn eruka_context_intercept_replaces_previous_value() {
        let ctx = Context::new_root().with_intercept(ExternalContext("first".into()));
        let ctx = ctx.with_intercept(ExternalContext("second".into()));
        assert_eq!(
            ctx.get::<ExternalContext>()
                .as_deref()
                .map(|e| e.0.as_str()),
            Some("second")
        );
    }
}
