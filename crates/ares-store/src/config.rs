use serde::{Deserialize, Serialize};

// ============= Database Configuration =============

/// Which storage backend the store uses.
///
/// This is a closed choice, deliberately. The previous shape was
/// `DatabaseConfig { url: String }` with a default of
/// `postgres://postgres:postgres@localhost:5432/ares`, while the shipped
/// example config set `url = "./data/ares.db"` under a comment claiming it was
/// the "Local SQLite database (default)". Both were accepted and neither
/// selected anything: the string was handed to a Postgres client that discarded
/// it and read `DATABASE_URL` instead. A setting that cannot be honored is a
/// setting that should not exist, so the backend is named explicitly and its
/// parameters are typed per backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseBackend {
    /// In-process embedded database. No server, no network, no external
    /// service. This is the default because it is the only mode that needs
    /// nothing from the environment.
    Embedded {
        /// Path to the database file. Relative paths resolve against the
        /// working directory. `:memory:` is accepted for throwaway runs.
        path: String,
    },
    /// PostgreSQL. Requires a reachable server; the URL is validated at load.
    Postgres {
        /// Connection URL. Must parse as `postgres://` or `postgresql://`.
        url: String,
        /// Pool ceiling. Falls back to `DATABASE_MAX_CONNECTIONS`, then 20.
        max_connections: Option<u32>,
    },
}

impl<'de> Deserialize<'de> for DatabaseBackend {
    /// Deserialized through an explicit raw shape rather than `#[serde(flatten)]`
    /// on a tagged enum.
    ///
    /// Two reasons:
    /// 1. An empty `[database]` section must mean "use the defaults". A tagged
    ///    enum demands its tag, so `[database]` with nothing under it failed with
    ///    `missing field 'backend'` — a config that is merely sparse is not a
    ///    config that is wrong.
    /// 2. serde does not support `deny_unknown_fields` on a flattened inner
    ///    type, and rejecting unknown keys matters here: a typo such as
    ///    `pth = "./x"` silently falling back to a default is exactly the defect
    ///    this rewrite exists to remove.
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(default)]
            backend: Option<String>,
            #[serde(default)]
            path: Option<String>,
            #[serde(default)]
            url: Option<String>,
            #[serde(default)]
            max_connections: Option<u32>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let name = raw
            .backend
            .as_deref()
            .unwrap_or_else(|| default_backend_name())
            .trim()
            .to_ascii_lowercase();
        match name.as_str() {
            "embedded" => {
                if raw.url.is_some() {
                    return Err(serde::de::Error::custom(
                        "[database] backend = \"embedded\" does not take a `url`; use `path` \
                         for the file, or set backend = \"postgres\"",
                    ));
                }
                if raw.max_connections.is_some() {
                    return Err(serde::de::Error::custom(
                        "[database] `max_connections` applies to backend = \"postgres\" only",
                    ));
                }
                Ok(DatabaseBackend::Embedded {
                    path: raw.path.unwrap_or_else(default_embedded_path),
                })
            }
            "postgres" => {
                if raw.path.is_some() {
                    return Err(serde::de::Error::custom(
                        "[database] backend = \"postgres\" does not take a `path`; use `url` \
                         for the connection string, or set backend = \"embedded\"",
                    ));
                }
                let url = raw.url.ok_or_else(|| {
                    serde::de::Error::custom(
                        "[database] backend = \"postgres\" requires a `url`; set e.g. \
                         url = \"postgres://user:pass@host:5432/ares\"",
                    )
                })?;
                Ok(DatabaseBackend::Postgres {
                    url,
                    max_connections: raw.max_connections,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "[database] unknown backend {other:?}; expected \"embedded\" or \"postgres\""
            ))),
        }
    }
}

/// The backend a configuration that names none gets.
///
/// Build-aware on purpose. `Embedded` is preferred because it needs nothing from
/// the environment, but it is only compiled in with the `turso` feature, and
/// `turso` cannot be a default feature: `libsql-ffi` and `rusqlite` each vendor
/// SQLite, so a binary linking both fails with duplicate `sqlite3_*` symbols.
/// See the root `Cargo.toml`.
///
/// A default the binary cannot serve would be the same class of bug this module
/// exists to remove, so a build without `turso` defaults to the backend its
/// feature set can always serve.
pub const fn default_backend_name() -> &'static str {
    if cfg!(feature = "turso") {
        "embedded"
    } else {
        "postgres"
    }
}

/// Connection string a `postgres`-defaulting build falls back to when it names
/// no url.
///
/// Duplicated from `crate::postgres::DEFAULT_POSTGRES_URL` only because
/// `postgres` is an optional feature that may be off; a test asserts the two
/// agree whenever that feature is enabled, so they cannot drift.
const FALLBACK_POSTGRES_URL: &str = "postgres://postgres:postgres@localhost:5432/ares";

