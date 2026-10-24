// FASE 1 - Item 5: política de prefijos "a:*".
// "a2:x", "a" (sin sufijo), "A:x" y "a:" (segmento vacío) NO son parte de "a:*".
mod common;

use common::{Hub, Project};

#[tokio::test]
async fn prefijo_a_rechaza_a2() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "a2:x").await;
    assert_eq!(res.status(), 403, "a2:x no pertenece a a:*");
}

#[tokio::test]
async fn prefijo_a_rechaza_exacto_sin_prefijo() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "a").await;
    let s = res.status().as_u16();
    assert!(
        s == 403 || s == 404,
        "\"a\" (sin puntos de canal) debe rechazarse para prefijo a:*, status={}",
        s
    );
}

#[tokio::test]
async fn prefijo_a_rechaza_mayusculas() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "A:x").await;
    let s = res.status().as_u16();
    assert!(
        s == 403 || s == 404,
        "A:x no pertenece a a:* (sensible a mayúsculas), status={}",
        s
    );
}

#[tokio::test]
async fn prefijo_a_rechaza_segmento_vacio() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "a:").await;
    let s = res.status().as_u16();
    assert!(
        s == 403 || s == 404 || s == 400,
        "\"a:\" (segmento vacío) debe rechazarse, status={}",
        s
    );
}

#[tokio::test]
async fn prefijo_a_acepta_canal_legitimo() {
    let hub = Hub::start().await;
    let res = hub.create_channel_with_name(Project::A, "a:alertas").await;
    assert_eq!(res.status(), 201, "a:alertas debe ser canal válido de a:*");
}
