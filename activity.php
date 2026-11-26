<?php
/*
 * EPGArr activity: background TMDB lookups (epg-loader enrich) and
 * guide builds, live. The page polls activity.php?json=1 every few seconds.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
$p = epg_paths();

if (isset($_GET['json'])) {
    header('Content-Type: application/json; charset=utf-8');
    header('Cache-Control: no-store');
    if (!epg_authed($p)) {
        http_response_code(401);
        echo json_encode(['error' => 'Log in first.']);
        exit;
    }
    echo json_encode(activity_data($p), JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}

epg_require_login($p);

if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    $action = (string)($_POST['action'] ?? '');
    if (!epg_csrf_ok()) {
        epg_flash('error', 'The form expired. Please try again.');
    } elseif ($action === 'enrich_start') {
        $err = epg_start_enrich($p);
        $err === '' ? epg_flash('ok', 'Background TMDB lookups started.') : epg_flash('error', $err);
    } elseif ($action === 'enrich_stop') {
        $err = epg_stop_enrich($p);
        $err === '' ? epg_flash('ok', 'Paused. The results so far are kept; press Start to continue.') : epg_flash('error', $err);
    } elseif ($action === 'build') {
        $err = epg_start_build($p);
        $err === '' ? epg_flash('ok', 'Guide build started.') : epg_flash('error', $err);
    } elseif ($action === 'cancel') {
        $err = epg_cancel($p);
        $err === '' ? epg_flash('ok', 'Cancelled.') : epg_flash('error', $err);
    }
    epg_redirect_self();
}

/** Does the installed epg-loader know the "enrich" command? */
function activity_supported(array $p): bool
{
    return epg_bin_supports($p, 'enrich');
}

/** The latest guide build output: a build started from these pages (logs/guide-build.log)
 * or by the schedule (logs/cron.log), whichever ran last. */
function activity_build_log(array $p): string
{
    if ($p['data_dir'] === '') {
        return '';
    }
    $best = '';
    $bestTime = -1;
    foreach (['guide-build.log' => 'Started from these pages', 'cron.log' => 'Scheduled builds'] as $f => $label) {
        $path = epg_logs_dir($p) . '/' . $f;
        $t = is_file($path) ? (int)filemtime($path) : -1;
        if ($t > $bestTime) {
            $bestTime = $t;
            $best = $label . ' (' . date('D j M, H:i', $t) . "):\n" . epg_tail($path, 120);
        }
    }
    return $bestTime < 0 ? '' : $best;
}

function activity_data(array $p): array
{
    $e = epg_enrich_status($p);
    $cached = 0;
    $matched = 0;
    $tvmaze = 0;
    $sports = 0;
    try {
        $row = epg_db($p)->query('SELECT COUNT(*) AS n, COALESCE(SUM(found), 0) AS f FROM tmdb_cache')->fetch();
        $cached = (int)($row['n'] ?? 0);
        $matched = (int)($row['f'] ?? 0);
        if (epg_has_table(epg_db($p), 'tvmaze_cache')) {
            $tvmaze = (int)epg_db($p)->query('SELECT COUNT(*) FROM tvmaze_cache WHERE found = 1')->fetchColumn();
        }
        if (epg_has_table(epg_db($p), 'sportsdb_cache')) {
            $sports = (int)epg_db($p)->query('SELECT COUNT(*) FROM sportsdb_cache WHERE found = 1')->fetchColumn();
        }
    } catch (Throwable $t) {
        // No database yet: the counts stay 0.
    }
    $titles = (int)($e['titles'] ?? 0);
    $done = min($titles, (int)($e['cached'] ?? 0) + (int)($e['done'] ?? 0));
    if ($e['active']) {
        $state = 'running';
    } elseif (!empty($e['stopped_by_user'])) {
        $state = 'paused';
    } elseif (($e['state'] ?? '') === 'done' && (int)($e['pending'] ?? 0) === 0) {
        $state = 'finished';
    } elseif (($e['state'] ?? '') === 'failed') {
        $state = 'failed';
    } else {
        $state = 'idle';
    }
    $job = epg_job_status($p);
    $log = $p['data_dir'] !== '' ? epg_tail(epg_logs_dir($p) . '/enrich.log', 100) : '';
    return [
        'state' => $state,
        'phase' => (string)($e['state'] ?? ''),
        'message' => (string)($e['message'] ?? ''),
        'titles' => $titles,
        'done' => $done,
        'percent' => $titles > 0 ? round($done * 100 / $titles, 1) : 0,
        'pending' => (int)($e['pending'] ?? 0),
        'failed' => (int)($e['failed'] ?? 0),
        'skipped' => (int)($e['skipped'] ?? 0),
        'rate' => (float)($e['rate'] ?? 0),
        'eta_secs' => isset($e['eta_secs']) ? (int)$e['eta_secs'] : null,
        'updated' => (int)($e['updated_unix'] ?? 0),
        'started' => (int)($e['started_unix'] ?? 0),
        'cached' => $cached,
        'matched' => $matched,
        'not_found' => $cached - $matched,
        'tvmaze' => $tvmaze,
        'sports' => $sports,
        'recent' => array_values(array_filter((array)($e['recent'] ?? []), 'is_array')),
        'log' => $log,
        'build_log' => activity_build_log($p),
        'build_running' => epg_build_running($p),
        'job' => $job['running'] ? (string)$job['label'] : '',
        'now' => time(),
    ];
}

