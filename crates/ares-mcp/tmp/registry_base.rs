use std::{collections::HashMap, path::Path, sync::Arc};

use super::client::{McpClient, McpServerConfig};

pub struct McpRegistry {
    clients: HashMap<String, Arc<McpClient>>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    /// Register (or replace) an MCP client by config name.
    pub fn register(&mut self, config: McpServerConfig) -> Arc<McpClient> {
        let client = McpClient::new(config);
        let name = client.name().to_string();
        let arc = Arc::new(client);
        self.clients.insert(name, arc.clone());
        arc
    }

    /// Remove a client by name. Returns true if it existed.
    pub fn deregister(&mut self, name: &str) -> bool {
        self.clients.remove(name).is_some()
    }

    pub fn from_dir(config_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut clients = HashMap::new();
        let path = Path::new(config_dir);

        if !path.exists() {
            tracing::warn!("MCP config directory not found: {}", config_dir);
            return Ok(Self::new());
        }

        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file_path = entry.path();

            if !is_mcp_config_file(&file_path) {
                continue;
            }

            match load_mcp_config(&file_path) {
                Ok(config) if config.enabled => {
                    let client = McpClient::new(config);
                    let name = client.name().to_string();
                    tracing::info!("Registered MCP client: {}", name);
                    clients.insert(name, Arc::new(client));
                }
                Ok(config) => {
                    tracing::debug!(name = %config.name, "Skipping disabled MCP client");
                }
                Err(error) => {
                    tracing::warn!(
                        path = %file_path.display(),
                        error = %error,
                        "Skipping invalid MCP config"
                    );
                }
            }
        }

        let mut registry = Self::new();
        registry.clients = clients;
        Ok(registry)
    }

    pub fn get_client(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.clients.get(name)
    }

    pub fn eruka(&self) -> Option<&Arc<McpClient>> {
        self.clients.get("eruka")
    }

    pub fn client_names(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }
}

fn is_mcp_config_file(path: &Path) -> bool {
    if path.extension().and_then(|s| s.to_str()) != Some("toon") {
        return false;
    }

    !path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|name| name.ends_with(".example.toon"))
        .unwrap_or(false)
}

fn load_mcp_config(path: &Path) -> Result<McpServerConfig, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;

    match toml::from_str::<McpServerConfig>(&content) {
        Ok(config) => Ok(config),
        Err(toml_error) => {
            toon_format::decode_default::<McpServerConfig>(&content).map_err(|toon_error| {
                format!(
                    "failed to parse as TOML ({}) or TOON ({})",
                    toml_error, toon_error
                )
                .into()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_toml_and_toon_mcp_configs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("eruka.toon"),
            r#"name = "eruka"
enabled = true
endpoint = "https://eruka.dirmacs.com/mcp"
transport = "http"
timeout_secs = 30
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filesystem.toon"),
            r#"name: filesystem
enabled: true
command: npx
args[2]: "-y","@modelcontextprotocol/server-filesystem"
timeout_secs: 30
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("eruka.example.toon"),
            r#"name: eruka
enabled: true
command: eruka-mcp
"#,
        )
        .unwrap();

        let registry = McpRegistry::from_dir(dir.path().to_str().unwrap()).unwrap();
        let mut names = registry.client_names();
        names.sort();

        assert_eq!(names, vec!["eruka".to_string(), "filesystem".to_string()]);
        assert!(registry.eruka().is_some());
    }

    #[test]
    fn register_and_deregister_client() {
        let mut registry = McpRegistry::new();
        assert!(registry.get_client("pom").is_none());

        registry.register(McpServerConfig {
            name: "pom".into(),
            enabled: true,
            command: None,
            args: None,
            timeout_secs: Some(15),
            endpoint: Some("http://localhost:3002/mcp".into()),
            transport: Some("http".into()),
            api_key: None,
        });

        assert!(registry.get_client("pom").is_some());
        assert_eq!(registry.client_names(), vec!["pom".to_string()]);

        assert!(registry.deregister("pom"));
        assert!(!registry.deregister("pom"));
        assert!(registry.get_client("pom").is_none());
    }

    #[test]
    fn skips_invalid_mcp_config_without_failing_registry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pom.toon"),
            r#"name: pom
enabled: true
transport: http
endpoint: http://localhost:3002/mcp
timeout_secs: 15
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.toon"), "not valid =").unwrap();

        let registry = McpRegistry::from_dir(dir.path().to_str().unwrap()).unwrap();

        assert_eq!(registry.client_names(), vec!["pom".to_string()]);
    }

    #[test]
    fn from_dir_missing_directory_returns_empty_registry() {
        let path = std::env::temp_dir().join(format!(
            "ares-mcp-missing-{}",
            uuid::Uuid::new_v4()
        ));
        assert!(!path.exists());

        let registry = McpRegistry::from_dir(path.to_str().unwrap()).unwrap();

        assert!(registry.client_names().is_empty());
        assert!(registry.eruka().is_none());
    }

    #[test]
    fn from_dir_skips_disabled_configs_and_non_toon_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("disabled.toon"),
            r#"name = "disabled"
enabled = false
endpoint = "http://localhost/mcp"
transport = "http"
timeout_secs = 10
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("readme.txt"), "not an mcp config").unwrap();

        let registry = McpRegistry::from_dir(dir.path().to_str().unwrap()).unwrap();

        assert!(registry.client_names().is_empty());
    }

    #[test]
    fn register_replaces_existing_client() {
        let mut registry = McpRegistry::new();

        registry.register(McpServerConfig {
            name: "svc".into(),
            enabled: true,
            command: None,
            args: None,
            timeout_secs: Some(10),
            endpoint: Some("http://localhost:3001/mcp".into()),
            transport: Some("http".into()),
            api_key: None,
        });
        registry.register(McpServerConfig {
            name: "svc".into(),
            enabled: true,
            command: None,
            args: None,
            timeout_secs: Some(20),
            endpoint: Some("http://localhost:3002/mcp".into()),
            transport: Some("http".into()),
            api_key: None,
        });

        assert_eq!(registry.client_names(), vec!["svc".to_string()]);
        assert!(registry.get_client("svc").is_some());
    }

}

