use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::llm::ProviderRegistry;
use crate::turn_stream::TurnStreamHub;
use crate::turn_task_manager::TurnTaskManager;

/// Wire protocol a session (and by extension its turns) speaks to the upstream
/// LLM vendor. Locked at session creation time and immutable thereafter.
///
/// # Storage strategy: TEXT + CHECK, not a Postgres ENUM
///
/// A Postgres ENUM (`CREATE TYPE protocol AS ENUM (...)`) would require a
/// dedicated `ALTER TYPE ... ADD VALUE` migration every time a new protocol is
/// added.  Using a plain `TEXT` column with a `CHECK (protocol IN ('openai',
/// 'anthropic'))` constraint is equivalent in safety but only needs a table
/// rewrite (via the single initial migration) — far simpler during early
/// development when protocols are still being added.
///
/// The sqlx derive `#[sqlx(type_name = "TEXT", rename_all = "lowercase")]`
/// tells sqlx to treat this as a TEXT column and apply lowercase
/// serialization, which is enough for round-tripping without a custom
/// `sqlx::Type` impl.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum Protocol {
    Openai,
    Anthropic,
}

impl Protocol {
    /// Returns the lowercase string used both in JSON payloads and in the
    /// database CHECK constraint.  Kept as `&'static str` so callers can
    /// borrow without allocating.
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Openai => "openai",
            Protocol::Anthropic => "anthropic",
        }
    }
}

/// A model exposed by a provider.
///
/// - `id` is the **wire model identifier** sent verbatim in the `model` field
///   of the upstream API request (e.g. `"gpt-4o"`, `"deepseek-chat"`). It must
///   exactly match what the provider expects.
/// - `name` is an optional **display label** for the UI.  When `None`, the
///   frontend falls back to showing `id` directly.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelConfig {
    /// Wire model id sent to the upstream LLM API verbatim.
    pub id: String,
    /// Optional human-friendly display name for the UI. Falls back to `id`.
    #[serde(default)]
    pub name: Option<String>,
}

/// Credentials for reaching a particular provider over a particular wire
/// protocol.
///
/// This is the "glue" between a provider and a protocol: the same provider
/// (e.g. DeepSeek) may expose both an OpenAI-compatible and a native Anthropic
/// endpoint, each with different `base_url` / `api_key` pairs.  Each such pair
/// is one `ProtocolBinding`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProtocolBinding {
    /// Base URL for the upstream API (e.g. `"https://api.openai.com/v1"`).
    pub base_url: String,
    /// API key for authentication.  Loaded from config, never written back.
    pub api_key: String,
}

/// A provider (DeepSeek, GLM, OpenAI, Anthropic, ...).
///
/// One provider can speak **multiple protocols** — the `protocols` map keys are
/// `Protocol` variants and the values are the per-protocol credentials.  This
/// lets a single provider entry like DeepSeek support both OpenAI-compatible
/// requests and (hypothetically) an Anthropic-style endpoint, without
/// duplicating the model list.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// Unique human-readable name used as a stable key in the UI and API.
    pub name: String,
    /// Models this provider exposes. At least one required (enforced by `validate`).
    pub models: Vec<ModelConfig>,
    /// Protocol -> credentials map. At least one entry required (enforced by `validate`).
    pub protocols: HashMap<Protocol, ProtocolBinding>,
}

impl ProviderConfig {
    /// Look up credentials for a specific protocol.  Returns `None` if this
    /// provider does not support the requested protocol.
    pub fn binding(&self, protocol: Protocol) -> Option<&ProtocolBinding> {
        self.protocols.get(&protocol)
    }

    /// Check whether the provider exposes a model with the given wire id.
    pub fn has_model(&self, model_id: &str) -> bool {
        self.models.iter().any(|m| m.id == model_id)
    }

    /// Returns the list of protocols this provider supports, sorted
    /// deterministically by name.  The sort ensures API responses and test
    /// assertions are stable regardless of HashMap iteration order.
    pub fn supported_protocols(&self) -> Vec<Protocol> {
        let mut v: Vec<Protocol> = self.protocols.keys().copied().collect();
        // Deterministic order for client/test stability.
        v.sort_by_key(|p| p.as_str());
        v
    }
}

