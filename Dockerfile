# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 app

WORKDIR /app

COPY --from=builder /app/target/release/exchangespreadlog /usr/local/bin/exchangespreadlog
COPY config.generated.toml /app/config.toml

RUN chown -R app:app /app

USER app

ENV RUST_LOG=info

ENTRYPOINT ["exchangespreadlog"]
CMD ["--config", "/app/config.toml", "--storage", "clickhouse", "--no-tui"]
