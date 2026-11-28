# Changelog

Every change to EPGArr is written down here, newest first. Each release lists
what was added, changed, fixed and removed, and anything you must do when you
update.

## 0.8.2 — 2026-11-28

### Fixed
- The same XMLTV source can no longer be added twice under different names:
  saving says which guide already uses it. Small differences in the address
  don't matter (http/https, capitals, a trailing slash, `.xml` vs `.xml.gz`).
- Public guides: guides you already added are greyed out in the list with
  "Already added as …".

## 0.8.1 — 2026-11-28

### Changed
- Guides are rebuilt every **12 hours** by default (was 6). A saved default of
  6 hours is moved to 12 when you update; change it in Settings › Schedule. A
  guide with its own interval on the Guides page keeps it.

## 0.8.0 — 2026-11-28: admins and viewers

### Added
- **User roles.** Settings › Users now has two kinds of user:
  - **Admin**: everything, as before.
  - **Viewer**: read-only. Viewers only see the **TV Guide**: they can browse
    and search it, open programme details, switch guides and **copy a guide's
    address** for their media server. They cannot add or change guides, logos
    or TMDB matches, and have no Dashboard, Activity or Settings. This is
    enforced by the server, not just hidden.
  - Pick the role when adding a user; **Make viewer / Make admin** changes it.
    You cannot change your own role, and the last active admin stays an admin.
  - The menu shows "Admin" or "Viewer" under the name.
- TV Guide: a **Copy address** button next to the guide picker copies the
  guide's address (`/guides/<name>.xml`), for admins and viewers.

### When you update
- Existing users stay admins.

## 0.7.2 — 2026-11-28

### Changed
- Add / edit guide: the source has two tabs, **Source address** (type or paste
  an address, Test source) and **Public guides** (choose epgshare01,
  iptv-epg.org or epg.guru, filter, click one). Picking a public guide fills
  in the address and a name and goes back to the Source address tab. This
  replaces the "Or pick a public guide…" link under the address.

## 0.7.1 — 2026-11-28

