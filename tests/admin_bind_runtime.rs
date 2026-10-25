// N1 — El listener admin solo escucha en loopback (no en otras interfaces).
mod common;

#[tokio::test]
async fn admin_no_escucha_en_otra_interfaz() {
    let hub = common::Hub::start().await;
    let port: u16 = hub
        .admin_base
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .expect("puerto admin");

    // IP local NO loopback (no envía nada; solo resuelve la interfaz de salida).
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    let _ = probe.connect("192.0.2.1:80");
    let ip = probe.local_addr().unwrap().ip();
    if ip.is_loopback() {
        eprintln!("sin interfaz no-loopback en este entorno: comprobación omitida");
        return;
    }

    let res = tokio::net::TcpStream::connect((ip, port)).await;
    assert!(
        res.is_err(),
        "el admin NO debe escuchar en {}:{} (solo 127.0.0.1)",
        ip,
        port
    );
}
