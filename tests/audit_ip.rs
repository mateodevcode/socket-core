// M3 — La auditoría de auth_failed/rate_limited incluye IP y nunca secretos.
mod common;

use common::Hub;

#[tokio::test]
async fn auditoria_incluye_ip_sin_secretos() {
    let limits = socket_core::limits::Limits {
        ip_rate_per_min: 1,
        ..Default::default()
    };
    let hub = Hub::start_with_limits(limits).await;
    let raw = "sc_live_inventada_inventada_inventada";
    let url = format!("{}/channels", hub.base);

    let a = hub
        .client
        .get(&url)
        .header("x-api-key", raw)
        .send()
        .await
        .unwrap();
    let b = hub
        .client
        .get(&url)
        .header("x-api-key", raw)
        .send()
        .await
        .unwrap();
    assert_eq!(a.status(), 401);
    assert_eq!(b.status(), 429);

    let entries = hub.state.audit.snapshot();
    assert!(
        entries
            .iter()
            .any(|e| e.contains("auth_failed") && e.contains("ip=")),
        "auth_failed debe incluir ip=: {:?}",
        entries
    );
    assert!(
        entries
            .iter()
            .any(|e| e.contains("rate_limited") && e.contains("ip=")),
        "rate_limited debe incluir ip=: {:?}",
        entries
    );
    for e in &entries {
        assert!(!e.contains(raw), "no debe aparecer la key: {}", e);
        assert!(
            !e.contains("sc_live_"),
            "no debe aparecer prefijo de key: {}",
            e
        );
    }
}
