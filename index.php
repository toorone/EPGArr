<?php
/*
 * EPGArr dashboard: are the guides healthy, and where to point the media server.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
$p = epg_paths();
epg_require_login($p);

if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'cancel') {
    if (!epg_authed($p)) {
        epg_flash('error', 'Log in (Settings) to cancel a task.');
    } elseif (!epg_csrf_ok()) {
        epg_flash('error', 'The form expired. Please try again.');
    } else {
        $err = epg_cancel($p);
        $err === '' ? epg_flash('ok', 'Cancelled.') : epg_flash('error', $err);
    }
    epg_redirect_self();
}

$enrich = epg_enrich_status($p);

$db = epg_db($p);
$settings = [];
foreach ($db->query('SELECT key, value FROM settings') as $r) {
    $settings[$r['key']] = $r['value'];
}
$last = $db->query('SELECT * FROM runs ORDER BY id DESC LIMIT 1')->fetch() ?: null;
$lastOk = $db->query('SELECT * FROM runs WHERE ok = 1 ORDER BY id DESC LIMIT 1')->fetch() ?: null;
$sum = $lastOk && $lastOk['summary'] ? (json_decode((string)$lastOk['summary'], true) ?: []) : [];
$history = $db->query('SELECT * FROM runs ORDER BY id DESC LIMIT ' . (isset($_GET['history']) ? 50 : 5))->fetchAll();
$showAllRuns = isset($_GET['history']);
$guides = epg_guides($db);
$runGuide = epg_has_column($db, 'runs', 'guide_id') && count($guides) > 1;

// Artwork and logo coverage of the guides as they are now. Counting posters reads every
// programme row (slow with large guides), so the result is cached until the next build.
$cov = ['progs' => 0, 'posters' => 0, 'chans' => 0, 'logos' => 0];
try {
    $stamp = (string)$db->query('SELECT COALESCE(MAX(finished_at), 0) || \'-\' || COUNT(*) FROM runs')->fetchColumn();
    $cacheFile = $p['data_dir'] !== '' ? $p['data_dir'] . '/dashboard-cache.json' : '';
    $cached = $cacheFile !== '' && is_readable($cacheFile) ? json_decode((string)file_get_contents($cacheFile), true) : null;
    if (is_array($cached) && ($cached['stamp'] ?? '') === $stamp && is_array($cached['cov'] ?? null)) {
        $cov = $cached['cov'] + $cov;
    } else {
        $r = $db->query("SELECT COUNT(*) AS n, COUNT(NULLIF(icon, '')) AS i FROM guide_programmes")->fetch();
        $cov['progs'] = (int)($r['n'] ?? 0);
        $cov['posters'] = (int)($r['i'] ?? 0);
        $r = $db->query("SELECT COUNT(*) AS n, COUNT(NULLIF(logo, '')) AS l FROM guide_channels")->fetch();
        $cov['chans'] = (int)($r['n'] ?? 0);
        $cov['logos'] = (int)($r['l'] ?? 0);
        if ($cacheFile !== '') {
            @file_put_contents($cacheFile, json_encode(['stamp' => $stamp, 'cov' => $cov]));
        }
    }
} catch (Throwable $t) {
    // No guide tables yet.
}
$logoSrc = [];
try {
    foreach ($db->query('SELECT source, COUNT(*) AS n FROM channel_logo_sources GROUP BY source') as $r) {
        $logoSrc[(string)$r['source']] = (int)$r['n'];
    }
} catch (Throwable $t) {
    // Older database without the table.
}
$running = epg_build_running($p);
$job = epg_job_status($p);
$busy = $running || $job['running'];

// Next run: the schedule fires every 15 minutes and builds once refresh_hours have passed.
$hours = (float)($settings['refresh_hours'] ?? 6) ?: 6.0;
$next = 0;
if ($last) {
    $due = (int)$last['started_at'] + (int)round($hours * 3600);
    $next = max($due, time());
    $next = (int)(ceil($next / 900) * 900);
}

$guideUrl = epg_base_url() . '/epg.xml';
$hasGz = is_file($p['web_dir'] . '/epg.xml.gz');

function fmt_time(?int $t): string
{
    return $t ? date('D j M, H:i', $t) : '—';
}

function fmt_ago(?int $t): string
{
    if (!$t) {
        return '';
    }
    $d = time() - $t;
    if ($d < 0) {
        $d = -$d;
        return 'in ' . ($d < 3600 ? max(1, (int)round($d / 60)) . ' min' : round($d / 3600, 1) . ' h');
    }
    if ($d < 90) {
        return 'just now';
    }
    if ($d < 3600) {
        return (int)round($d / 60) . ' min ago';
    }
    if ($d < 86400 * 2) {
        return round($d / 3600, 1) . ' h ago';
    }
    return (int)round($d / 86400) . ' days ago';
}

function num(mixed $v): string
{
    return number_format((int)$v);
}

$authed = epg_authed($p);
epg_layout_start($p, 'dashboard', 'Dashboard', 'Are your guides healthy, and where to point your media server', epg_take_flashes(), '');

$lastOkTime = $lastOk ? (int)$lastOk['finished_at'] : 0;
$lastFailed = $last && $last['finished_at'] !== null && (int)$last['ok'] === 0 && $last['error'] !== 'cancelled';
?>
    <?php
    if ($running) {
        $health = ['building', 'Building now', 'A new guide is being built. Your media server keeps the previous one until it is ready.'];
    } elseif (!$lastOk) {
        $health = ['none', 'Not built yet', 'No guide has been built yet. It is built on the schedule, or start one on the Activity page.'];
    } elseif ($lastFailed) {
        $health = ['failed', 'Last build failed', (string)$last['error']];
    } else {
        $health = ['ok', 'Up to date', ''];
    }
    $pct = fn(int $a, int $b): string => $b > 0 ? (string)round($a * 100 / $b) . '%' : '—';
    $eTitles = (int)($enrich['titles'] ?? 0);
    $eDone = min($eTitles, (int)($enrich['cached'] ?? 0) + (int)($enrich['done'] ?? 0));
    $ePending = (int)($enrich['pending'] ?? 0);
    $eState = $enrich['active'] ? 'running' : (!empty($enrich['stopped_by_user']) ? 'paused' : ($eTitles > 0 && $ePending === 0 ? 'finished' : 'idle'));
    $srcNames = ['feed' => 'from the source', 'override' => 'overrides', 'tv-logos' => 'tv-logos', 'iptv-org' => 'iptv-org'];
    $srcTip = [];
    foreach ($srcNames as $k => $label) {
        if (!empty($logoSrc[$k])) {
            $srcTip[] = num($logoSrc[$k]) . ' ' . $label;
        }
    }
    ?>
    <?php
    $gNames = [];
    foreach ($guides as $g) {
        $gNames[$g['id']] = $g['name'];
    }
    $enabledCount = count(array_filter($guides, fn($g) => $g['enabled']));
    ?>
    <section class="tiles tiles-5">
      <div class="tile"><div class="label">Guides</div><div class="value"><?= h(num(count($guides))) ?></div><div class="note"><?= h(num($enabledCount)) ?> on</div></div>
      <div class="tile"><div class="label">Channels</div><div class="value"><?= h(num($cov['chans'] ?: ($sum['channels'] ?? 0))) ?></div><div class="note">across all guides</div></div>
      <div class="tile"><div class="label">Programmes</div><div class="value"><?= h(num($cov['progs'] ?: ($sum['programmes'] ?? 0))) ?></div><div class="note">across all guides</div></div>
      <div class="tile" title="<?= h(num($cov['posters']) . ' of ' . num($cov['progs']) . ' programmes have a poster') ?>">
        <div class="label">Artwork</div><div class="value"><?= h($pct($cov['posters'], $cov['progs'])) ?></div><div class="note">programmes with a poster</div></div>
      <div class="tile" title="<?= h($srcTip ? implode(' · ', $srcTip) : num($cov['logos']) . ' of ' . num($cov['chans']) . ' channels') ?>">
        <div class="label">Logos</div><div class="value"><?= h($pct($cov['logos'], $cov['chans'])) ?></div><div class="note">channels with a logo</div></div>
    </section>

    <div class="card">
      <div class="hero-head">
        <h2><?= epg_icon('list') ?>Your guides</h2>
        <a href="guides.php">Manage guides</a>
      </div>
      <p class="hint" style="margin:0 0 0.8rem">Add a guide's address as an XMLTV guide source in your media server (Emby, Plex, Jellyfin or any XMLTV client).</p>
      <div class="guide-rows">
      <?php foreach ($guides as $g):
          $files = epg_guide_files($p, $g);
          $st = epg_guide_status($p, $g);
          $built = (int)($st['built_at_unix'] ?? 0);
          $mine = $running && ($job['label'] === '' || $job['label'] === 'Guide build' || $job['label'] === 'Guide build: ' . $g['name'] || !empty($g['legacy']));
          $pill = !$g['enabled'] ? ['off', 'Off'] : ($mine ? ['building', 'Building'] : ($built === 0 ? ['none', 'Not built yet'] : (empty($st['ok']) ? ['failed', 'Last build failed'] : ['ok', 'Up to date'])));
          $gh = $g['refresh_hours'] !== '' ? (float)$g['refresh_hours'] : $hours;
          $gNext = $built > 0 && $g['enabled'] ? (int)(ceil(max(time(), $built + (int)round($gh * 3600)) / 900) * 900) : 0;
          $uid = 'url-' . $g['slug'];
          ?>
        <div class="guide-row<?= $g['enabled'] ? '' : ' is-off' ?>">
          <div class="gr-name"><strong><?= h($g['name']) ?></strong><span class="pill pill-<?= h($pill[0]) ?>"><?= h($pill[1]) ?></span></div>
          <div class="gr-facts muted">
            <span>Updated <?= h($built ? fmt_ago($built) : '—') ?></span>
            <span>Next <?= h($gNext ? fmt_ago($gNext) : ($g['enabled'] ? 'after the first build' : 'off')) ?></span>
            <span><?= h(num($st['channels'] ?? 0)) ?> channels</span>
          </div>
          <div class="url-box gr-url">
            <span class="url-fmt" role="group" aria-label="File format"><button type="button" class="on" data-for="<?= h($uid) ?>" data-url="<?= h($files['url']) ?>">XML</button><button type="button" data-for="<?= h($uid) ?>" data-url="<?= h($files['gz_url']) ?>">GZ</button></span>
            <input type="text" id="<?= h($uid) ?>" value="<?= h($files['url']) ?>" readonly aria-label="<?= h($g['name']) ?> guide address">
            <button type="button" class="copy" data-for="<?= h($uid) ?>"><?= epg_icon('copy') ?><span>Copy</span></button>
          </div>
          <form method="post" action="guides.php" class="inline gr-build"><?= epg_csrf_field() ?><input type="hidden" name="action" value="build_guide"><input type="hidden" name="back" value="index"><input type="hidden" name="id" value="<?= (int)$g['id'] ?>"><button class="secondary"<?= $running || !$g['enabled'] ? ' disabled' : '' ?>>Build now</button></form>
        </div>
      <?php endforeach; ?>
      </div>
    </div>


    <?php if (!empty($sum['warnings']) || !empty($sum['errors'])): ?>
    <details class="card notes">
      <summary><?= epg_icon('list') ?>Notes from the last build</summary>
      <?php foreach (($sum['warnings'] ?? []) as $w): ?><p class="warn-text" style="margin:0.2rem 0"><?= h((string)$w) ?></p><?php endforeach; ?>
      <?php foreach (array_slice($sum['errors'] ?? [], 0, 12) as $e): ?><p class="bad-text" style="margin:0.2rem 0"><?= h((string)$e) ?></p><?php endforeach; ?>
    </details>
    <?php endif; ?>

    <div class="card" id="history">
      <div class="hero-head">
        <h2><?= epg_icon('history') ?><?= $showAllRuns ? 'Build history' : 'Recent builds' ?></h2>
        <?php if ($history): ?><a href="<?= $showAllRuns ? 'index.php#history' : 'index.php?history=all#history' ?>"><?= $showAllRuns ? 'Show fewer' : 'View all' ?></a><?php endif; ?>
      </div>
      <?php if (!$history): ?>
        <p class="muted">No builds yet.</p>
      <?php else: ?>
      <div class="table-scroll"><table class="compact runs">
        <thead><tr><th class="chev-col"></th><?php if ($runGuide): ?><th>Guide</th><?php endif; ?><th>Result</th><th>When</th><th class="num">Duration</th><th class="num">Channels</th><th class="num">Programmes</th><th class="num" title="Channel logos added (not already in the source)">Logos added</th><th class="num">Posters added</th><th class="num" title="Categories (groups) added, e.g. Movie, News, Sports">Groups added</th></tr></thead>
        <tbody>
        <?php
        $na = '<span class="muted">n/a</span>';
        $fmtBytes = fn($b): string => $b === null ? $na : h($b >= 1048576 ? round($b / 1048576, 1) . ' MB' : round($b / 1024) . ' KB');
        foreach ($history as $r):
            $res = $r['finished_at'] === null ? 'running' : ((int)$r['ok'] === 1 ? 'ok' : ($r['error'] === 'cancelled' ? 'cancelled' : 'failed'));
            $cls = $res === 'ok' ? 'ok' : ($res === 'failed' ? 'fail' : 'info');
            $rs = json_decode((string)($r['summary'] ?? ''), true) ?: [];
            $v = fn(string $k): string => isset($rs[$k]) && is_numeric($rs[$k]) ? h(num($rs[$k])) : '<span class="muted">n/a</span>';
            $cols = 9 + ($runGuide ? 1 : 0);
            $rid = 'run-' . (int)$r['id'];
            $problems = (int)($r['errors_total'] ?? 0) + count($rs['warnings'] ?? []);
            ?>
          <tr class="run-row" tabindex="0" aria-expanded="false" aria-controls="<?= $rid ?>">
            <td class="chev-col"><span class="chev" aria-hidden="true">&#8250;</span></td>
            <?php if ($runGuide): ?><td><?= h($gNames[(int)($r['guide_id'] ?? 0)] ?? '—') ?></td><?php endif; ?>
            <td><span class="dot <?= $cls ?>"></span><?= h($res) ?><?= $problems ? ' <span class="muted">· ' . (int)$problems . ' note' . ($problems > 1 ? 's' : '') . '</span>' : '' ?></td>
            <td><?= h(fmt_time((int)$r['started_at'])) ?></td>
            <td class="num"><?= $r['duration_secs'] !== null ? h((string)round((float)$r['duration_secs']) . ' s') : '' ?></td>
            <td class="num"><?= $r['channels'] !== null ? h(num($r['channels'])) : '' ?></td>
            <td class="num"><?= $r['programmes'] !== null ? h(num($r['programmes'])) : '' ?></td>
            <td class="num"><?= $v('logos_inserted') ?></td>
            <td class="num"><?= $r['posters_added'] !== null ? h(num($r['posters_added'])) : '' ?></td>
            <td class="num"><?= $v('categories_added') ?></td>
          </tr>
          <tr class="run-detail" id="<?= $rid ?>" hidden>
            <td colspan="<?= $cols ?>">
              <?php if ($r['error']): ?><p class="bad-text" style="margin:0 0 0.6rem"><?= h((string)$r['error']) ?></p><?php endif; ?>
              <div class="run-grid">
                <section>
                  <h4>Logos</h4>
                  <dl>
                    <dt>Kept from the source</dt><dd><?= $v('logos_detected') ?></dd>
                    <dt>From overrides</dt><dd><?= $v('logos_from_overrides') ?></dd>
                    <dt>From tv-logos</dt><dd><?= $v('logos_from_tvlogos') ?></dd>
                    <dt>From iptv-org</dt><dd><?= $v('logos_from_iptv') ?></dd>
                    <dt>Still missing</dt><dd><?= $v('logos_missing') ?></dd>
                  </dl>
                </section>
                <section>
                  <h4>Added to programmes</h4>
                  <dl>
                    <dt>Posters</dt><dd><?= $v('posters_added') ?></dd>
                    <dt>Ratings</dt><dd><?= $v('ratings_added') ?></dd>
                    <dt>IMDb ratings</dt><dd><?= $v('imdb_ratings_added') ?></dd>
                    <dt class="sub">Posters from TVmaze</dt><dd><?= $v('posters_from_tvmaze') ?></dd>
                    <dt class="sub">Posters from TheSportsDB</dt><dd><?= $v('posters_from_sportsdb') ?></dd>
                    <dt>Descriptions</dt><dd><?= $v('descs_added') ?></dd>
                    <dt>Groups</dt><dd><?= $v('categories_added') ?></dd>
                    <?php if (!empty($rs['categories_by_group']) && is_array($rs['categories_by_group'])): ?>
                      <?php foreach ($rs['categories_by_group'] as $gname => $n): ?><dt class="sub"><?= h((string)$gname) ?></dt><dd><?= h(num($n)) ?></dd><?php endforeach; ?>
                    <?php endif; ?>
                  </dl>
                </section>
                <section>
                  <h4>TMDB titles</h4>
                  <dl>
                    <dt>Titles in the guide</dt><dd><?= $v('tmdb_titles') ?></dd>
                    <dt>From the cache</dt><dd><?= $v('tmdb_cache_hits') ?></dd>
                    <dt>Looked up</dt><dd><?= $v('tmdb_lookups') ?></dd>
                    <dt>Not on TMDB</dt><dd><?= $v('tmdb_not_found') ?></dd>
                    <dt>Left for later</dt><dd><?= $v('tmdb_deferred') ?></dd>
                    <dt>Skipped (news, sports, filler)</dt><dd><?= $v('tmdb_skipped') ?></dd>
                    <dt>TVmaze looked up</dt><dd><?= $v('tvmaze_lookups') ?></dd>
                    <dt>TheSportsDB looked up</dt><dd><?= $v('sportsdb_lookups') ?></dd>
                  </dl>
                </section>
                <section>
                  <h4>Output</h4>
                  <dl>
                    <dt>.xml</dt><dd><?= $fmtBytes(isset($rs['xml_bytes']) ? (int)$rs['xml_bytes'] : null) ?></dd>
                    <dt>.xml.gz</dt><dd><?= $fmtBytes(isset($rs['gz_bytes']) ? (int)$rs['gz_bytes'] : null) ?></dd>
                    <dt>Dropped (days limit)</dt><dd><?= $v('programmes_dropped') ?></dd>
                  </dl>
                </section>
              </div>
              <?php foreach (($rs['warnings'] ?? []) as $w): ?><p class="warn-text run-note"><?= h((string)$w) ?></p><?php endforeach; ?>
              <?php foreach (array_slice($rs['errors'] ?? [], 0, 12) as $e): ?><p class="bad-text run-note"><?= h((string)$e) ?></p><?php endforeach; ?>
            </td>
          </tr>
        <?php endforeach; ?>
        </tbody>
      </table></div>
      <?php endif; ?>
    </div>

    <script>
    (function () {
      // XML | GZ switch next to each guide address; the choice is remembered.
      var fmtKey = 'epg-url-format';
      function setFmt(gz) {
        document.querySelectorAll('.url-fmt').forEach(function (g) {
          var b = g.querySelectorAll('button');
          var pick = b[gz ? 1 : 0];
          b.forEach(function (x) { x.classList.toggle('on', x === pick); x.setAttribute('aria-pressed', x === pick ? 'true' : 'false'); });
          var input = document.getElementById(pick.getAttribute('data-for'));
          if (input) { input.value = pick.getAttribute('data-url'); }
        });
      }
      document.querySelectorAll('.url-fmt button').forEach(function (b, i) {
        b.addEventListener('click', function () {
          var gz = b.textContent.trim() === 'GZ';
          try { localStorage.setItem(fmtKey, gz ? 'gz' : 'xml'); } catch (e) {}
          setFmt(gz);
        });
      });
      try { setFmt(localStorage.getItem(fmtKey) === 'gz'); } catch (e) { setFmt(false); }
      // Recent builds: a click (or Enter) on a row shows everything recorded for that run.
      document.querySelectorAll('tr.run-row').forEach(function (row) {
        var toggle = function () {
          var d = document.getElementById(row.getAttribute('aria-controls'));
          var open = d.hidden;
          d.hidden = !open;
          row.setAttribute('aria-expanded', open ? 'true' : 'false');
        };
        row.addEventListener('click', toggle);
        row.addEventListener('keydown', function (e) { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); } });
      });
      document.querySelectorAll('button.copy').forEach(function (btn) {
        var input = document.getElementById(btn.getAttribute('data-for'));
        btn.addEventListener('click', function () {
          var label = btn.querySelector('span');
          var done = function () {
            label.textContent = 'Copied';
            setTimeout(function () { label.textContent = 'Copy'; }, 1800);
          };
          if (navigator.clipboard && window.isSecureContext) {
            navigator.clipboard.writeText(input.value).then(done, function () { input.select(); document.execCommand('copy'); done(); });
          } else {
            input.select();
            document.execCommand('copy');
            done();
          }
        });
      });
      <?php if ($busy): ?>setTimeout(function () { location.reload(); }, 15000);<?php endif; ?>
    })();
    </script>
<?php epg_layout_end();
