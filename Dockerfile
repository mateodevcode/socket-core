# ============================================
# socket-core (build multi-stage)
# ============================================
FROM rust:1-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs && cargo build --release || true
RUN rm -rf src
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates libsqlite3-0 curl \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /app/data
COPY --from=builder /app/target/release/socket-core /app/socket-core
EXPOSE 3005
CMD ["./socket-core"]
