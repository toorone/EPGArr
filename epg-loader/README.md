# EPGArr (epg-loader)

EPGArr is the web app; `epg-loader` is the program behind it (the folder, program and schedule keep that name).

epg-loader takes an XMLTV guide and fills in what media servers show best (Emby, Plex, Jellyfin or any XMLTV client):

- **Channel logos.** A channel that already has an `<icon>` keeps it. Otherwise the logo comes from your channel overrides, then from [tv-logos](https://github.com/tv-logo/tv-logos), then from the public [iptv-org](https://github.com/iptv-org/api) lists.
- **Posters, age ratings and short descriptions** from TMDB. They are added only where the programme has none.
- **One main category per programme** (Movie, Series, News, Sports, Kids, Music or Documentary), so your media server can sort the guide.

Everything else in the source is copied through unchanged. Each guide is written as `guides/<name>.xml` and `.xml.gz` in `/data`, and served by the program at `/guides/<name>.xml`.

## Installing

EPGArr runs only as a Docker container; see the README in the project root for the install, update and move-over steps. This folder is the program it runs inside the container.

## How it fits together

| In the container | What |
|---|---|
| `/app/assets` | The pages' style sheet, scripts and icon. The pages themselves are built into the program. |
| `/usr/local/bin/epg-loader` | This program, built into the image. |
| `/data` (the volume) | `epg.db` (settings including the TMDB key, guides, overrides, TMDB cache, run history), `users.json`, `backups/`, `logs/`, `cache/`, `rules.toml`, the guide files in `guides/` and custom logos in `logos/custom/`. |

The container runs `epg-loader serve`: the web pages on port 80 (`EPGARR_LISTEN`), plus the schedule, which rebuilds each guide when its interval has passed and resumes the background lookups. Builds and tests started from the pages run as child processes of the program. The program is given `EPG_LOADER_DIR=/data` and `EPG_OUTPUT_DIR=/data`; guide files are served from `/data/guides/` and custom logos from `/data/logos/`.

`epg.db` is SQLite in WAL mode with a busy timeout, so the pages and a running build can use it at the same time. The TMDB key is never written to `status.json`, the logs or the guide files, and the pages only show its last 4 characters.

## Several guides

Each guide has its own XMLTV source, days to keep, rebuild interval and logo country (Guides page). Every guide writes `guides/<slug>.xml` and `.xml.gz`, named after the guide (renaming it changes the address; the old files are removed after its next build), and `guides/status-<id>.json`. The old single `epg.xml` is removed after the first guide's first build. TMDB results and logo lists are shared, so a title is looked up once for all guides.

- `epg-loader build` builds every enabled guide in turn; one failing source does not stop the others.
- `epg-loader build --guide <slug>` builds one guide; `check --guide <slug>` tests its source.
- `epg-loader build --if-due` (the schedule) builds only the guides whose interval has passed.

## Adding the guide to your media server

In Emby, Plex, Jellyfin or any XMLTV client:

1. Add an **XMLTV** guide source (Emby: Live TV → TV Guide Data Providers; Jellyfin: Live TV → TV Guide Data Providers; Plex: Live TV & DVR → XMLTV guide).
2. URL: the guide's address from the Guides page (`<base URL>/guides/<name>.xml`); the `.xml.gz` works too.
3. Category mapping. Each programme gets exactly one of these as a `<category>`:
   - Movie categories: `Movie`
   - Kids categories: `Kids`
   - News categories: `News`
   - Sports categories: `Sports`

   `Series`, `Music` and `Documentary` are added as categories too, and show up as genres.
4. Turn on channel logos (images) from the guide. If your media server doesn't match your tuner's channels to the guide channels by itself, map them by hand.

## How groups are chosen

The main group is the first match from:

1. the channel's group in the channel overrides;
2. the source's own categories: an exact group name, then the `[[category_rule]]` entries in `rules.toml`;
3. TMDB genres (kids, documentary, news, music), then the media type (movie → Movie, tv → Series);
4. a `[[keyword_rule]]` matching the title;
5. Series, for programmes with an `<episode-num>`.

The group is added unless the programme already has it. Edit `rules.toml` in the data folder to change the keywords. It is read at the start of every build.

News and sports programmes are not looked up on TMDB, because they rarely match anything real.

## TMDB details

- Matching:
  - Each unique title is searched with `search/multi`.
  - A result is accepted only when its title or original title equals the programme title, ignoring case and punctuation.
  - Episodes only match TV results.
  - A year in `<date>` favours the film from that year.
- Ratings:
  - Movies use the MPAA system, from `release_dates`, preferring the theatrical certificate.
  - TV uses VCHIP, from `content_ratings`.
  - Both use the rating country you set.
- Caching: results go into the `tmdb_cache` table. Titles that were found are kept for 90 days, "not on TMDB" results for 14 days.
- Rate limits: at most 40 requests per 10 seconds, 4 at a time, with retries on 429 and 5xx answers.

## Commands

```
epg-loader --data-dir DIR [--output-dir DIR] [--dry-run] [--no-tmdb] [--if-due] <command>

  build     build once (the default)
  serve     the web pages and the schedule (what the container runs)
  watch     the schedule only, without the pages
  migrate   create or upgrade epg.db and write rules.toml
  check     test internet, the TMDB key and the source URL; prints JSON
  info      print version and expected schema version as JSON

  --data-dir / EPG_LOADER_DIR     the data folder (required)
  --output-dir / EPG_OUTPUT_DIR   where guides/ and logos/ go (default: the data folder)
  --guide SLUG                    build or check one guide only
  --listen / EPGARR_LISTEN        serve: address to listen on (default 0.0.0.0:80)
  --assets / EPGARR_ASSETS        serve: page assets folder (default /app/assets)
  --if-due                        build only the guides whose interval has passed
  --dry-run                       write no output and record no run
  --no-tmdb                       make no TMDB requests (cached results still used)
```

The `TMDB_API_KEY` environment variable overrides the saved key.

## Files in this folder

| File | Purpose |
|---|---|
| `src/main.rs` | CLI, build pipeline, schedule, lock, status and run history |
| `src/web/` | The web pages: login and users, Dashboard, Guides, TV Guide, Activity, Settings, backups, channel logo uploads |
| `src/db.rs` | epg.db schema, migrations, settings, overrides, runs, one-time import of old toml files |
| `src/checks.rs` | `check` command (connection and source tests) |
| `src/xmltv.rs` | Streaming parse and pass-through rewrite (quick-xml) |
| `src/logos.rs` | iptv-org logo matching by channel id or display name |
| `src/tmdb.rs` | TMDB search, ratings, cache, rate limiting |
| `src/imdb.rs`, `tvmaze.rs`, `sportsdb.rs`, `tvlogos.rs` | IMDb ratings, TVmaze, TheSportsDB, tv-logos |
| `src/enrich.rs` | Background lookups |
| `src/groups.rs` | Group selection and `rules.toml` |
| `src/source.rs` | Download, gzip detection by magic bytes |
| `src/config.rs`, `timeutil.rs`, `output.rs` | Settings, XMLTV dates, atomic writes |
| `rules.toml` | Default category rules, built in and copied to the data folder |
| `tests/sample.xml` | Sample XMLTV for the unit tests |

## Troubleshooting

- **Something does not work:** Settings › Advanced › System checks, the Activity page and `docker logs epgarr` say why. Logs are kept in `/data/logs`.
- **"attempt to write a readonly database":** the `/data` volume is not writable by the container (user www-data, uid 82).
- **Garbled characters:** the source is assumed to be UTF-8, which XMLTV almost always is.
