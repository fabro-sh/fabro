# syntax=docker/dockerfile:1.9
#
# Runtime image for the Fabro server.
#
# Binaries are supplied pre-built via the release workflow:
#   tmp/docker-context/amd64/fabro  (x86_64-unknown-linux-musl)
#   tmp/docker-context/arm64/fabro  (aarch64-unknown-linux-musl)
#
# The image serves the HTTP API (with embedded web UI) on $PORT (default
# 32276), persists state to /storage, and runs as the unprivileged `fabro`
# user. Honoring $PORT lets PaaS providers (Railway, Fly, Render, Heroku,
# Cloud Run) route traffic without extra configuration.

FROM ghcr.io/fabro-sh/dhi-alpine-base:3.23-dev-2026-04-18

USER root

ARG TARGETARCH

RUN apk add --no-cache \
      ca-certificates \
      git \
      su-exec \
      tini \
 && addgroup -S -g 1000 fabro \
 && adduser -S -u 1000 -G fabro -h /var/fabro -s /sbin/nologin fabro \
 && install -d -o fabro -g fabro -m 0755 /var/fabro /storage

COPY --chmod=0755 tmp/docker-context/${TARGETARCH}/fabro /usr/local/bin/fabro

COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/fabro-entrypoint

ENV FABRO_HOME=/storage/.home \
    FABRO_STORAGE_DIR=/storage \
    FABRO_LOG_DESTINATION=stdout

VOLUME ["/storage"]
EXPOSE 32276

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/fabro-entrypoint"]
CMD ["sh", "-c", "exec fabro server start --foreground --bind 0.0.0.0:${PORT:-32276}"]
