#!/bin/sh
# EPGArr container start: prepare /data, upgrade the database, then run
# "epg-loader serve" (web pages and schedule) as www-data. tini reaps child processes.
set -e
DATA="${EPGARR_DATA:-/data}"
mkdir -p "$DATA/guides" "$DATA/logos/custom" "$DATA/logs" "$DATA/backups"
chown www-data:www-data "$DATA" "$DATA/guides" "$DATA/logos" "$DATA/logos/custom" "$DATA/logs" "$DATA/backups"
# Files from an older install (or a restored copy) must belong to www-data too.
find "$DATA" -maxdepth 1 -type f ! -user www-data -exec chown www-data:www-data {} + 2>/dev/null || true
find "$DATA/guides" "$DATA/logos/custom" -maxdepth 1 -type f ! -user www-data -exec chown www-data:www-data {} + 2>/dev/null || true

# Create or upgrade the database before anything uses it.
su-exec www-data:www-data epg-loader migrate

echo "EPGArr is running on ${EPGARR_LISTEN:-0.0.0.0:80} (data in $DATA)"
exec su-exec www-data:www-data epg-loader serve
