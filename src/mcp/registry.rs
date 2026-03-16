use std::{collections::HashMap, path::Path, sync::Arc};

use super::client::{McpClient, McpServerConfig};

pub struct McpRegistry {
    clients: HashMap<String, Arc<McpClient>>,
}

impl McpRegistry {
    pub fn from_dir(config_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut clients = HashMap::new();
        let path = Path::new(config_dir);

        if !path.exists() {
            tracing::warn!("MCP config directory not found: {}", config_dir);
            return Ok(Self { clients });
        }

        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file_path = entry.path();

            if file_path.extension().and_then(|s| s.to_str()) == Some("toon") {
                let content = std::fs::read_to_string(&file_path)?;
                let config: McpServerConfig = toml::from_str(&content)?;

                if config.enabled {
                    let client = McpClient::new(config);
                    let name = client.name().to_string();
                    tracing::info!("Registered MCP client: {}", name);
                    clients.insert(name, Arc::new(client));
                }
            }
        }

        Ok(Self { clients })
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
