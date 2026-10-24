use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;
use tracing::info;

/// Inicializa el pool SQLite y corre el schema mínimo (idempotente).
pub async fn init_pool(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    // Asegurar directorio padre para sqlite:./data/...
    if let Some(path_part) = database_url.trim_start_matches("sqlite:").split('?').next() {
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

    // Registro de canales: sobrevive a reinicios (los clientes/suscriptores no).
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS channels (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            description TEXT,
            created_at INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // Migración idempotente: canales con dueño (project_id = api_keys.id).
    migrate_channels(&pool).await?;

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

/// Migración idempotente a canales con dueño:
/// 1) añade la columna project_id si no existe (ignora el error de "duplicate column"),
/// 2) asigna dueño a los canales huérfanos usando el prefijo del nombre ("a:chat" -> a key
///    con prefijo a:*/a). El comodín "*" es el último recurso (menos específico).
///
/// Los canales sin dueño posible quedan con project_id = '' (invisibles: 404 para todos).
pub async fn migrate_channels(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let alter = sqlx::query("ALTER TABLE channels ADD COLUMN project_id TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await;
    if let Err(e) = alter {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            return Err(e);
        }
    }

    let orphans = sqlx::query_as::<_, (String, String)>(
        "SELECT id, name FROM channels WHERE project_id = ''",
    )
    .fetch_all(pool)
    .await?;

    if !orphans.is_empty() {
        let n_orphans = orphans.len();
        let keys = sqlx::query_as::<_, (String, String)>("SELECT id, channel_prefix FROM api_keys")
            .fetch_all(pool)
            .await?;
        // Precalcula los tokens de prefijo de cada key con su especificidad.
        let candidates: Vec<(String, Vec<String>, u8)> = keys
            .iter()
            .map(|(id, prefixes)| {
                let toks: Vec<String> = prefixes
                    .split(',')
                    .map(|p| p.trim().trim_end_matches(":*"))
                    .filter(|p| !p.is_empty())
                    .map(|p| p.to_string())
                    .collect();
                let specificity = if toks.iter().any(|t| t == "*") { 0 } else { 1 };
                (id.clone(), toks, specificity)
            })
            .collect();

        for (chan_id, name) in orphans {
            let token = name.split(':').next().unwrap_or("");
            if token.is_empty() {
                continue;
            }
            let best = candidates
                .iter()
                .filter(|(_, toks, spec)| {
                    toks.iter().any(|t| (t == token && *spec == 1) || t == "*")
                })
                .max_by_key(|(_, _, spec)| *spec);
            if let Some((owner, _, _)) = best {
                let _ = sqlx::query("UPDATE channels SET project_id = ? WHERE id = ?")
                    .bind(owner)
                    .bind(&chan_id)
                    .execute(pool)
                    .await;
            }
        }
        info!("Migración de canales: {} repartidos por prefijo", n_orphans);
    }
    Ok(())
}