impl Default for DatabaseBackend {
    fn default() -> Self {
        if cfg!(feature = "turso") {
            DatabaseBackend::Embedded {
                path: default_embedded_path(),
            }
        } else {
            DatabaseBackend::Postgres {
                url: FALLBACK_POSTGRES_URL.to_string(),
                max_connections: None,
            }
        }
    }
}

fn default_embedded_path() -> String {
    "./data/ares.db".to_string()
}

impl DatabaseBackend {
    /// Backend name for logging and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            DatabaseBackend::Embedded { .. } => "embedded",
            DatabaseBackend::Postgres { .. } => "postgres",
        }
    }

    /// Reject a configuration that cannot work, naming the field and the fix.
    ///
    /// This runs at load, so a bad setting fails at startup with a sentence an
    /// operator can act on, instead of surfacing later as a pool timeout at
    /// request time.
    pub fn validate(&self) -> std::result::Result<(), String> {
        match self {
            DatabaseBackend::Embedded { path } => {
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    return Err(
                        "[database] backend = \"embedded\" requires a non-empty `path`; \
                         set e.g. path = \"./data/ares.db\" or path = \":memory:\""
                            .to_string(),
                    );
                }
                Ok(())
            }
            DatabaseBackend::Postgres {
                url,
                max_connections,
            } => {
                let trimmed = url.trim();
                if trimmed.is_empty() {
                    return Err(
                        "[database] backend = \"postgres\" requires a `url`; set e.g. \
                         url = \"postgres://user:pass@host:5432/ares\""
                            .to_string(),
                    );
                }
                if !(trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://")) {
                    return Err(format!(
                        "[database] url must start with postgres:// or postgresql://, got {trimmed:?}. \
                         A filesystem path is not a connection string; to use a local file set \
                         backend = \"embedded\" with `path` instead."
                    ));
                }
                if max_connections.is_some_and(|mc| mc < 1) {
                    return Err("[database] max_connections must be at least 1".to_string());
                }
                Ok(())
            }
        }
    }
}

/// Database configuration settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatabaseConfig {
    /// Which backend to use, and that backend's parameters.
    pub backend: DatabaseBackend,

    /// Qdrant vector database configuration (optional).
    pub qdrant: Option<QdrantConfig>,
}

impl Serialize for DatabaseConfig {
    /// Emits a flat `[database]` table: `backend`, the backend's own keys, and
    /// `qdrant` beside them. The derive would nest the enum under a `[backend]`
    /// sub-table, which the reader does not accept — serialisation and
    /// deserialisation must describe the same shape, or a round trip silently
    /// produces a config the program cannot read back.
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match &self.backend {
            DatabaseBackend::Embedded { path } => {
                map.serialize_entry("backend", "embedded")?;
                map.serialize_entry("path", path)?;
            }
            DatabaseBackend::Postgres {
                url,
                max_connections,
            } => {
                map.serialize_entry("backend", "postgres")?;
                map.serialize_entry("url", url)?;
                if let Some(mc) = max_connections {
                    map.serialize_entry("max_connections", mc)?;
                }
            }
        }
        if let Some(q) = &self.qdrant {
            map.serialize_entry("qdrant", q)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for DatabaseConfig {
    /// Reads the whole `[database]` table in one pass, so a backend's keys sit
    /// beside `qdrant` exactly as an operator writes them, and so the same
    /// strictness rules apply to every key in the table.
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(default)]
            backend: Option<String>,
            #[serde(default)]
            path: Option<String>,
            #[serde(default)]
            url: Option<String>,
            #[serde(default)]
            max_connections: Option<u32>,
            #[serde(default)]
            qdrant: Option<QdrantConfig>,
        }

        let raw = Raw::deserialize(deserializer)?;

        // Nothing names a backend and nothing describes one: take the build's
        // default whole, including its url/path. Treating this as "postgres with
        // no url" would fail a config that is merely sparse — the bug class this
        // module exists to remove.
        if raw.backend.is_none() && raw.path.is_none() && raw.url.is_none() {
            if raw.max_connections.is_some() {
                return Err(serde::de::Error::custom(
                    "[database] `max_connections` requires backend = \"postgres\"",
                ));
            }
            return Ok(DatabaseConfig {
                backend: DatabaseBackend::default(),
                qdrant: raw.qdrant,
            });
        }

