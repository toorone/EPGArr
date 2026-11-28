#!/bin/sh
# EPGArr container start: prepare /data, upgrade the database, then run
# "epg-loader serve" (web pages and the built-in scheduler) as the epgarr user.
# PUID and PGID (default 1000) set which user and group that is, so the files
# in /data belong to you on the host. tini reaps child processes.
set -e
DATA="${EPGARR_DATA:-/data}"
PUID="${PUID:-1000}"
PGID="${PGID:-1000}"
case "$PUID$PGID" in
  *[!0-9]*|'') echo "PUID and PGID must be numbers (got PUID=$PUID PGID=$PGID)" >&2; exit 1 ;;
esac

mkdir -p "$DATA/guides" "$DATA/logos/custom" "$DATA/logs" "$DATA/backups"
chown "$PUID:$PGID" "$DATA" "$DATA/guides" "$DATA/logos" "$DATA/logos/custom" "$DATA/logs" "$DATA/backups"

# Create or upgrade the database before anything uses it.
su-exec "$PUID:$PGID" epg-loader migrate

echo "EPGArr is running on ${EPGARR_LISTEN:-0.0.0.0:80} as $PUID:$PGID (data in $DATA)"
exec su-exec "$PUID:$PGID" epg-loader serve
