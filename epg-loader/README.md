# EPGArr (epg-loader)

EPGArr is the web app; `epg-loader` is the program behind it (the folder, program and schedule keep that name).

epg-loader takes an XMLTV guide and fills in what media servers show best (Emby, Plex, Jellyfin or any XMLTV client):

- **Channel logos.** A channel that already has an `<icon>` keeps it. Otherwise the logo comes from your channel overrides, then from [tv-logos](https://github.com/tv-logo/tv-logos), then from the public [iptv-org](https://github.com/iptv-org/api) lists.
- **Posters, age ratings and short descriptions** from TMDB. They are added only where the programme has none.
- **One main category per programme** (Movie, Series, News, Sports, Kids, Music or Documentary), so your media server can sort the guide.

Everything else in the source is copied through unchanged. The result is `epg.xml` and `epg.xml.gz` in your web folder, next to `index.html`, plus a `status.json` that the guide page reads.

## Setup: in the browser

Open **`setup.php`** in your web folder, for example `http://<your server>/<folder>/setup.php`.

1. **Choose an admin password.** The setup and settings pages share it. The first visitor sets it, so do this straight away.
2. **Work down the Checks list.** Every check shows green, amber or red with a plain-English explanation. Most fixes are a button:
   - Create data folder
   - Install Rust: no admin rights needed; it goes into the PHP user's home or the data folder
   - Build the program, Run tests
   - Initialise or upgrade database
   - Test TMDB key and source URL
   - Build the guide
   - Install schedule / Remove schedule

   Each button runs in the background, as the user PHP runs as. Its log streams live at the top of the page, and only one task runs at a time.
3. **Open Settings** and enter the XMLTV URL and the TMDB key. The key can be a v3 "API Key" or a v4 "API Read Access Token", from themoviedb.org → Settings → API. Then run **Test TMDB key and source URL** and **Build the guide**.
4. **Install the schedule.** This adds one line to the PHP user's crontab, tagged so it can be removed cleanly. The line runs every 15 minutes, and the program only builds when `refresh_hours` (from the settings) have passed.

When everything is green, the page shows **Setup complete**, and the actions stay hidden until you press **Re-run setup**.

### What still needs an administrator

Some fixes need root. The page never tries `sudo`. Instead it shows the exact command in an orange **"Needs admin rights"** box, built from the detected paths, users and PHP version. These are:

- the C compiler (`build-essential`), which Rust needs to link the program
- the PHP SQLite driver, or `exec` disabled in PHP
- the web server rule that hides the source folder. The setup page tests this itself, by fetching the source folder's `Cargo.toml` from your own server, and shows the nginx `location ... { deny all; }` line for your exact path.
- files that PHP cannot read. Files copied in over SMB from Windows often arrive readable only by their owner; the page lists them and shows the `chmod` to run.

## How it fits together

| Where | What |
|---|---|
| your web folder | `index.html`, `settings.php`, `setup.php`, `epg_lib.php`, `setup-paths.php`, and the output files `epg.xml`, `epg.xml.gz`, `status.json`. Nothing secret. |
| `<web folder>/epg-loader/` | This source folder. It should not be served (see above). No secrets. |
| the data folder (outside the web root) | `epg.db` (settings including the TMDB key, channel overrides, TMDB cache, run history), `admin.json` (password hash), `rules.toml`, `build.lock`, `logs/`, `cache/`, and the build output `target/` |

**Nothing is hard-coded.** At runtime:
- the web folder is the folder the pages are in;
- the base URL comes from the request;
- the PHP user and home come from PHP's posix functions;
- the data folder is the first usable one of: a folder you saved; an existing install; `~/.local/share/epg-loader` in the PHP user's home; `epg-loader-data` beside your top-level web folder;
- the program is the newest working build;
- cargo is found in `~/.cargo/bin`, the data folder or `PATH`.

The setup page shows all of this under **Detected paths**, and you can override any of it. Choices are saved in `setup-paths.php`. If a saved path stops working, for example after moving to another server, detection takes over again and the page says so.

The program has no built-in paths either. It is always given `--data-dir` / `EPG_LOADER_DIR` and `--output-dir` / `EPG_OUTPUT_DIR`, by the pages and by the crontab line.

`epg.db` is SQLite in WAL mode with a busy timeout, so the pages and a running build can use it at the same time. Only the program creates or migrates tables; the pages only read and write rows. The TMDB key is never written to `status.json`, the logs or anything in the web folder, and the pages only show its last 4 characters.

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
  watch     keep running; rebuild every refresh_hours (alternative to cron)
  migrate   create or upgrade epg.db and write rules.toml
  check     test internet, the TMDB key and the source URL; prints JSON
  info      print version and expected schema version as JSON

  --data-dir / EPG_LOADER_DIR     the data folder (required)
  --output-dir / EPG_OUTPUT_DIR   where epg.xml goes (else the output_dir setting)
  --if-due                        build only if refresh_hours have passed (for cron)
  --dry-run                       write no output and record no run
  --no-tmdb                       make no TMDB requests (cached results still used)
```

The `TMDB_API_KEY` environment variable overrides the saved key.

## Files in this folder

| File | Purpose |
|---|---|
| `src/main.rs` | CLI, build pipeline, lock, status and run history |
| `src/db.rs` | epg.db schema, migrations, settings, overrides, runs, one-time import of old toml files |
| `src/checks.rs` | `check` command used by the setup page |
| `src/xmltv.rs` | Streaming parse and pass-through rewrite (quick-xml) |
| `src/logos.rs` | iptv-org logo matching by channel id or display name |
| `src/tmdb.rs` | TMDB search, ratings, cache, rate limiting |
| `src/groups.rs` | Group selection and `rules.toml` |
| `src/source.rs` | Download, gzip detection by magic bytes |
| `src/config.rs`, `timeutil.rs`, `output.rs` | Settings, XMLTV dates, atomic writes |
| `rules.toml` | Default category rules, built in and copied to the data folder |
| `config.example.toml` | Optional one-time import |
| `epg-loader.service` | Optional systemd unit (watch mode), instead of cron |
| `tests/sample.xml` | Sample XMLTV for the unit tests |

## Appendix: doing it from a console

You only need this if you can't use the setup page. Replace the placeholders with the values the setup page shows under "Detected paths". Run everything as the user PHP runs as, so that the pages can use the same files.

```bash
WEB="<your web folder>"                  # the folder with index.html
DATA="<a folder outside the web root>"   # for example ~/.local/share/epg-loader

# Admin rights, once: compiler for Rust
sudo apt install build-essential pkg-config curl

# Rust (no admin rights)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"

# Build and test
cd "$WEB/epg-loader"
CARGO_TARGET_DIR="$DATA/target" cargo build --release
CARGO_TARGET_DIR="$DATA/target" cargo test --release
BIN="$DATA/target/release/epg-loader"

# Database, connection test, first build
mkdir -p "$DATA"
"$BIN" --data-dir "$DATA" migrate
"$BIN" --data-dir "$DATA" check
"$BIN" --data-dir "$DATA" --output-dir "$WEB" build

# Schedule (what "Install schedule" adds)
( crontab -l 2>/dev/null; echo "*/15 * * * * $BIN --data-dir $DATA --output-dir $WEB build --if-due >> $DATA/logs/cron.log 2>&1 # epg-loader" ) | crontab -
```

To hide the source folder on nginx, add this to the site's `server { }` block, then run `sudo nginx -t && sudo systemctl reload nginx`. The setup page shows this line with your real path:

```nginx
location ^~ /<url path of your web folder>/epg-loader/ { deny all; return 404; }
```

## Troubleshooting

- **A button does nothing:** the log at the top of the setup page says why. Logs are kept in `logs/` in the data folder.
- **"attempt to write a readonly database":** the data folder is not writable by the PHP user. The Data folder check shows the fix.
- **A file can't be read after copying it over SMB:** the "Source folder readable by PHP" check lists those files and shows the `chmod` to run.
- **Garbled characters:** the source is assumed to be UTF-8, which XMLTV almost always is.