$supported = activity_supported($p);
$data = activity_data($p);
$bgOn = true;
try {
    $v = epg_db($p)->query("SELECT value FROM settings WHERE key = 'tmdb_background'")->fetchColumn();
    $bgOn = $v === false || (string)$v !== '0';
} catch (Throwable $t) {
}
$actGuides = [];
try {
    $actGuides = epg_guides(epg_db($p));
} catch (Throwable $t) {
}
$csrf = epg_csrf_field();
epg_layout_start($p, 'activity', 'Activity', 'Background TMDB lookups and guide builds, live', epg_take_flashes(), '');
?>
    <?php if (!$supported): ?>
    <div class="card act-banner act-unavailable">
      <h2><?= epg_icon('pulse') ?>Not available yet</h2>
      <p>The installed epg-loader program does not have background lookups yet. Open <a href="setup.php">Setup</a> and press <strong>Build the program</strong> once, then come back here.</p>
    </div>
    <?php endif; ?>

    <div class="card act-main full" id="act">
      <div class="act-head">
        <div>
          <span class="act-state" id="act-state">…</span>
          <p class="muted act-msg" id="act-msg"></p>
        </div>
        <div class="act-controls">
          <form method="post" class="inline" id="f-start"><?= $csrf ?><input type="hidden" name="action" value="enrich_start"><button<?= $supported ? '' : ' disabled' ?>>Start</button></form>
          <form method="post" class="inline" id="f-pause" hidden><?= $csrf ?><input type="hidden" name="action" value="enrich_stop"><button class="ghost">Pause</button></form>
        </div>
      </div>
      <div class="progress act-progress" role="progressbar" aria-label="TMDB titles looked up" aria-valuemin="0" aria-valuemax="100"><span id="act-bar" style="width:0%"></span></div>
      <div class="act-figures">
        <div><strong id="act-done">0</strong> of <span id="act-titles">0</span> titles <span class="muted">(<span id="act-pct">0</span>%)</span></div>
        <div><strong id="act-rate">–</strong> <span class="muted">titles/s</span></div>
        <div><strong id="act-eta">–</strong> <span class="muted">left</span></div>
        <div class="muted" id="act-updated"></div>
      </div>
      <p class="hint" style="margin:0.6rem 0 0"><?= $bgOn ? 'Resumes on its own every 15 minutes until every title is done. Pause stops it until you press Start.' : 'Automatic resume is off in <a href="settings.php">Settings</a>; use Start here.' ?></p>
    </div>

    <section class="tiles act-counts full">
      <div class="tile"><div class="label">Cached</div><div class="value" id="c-cached">0</div><div class="note">titles in the TMDB cache</div></div>
      <div class="tile"><div class="label">Matched</div><div class="value" id="c-matched">0</div><div class="note">found on TMDB</div></div>
      <div class="tile"><div class="label">Not found</div><div class="value" id="c-notfound">0</div><div class="note">no TMDB match</div></div>
      <div class="tile"><div class="label">TVmaze</div><div class="value" id="c-tvmaze">0</div><div class="note">series found there</div></div>
      <div class="tile"><div class="label">TheSportsDB</div><div class="value" id="c-sports">0</div><div class="note">matches found there</div></div>
      <div class="tile"><div class="label">Pending</div><div class="value" id="c-pending">0</div><div class="note">still to look up</div></div>
      <div class="tile"><div class="label">Skipped</div><div class="value" id="c-skipped">0</div><div class="note">news, sports, filler</div></div>
    </section>

    <div class="card full">
      <h2><?= epg_icon('grid') ?>Recently looked up</h2>
      <div class="act-recent" id="act-recent"><p class="muted">Nothing yet.</p></div>
    </div>

    <div class="card full">
      <h2><?= epg_icon('list') ?>Background lookups log</h2>
      <pre class="act-log" id="act-log">…</pre>
    </div>

    <div class="card full" id="act-build">
      <h2><?= epg_icon('tv') ?>Guide build</h2>
      <p id="act-build-text" class="muted">No guide build is running.</p>
      <div id="act-build-cancel" hidden><?= epg_cancel_form() ?></div>
      <pre class="act-log act-build-log" id="act-build-log">…</pre>
      <form method="post" class="inline" id="act-build-start"><?= $csrf ?><input type="hidden" name="action" value="build"><button class="secondary">Build all guides now</button></form>
      <?php if ($actGuides): ?>
      <table class="compact act-guides">
        <thead><tr><th>Guide</th><th>Status</th><th>Last update</th><th>Channels</th><th></th></tr></thead>
        <tbody>
        <?php foreach ($actGuides as $g): $gs = epg_guide_status($p, $g); $gb = (int)($gs['built_at_unix'] ?? 0); ?>
          <tr>
            <td><strong><?= h($g['name']) ?></strong></td>
            <td><?= !$g['enabled'] ? '<span class="muted">off</span>' : ($gb === 0 ? '<span class="muted">not built yet</span>' : (empty($gs['ok']) ? '<span class="bad-text">last build failed</span>' : '<span class="ok-text">up to date</span>')) ?></td>
            <td><?= $gb ? h(date('D j M, H:i', $gb)) : '—' ?></td>
            <td><?= h(number_format((int)($gs['channels'] ?? 0))) ?></td>
            <td><form method="post" action="guides.php" class="inline act-gbuild"><?= $csrf ?><input type="hidden" name="action" value="build_guide"><input type="hidden" name="back" value="activity"><input type="hidden" name="id" value="<?= (int)$g['id'] ?>"><button class="ghost"<?= $g['enabled'] ? '' : ' disabled' ?>>Build</button></form></td>
          </tr>
        <?php endforeach; ?>
        </tbody>
      </table>
      <?php endif; ?>
    </div>

    <script>
    (function () {
      var data = <?= json_encode($data, JSON_UNESCAPED_SLASHES | JSON_HEX_TAG | JSON_HEX_AMP | JSON_INVALID_UTF8_SUBSTITUTE) ?>;
      var supported = <?= $supported ? 'true' : 'false' ?>;
      var $ = function (id) { return document.getElementById(id); };
      var nf = new Intl.NumberFormat();
      var labels = { running: 'Running', paused: 'Paused', idle: 'Idle', finished: 'Finished', failed: 'Stopped with an error' };
      function dur(s) {
        if (s == null) return '–';
        if (s < 90) return Math.max(1, Math.round(s)) + ' s';
        if (s < 5400) return Math.round(s / 60) + ' min';
        return (s / 3600).toFixed(1) + ' h';
      }
      function ago(t, now) {
        if (!t) return '';
        var d = Math.max(0, now - t);
        return 'updated ' + (d < 60 ? d + ' s' : d < 3600 ? Math.round(d / 60) + ' min' : (d / 3600).toFixed(1) + ' h') + ' ago';
      }
      function render(d) {
        var state = supported ? d.state : 'unavailable';
        var st = $('act-state');
        st.textContent = supported ? (labels[state] || state) : 'Not available yet';
        st.className = 'act-state s-' + state;
        $('act-msg').textContent = d.message || (state === 'idle' ? 'Nothing is running. Press Start to look up the remaining titles.' : '');
        $('f-start').hidden = state === 'running';
        $('f-pause').hidden = state !== 'running';
        $('act-bar').style.width = Math.min(100, d.percent) + '%';
        $('act-done').textContent = nf.format(d.done);
        $('act-titles').textContent = nf.format(d.titles);
        $('act-pct').textContent = d.percent;
        $('act-rate').textContent = state === 'running' && d.rate > 0 ? d.rate : '–';
        $('act-eta').textContent = state === 'running' ? dur(d.eta_secs) : '–';
        $('act-updated').textContent = ago(d.updated, d.now);
        $('c-cached').textContent = nf.format(d.cached);
        $('c-matched').textContent = nf.format(d.matched);
        $('c-notfound').textContent = nf.format(d.not_found);
        $('c-pending').textContent = nf.format(d.pending);
        $('c-tvmaze').textContent = nf.format(d.tvmaze || 0);
        $('c-sports').textContent = nf.format(d.sports || 0);
        $('c-skipped').textContent = nf.format(d.skipped);

        var box = $('act-recent');
        if (d.recent.length) {
          box.textContent = '';
          d.recent.forEach(function (r) {
            var el = document.createElement('div');
            el.className = 'act-title' + (r.found ? '' : ' miss');
            var img = document.createElement(r.found && r.poster ? 'img' : 'div');
            img.className = 'act-poster';
            if (r.found && r.poster) { img.src = r.poster; img.alt = ''; img.loading = 'lazy'; }
            var t = document.createElement('div');
            t.className = 'act-t';
            t.textContent = r.title;
            var m = document.createElement('div');
            m.className = 'act-m';
            m.textContent = r.found ? [r.year, r.rating].filter(Boolean).join(' · ') || 'matched' : 'no match';
            el.title = r.found && r.tmdb_name ? 'TMDB: ' + r.tmdb_name : r.title;
            el.appendChild(img); el.appendChild(t); el.appendChild(m);
            box.appendChild(el);
          });
        }

        var log = $('act-log');
        var atEnd = log.scrollTop + log.clientHeight >= log.scrollHeight - 30;
        log.textContent = d.log || 'No log yet.';
        if (atEnd) log.scrollTop = log.scrollHeight;

        var busy = d.build_running || d.job !== '';
        $('act-build-text').textContent = d.build_running ? 'A guide build is running now.' : (d.job ? d.job + ' is running.' : 'No guide build is running.');
        $('act-build-cancel').hidden = !busy;
        var blog = $('act-build-log');
        var bEnd = blog.scrollTop + blog.clientHeight >= blog.scrollHeight - 30;
        blog.textContent = d.build_log || 'No guide build has been started from these pages yet.';
        if (bEnd) blog.scrollTop = blog.scrollHeight;
        $('act-build-start').hidden = busy;
        document.querySelectorAll('.act-gbuild button').forEach(function (b) { if (!b.dataset.off) { b.dataset.off = b.disabled ? '1' : '0'; } b.disabled = busy || b.dataset.off === '1'; });
      }
      render(data);
      $('act-log').scrollTop = $('act-log').scrollHeight;
      function poll() {
        if (document.hidden) { setTimeout(poll, 2500); return; }
        fetch('activity.php?json=1', { credentials: 'same-origin', cache: 'no-store' })
          .then(function (r) { if (r.status === 401) { location.reload(); throw new Error('login'); } return r.json(); })
          .then(function (d) { render(d); })
          .catch(function () {})
          .then(function () { setTimeout(poll, 2500); });
      }
      setTimeout(poll, 2500);
    })();
    </script>
<?php
epg_layout_end();
