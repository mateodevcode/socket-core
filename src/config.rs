use ipnet::IpNet;
use std::env;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

pub const DEFAULT_JWT_SECRET: &str = "dev-change-me-please-rotate";

/// Configuración central del servicio. Todo sale de ENV para 12-factor.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    /// Listener de administración (solo loopback).
    pub admin_port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_ttl_secs: i64,
    pub master_key: String,
    /// Orígenes globales permitidos para CORS (coma-separados).
    pub allowed_origins: Vec<String>,
    /// "dev" relaja CORS; cualquier otro valor (prod) exige ALLOWED_ORIGINS.
    pub app_env: String,
    /// Bootstrap de desarrollo: desactivado por defecto.
    pub allow_bootstrap: bool,
    /// IPs/CIDR de proxies de confianza para leer X-Forwarded-For / X-Real-IP.
    pub trusted_proxies: Vec<String>,
    /// Tiempo máximo total de apagado ordenado.
    pub shutdown_grace_secs: u64,
    /// Peticiones por minuto por IP (antes de validar la API key).
    pub ip_rate_per_min: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self::from_env_reader(|k| env::var(k).ok())
    }

    /// Constructor testeable: permite inyectar la fuente de variables.
    pub fn from_env_reader(get: impl Fn(&str) -> Option<String>) -> Self {
        let host = get("HOST").unwrap_or_else(|| "0.0.0.0".to_string());
        let port: u16 = get("PORT").and_then(|v| v.parse().ok()).unwrap_or(3005);
        let admin_port: u16 = get("ADMIN_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3015);
        let database_url = get("DATABASE_URL")
            .unwrap_or_else(|| "sqlite:./data/socket-core.db?mode=rwc".to_string());
        let jwt_secret = get("JWT_SECRET").unwrap_or_else(|| DEFAULT_JWT_SECRET.to_string());
        let jwt_ttl_secs: i64 = get("JWT_TTL_SECS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        let master_key = get("MASTER_KEY").unwrap_or_default();
        let allowed_origins: Vec<String> = get("ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let app_env = get("APP_ENV").unwrap_or_else(|| "prod".to_string());
        let allow_bootstrap = get("ALLOW_BOOTSTRAP")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let trusted_proxies: Vec<String> = get("TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let shutdown_grace_secs: u64 = get("SHUTDOWN_GRACE_SECS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        let ip_rate_per_min: i64 = get("IP_RATE_PER_MIN")
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);

        Self {
            host,
            port,
            admin_port,
            database_url,
            jwt_secret,
            jwt_ttl_secs,
            master_key,
            allowed_origins,
            app_env,
            allow_bootstrap,
            trusted_proxies,
            shutdown_grace_secs,
            ip_rate_per_min,
        }
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn admin_addr(&self) -> String {
        format!("127.0.0.1:{}", self.admin_port)
    }

    pub fn is_dev(&self) -> bool {
        self.app_env.eq_ignore_ascii_case("dev")
    }

    /// Valida los secretos al arrancar. Err => NO iniciar. Nunca incluye el secreto.
    pub fn validate_secrets(&self) -> Result<(), String> {
        if self.jwt_secret == DEFAULT_JWT_SECRET {
            return Err(
                "JWT_SECRET sigue siendo el valor por defecto; define uno aleatorio".into(),
            );
        }
        if self.jwt_secret.len() < 32 {
            return Err("JWT_SECRET debe tener al menos 32 caracteres".into());
        }
        if self.master_key.len() < 32 {
            return Err("MASTER_KEY debe tener al menos 32 caracteres".into());
        }
        if !(60..=3600).contains(&self.jwt_ttl_secs) {
            return Err("JWT_TTL_SECS debe estar entre 60 y 3600".into());
        }
        if self.shutdown_grace_secs == 0 || self.shutdown_grace_secs >= 20 {
            return Err(
                "SHUTDOWN_GRACE_SECS debe estar entre 1 y 19 (stop_grace_period del contenedor es 20 s)"
                    .into(),
            );
        }
        if !(1..=100_000).contains(&self.ip_rate_per_min) {
            return Err("IP_RATE_PER_MIN debe estar entre 1 y 100000".into());
        }
        if self.allow_bootstrap && !self.is_dev() {
            return Err("ALLOW_BOOTSTRAP=true solo está permitido con APP_ENV=dev".into());
        }
        for p in &self.trusted_proxies {
            let net =
                parse_ip_or_cidr(p).ok_or_else(|| format!("TRUSTED_PROXIES inválido: {}", p))?;
            if net.prefix_len() == 0
                || net.addr().is_loopback()
                || net.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST))
                || net.contains(&IpAddr::V6(Ipv6Addr::LOCALHOST))
            {
                return Err(
                    "TRUSTED_PROXIES no puede incluir loopback ni 0.0.0.0/0 (confiarías en cabeceras de cualquiera)"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// ALLOWED_ORIGINS obligatorio salvo APP_ENV=dev. Sin "*" en producción.
    pub fn validate_origins(&self) -> Result<(), String> {
        if self.is_dev() {
            return Ok(());
        }
        if self.allowed_origins.is_empty() {
            return Err(
                "ALLOWED_ORIGINS es obligatorio en producción (APP_ENV=dev para desarrollo)".into(),
            );
        }
        if self.allowed_origins.iter().any(|o| o == "*") {
            return Err("ALLOWED_ORIGINS no puede contener '*' en producción".into());
        }
        Ok(())
    }

    /// Parsea TRUSTED_PROXIES a redes normalizadas.
    pub fn trusted_proxy_nets(&self) -> Vec<IpNet> {
        self.trusted_proxies
            .iter()
            .filter_map(|s| parse_ip_or_cidr(s))
            .collect()
    }
}

fn parse_ip_or_cidr(s: &str) -> Option<IpNet> {
    let s = s.trim();
    if let Ok(net) = IpNet::from_str(s) {
        return Some(net);
    }
    if let Ok(ip) = IpAddr::from_str(s) {
        return Some(IpNet::from(ip));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trusted_proxies() {
        let cfg = AppConfig::from_env_reader(|k| match k {
            "TRUSTED_PROXIES" => Some("10.0.0.0/8, 192.168.1.5".to_string()),
            _ => None,
        });
        let nets = cfg.trusted_proxy_nets();
        assert_eq!(nets.len(), 2);
        let ip1: std::net::IpAddr = "10.1.2.3".parse().unwrap();
        let ip2: std::net::IpAddr = "192.168.1.5".parse().unwrap();
        assert!(nets[0].contains(&ip1));
        assert!(nets[1].contains(&ip2));
    }
}
