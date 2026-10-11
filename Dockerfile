# ============================================
# socket-core definitivo (seguro, SQLite, sin Tailscale)
# Nota: el manifest en este repo se llama `cargo.toml` en minúsculas
# (Windows es case-insensitive). Por eso el COPY lo renombra a Cargo.toml.
# ============================================
FROM rust:1-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*
# Manifest (tolerante al nombre en minúsculas)
COPY cargo.toml ./Cargo.toml
COPY Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release || true
RUN rm -rf src
COPY src ./src
# Asegura nombre correcto dentro del contenedor y compila
RUN cp ./Cargo.toml ./Cargo.toml.bak 2>/dev/null || true
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates libsqlite3-0 curl \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /app/data
COPY --from=builder /app/target/release/socket-core /app/socket-core
EXPOSE 3005
CMD ["./socket-core"]
