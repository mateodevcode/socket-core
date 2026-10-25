# ============================================
# socket-core — imagen multi-stage, no root
# ============================================
FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
# Cache de dependencias con un src vacío
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs \
    && cargo build --release --locked || true
RUN rm -rf src
COPY src ./src
# COPY puede conservar mtimes antiguos y Cargo creería que el binario dummy
# (del cache de dependencias) sigue al día. Forzamos mtime antes de compilar.
RUN find src -type f -exec touch {} + && cargo build --release --locked

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libsqlite3-0 curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --home-dir /app --shell /usr/sbin/nologin app
WORKDIR /app
RUN mkdir -p /app/data && chown -R app:app /app
COPY --from=builder --chown=app:app /app/target/release/socket-core /app/socket-core
COPY --chown=app:app docker-entrypoint.sh /app/docker-entrypoint.sh
RUN chmod 0555 /app/docker-entrypoint.sh

# Usuario sin privilegios y sin shell.
USER app

EXPOSE 3005
# El listener admin (ADMIN_PORT, 3015) escucha en 127.0.0.1 dentro del contenedor:
# no se expone y no es alcanzable desde otros contenedores.

# Comprueba AMBOS listeners (público y admin) desde dentro del contenedor.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD sh -c 'curl -fsS http://127.0.0.1:3005/health >/dev/null && curl -fsS http://127.0.0.1:3015/health >/dev/null' || exit 1

ENTRYPOINT ["/app/docker-entrypoint.sh"]
