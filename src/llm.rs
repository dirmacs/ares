pub use ares_llm::*;

#[cfg(test)]
mod tests {
    use super::CapabilityRequirements;

    #[test]
    fn llm_crate_reexport_exposes_capability_requirements() {
        let reqs = CapabilityRequirements::builder().build();
        assert!(reqs.min_context_window.is_none());
    }
}
