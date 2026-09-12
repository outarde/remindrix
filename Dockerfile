FROM debian:trixie-slim

RUN apt-get update && apt-get install -y \
    ca-certificates libssl3 libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -g 1000 appuser && \
    useradd -u 1000 -g appuser -s /bin/sh appuser

WORKDIR /app

ARG TARGETARCH
COPY artifacts/bot-${TARGETARCH} /app/bot
COPY locales ./locales
COPY migrations ./migrations

RUN mkdir -p /app/data && chown -R appuser:appuser /app

ENV XDG_DATA_HOME=/app/data
ENV RUST_LOG=info

USER appuser

ENTRYPOINT ["/app/bot"]
