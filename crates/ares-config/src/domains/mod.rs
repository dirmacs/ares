pub mod server;
pub mod auth;
pub mod database;
pub mod providers;
pub mod tools;
pub mod agents;
pub mod workflows;
pub mod rag;
pub mod billing;

#[cfg(test)]
mod tests {
    use super::server::ServerConfig;

    #[test]
    fn domain_defaults_match_serde_defaults() {
        let server = ServerConfig::default();
        assert_eq!(server.port, 3000);
        assert_eq!(server.host, "127.0.0.1");
    }
}
