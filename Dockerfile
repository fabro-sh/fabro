# syntax=docker/dockerfile:1.9
#
# Runtime image for the Fabro server.
#
# Binaries are supplied pre-built via the release workflow:
#   docker-context/amd64/fabro  (x86_64-unknown-linux-gnu)
#   docker-context/arm64/fabro  (aarch64-unknown-linux-gnu)
#
# The image serves the HTTP API (with embedded web UI) on $PORT (default
# 32276), persists state to /storage, and runs as the unprivileged `fabro`
# user. Honoring $PORT lets PaaS providers (Railway, Fly, Render, Heroku,
# Cloud Run) route traffic without extra configuration.

FROM debian:trixie-slim

ARG TARGETARCH

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      git \
      tini \
 && rm -rf /var/lib/apt/lists/*

RUN groupadd --system --gid 1000 fabro \
 && useradd --system --uid 1000 --gid fabro \
      --home-dir /var/fabro --shell /usr/sbin/nologin fabro \
 && install -d -o fabro -g fabro -m 0755 /var/fabro /storage \
 && install -d -m 0755 /etc/fabro

COPY --chmod=0755 docker-context/${TARGETARCH}/fabro /usr/local/bin/fabro

COPY docker/settings.toml /etc/fabro/settings.toml
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/fabro-entrypoint

ENV FABRO_HOME=/var/fabro \
    FABRO_CONFIG=/etc/fabro/settings.toml

VOLUME ["/storage"]
EXPOSE 32276

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/fabro-entrypoint"]
CMD ["sh", "-c", "exec fabro server start --foreground --bind 0.0.0.0:${PORT:-32276}"]