        let name = raw
            .backend
            .as_deref()
            .unwrap_or_else(|| default_backend_name())
            .trim()
            .to_ascii_lowercase();
        let backend = match name.as_str() {
            "embedded" => {
                if raw.url.is_some() {
                    return Err(serde::de::Error::custom(
                        "[database] backend = \"embedded\" does not take a `url`; use `path` \
                         for the file, or set backend = \"postgres\"",
                    ));
                }
                if raw.max_connections.is_some() {
                    return Err(serde::de::Error::custom(
                        "[database] `max_connections` applies to backend = \"postgres\" only",
                    ));
                }
                DatabaseBackend::Embedded {
                    path: raw.path.unwrap_or_else(default_embedded_path),
                }
            }
            "postgres" => {
                if raw.path.is_some() {
                    return Err(serde::de::Error::custom(
                        "[database] backend = \"postgres\" does not take a `path`; use `url` \
                         for the connection string, or set backend = \"embedded\"",
                    ));
                }
                let url = raw.url.ok_or_else(|| {
                    serde::de::Error::custom(
                        "[database] backend = \"postgres\" requires a `url`; set e.g. \
                         url = \"postgres://user:pass@host:5432/ares\"",
                    )
                })?;
                DatabaseBackend::Postgres {
                    url,
                    max_connections: raw.max_connections,
                }
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "[database] unknown backend {other:?}; expected \"embedded\" or \"postgres\""
                )));
            }
        };
        Ok(DatabaseConfig {
            backend,
            qdrant: raw.qdrant,
        })
    }
}

impl DatabaseConfig {
    /// Validate the whole database configuration at load time.
    pub fn validate(&self) -> std::result::Result<(), String> {
        self.backend.validate()
    }
}

/// Qdrant vector database configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QdrantConfig {
    /// Qdrant server URL (default: "http://localhost:6334").
    #[serde(default = "default_qdrant_url")]
    pub url: String,

    /// Environment variable for Qdrant API key.
    pub api_key_env: Option<String>,
}

pub fn default_qdrant_url() -> String {
    "http://localhost:6334".to_string()
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: default_qdrant_url(),
            api_key_env: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> std::result::Result<DatabaseConfig, serde_json::Error> {
        serde_json::from_str(s)
    }

    #[test]
    fn default_backend_matches_what_this_build_can_serve() {
        let d = DatabaseBackend::default();
        if cfg!(feature = "turso") {
            assert_eq!(
                d,
                DatabaseBackend::Embedded {
                    path: default_embedded_path()
                },
                "a build with `turso` should default to the embedded store"
            );
        } else {
            assert!(
                matches!(d, DatabaseBackend::Postgres { .. }),
                "a build without `turso` must not default to a backend it cannot serve"
            );
        }
        assert_eq!(DatabaseConfig::default().backend, d);
        assert!(
            DatabaseConfig::default().validate().is_ok(),
            "the default config must be valid in this build"
        );
    }

    #[test]
    fn empty_table_parses_to_the_builds_default_backend() {
        let cfg = parse("{}").expect("empty object");
        assert_eq!(cfg.backend, DatabaseBackend::default());
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn fallback_postgres_url_matches_the_postgres_module_constant() {
        assert_eq!(
            FALLBACK_POSTGRES_URL,
            crate::postgres::DEFAULT_POSTGRES_URL,
            "the two constants must not drift"
        );
    }

    #[test]
    fn embedded_parses_and_validates() {
        let cfg = parse(r#"{"backend":"embedded","path":":memory:"}"#).expect("parse embedded");
        assert_eq!(
            cfg.backend,
            DatabaseBackend::Embedded {
                path: ":memory:".into()
            }
        );
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn postgres_parses_pool_ceiling() {
        let cfg =
            parse(r#"{"backend":"postgres","url":"postgres://host/db","max_connections":12}"#)
                .expect("parse postgres");
        assert_eq!(
            cfg.backend,
            DatabaseBackend::Postgres {
                url: "postgres://host/db".into(),
                max_connections: Some(12)
            }
        );
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn postgres_rejects_a_filesystem_path_with_an_actionable_message() {
        let cfg = parse(r#"{"backend":"postgres","url":"./data/ares.db"}"#).expect("parse");
        let err = cfg.validate().expect_err("must reject a path as a url");
        assert!(
            err.contains("postgres://"),
            "message should name the expected form: {err}"
        );
        assert!(
            err.contains("embedded"),
            "message should point at the embedded backend: {err}"
        );
    }

    #[test]
    fn embedded_rejects_blank_path() {
        let cfg = parse(r#"{"backend":"embedded","path":"  "}"#).expect("parse");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn unknown_backend_is_a_parse_error_not_a_silent_default() {
        // The whole point: a value the program cannot honor must not be
        // accepted and quietly replaced by a default.
        assert!(parse(r#"{"backend":"sqlite","path":"./x.db"}"#).is_err());
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(parse(r#"{"backend":"embedded","path":"./x.db","nonsense":1}"#).is_err());
    }

    #[test]
    fn config_round_trips_through_serialisation() {
        let cfg = DatabaseConfig {
            backend: DatabaseBackend::Postgres {
                url: "postgres://u:p@h:5432/d".into(),
                max_connections: Some(5),
            },
            qdrant: None,
        };
        let toml = toml::to_string(&cfg).expect("serialise");
        let back: DatabaseConfig = toml::from_str(&toml).expect("deserialise");
        assert_eq!(cfg, back);
    }
}
