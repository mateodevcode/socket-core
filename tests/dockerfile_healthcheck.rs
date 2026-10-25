// N3 — El HEALTHCHECK del contenedor prueba el puerto público Y el admin.
#[test]
fn healthcheck_prueba_ambos_listeners() {
    let d = std::fs::read_to_string("Dockerfile").expect("Dockerfile");
    let hc = d
        .lines()
        .skip_while(|l| !l.starts_with("HEALTHCHECK"))
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        hc.contains("3005/health"),
        "healthcheck debe probar el público: {}",
        hc
    );
    assert!(
        hc.contains("3015/health"),
        "healthcheck debe probar el admin: {}",
        hc
    );

    // docker-compose define SU PROPIO healthcheck (sobreescribe el del Dockerfile).
    let dc = std::fs::read_to_string("docker-compose.yml").expect("docker-compose.yml");
    let bloque = dc
        .lines()
        .skip_while(|l| !l.contains("healthcheck:"))
        .take(6)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bloque.contains("3005/health") && bloque.contains("3015/health"),
        "el healthcheck de compose debe probar AMBOS listeners: {}",
        bloque
    );
}
