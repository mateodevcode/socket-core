// M1 — Sin ConnectInfo no se asume 127.0.0.1: se responde 500 y se loguea.
mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::net::SocketAddr;
use tower::ServiceExt;

#[tokio::test]
async fn sin_connect_info_responde_500() {
    let hub = common::Hub::start().await;
    let app = socket_core::build_public_router(hub.state.clone());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "sin extensión ConnectInfo debe fallar cerrado (500), no asumir loopback"
    );
}

#[tokio::test]
async fn con_connect_info_sigue_el_flujo_normal() {
    let hub = common::Hub::start().await;
    let app = socket_core::build_public_router(hub.state.clone());
    let mut req = Request::builder()
        .uri("/stats")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4242))));
    let res = app.oneshot(req).await.unwrap();
    // Sin X-Api-Key: 401 normal (no 500).
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
