# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --locked --release

FROM ghcr.io/astral-sh/uv:latest AS uv

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata python3 python-is-python3 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 app

WORKDIR /app

COPY --from=uv /uv /uvx /usr/local/bin/
COPY --from=builder /app/target/release/exchangespreadlog /usr/local/bin/exchangespreadlog
COPY scripts ./scripts
COPY docker/entrypoint.sh /usr/local/bin/exchangespreadlog-entrypoint

RUN chmod +x /usr/local/bin/exchangespreadlog-entrypoint

RUN chown -R app:app /app

USER app

ENV RUST_LOG=info
ENV UV_PYTHON_DOWNLOADS=never
ENV GENERATED_CONFIG_PATH=/app/config.generated.toml

ENTRYPOINT ["exchangespreadlog-entrypoint"]
CMD ["--storage", "clickhouse", "--no-tui"]
