# EPGArr

EPGArr builds XMLTV guides for your media server (Emby, Plex, Jellyfin or any XMLTV client): it takes your XMLTV sources and fills in channel logos (tv-logos, iptv-org), posters, ratings and descriptions (TMDB, TVmaze, TheSportsDB), IMDb star ratings and one main category per programme. Several guides, a TV Guide viewer, background lookups, users and backups are all in the web pages.

EPGArr runs as a single Docker container.

## Install

```bash
git clone <this repository> epgarr
cd epgarr
docker compose up -d --build
```

Open `http://<server>:8080`, create your account, then add a guide on the **Guides** page and press **Build now**. Each guide's address (e.g. `http://<server>:8080/guides/us-locals.xml`, or `.xml.gz`) goes into your media server as an XMLTV guide source.

`docker-compose.yml`:

```yaml
services:
  epgarr:
    build: .
    image: epgarr:latest
    container_name: epgarr
    restart: unless-stopped
    ports:
      - "8080:80"
    environment:
      - TZ=America/New_York   # your time zone
    volumes:
      - ./data:/data          # database, users, backups, logs, guide files, logos
```

Everything that matters is in `./data`. The container runs as user `www-data` (uid 82) and fixes the ownership of `./data` when it starts.

## Update

```bash
git pull
docker compose up -d --build
```

The database is upgraded automatically when the container starts. Your data in `./data` stays.

## Moving an existing (non-Docker) install over

1. Stop the old schedule: `crontab -e` and remove the line ending in `# epg-loader`.
2. Copy the old data folder (usually `/var/lib/epg-loader`) into `./data`: `epg.db`, `users.json` (or `admin.json`), `rules.toml` and `backups/` are what matter.
3. Copy custom channel logos from the old web folder's `logos/custom/` to `./data/logos/custom/`.
4. `docker compose up -d --build`, log in with your existing account, and press **Build all guides now** on the Activity page.
5. Point your media server at the new guide addresses shown on the Guides page.

## Behind a reverse proxy

Proxy to port 8080. Pass `Host` and `X-Forwarded-Proto` so the guide addresses shown in the pages use your public URL.

## Backups

Settings › Backups makes a backup every day and before every restore (settings, guides, overrides, users, custom logos; the newest 30 are kept in `./data/backups`). Download one to keep a copy elsewhere. Backup files contain your TMDB key and password hashes: keep them private.

## Logs and problems

- `docker logs epgarr`: the scheduler (guide builds and background lookups).
- Settings › Advanced › System checks: program, scheduler, data volume and connection tests.
- `./data/logs/`: logs of builds and tests started from the pages.

The program itself is described in [epg-loader/README.md](epg-loader/README.md).
