use std::env;

/// Configuración central del servicio. Todo sale de ENV para 12-factor.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_ttl_secs: i64,
    pub master_key: String,
    /// Orígenes globales permitidos para CORS (coma-separados). Ej: https://app.midominio.com
    /// "*" = permisivo (solo dev, se loguea warning).
    pub allowed_origins: Vec<String>,
    /// Si true y no hay keys en DB, crea una key bootstrap de desarrollo.
    pub allow_bootstrap: bool,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port: u16 = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3005);
        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite:./data/socket-core.db?mode=rwc".to_string());
        let jwt_secret =
            env::var("JWT_SECRET").unwrap_or_else(|_| "dev-change-me-please-rotate".to_string());
        let jwt_ttl_secs: i64 = env::var("JWT_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        let master_key = env::var("MASTER_KEY").unwrap_or_else(|_| "".to_string());
        let allowed_origins: Vec<String> = env::var("ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let allow_bootstrap = env::var("ALLOW_BOOTSTRAP")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(true);

        Self {
            host,
            port,
            database_url,
            jwt_secret,
            jwt_ttl_secs,
            master_key,
            allowed_origins,
            allow_bootstrap,
        }
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn cors_is_open(&self) -> bool {
        self.allowed_origins.is_empty() || self.allowed_origins.iter().any(|o| o == "*")
    }
}
