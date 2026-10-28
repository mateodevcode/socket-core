// Q2/Q3/Q4 — contexto de build, line endings, --locked y digests.
#[test]
fn dockerignore_existe_y_excluye_lo_sensible() {
    let d = std::fs::read_to_string(".dockerignore").expect(".dockerignore debe existir");
    for pat in [
        ".env",
        "data",
        "backups",
        "proyectos",
        "target",
        ".git",
        "node_modules",
    ] {
        assert!(d.contains(pat), ".dockerignore debe excluir {}", pat);
    }
}

#[test]
fn gitattributes_fuerza_lf_en_scripts() {
    let g = std::fs::read_to_string(".gitattributes").expect(".gitattributes debe existir");
    assert!(
        g.lines().any(|l| l.trim() == "*.sh text eol=lf"),
        ".gitattributes debe contener '*.sh text eol=lf'"
    );
}

#[test]
fn entrypoint_no_tiene_crlf() {
    let bytes = std::fs::read("docker-entrypoint.sh").expect("docker-entrypoint.sh");
    assert!(
        !bytes.windows(2).any(|w| w == b"\r\n"),
        "docker-entrypoint.sh no debe contener CRLF"
    );
}

#[test]
fn scripts_sh_no_tienen_crlf() {
    for f in ["docker-entrypoint.sh", "scripts/deploy.sh"] {
        let bytes = std::fs::read(f).unwrap_or_else(|_| panic!("falta {}", f));
        assert!(
            !bytes.windows(2).any(|w| w == b"\r\n"),
            "{} no debe contener CRLF (shellcheck/CI)",
            f
        );
    }
}

#[test]
fn dockerfile_usa_locked_y_digests() {
    let d = std::fs::read_to_string("Dockerfile").expect("Dockerfile");
    let builds = d.matches("cargo build --release").count();
    let locked = d.matches("cargo build --release --locked").count();
    assert_eq!(
        builds, locked,
        "todos los builds del Dockerfile deben usar --locked"
    );
    assert!(
        d.contains("rust:1.97-bookworm@sha256:"),
        "FROM rust por digest"
    );
    assert!(
        d.contains("debian:bookworm-slim@sha256:"),
        "FROM debian por digest"
    );
}

#[test]
fn ci_usa_locked() {
    let c = std::fs::read_to_string(".github/workflows/ci.yml").expect("ci.yml");
    for cmd in [
        "cargo build --locked",
        "cargo clippy --locked",
        "cargo test --locked",
    ] {
        assert!(c.contains(cmd), "CI debe usar '{}'", cmd);
    }
}

#[test]
fn compose_espera_mas_que_la_gracia_de_apagado() {
    let c = std::fs::read_to_string("docker-compose.yml").expect("compose");
    assert!(
        c.contains("stop_grace_period: 20s"),
        "compose debe definir stop_grace_period: 20s (> SHUTDOWN_GRACE_SECS)"
    );
}

#[test]
fn nginx_superficie_y_compose_unless_stopped() {
    let lim = std::fs::read_to_string("nginx/conf.d/socketcore-limits.conf").unwrap();
    assert!(lim.contains("limit_conn_status 429;"));
    assert!(lim.contains("map $http_upgrade $connection_upgrade"));
    let site = std::fs::read_to_string("nginx/ws.seventwo.tech.https.conf").unwrap();
    assert!(site.contains("limit_conn ws_conn 30;"));
    assert!(site.contains("proxy_set_header Connection $connection_upgrade;"));
    assert!(site.contains("location /admin/ {"));
    assert!(site.contains("return 404;"));
    for loc in [
        "location = /health",
        "location = /auth/token",
        "location /ws",
        "location /channels",
        "location = /stats",
        "location / {",
    ] {
        assert!(site.contains(loc), "falta {}", loc);
    }
    let dc = std::fs::read_to_string("docker-compose.yml").unwrap();
    assert!(dc.contains("restart: unless-stopped"));
}
