use async_openai::Client;
use async_openai::config::OpenAIConfig;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::env;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub name: String,
    pub provider: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub openai_api_key: String,
    pub openai_base_url: Option<String>,
    pub server_addr: String,
    pub models: Vec<ModelConfig>,
}

impl Config {
    pub fn from_env() -> eyre::Result<Self> {
        dotenvy::dotenv().ok();

        let models_env = env::var("MODELS")
            .unwrap_or_else(|_| "gpt-4o-mini:GPT-4o Mini:openai,gpt-4o:GPT-4o:openai".to_string());

        let models: Vec<ModelConfig> = models_env
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| {
                let parts: Vec<&str> = s.split(':').collect();
                ModelConfig {
                    id: parts[0].to_string(),
                    name: parts.get(1).map_or(parts[0], |v| *v).to_string(),
                    provider: parts.get(2).map_or("openai", |v| *v).to_string(),
                }
            })
            .collect();

        Ok(Self {
            database_url: env::var("DATABASE_URL")
                .map_err(|_| eyre::eyre!("DATABASE_URL not set"))?,
            openai_api_key: env::var("OPENAI_API_KEY")
                .map_err(|_| eyre::eyre!("OPENAI_API_KEY not set"))?,
            openai_base_url: env::var("OPENAI_BASE_URL").ok(),
            server_addr: env::var("SERVER_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string()),
            models,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Config,
    pub openai_client: Client<OpenAIConfig>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Self {
        let mut openai_config = OpenAIConfig::new().with_api_key(&config.openai_api_key);
        if let Some(base_url) = &config.openai_base_url {
            openai_config = openai_config.with_api_base(base_url);
        }
        let openai_client = Client::with_config(openai_config);

        Self {
            db,
            config,
            openai_client,
        }
    }
}