/// Root application configuration.
///
/// Loaded once at process startup via [`Config::load`].  After loading the
/// config is wrapped in `Arc` and shared immutably across all handlers and
/// background tasks — it is never mutated at runtime.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// PostgreSQL connection string (e.g. `postgres://user:pass@localhost/db`).
    pub database_url: String,
    /// Socket address to bind the HTTP server to. Defaults to `0.0.0.0:3000`.
    #[serde(default = "default_server_addr")]
    pub server_addr: String,
    /// Optional directory containing the built frontend assets (`index.html`,
    /// JS, CSS, fonts, etc.). When present and valid, the backend will serve
    /// those files directly instead of requiring a separate frontend server.
    #[serde(default)]
    pub frontend_dist_dir: Option<String>,
    /// List of LLM providers.  At least one required (enforced by `validate`).
    pub providers: Vec<ProviderConfig>,
}

fn default_server_addr() -> String {
    "0.0.0.0:3000".to_string()
}

impl Config {
    /// Look up a provider by its unique name.  Used by handlers to resolve
    /// the `provider` field from incoming requests into actual credentials.
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Load the config using a two-layer strategy:
    ///
    /// 1. **JSON file** — the path comes from `FORK_CHAT_CONFIG` env (defaults
    ///    to `./config.json`).  This is the primary config source.
    /// 2. **Environment overrides** — any env var with prefix `FORK_CHAT_`
    ///    (separator `__`) overrides a value from the file.  For example,
    ///    `FORK_CHAT_DATABASE_URL` overrides `database_url`, and
    ///    `FORK_CHAT_PROVIDERS__0__NAME` overrides the first provider's name.
    ///
    /// This layering lets operators keep a base `config.json` in version
    /// control and override secrets (API keys, database URL) via env vars in
    /// production.
    pub fn load() -> eyre::Result<Self> {
        // Load `.env` file if present.  This is purely a convenience for local
        // development so `DATABASE_URL` and `FORK_CHAT_CONFIG` can live there.
        // In production, env vars are set directly by the orchestrator.
        dotenvy::dotenv().ok();

        // Determine the config file path.  The env override is useful for
        // pointing to a different config in tests or CI.
        let path: PathBuf = std::env::var("FORK_CHAT_CONFIG")
            .unwrap_or_else(|_| "./config.json".to_string())
            .into();

        // Layer 1: JSON file (base values).
        // Layer 2: Environment with prefix `FORK_CHAT_` (overrides).
        // `try_parsing(true)` lets the env layer attempt automatic type
        // coercion (e.g. string "3000" -> integer 3000), which is necessary
        // because env vars are always strings.
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

        // Structural validation (cross-field checks that serde can't express).
        parsed.validate()?;
        Ok(parsed)
    }

