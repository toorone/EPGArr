# EPGArr: one container with the epg-loader program, PHP and nginx.
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
FROM php:8.3-fpm-alpine
RUN apk add --no-cache nginx tini su-exec tzdata libpng libjpeg-turbo libwebp freetype \
 && apk add --no-cache --virtual .build $PHPIZE_DEPS libpng-dev libjpeg-turbo-dev libwebp-dev freetype-dev \
 && docker-php-ext-configure gd --with-jpeg --with-webp --with-freetype \
 && docker-php-ext-install -j"$(nproc)" gd \
 && apk del .build

COPY --from=build /src/target/release/epg-loader /usr/local/bin/epg-loader
COPY docker/nginx.conf /etc/nginx/http.d/default.conf
COPY docker/php.ini /usr/local/etc/php/conf.d/zz-epgarr.ini
COPY docker/entrypoint.sh /entrypoint.sh
COPY *.php /app/www/
COPY assets /app/www/assets

# Guide files and custom logos are written into the volume but served by nginx.
RUN chmod +x /entrypoint.sh \
 && ln -s /data/guides /app/www/guides \
 && ln -s /data/logos /app/www/logos \
 && rm -f /etc/nginx/http.d/default.conf.apk-new

ENV EPG_LOADER_DIR=/data \
    EPG_OUTPUT_DIR=/app/www \
    EPGARR_DATA=/data \
    TZ=UTC
VOLUME ["/data"]
EXPOSE 80
HEALTHCHECK --interval=60s --timeout=10s --start-period=60s CMD wget -q -O /dev/null http://127.0.0.1/index.php || exit 1
ENTRYPOINT ["/sbin/tini", "--", "/entrypoint.sh"]
