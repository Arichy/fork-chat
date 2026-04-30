use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::llm::ProviderRegistry;

/// Wire protocol a session (and by extension its turns) speaks to the upstream
/// LLM vendor. Locked at session creation time.
///
/// Persisted as a plain TEXT column (constrained by a CHECK) — not a Postgres
/// ENUM type — so `#[sqlx(type_name = "TEXT")]` + `rename_all` is enough for
/// sqlx to auto-derive `Type`/`Encode`/`Decode`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum Protocol {
    Openai,
    Anthropic,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Openai => "openai",
            Protocol::Anthropic => "anthropic",
        }
    }
}

/// A model exposed by a provider. `id` is the wire model id sent to the
/// upstream API; `name` is an optional display label for the UI.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// One row of the (protocol, provider) join table: the credentials used to
/// reach a particular provider over a particular wire protocol.
#[derive(Debug, Clone, Deserialize)]
pub struct ProtocolBinding {
    pub base_url: String,
    pub api_key: String,
}

/// A provider (DeepSeek, GLM, OpenAI, Anthropic, ...). Owns a list of models
/// and a set of protocol bindings (at least one).
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub models: Vec<ModelConfig>,
    pub protocols: HashMap<Protocol, ProtocolBinding>,
}

impl ProviderConfig {
    pub fn binding(&self, protocol: Protocol) -> Option<&ProtocolBinding> {
        self.protocols.get(&protocol)
    }

    pub fn has_model(&self, model_id: &str) -> bool {
        self.models.iter().any(|m| m.id == model_id)
    }

    pub fn supported_protocols(&self) -> Vec<Protocol> {
        let mut v: Vec<Protocol> = self.protocols.keys().copied().collect();
        // Deterministic order for client/test stability.
        v.sort_by_key(|p| p.as_str());
        v
    }
}

/// Root application configuration, loaded from a JSON file via the `config`
/// crate. Env overrides are supported with prefix `FORK_CHAT_` (e.g.
/// `FORK_CHAT_DATABASE_URL`).
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database_url: String,
    #[serde(default = "default_server_addr")]
    pub server_addr: String,
    pub providers: Vec<ProviderConfig>,
}

fn default_server_addr() -> String {
    "0.0.0.0:3000".to_string()
}

impl Config {
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Load the config from the JSON file pointed to by `FORK_CHAT_CONFIG`
    /// (default `./config.json`), layered with `FORK_CHAT_*` env overrides.
    pub fn load() -> eyre::Result<Self> {
        // `.env` is still honoured so `DATABASE_URL` and `FORK_CHAT_CONFIG`
        // can live there in development.
        dotenvy::dotenv().ok();

        let path: PathBuf = std::env::var("FORK_CHAT_CONFIG")
            .unwrap_or_else(|_| "./config.json".to_string())
            .into();

        let cfg = config::Config::builder()
            .add_source(config::File::from(path).format(config::FileFormat::Json))
            .add_source(
                config::Environment::with_prefix("FORK_CHAT")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .map_err(|e| eyre::eyre!("failed to load config: {e}"))?;

        let parsed: Config = cfg
            .try_deserialize()
            .map_err(|e| eyre::eyre!("failed to parse config: {e}"))?;

        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> eyre::Result<()> {
        if self.database_url.is_empty() {
            eyre::bail!("config: database_url is required");
        }
        if self.providers.is_empty() {
            eyre::bail!("config: providers must be non-empty");
        }

        let mut seen = std::collections::HashSet::new();
        for p in &self.providers {
            if p.name.is_empty() {
                eyre::bail!("config: provider name must be non-empty");
            }
            if !seen.insert(p.name.clone()) {
                eyre::bail!("config: duplicate provider name '{}'", p.name);
            }
            if p.protocols.is_empty() {
                eyre::bail!(
                    "config: provider '{}' must declare at least one protocol binding",
                    p.name
                );
            }
            if p.models.is_empty() {
                eyre::bail!(
                    "config: provider '{}' must declare at least one model",
                    p.name
                );
            }
            let mut model_ids = std::collections::HashSet::new();
            for m in &p.models {
                if m.id.is_empty() {
                    eyre::bail!("config: provider '{}' has empty model id", p.name);
                }
                if !model_ids.insert(m.id.clone()) {
                    eyre::bail!(
                        "config: provider '{}' has duplicate model id '{}'",
                        p.name,
                        m.id
                    );
                }
            }
        }

        Ok(())
    }
}

/// Runtime state shared by every handler.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub registry: Arc<ProviderRegistry>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Self {
        let config = Arc::new(config);
        let registry = Arc::new(ProviderRegistry::from_config(&config));
        Self {
            db,
            config,
            registry,
        }
    }
}
