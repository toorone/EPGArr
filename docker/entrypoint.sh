#!/bin/sh
# EPGArr container start: prepare /data, start PHP and nginx, then run the
# scheduler ("epg-loader watch") in the foreground. tini reaps child processes.
set -e
DATA="${EPGARR_DATA:-/data}"
mkdir -p "$DATA/guides" "$DATA/logos/custom" "$DATA/logs" "$DATA/backups"
chown www-data:www-data "$DATA" "$DATA/guides" "$DATA/logos" "$DATA/logos/custom" "$DATA/logs" "$DATA/backups"
# Files from an older install (or a restored copy) must belong to the web user too.
find "$DATA" -maxdepth 1 -type f ! -user www-data -exec chown www-data:www-data {} + 2>/dev/null || true

# PHP uses the container's time zone (TZ=Europe/London etc.).
if [ -n "$TZ" ] && [ -f "/usr/share/zoneinfo/$TZ" ]; then
  echo "date.timezone=$TZ" > /usr/local/etc/php/conf.d/zz-timezone.ini
fi

# Create or upgrade the database before anything uses it.
su-exec www-data:www-data epg-loader migrate

php-fpm -D
nginx
echo "EPGArr is running on port 80 (data in $DATA)"
exec su-exec www-data:www-data epg-loader watch
