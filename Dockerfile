# EPGArr: one container, one program. epg-loader serves the web pages and runs
# the schedule itself (no PHP, no nginx).
#   docker compose up -d --build
# Data (database, users, backups, guides, logos) lives in the /data volume.

# ---- build the program -------------------------------------------------------
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
WORKDIR /src
COPY epg-loader/Cargo.toml epg-loader/Cargo.lock ./
COPY epg-loader/rules.toml ./
COPY epg-loader/src ./src
COPY epg-loader/tests ./tests
RUN cargo build --release --locked

# ---- the app -----------------------------------------------------------------
FROM alpine:3
RUN apk add --no-cache tini su-exec tzdata ca-certificates libcap \
 && (addgroup -S -g 82 www-data 2>/dev/null || true) \
 && adduser -S -D -H -u 82 -G www-data www-data

COPY --from=build /src/target/release/epg-loader /usr/local/bin/epg-loader
COPY docker/entrypoint.sh /entrypoint.sh
COPY assets /app/assets

# The pages listen on port 80 without running as root.
RUN chmod +x /entrypoint.sh \
 && setcap cap_net_bind_service=+ep /usr/local/bin/epg-loader

ENV EPG_LOADER_DIR=/data \
    EPG_OUTPUT_DIR=/data \
    EPGARR_DATA=/data \
    EPGARR_LISTEN=0.0.0.0:80 \
    EPGARR_ASSETS=/app/assets \
    TZ=UTC
VOLUME ["/data"]
EXPOSE 80
HEALTHCHECK --interval=60s --timeout=10s --start-period=30s CMD wget -q -O /dev/null http://127.0.0.1/index.php || exit 1
ENTRYPOINT ["/sbin/tini", "--", "/entrypoint.sh"]
