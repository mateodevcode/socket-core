// FASE 7 - Todos los códigos de error usados en src/ deben estar documentados
// en docs/CODIGOS.md (contrato estable para clientes/SDK).
use std::collections::BTreeSet;
use std::fs;

fn extraer_codigos(src: &str, out: &mut BTreeSet<String>) {
    // JSON: "code": "xxx"
    for (i, _) in src.match_indices("\"code\":") {
        let rest = &src[i + "\"code\":".len()..];
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                let code = &rest[..end];
                if !code.is_empty()
                    && code
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                {
                    out.insert(code.to_string());
                }
            }
        }
    }
    // close_with(&mut sender, "xxx", ...)
    for (i, _) in src.match_indices("close_with(") {
        let rest = &src[i..];
        if let Some(q) = rest.find('"') {
            let after = &rest[q + 1..];
            if let Some(end) = after.find('"') {
                let code = &after[..end];
                if !code.is_empty()
                    && code
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                {
                    out.insert(code.to_string());
                }
            }
        }
    }
}

#[test]
fn todos_los_codigos_estan_documentados() {
    let mut codigos = BTreeSet::new();
    for entry in fs::read_dir("src").expect("src") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let contenido = fs::read_to_string(&path).expect("leer src");
            extraer_codigos(&contenido, &mut codigos);
        }
    }
    assert!(!codigos.is_empty(), "no se extrajo ningún código de src/");

    let doc = fs::read_to_string("docs/CODIGOS.md")
        .expect("debe existir docs/CODIGOS.md con los códigos estables");
    let faltan: Vec<&String> = codigos
        .iter()
        .filter(|c| !doc.contains(c.as_str()))
        .collect();
    assert!(
        faltan.is_empty(),
        "códigos usados en src/ y NO documentados en docs/CODIGOS.md: {:?}",
        faltan
    );

    // Lista mínima exigida por el contrato del producto.
    for obligatorio in [
        "token_expired",
        "forbidden",
        "rate_limited",
        "message_too_large",
        "missed_events",
        "credential_revoked",
        "token_revoked",
        "channel_deleted",
        "server_shutdown",
        "idle_timeout",
        "handshake_timeout",
        "invalid_payload",
        "not_found",
    ] {
        assert!(
            doc.contains(obligatorio),
            "falta el código obligatorio {} en docs/CODIGOS.md",
            obligatorio
        );
    }
}
