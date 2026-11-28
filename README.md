# EPGArr

EPGArr builds XMLTV guides for your media server (Emby, Plex, Jellyfin or any XMLTV client): it takes your XMLTV sources and fills in channel logos (tv-logos, iptv-org), posters, ratings and descriptions (TMDB, TVmaze, TheSportsDB), IMDb star ratings and one main category per programme. Several guides, a TV Guide viewer, background lookups, users and backups are all in the web pages.

EPGArr runs as a single Docker container with one program in it: `epg-loader` serves the web pages and rebuilds the guides on their schedule (no PHP, no web server to set up).

## Install

The image is published on GitHub as `ghcr.io/toorone/epgarr:latest` (every push to `main` updates it).

```bash
docker pull ghcr.io/toorone/epgarr:latest
```

Make a folder for EPGArr with this `docker-compose.yml` in it, and start it:

```yaml
services:
  epgarr:
    image: ghcr.io/toorone/epgarr:latest
    container_name: epgarr
    restart: unless-stopped
    ports:
      - "8080:80"
    environment:
      - TZ=America/New_York   # your time zone
      - PUID=1000             # user that owns ./data (run "id" on the host)
      - PGID=1000             # group that owns ./data
    volumes:
      - ./data:/data          # database, users, backups, logs, guide files, logos
```

```bash
docker compose up -d
```

Without Compose, the same in one command:

```bash
docker run -d --name epgarr --restart unless-stopped -p 8080:80 -e TZ=America/New_York -e PUID=1000 -e PGID=1000 -v "$PWD/data:/data" ghcr.io/toorone/epgarr:latest
```

Open `http://<server>:8080`, create your account, then add a guide on the **Guides** page and press **Build now**. Each guide's address (e.g. `http://<server>:8080/guides/us-locals.xml`, or `.xml.gz`) goes into your media server as an XMLTV guide source.

Everything that matters is in `./data`. The program runs as its own `epgarr` user with the ids you give in `PUID` and `PGID` (default 1000:1000), so the files in `./data` belong to you on the host. Run `id` on the host to see your ids.

## Users

Settings › Users: **admins** can change everything; **viewers** only get the TV Guide (read-only, with a button to copy a guide's address for their media server).

## Update

```bash
docker compose pull
docker compose up -d
```

(Without Compose: `docker pull ghcr.io/toorone/epgarr:latest`, then remove the container and run the `docker run` line again.)

The database is upgraded automatically when the container starts. Your data in `./data` stays. Read [CHANGELOG.md](CHANGELOG.md) for what changed and anything to do when updating.

To build the image yourself instead: clone the repository and run `docker build -t ghcr.io/toorone/epgarr:latest .` in it.

## Behind a reverse proxy

Proxy to port 8080. Pass `Host` and `X-Forwarded-Proto` so the guide addresses shown in the pages use your public URL.

## Backups

Settings › Backups makes a backup every day and before every restore (settings, guides, overrides, users, custom logos; the newest 30 are kept in `./data/backups`). Download one to keep a copy elsewhere. Backup files contain your TMDB key and password hashes: keep them private.

## Logs and problems

- `docker logs epgarr`: the program (page requests are not logged; scheduled guide builds and background lookups are).
- Settings › Advanced › System checks: program, scheduler, data volume and connection tests.
- `./data/logs/`: logs of builds and tests started from the pages.

The program itself is described in [epg-loader/README.md](epg-loader/README.md). Every change is listed in [CHANGELOG.md](CHANGELOG.md).
