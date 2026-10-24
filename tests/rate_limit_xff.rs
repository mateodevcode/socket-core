// FASE 1 - Item 11: rotar X-Forwarded-For NO debe evadir el rate limit por IP.
mod common;

use common::Hub;
use serde_json::json;

#[tokio::test]
async fn xff_rotante_no_evita_rate_limit() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(common::Project::A, "a:chat").await;

    // 700 emisiones en ráfaga con X-Forwarded-For distinto cada vez.
    let (mut ok, mut otros) = (0u32, Vec::new());
    for i in 0..700u32 {
        let f1 = (i / 250) % 256;
        let f2 = i % 250;
        let res = hub
            .client
            .post(format!("{}/channels/{}/events", hub.base, id_a))
            .header("x-api-key", &hub.key_a)
            .header("x-forwarded-for", format!("10.0.{}.{}.1", f1, f2))
            .json(&json!({"source": "backend", "targets": ["*"], "payload": {"seq": i}}))
            .send()
            .await
            .expect("emit");
        match res.status().as_u16() {
            200 => ok += 1,
            429 => otros.push(429),
            s => otros.push(s),
        }
    }
    assert!(
        otros.contains(&429),
        "rotar X-Forwarded-For evadió el rate limit por IP en absoluto: \
         {}/700 pasaron como 200, 429 recibidos = 0, otros estados = {:?}",
        ok,
        otros,
    );
}

#[tokio::test]
async fn exceder_emit_bajo_limiter_devuelve_429_no_401() {
    // Control: con X-Api-Key válida y SIN XFF, agotar el bucket "emit:{ip}" (600/min)
    // debe responder 429. Hoy la rama fallback JWT enmascara el rechazo como 401
    // ("requiere X-Api-Key o JWT publish"), lo que rompe la semántica del límite.
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(common::Project::A, "a:chat").await;
    let mut hubo_429 = false;
    let mut hubo_401 = false;
    for i in 0..700u32 {
        let res = hub
            .client
            .post(format!("{}/channels/{}/events", hub.base, id_a))
            .header("x-api-key", &hub.key_a)
            .json(&json!({"source": "backend", "targets": ["*"], "payload": {"seq": i}}))
            .send()
            .await
            .expect("emit");
        match res.status().as_u16() {
            429 => hubo_429 = true,
            401 => hubo_401 = true,
            _ => {}
        }
        if hubo_429 {
            break;
        }
    }
    assert!(hubo_429, "agotar el límite de emisión debe devolver 429");
    assert!(
        !hubo_401,
        "con X-Api-Key válida NUNCA debe aparecer 401 (enmascaramiento de la rama fallback)"
    );
}