### Added
- **Pick a public guide** when adding a guide: "Or pick a public guide…" under
  the source address lists the guides of
  [epgshare01](https://epgshare01.online/epgshare01/),
  [iptv-epg.org](https://iptv-epg.org/guides) and [epg.guru](https://epg.guru/)
  (its 7-day IPTV guides). Filter the list (e.g. "United States"), click one,
  and the source address and a name are filled in. The lists are read from the
  sites themselves (kept for 12 hours), so new guides there show up by
  themselves.

## 0.7.0 — 2026-11-28: sorting and logo search

### Added
- TV Guide: a small **sort** button next to the channel count (top left of the
  guide). Sort channels by **channel name** (the default, A–Z with numbers in
  order, so "Channel 2" comes before "Channel 10"), by the **source's order**,
  or by **group**. Your choice is remembered.
- Channel logo dialog: a **Search tv-logos** tab next to Upload and From URL.
  It searches the [tv-logo/tv-logos](https://github.com/tv-logo/tv-logos) list
  (starting with the channel's name), shows the logos with their country, and
  the channel's own country first. Pick one (or double-click it) and Save; it
  is stored like an uploaded logo.

## 0.6.3 — 2026-11-28

### Changed
- **One log for everything.** The Activity page has a single **Log** instead of
  a separate background-lookups log and guide-build log. It shows everything
  EPGArr does, with the date and time on every line: guide builds (scheduled
  and started from the pages), source tests, background lookups, start-up,
  shutdown and warnings. The same lines go to `docker logs epgarr` and to
  `logs/epgarr.log` in the data folder (kept to 5 MB, the previous one as
  `epgarr.log.1`).
- The background lookups now also get `docker stop`, so they save their work
  before the container stops.

### Removed
- The separate `logs/guide-build.log`, `logs/check*.log` and
  `logs/enrich.log` files.

## 0.6.2 — 2026-11-28

### Added
- TV Guide remembers where you were: go to another page and back, and the
  guide opens at the same spot (scroll position, time of day, day and group
  filter). It is kept for 30 minutes, per guide; after that it opens at "Now"
  again.
- TV Guide: a small round **back to the top** button in the guide's
  bottom-right corner, shown once you have scrolled down.

## 0.6.1 — 2026-11-28

### Added
- **Get started** asks for the keys too: paste your TMDB key (and,
  optionally, a TheSportsDB key) right there and press **Save keys**. A
  **Get a free key ↗** button opens the TMDB page where your key is
  (https://www.themoviedb.org/settings/api). The key is tested straight away.
- Settings › Artwork & info links to the same TMDB page when no key is saved.

## 0.6.0 — 2026-11-27: its own user

### Changed
- EPGArr runs as its own **`epgarr` user** with the ids you set in
  `docker-compose.yml`: `PUID` and `PGID` (default 1000:1000). Files in
  `./data` belong to you on the host. It used to run as `www-data` (uid 82).

### Removed
- `epg-loader build --if-due` and the "every 15 minutes" schedule wording: the
  built-in scheduler in `epg-loader serve` decides when to build.
- The start-up step that changed the owner of files "from an older install".
- Leftover mentions of `status.json`, the single `epg.xml` and `channels.toml`
  in the README and comments.

### When you update
- Add `PUID` and `PGID` to your `docker-compose.yml` (see the README; run `id`
  on the host for your ids).
- Your current `./data` was written as uid 82, so give it to the new user once:
  `sudo chown -R 1000:1000 ./data` (use your PUID:PGID), then
  `docker compose up -d`.

## 0.5.1 — 2026-11-27

### Changed
- Settings opens on **Guides**, its first section: the Settings item in the
  menu goes straight there.
- Settings › Guides: "+ Add guide" sits on the same row as the "Guides" title
  inside the section, instead of in a bar above the page that pushed the whole
  page down. Guides now lines up with the other Settings sections.

## 0.5.0 — 2026-11-27: identify programmes on TMDB yourself

### Added
- **Identify on TMDB** in the TV Guide. Click a programme, movie, show or
  event; the details panel shows what it is matched to on TMDB ("set by you"
  or "matched automatically", with a link to TMDB) and an **Identify on TMDB**
  button. Search TMDB (all, movies or series), see posters, years and a short
  description, and pick the right one.
  - The match is saved in the database and used for **every programme with
    that title, in every guide and every build from now on**. It wins over the
    automatic search, never expires, and works even for titles EPGArr would
    normally skip (news, sports).
  - The poster shows in the TV Guide straight away; your media server gets the
    poster, age rating, description and IMDb rating with the next build.
  - **Back to automatic** removes your match.
  - Your matches are included in backups.
  - Needs a TMDB key (Settings › Artwork & info).

## 0.4.3 — 2026-11-27

### Fixed
- TV Guide: the whole page had a scroll bar of its own next to the guide's.
  The guide now fills the rest of the window exactly and scrolls inside
  itself.
- TV Guide: the guide picker was the browser's own drop-down (system font,
  white list, light-blue highlight). It is now a menu in EPGArr's style: the
  current guide is ticked, guides that are turned off are marked "off", and
  "+ Add guide…" opens the add dialog. Works with the keyboard too.
- TV Guide: a guide that is built but has no programmes from today on no longer
  says it "has not been built yet".

## 0.4.2 — 2026-11-27

### Removed
- The last pieces of the old single-guide setup:
  - the global "source URL" and "days to keep" settings (each guide has its
    own, on the Guides page), and the connection test's fallback to them;
  - the `output_dir` setting the Settings page used to save: guide files and
    logos go to the data folder (`/data`) unless `EPG_OUTPUT_DIR` says
    otherwise;
  - the hidden "legacy guide" switch in the add/edit guide dialog;
  - the old config-file format support in the settings code, and some unused
    code.

## 0.4.1 — 2026-11-27

### Changed
- Activity: the time left for the background lookups is shown as an estimate
  ("~12 min est. left"), since it is worked out from the current speed.

### Removed
- Activity: the note "Resumes on its own every 15 minutes until every title is
  done…"; the lookups simply keep going. The note that automatic lookups are
  off is still shown when they are turned off in Settings.

## 0.4.0 — 2026-11-27: picks up where it left off

### Added
- **Work cut short by a shutdown starts again by itself.** If the container is
  stopped, restarted or the server loses power while something is running:
  - a task started from the pages (a guide build, *Build all*, a source test)
    is started again when EPGArr comes back ("Resuming … which was cut short"
    in `docker logs epgarr`);
  - a scheduled guide build that was cut short is built again straight away
    instead of waiting for its next rebuild time;
  - the background lookups carry on from where they were (this already
    worked; they keep everything looked up so far).
  A task you cancel or pause yourself stays stopped.
- `docker stop` now reaches every process in the container, so a running build
  or lookup saves what it has before it stops (tini `-g`), and EPGArr waits a
  few seconds for that before it exits.

### Fixed
- A cancel left over from before a shutdown can no longer stop the first task
  after the restart.

## 0.3.0 — 2026-11-27

### Changed
- The signed-in user and Log out at the bottom of the menu are laid out
  properly: an avatar, the name with "Signed in" under it, and a separate
  Log out button on the right.
- Menu: Dashboard, TV Guide, Activity, Settings. **Guides moved into
  Settings**: it is the first section in Settings' list (still at `/guides`),
  next to Artwork & info, Channel logos, Schedule, Users, Backups and Advanced.
- **No more `.php` in any address.** The pages are now `/`, `/guides`, `/tv`
  (TV Guide), `/activity` and `/settings`; the page scripts use `/api/guide`
  and `/api/logo`. The old addresses (`index.php`, `guides.php`, `guide.php`,
  `activity.php`, `settings.php`, `guide_api.php`, `logo_api.php`) are gone.
  Guide files are unchanged (`/guides/<name>.xml`).
- Install and update from the published image: `docker pull
  ghcr.io/toorone/epgarr:latest` (README and `docker-compose.yml` now use it
  instead of building from the source).

### Added
- **GitHub releases** follow this changelog: each version here becomes a
  release (tag `vX.Y.Z`) with its section as the release notes, made
  automatically when it reaches `main`. The image is also tagged with its
  version (`ghcr.io/toorone/epgarr:0.3.0`) besides `latest`.
- The version (e.g. `EPGArr v0.3.0`) is shown at the bottom of the menu and
  links to this changelog. When GitHub has a newer version it lights up green
  with "v… available". The check runs every 6 hours and can be turned off in
  Settings › Advanced ("Check GitHub for new versions"); it only fetches the
  version number from GitHub.
- The **Activity** menu item lights up (green, glowing, with a dot) while a
  guide build, source test or background lookup is running, and goes back to
  normal when it finishes. It checks every few seconds (`/api/busy`).
- **Get started** on the Dashboard after a fresh install: until the first guide
  exists, the Dashboard shows the steps (add a guide, optional TMDB key, build
  and add the address to your media server) with buttons to each.
- The GitHub build check publishes the image to
  `ghcr.io/toorone/epgarr:latest` (and a tag per commit) on every push to `main`.

### Removed
- Everything kept only for upgrading old installs (EPGArr starts fresh from
  the published image): the redirects from the old `.php` addresses, the
  one-time import of `config.toml` / `channels.toml`, the `admin.json`
  single-password login, and the README section on moving an old install over.
- The database upgrade steps from older versions: a new database is created
  in one step with the current tables. A fresh install no longer starts with an
  empty "Main" guide (which made every scheduled build log an error); you add
  your first guide from the Dashboard's Get started steps.
- The old single `epg.xml` / `status.json` output and the code that cleaned it
  up.
- Old development leftovers: `epg-loader/config.example.toml`, ignore rules
  for the old web folder (`epg.xml`, `status*.json`, `setup-paths.php`), and
  references to the Setup page, `settings.php`, `index.html`, the web folder
  and cron in comments and docs. The program README now lists `serve`,
  `--guide`, `--listen` and `--assets` and the newer source files.

### Fixed
- GitHub build check warned that Node.js 20 is deprecated: the workflow now
  uses the Node.js 24 versions of its actions (checkout v7, setup-buildx v4,
  login v4, build-push v7).
- Logins now survive a restart or update of the container (kept in
  `/data/sessions.json`). Before, every update logged everyone out, and a page
  left open across the update failed: **Test source** said "Could not start
  the test" and reordering guides failed. If a session has ended, those now
  say "You were logged out… reload the page", and other errors say what the
  server answered.
- **Test source** in the add/edit guide dialog said "Could not start the test",
  and dragging guides into a new order did nothing: the Guides page did not
  accept the way its own script sends forms. The Guides and Activity pages now
  accept both kinds of form.
- TheSportsDB lookups all failed with HTTP 403: TheSportsDB switched off the old
  free key `3`. The free key is now `123`, and a saved `3` is treated as `123`.
- When TheSportsDB refuses the key, the lookups stop with one clear message
  instead of failing for every match.

## 0.2.0 — 2026-11-27: one program, no PHP

### Changed
- **The web pages are now part of the program.** `epg-loader serve` serves every
  page (Dashboard, Guides, TV Guide, Activity, Settings) and runs the rebuild
  schedule in the same process. PHP and nginx are gone.
- The Docker image holds only the program and the page assets (`/app/assets`).
  It still runs as `www-data` (uid 82) and listens on port 80, so
  `docker-compose.yml` is unchanged.
- Guide files and custom logos are served straight from `/data/guides/` and
  `/data/logos/` (`EPG_OUTPUT_DIR=/data`); the guide addresses stay the same.
- Builds, source tests and background lookups started from the pages run as
  child processes of the program, with their logs in `/data/logs/`.
- Page scripts moved out of the pages into static files
  (`assets/page-*.js`, `assets/tvguide.js`).
- Uploaded channel logos are always stored as PNG, at most 512 px (decoded by
  the program; no GD needed).
- Settings › Advanced › System checks now shows "All good" or the number of
  problems; the scheduler check reads the program's own state.
- The GitHub build check compiles with `--locked` and no longer checks PHP.

### Added
- A backup is made every day by the program itself (and at start-up when the
  newest is more than a day old), not only when Settings is opened.
- Two backups made in the same second no longer overwrite each other.
- Settings that fail validation keep the values you typed.
- Pages send `X-Content-Type-Options`, `X-Frame-Options` and `Referrer-Policy`
  headers; the session cookie is `HttpOnly` and `SameSite=Lax`.

### Removed
- All PHP pages (`*.php`, `epg_lib.php`), `docker/nginx.conf` and
  `docker/php.ini`.

### When you update
- Rebuild the image: `git pull` then `docker compose up -d --build`.
- Users, passwords, guides, settings, backups and custom logos carry over.
  Everyone has to log in once more (sessions are new).

## 0.1.2 — 2026-11-27: Docker only, GitHub

### Added
- `Dockerfile`, `docker-compose.yml` and `docker/entrypoint.sh`: EPGArr runs as
  a single container with all data in the `/data` volume.
- GitHub build check (`.github/workflows/build.yml`): builds and tests the
  program and builds the Docker image on every push.
- `.gitattributes` keeps line endings exactly as written.

### Removed
- The Setup page, the cron schedule, the Rust installer and host path
  detection (the container sets the paths).

## 0.1.1 — 2026-11-26

### Added
- TV Guide: programme search (Channels | Programmes), with results grouped as
  on now, later today and upcoming.
- Settings › Backups: daily and manual backups of settings, guides, channel
  overrides, users and custom logos; download, restore, upload a file to
  restore, delete. The newest 30 are kept; a backup is made before every
  restore.

### Changed
- Activity: "Recently looked up" shows one full row of posters.

### Fixed
- Dashboard was slow: poster and logo coverage is now cached until the next
  build instead of scanning every guide on each visit.
- Memory use on very large sources (several GB): guide output is streamed to
  disk, and background lookups use the saved title lists instead of reading the
  sources again.

## 0.1.0 — 2026-11-26: first snapshot

The state of EPGArr when version history started.

- Several guides, each with its own XMLTV source, days to keep, rebuild
  interval and logo country; drag-and-drop order; `.xml` and `.xml.gz` per
  guide, named after the guide.
- Channel logos from the source, channel overrides, tv-logos and iptv-org;
  set a channel's logo from the TV Guide by upload or URL.
- Posters, ratings and descriptions from TMDB, IMDb star ratings, TVmaze for
  series and TheSportsDB for sports; background lookups with progress on the
  Activity page.
- One main category per programme (Movie, Series, News, Sports, Kids, Music,
  Documentary).
- TV Guide viewer: 2-hour view, guide selector, group filters in a one-line
  scrollable toolbar, details for programmes already started.
- Dashboard with coverage and Recent Builds (full stats per run).
- Users with username and password; only the login page is visible until you
  log in.
- Renamed to EPGArr, with its own icon.
