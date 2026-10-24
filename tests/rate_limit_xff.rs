// FASE 1 - Item 11: rotar X-Forwarded-For NO debe evadir el rate limit por IP.
mod common;

use common::Hub;
use serde_json::json;

#[tokio::test]
async fn xff_rotante_no_evita_rate_limit() {
    let hub = Hub::start().await;
    let id_a = hub.create_channel_ok(common::Project::A, "a:chat").await;

    // 700 emisiones en ráfaga con X-Forwarded-For distinto cada vez.
    let mut hubo_429 = false;
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
        if res.status() == 429 {
            hubo_429 = true;
            break;
        }
    }
    assert!(
        hubo_429,
        "rotar X-Forwarded-For NO debe evadir el rate limit por IP (600/desde la emisión)"
    );
}
