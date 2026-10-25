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
}
