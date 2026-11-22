FROM node:24.11.1-bookworm-slim AS web-builder
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.89-bookworm AS rust-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/gateway/Cargo.toml crates/gateway/Cargo.toml
COPY crates/gateway/src crates/gateway/src
COPY crates/gateway/migrations crates/gateway/migrations
RUN cargo build --release --locked -p context7-gateway

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl sqlite3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /srv/gateway gateway
WORKDIR /srv/gateway
COPY --from=rust-builder /src/target/release/context7-gateway /usr/local/bin/context7-gateway
COPY --from=web-builder /src/web/dist /srv/gateway/web/dist
COPY scripts/backup_sqlite.sh scripts/restore_sqlite.sh /usr/local/bin/
RUN mkdir -p /srv/gateway/data && chown -R gateway:gateway /srv/gateway
USER 10001:10001
ENV APP_ENV=production \
    GATEWAY_BIND=0.0.0.0:8787 \
    DATABASE_PATH=/srv/gateway/data/context7_gateway.sqlite3 \
    WEB_DIST_DIR=/srv/gateway/web/dist
EXPOSE 8787
VOLUME ["/srv/gateway/data"]
ENTRYPOINT ["/usr/local/bin/context7-gateway"]
