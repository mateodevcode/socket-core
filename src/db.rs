use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;
use tracing::info;

/// Inicializa el pool SQLite y corre el schema mínimo (idempotente).
pub async fn init_pool(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    // Asegurar directorio padre para sqlite:./data/...
    if let Some(path_part) = database_url
        .trim_start_matches("sqlite:")
        .split('?')
        .next()
    {
        if let Some(parent) = std::path::Path::new(path_part).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
    }

    let opts = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS api_keys (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            key_hash TEXT NOT NULL UNIQUE,
            domains TEXT NOT NULL DEFAULT '*',
            channel_prefix TEXT NOT NULL DEFAULT '*',
            scopes TEXT NOT NULL DEFAULT 'publish,subscribe',
            rate_limit_per_min INTEGER NOT NULL DEFAULT 120,
            expires_at INTEGER,
            revoked INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS revoked_tokens (
            jti TEXT PRIMARY KEY,
            exp INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);")
        .execute(&pool)
        .await?;

    // Limpieza oportunista de JTIs expirados
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query("DELETE FROM revoked_tokens WHERE exp < ?")
        .bind(now)
        .execute(&pool)
        .await;

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_keys")
        .fetch_one(&pool)
        .await?;
    info!("SQLite listo ({} keys registradas)", count.0);
    Ok(pool)
}