    /// Validate structural invariants that cannot be expressed through serde
    /// attributes alone.
    ///
    /// Rules checked:
    /// - `database_url` must be non-empty (a missing value would cause a
    ///   confusing sqlx error later).
    /// - `providers` must be non-empty (no point running without providers).
    /// - `frontend_dist_dir`, when provided, must not be an empty string
    ///   (blank paths silently disable static serving and are hard to debug).
    /// - Each provider name must be non-empty and globally unique (used as a
    ///   lookup key throughout the codebase).
    /// - Each provider must declare at least one protocol binding (otherwise
    ///   it has no way to talk to any upstream API).
    /// - Each provider must declare at least one model (otherwise the UI would
    ///   show an empty model dropdown).
    /// - Model ids within a provider must be unique and non-empty (duplicate
    ///   ids would cause ambiguous model resolution).
    fn validate(&self) -> eyre::Result<()> {
        if self.database_url.is_empty() {
            eyre::bail!("config: database_url is required");
        }
        if self.providers.is_empty() {
            eyre::bail!("config: providers must be non-empty");
        }
        if self
            .frontend_dist_dir
            .as_deref()
            .is_some_and(|dir| dir.trim().is_empty())
        {
            eyre::bail!("config: frontend_dist_dir must not be empty when provided");
        }

        // Track provider names globally to detect duplicates across the list.
        let mut seen = std::collections::HashSet::new();
        for p in &self.providers {
            // Provider name serves as a stable key in API requests, so it
            // cannot be empty.
            if p.name.is_empty() {
                eyre::bail!("config: provider name must be non-empty");
            }
            // Duplicate provider names would cause ambiguous lookups in
            // `Config::provider()`.
            if !seen.insert(p.name.clone()) {
                eyre::bail!("config: duplicate provider name '{}'", p.name);
            }
            // A provider with no protocol bindings has no way to reach any
            // upstream LLM API.
            if p.protocols.is_empty() {
                eyre::bail!(
                    "config: provider '{}' must declare at least one protocol binding",
                    p.name
                );
            }
            // A provider with no models would show an empty model dropdown in
            // the UI and cannot serve any requests.
            if p.models.is_empty() {
                eyre::bail!(
                    "config: provider '{}' must declare at least one model",
                    p.name
                );
            }
            // Track model ids within this provider to detect duplicates.
            let mut model_ids = std::collections::HashSet::new();
            for m in &p.models {
                if m.id.is_empty() {
                    eyre::bail!("config: provider '{}' has empty model id", p.name);
                }
                // Duplicate model ids within one provider would cause
                // ambiguous model resolution in the turn lifecycle.
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

/// Runtime state shared by every request handler via axum's `State` extractor.
///
/// All fields use `Arc` (or are `Clone`-cheap like `PgPool`) because axum
/// clones `AppState` for each handler invocation.  `Arc` ensures the inner
/// data is reference-counted rather than deep-copied.
///
/// # Why `Arc` for every field?
///
/// - `PgPool` is already internally `Arc`-wrapped, so cloning it is cheap.
/// - `Config` is immutable after startup — a single `Arc<Config>` is shared
///   across all handlers.
/// - `ProviderRegistry` is built once from config and never mutated.
/// - `TurnStreamHub` is a live pub/sub hub that SSE handlers subscribe to;
///   it must be the same instance everywhere (hence `Arc`).
/// - `TurnTaskManager` tracks in-flight turn tasks and their cancellation
///   tokens; it must also be a single shared instance.
#[derive(Clone)]
pub struct AppState {
    /// Shared PostgreSQL connection pool.  sqlx pools are designed to be
    /// cloned and shared across tasks.
    pub db: PgPool,
    /// Immutable application configuration loaded at process startup.
    pub config: Arc<Config>,
    /// Protocol/provider adapter registry used by the turn lifecycle to
    /// dispatch API calls to the correct upstream implementation.
    pub registry: Arc<ProviderRegistry>,
    /// Per-turn SSE pub/sub hub.  Turn workers publish events here;
    /// handler-held SSE connections subscribe to receive them.
    pub turn_stream_hub: Arc<TurnStreamHub>,
    /// Process-local task/cancellation registry for active turn loops.
    /// Allows the cancel handler to abort a running turn by its task handle.
    pub turn_task_manager: Arc<TurnTaskManager>,
}

impl AppState {
    /// Construct application state and all in-memory shared services.
    ///
    /// Called once in `main()` after the database pool is ready.  Each service
    /// is wrapped in `Arc` immediately so they can be freely cloned into
    /// handler closures.
    pub fn new(db: PgPool, config: Config) -> Self {
        let config = Arc::new(config);
        // Build the protocol adapter registry from config.  This maps each
        // (provider, protocol) pair to a concrete LLM client implementation.
        let registry = Arc::new(ProviderRegistry::from_config(&config));
        let turn_stream_hub = Arc::new(TurnStreamHub::new());
        let turn_task_manager = Arc::new(TurnTaskManager::new());
        Self {
            db,
            config,
            registry,
            turn_stream_hub,
            turn_task_manager,
        }
    }
}
