<?php
/*
 * EPGArr guides: each guide has its own XMLTV source and its own output
 * file, guides/<slug>.xml, named after the guide (renaming changes it).
 *
 * Until the program has created the guides table (on its first start), the single
 * source from the settings is shown as the "Main" guide and can be edited,
 * but guides cannot be added or removed yet.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
$p = epg_paths();
epg_require_login($p);

$fatal = null;
$db = null;
$guides = [];
$editable = false;
try {
    $db = epg_db($p);
    $editable = epg_guides_editable($db);
    $guides = epg_guides($db);
} catch (Throwable $e) {
    $fatal = $e->getMessage();
}

function guide_by_id(array $guides, int $id): ?array
{
    foreach ($guides as $g) {
        if ($g['id'] === $id) {
            return $g;
        }
    }
    return null;
}

/** A file name for this guide name that no other guide uses ("news", "news-2", ...). */
function guide_unique_slug(array $guides, string $name, int $exceptId): string
{
    $taken = [];
    foreach ($guides as $o) {
        if ($o['id'] !== $exceptId) {
            $taken[] = $o['slug'];
        }
    }
    $base = epg_slugify($name);
    $slug = $base;
    for ($i = 2; in_array($slug, $taken, true); $i++) {
        $slug = $base . '-' . $i;
    }
    return $slug;
}

/** What the add/edit dialog needs to show a guide. */
function guide_form_data(array $p, array $g): array
{
    return [
        'id' => $g['id'], 'name' => $g['name'], 'slug' => $g['slug'], 'source_url' => $g['source_url'],
        'days' => (string)$g['days'], 'refresh_hours' => (string)$g['refresh_hours'], 'logo_country' => (string)$g['logo_country'],
        'enabled' => (bool)$g['enabled'], 'legacy' => !empty($g['legacy']), 'url' => epg_guide_files($p, $g)['url'],
    ];
}

/** Validate the guide form. Returns [fields, errors]. */
function guide_input(): array
{
    $e = [];
    $name = trim((string)($_POST['name'] ?? ''));
    if ($name === '' || strlen($name) > 60) {
        $e[] = 'Give the guide a name (up to 60 characters).';
    }
    $src = trim((string)($_POST['source_url'] ?? ''));
    if ($src === '') {
        $e[] = 'The XMLTV source URL is required.';
    } elseif (strlen($src) > 2000 || preg_match('/[\x00-\x1f]/', $src)) {
        $e[] = 'The XMLTV source URL is not valid.';
    } elseif (!preg_match('~^https?://~i', $src) && !str_starts_with($src, '/')) {
        $e[] = 'The source must be an http(s) URL or an absolute file path on the server.';
    }
    $days = trim((string)($_POST['days'] ?? ''));
    if ($days !== '' && (!ctype_digit($days) || (int)$days > 60)) {
        $e[] = 'Days must be empty (keep everything) or a whole number up to 60.';
    }
    $hours = trim((string)($_POST['refresh_hours'] ?? ''));
    if ($hours !== '' && (!is_numeric($hours) || (float)$hours < 0.5 || (float)$hours > 168)) {
        $e[] = 'Rebuild interval must be empty (use the default) or between 0.5 and 168 hours.';
    }
    $lc = strtolower(trim((string)($_POST['logo_country'] ?? '')));
    if ($lc !== '' && !preg_match('/^[a-z]{2}$/', $lc)) {
        $e[] = 'Logo country must be empty (use the default) or a two-letter code such as us.';
    }
    return [[
        'name' => $name,
        'source_url' => $src,
        'days' => $days === '' ? null : (int)$days,
        'refresh_hours' => $hours === '' ? null : (float)$hours,
        'logo_country' => $lc === '' ? null : $lc,
        'enabled' => !empty($_POST['enabled']) ? 1 : 0,
    ], $e];
}

// "Test source" inside the add/edit form: tests the URL typed in the form, not the saved one.
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'test_source') {
    header('Content-Type: application/json');
    $src = trim((string)($_POST['source_url'] ?? ''));
    if (!epg_csrf_ok()) {
        $res = ['error' => 'The form expired. Reload the page.'];
    } elseif ($src === '' || strlen($src) > 2000 || preg_match('/[\x00-\x1f]/', $src) || (!preg_match('~^https?://~i', $src) && !str_starts_with($src, '/'))) {
        $res = ['error' => 'Enter an http(s) URL or an absolute file path first.'];
    } elseif ($p['data_dir'] === '' || @file_put_contents($p['data_dir'] . '/check-source.txt', $src) === false) {
        $res = ['error' => 'The data folder is not writable.'];
    } else {
        @chmod($p['data_dir'] . '/check-source.txt', 0600);
        $err = epg_start_guide_check($p, '@form');
        $res = $err === '' ? ['ok' => true] : ['error' => $err];
    }
    echo json_encode($res);
    exit;
}
if (isset($_GET['check'])) {
    header('Content-Type: application/json');
    header('Cache-Control: no-store');
    $job = epg_job_status($p);
    $running = $job['running'] && $job['name'] === 'check-guide';
    $chk = $running ? null : epg_guide_check($p, '@form');
    echo json_encode(['running' => $running, 'result' => $chk]);
    exit;
}

if ($_SERVER['REQUEST_METHOD'] === 'POST' && $fatal === null) {
    $action = (string)($_POST['action'] ?? '');
    $g = guide_by_id($guides, (int)($_POST['id'] ?? 0));
    $back = 'guides.php';
    if (!epg_csrf_ok()) {
        epg_flash('error', 'The form expired. Please try again.');
    } elseif ($action === 'reorder') {
        // Drag and drop on this page: the new order, as guide ids. JSON in, JSON out.
        header('Content-Type: application/json');
        if (!$editable) {
            echo json_encode(['error' => 'Reordering is available once the program has started once.']);
            exit;
        }
        $ids = array_values(array_filter(array_map('intval', (array)($_POST['ids'] ?? [])), fn($v) => $v > 0));
        $known = array_column($guides, 'id');
        sort($known);
        $check = $ids;
        sort($check);
        if ($check !== $known) {
            echo json_encode(['error' => 'The list changed; reload the page.']);
            exit;
        }
        $db->beginTransaction();
        $st = $db->prepare('UPDATE guides SET sort_order = ? WHERE id = ?');
        foreach ($ids as $i => $id) {
            $st->execute([$i, $id]);
        }
        $db->commit();
        echo json_encode(['ok' => true]);
        exit;
    } elseif ($action === 'save_guide') {
        [$f, $errors] = guide_input();
        $isNew = (int)($_POST['id'] ?? 0) === 0;
        if ($isNew && !$editable) {
            $errors[] = 'Adding guides is available once the program has started once (restart the container).';
        } elseif (!$isNew && $g === null) {
            $errors[] = 'That guide no longer exists.';
        }
        foreach ($guides as $other) {
            if ($f['name'] !== '' && strcasecmp($other['name'], $f['name']) === 0 && ($isNew || $other['id'] !== (int)$_POST['id'])) {
                $errors[] = 'There is already a guide called "' . $other['name'] . '". Pick another name.';
            }
        }
        if ($errors) {
            foreach ($errors as $e) {
                epg_flash('error', $e);
            }
            $_SESSION['guide_old'] = $_POST;
            $back = 'guides.php?' . ($isNew ? 'add=1' : 'edit=' . (int)$_POST['id']);
        } elseif (!$editable) {
            // One guide, stored in the settings until the program has the guides table.
            $st = $db->prepare('INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value');
            $st->execute(['main_guide_name', $f['name']]);
            $st->execute(['source_url', $f['source_url']]);
            $st->execute(['days', $f['days'] === null ? '' : (string)$f['days']]);
            if ($f['refresh_hours'] !== null) {
                $st->execute(['refresh_hours', (string)$f['refresh_hours']]);
            }
            $st->execute(['settings_saved_at', (string)time()]);
            epg_queue_check($p);
            epg_flash('ok', 'Guide saved. It applies from the next build.');
        } elseif ($isNew) {
            $slug = guide_unique_slug($guides, $f['name'], 0);
            $order = (int)$db->query('SELECT COALESCE(MAX(sort_order), 0) + 1 FROM guides')->fetchColumn();
            $st = $db->prepare('INSERT INTO guides (name, slug, source_url, days, refresh_hours, logo_country, enabled, created_at, sort_order)
                                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)');
            $st->execute([$f['name'], $slug, $f['source_url'], $f['days'], $f['refresh_hours'], $f['logo_country'], $f['enabled'], time(), $order]);
            epg_flash('ok', 'Guide "' . $f['name'] . '" added. Press Build now to create it; its address is ' . epg_base_url() . '/guides/' . $slug . '.xml');
        } else {
            // The file name follows the name; the program removes the old files after the next build.
            $slug = epg_slugify($f['name']) === epg_slugify($g['name']) ? $g['slug'] : guide_unique_slug($guides, $f['name'], $g['id']);
            $st = $db->prepare('UPDATE guides SET name = ?, slug = ?, source_url = ?, days = ?, refresh_hours = ?, logo_country = ?, enabled = ? WHERE id = ?');
            $st->execute([$f['name'], $slug, $f['source_url'], $f['days'], $f['refresh_hours'], $f['logo_country'], $f['enabled'], $g['id']]);
            if ($slug !== $g['slug']) {
                epg_flash('warn', 'The address of "' . $f['name'] . '" changes to ' . epg_base_url() . '/guides/' . $slug . '.xml with its next build. Update it in your media server; the old file is removed after that build.');
            }
            epg_flash('ok', 'Guide "' . $f['name'] . '" saved. It applies from the next build.');
        }
    } elseif ($action === 'toggle_guide' && $g !== null && $editable) {
        $db->prepare('UPDATE guides SET enabled = ? WHERE id = ?')->execute([$g['enabled'] ? 0 : 1, $g['id']]);
        epg_flash('ok', 'Guide "' . $g['name'] . '" ' . ($g['enabled'] ? 'turned off. Its file stays as it is.' : 'turned on.'));
    } elseif ($action === 'delete_guide' && $g !== null && $editable) {
        if (count($guides) <= 1) {
            epg_flash('error', 'This is the only guide, so it cannot be deleted. Turn it off instead.');
        } else {
            $db->beginTransaction();
            foreach (['guide_programmes', 'guide_channels', 'runs'] as $t) {
                if (epg_has_table($db, $t) && epg_has_column($db, $t, 'guide_id')) {
                    $db->prepare("DELETE FROM $t WHERE guide_id = ?")->execute([$g['id']]);
                }
            }
            $db->prepare('DELETE FROM guides WHERE id = ?')->execute([$g['id']]);
            $db->commit();
            $files = epg_guide_files($p, $g);
            foreach (['xml', 'gz', 'status'] as $k) {
                @unlink($files[$k]);
            }
            epg_flash('ok', 'Guide "' . $g['name'] . '" deleted, with its guide files.');
        }
    } elseif ($action === 'test_guide' && $g !== null) {
        $err = epg_start_guide_check($p, $g['slug']);
        $err === '' ? epg_flash('ok', 'Testing the source of "' . $g['name'] . '". Reload this page in a few seconds for the result.') : epg_flash('error', $err);
    } elseif ($action === 'build_guide' && $g !== null) {
        $err = epg_start_build($p, $editable ? $g['slug'] : '', $g['name']);
        $err === '' ? epg_flash('ok', 'Building "' . $g['name'] . '". Follow it on the Activity page.') : epg_flash('error', $err);
    }
    if (in_array($_POST['back'] ?? '', ['activity', 'index'], true) && $action === 'build_guide') {
        $back = $_POST['back'] . '.php';
    }
    header('Location: ' . $back, true, 303);
    exit;
}

$old = $_SESSION['guide_old'] ?? null;
unset($_SESSION['guide_old']);
$editId = (int)($_GET['edit'] ?? 0);
$adding = isset($_GET['add']) && $editable;
$editing = $editId > 0 ? guide_by_id($guides, $editId) : null;
$formGuide = $adding ? ['id' => 0, 'name' => '', 'source_url' => '', 'days' => '', 'refresh_hours' => '', 'logo_country' => '', 'enabled' => true, 'legacy' => false] : $editing;
if ($formGuide !== null && is_array($old)) {
    foreach (['name', 'source_url', 'days', 'refresh_hours', 'logo_country'] as $k) {
        $formGuide[$k] = (string)($old[$k] ?? $formGuide[$k]);
    }
    $formGuide['enabled'] = !empty($old['enabled']);
}

$defaultHours = 6.0;
if ($db) {
    $v = $db->query("SELECT value FROM settings WHERE key = 'refresh_hours'")->fetchColumn();
    $defaultHours = is_numeric($v) ? (float)$v : 6.0;
}
$buildRunning = epg_build_running($p);
$job = epg_job_status($p);
$csrf = epg_csrf_field();

function g_ago(int $t): string
{
    if ($t <= 0) {
        return '—';
    }
    $d = time() - $t;
    $in = $d < 0;
    $d = abs($d);
    $s = $d < 90 ? 'just now' : ($d < 3600 ? round($d / 60) . ' min' : ($d < 172800 ? round($d / 3600, 1) . ' h' : round($d / 86400) . ' days'));
    return $s === 'just now' ? $s : ($in ? 'in ' . $s : $s . ' ago');
}

$addButton = $editable
    ? '<button type="button" class="button" data-add>+ Add guide</button>'
    : '';
epg_layout_start($p, 'guides', 'Guides', 'Each guide has its own XMLTV source and its own address for your media server', epg_take_flashes(), $addButton);
?>
<?php if ($fatal !== null): ?>
    <div class="card">
      <h2>Not set up yet</h2>
      <p class="bad-text"><?= h($fatal) ?></p>
      <p>Check that the container is running and the /data volume is writable (Settings &rsaquo; Advanced &rsaquo; System checks).</p>
    </div>
<?php else: ?>
    <?php if (!$editable): ?>
    <div class="flash warn">The program has not created its guide list yet; restart the container. Until then you can edit the one guide you have.</div>
    <?php endif; ?>

    <dialog class="g-dialog" id="g-dialog" aria-labelledby="g-title">
      <form method="post" class="guide-form" id="g-form" autocomplete="off">
        <div class="hero-head">
          <h2 id="g-title">Add a guide</h2>
          <button type="button" class="ghost" data-close aria-label="Close">&times;</button>
        </div>
        <?= $csrf ?>
        <input type="hidden" name="action" value="save_guide">
        <input type="hidden" name="id" id="g-id" value="0">
        <label for="g-name">Name</label>
        <input type="text" id="g-name" name="name" maxlength="60" required>
        <p class="hint" id="g-addr">Its address is made from the name, e.g. guides/us-locals.xml.</p>
        <label for="g-src">XMLTV source URL</label>
        <div class="url-box">
          <input type="text" id="g-src" name="source_url" placeholder="https://example.com/guide.xml.gz" required>
          <button type="button" class="secondary" id="g-test"<?= $editable ? '' : ' hidden' ?>>Test source</button>
        </div>
        <p class="hint" id="g-test-out">Plain or gzipped XMLTV, or an absolute file path on the server.</p>
        <div class="row">
          <div>
            <label for="g-days">Days to keep</label>
            <input type="number" id="g-days" name="days" min="0" max="60" placeholder="all">
            <p class="hint">Empty or 0 keeps everything.</p>
          </div>
          <div>
            <label for="g-hours">Rebuild every (hours)</label>
            <input type="number" id="g-hours" name="refresh_hours" min="0.5" max="168" step="0.5" placeholder="<?= h(rtrim(rtrim(number_format($defaultHours, 1), '0'), '.')) ?> (default)">
            <p class="hint">Empty uses the default.</p>
          </div>
          <div class="g-only-new-schema">
            <label for="g-lc">Logo country</label>
            <input type="text" id="g-lc" name="logo_country" maxlength="2" placeholder="default" class="short">
            <p class="hint">For channels without a country.</p>
          </div>
        </div>
        <label class="check g-only-new-schema"><input type="checkbox" name="enabled" id="g-enabled" value="1" checked> Build this guide on the schedule</label>
        <div class="form-actions">
          <button>Save guide</button>
          <button type="button" class="ghost" data-close>Cancel</button>
        </div>
      </form>
    </dialog>

    <div class="guide-list">
    <?php foreach ($guides as $g):
        $files = epg_guide_files($p, $g);
        $st = epg_guide_status($p, $g);
        $built = (int)($st['built_at_unix'] ?? 0);
        $mine = $buildRunning && ($job['label'] === '' || $job['label'] === 'Guide build' || $job['label'] === 'Guide build: ' . $g['name'] || !empty($g['legacy']));
        if (!$g['enabled']) {
            $pill = ['off', 'Off'];
        } elseif ($mine) {
            $pill = ['building', 'Building'];
        } elseif ($built === 0) {
            $pill = ['none', 'Not built yet'];
        } elseif (empty($st['ok'])) {
            $pill = ['failed', 'Last build failed'];
        } else {
            $pill = ['ok', 'Up to date'];
        }
        $hours = $g['refresh_hours'] !== '' ? (float)$g['refresh_hours'] : $defaultHours;
        $next = $built > 0 && $g['enabled'] ? (int)(ceil(max(time(), $built + (int)round($hours * 3600)) / 900) * 900) : 0;
        $hasFile = is_file($files['xml']);
        ?>
      <article class="card guide-card<?= $g['enabled'] ? '' : ' is-off' ?>" data-id="<?= (int)$g['id'] ?>">
        <div class="hero-head">
          <?php if ($editable && count($guides) > 1): ?><button type="button" class="drag-handle" title="Drag to reorder (or use the arrow keys)" aria-label="Move <?= h($g['name']) ?>"><span aria-hidden="true">&#8942;&#8942;</span></button><?php endif; ?>
          <h2><?= epg_icon('tv') ?><?= h($g['name']) ?></h2>
          <span class="pill pill-<?= h($pill[0]) ?>"><?= h($pill[1]) ?></span>
        </div>
        <p class="muted guide-src"><?= h(epg_source_label($g['source_url'])) ?><?= $g['days'] !== '' && $g['days'] !== '0' ? ' · keeps ' . h($g['days']) . ' days' : '' ?> · every <?= h(rtrim(rtrim(number_format($hours, 1), '0'), '.')) ?> h</p>
        <?php $chk = epg_guide_check($p, $g['slug']); if ($chk !== null): ?><p class="guide-test <?= $chk['ok'] ? 'ok-text' : 'bad-text' ?>">Source test <?= h(g_ago($chk['at'])) ?>: <?= h($chk['detail']) ?></p><?php endif; ?>
        <?php if (!empty($st['last_error']) && $pill[0] === 'failed'): ?><p class="bad-text guide-err"><?= h((string)$st['last_error']) ?></p><?php endif; ?>
        <dl class="guide-facts">
          <dt>Channels</dt><dd><?= h(number_format((int)($st['channels'] ?? 0))) ?></dd>
          <dt>Programmes</dt><dd><?= h(number_format((int)($st['programmes'] ?? 0))) ?></dd>
          <dt>Last update</dt><dd><?= h(g_ago($built)) ?></dd>
          <dt>Next update</dt><dd><?= $g['enabled'] ? h($next ? g_ago($next) : 'after the first build') : 'off' ?></dd>
        </dl>
        <div class="url-box">
          <span class="url-fmt" role="group" aria-label="File format"><button type="button" class="on" data-for="url-<?= h($g['slug']) ?>" data-url="<?= h($files['url']) ?>">XML</button><button type="button" data-for="url-<?= h($g['slug']) ?>" data-url="<?= h($files['gz_url']) ?>">GZ</button></span>
          <input type="text" id="url-<?= h($g['slug']) ?>" value="<?= h($files['url']) ?>" readonly aria-label="<?= h($g['name']) ?> guide address">
          <button type="button" class="copy" data-for="url-<?= h($g['slug']) ?>"<?= $hasFile ? '' : ' title="Not built yet"' ?>><?= epg_icon('copy') ?><span>Copy</span></button>
        </div>
        <div class="guide-actions">
          <form method="post" class="inline"><?= $csrf ?><input type="hidden" name="action" value="build_guide"><input type="hidden" name="id" value="<?= (int)$g['id'] ?>"><button class="ga-primary"<?= $buildRunning || !$g['enabled'] ? ' disabled' : '' ?>><?= epg_icon('play') ?>Build now</button></form>
          <span class="ga-right">
          <a class="ga-icon" href="guide.php?guide=<?= h(rawurlencode($g['slug'])) ?>" title="View in TV Guide" aria-label="View <?= h($g['name']) ?> in TV Guide"><?= epg_icon('eye') ?></a>
          <button type="button" class="ga-icon" title="Edit" aria-label="Edit <?= h($g['name']) ?>" data-edit="<?= h(json_encode(guide_form_data($p, $g))) ?>"><?= epg_icon('edit') ?></button>
          <?php if ($editable): ?>
          <details class="ga-more">
            <summary aria-label="More actions" title="More actions"><span aria-hidden="true">&#8943;</span></summary>
            <div class="ga-menu">
              <form method="post"><?= $csrf ?><input type="hidden" name="action" value="test_guide"><input type="hidden" name="id" value="<?= (int)$g['id'] ?>"><button>Test source</button></form>
              <form method="post"><?= $csrf ?><input type="hidden" name="action" value="toggle_guide"><input type="hidden" name="id" value="<?= (int)$g['id'] ?>"><button><?= $g['enabled'] ? 'Turn off' : 'Turn on' ?></button></form>
              <?php if (count($guides) > 1): ?>
              <form method="post" onsubmit="return confirm(<?= h(json_encode('Delete the guide "' . $g['name'] . '"? Its guide files are removed too.')) ?>)"><?= $csrf ?><input type="hidden" name="action" value="delete_guide"><input type="hidden" name="id" value="<?= (int)$g['id'] ?>"><button class="danger-text">Delete</button></form>
              <?php endif; ?>
            </div>
          </details>
          <?php endif; ?>
          </span>
        </div>
      </article>
    <?php endforeach; ?>
    </div>

    <script>
    (function () {
      var $ = function (id) { return document.getElementById(id); };
      // "More" menus: close on a click elsewhere, on Escape, and when another one opens.
      var menus = document.querySelectorAll('.ga-more');
      document.addEventListener('click', function (e) {
        menus.forEach(function (m) { if (m.open && !m.contains(e.target)) { m.open = false; } });
      });
      document.addEventListener('keydown', function (e) {
        if (e.key !== 'Escape') { return; }
        menus.forEach(function (m) { if (m.open) { m.open = false; m.querySelector('summary').focus(); } });
      });
      menus.forEach(function (m) {
        m.addEventListener('toggle', function () {
          if (!m.open) { return; }
          menus.forEach(function (o) { if (o !== m) { o.open = false; } });
          var first = m.querySelector('.ga-menu button');
          if (first) { first.focus(); }
        });
      });
      // Drag and drop (or arrow keys on the grip) to set the order of the guides.
      var list = document.querySelector('.guide-list');
      var dragged = null;
      function cards() { return Array.prototype.slice.call(list.querySelectorAll('.guide-card')); }
      function toast(text, bad) {
        var t = document.createElement('div');
        t.className = 'toast' + (bad ? ' bad' : '');
        t.textContent = text;
        document.body.appendChild(t);
        setTimeout(function () { t.remove(); }, 2200);
      }
      function saveOrder(before) {
        var fd = new FormData();
        fd.append('action', 'reorder');
        fd.append('csrf', document.querySelector('#g-form input[name=csrf]').value);
        cards().forEach(function (c) { fd.append('ids[]', c.dataset.id); });
        fetch('guides.php', { method: 'POST', body: fd, credentials: 'same-origin' })
          .then(function (r) { return r.json(); })
          .then(function (j) {
            if (j.error) { throw new Error(j.error); }
            toast('Order saved');
          })
          .catch(function (e) {
            before.forEach(function (c) { list.appendChild(c); });
            toast(e.message || 'Could not save the order', true);
          });
      }
      if (list) {
        list.querySelectorAll('.drag-handle').forEach(function (h) {
          var card = h.closest('.guide-card');
          h.addEventListener('pointerdown', function () { card.draggable = true; });
          h.addEventListener('pointerup', function () { card.draggable = false; });
          h.addEventListener('keydown', function (e) {
            if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown' && e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') { return; }
            e.preventDefault();
            var before = cards(), i = before.indexOf(card);
            var up = e.key === 'ArrowUp' || e.key === 'ArrowLeft';
            if (up && i > 0) { list.insertBefore(card, before[i - 1]); }
            else if (!up && i < before.length - 1) { list.insertBefore(card, before[i + 1].nextSibling); }
            else { return; }
            h.focus();
            saveOrder(before);
          });
        });
        var start = null;
        list.addEventListener('dragstart', function (e) {
          dragged = e.target.closest('.guide-card');
          if (!dragged) { return; }
          start = cards();
          e.dataTransfer.effectAllowed = 'move';
          e.dataTransfer.setData('text/plain', dragged.dataset.id);
          setTimeout(function () { dragged.classList.add('dragging'); }, 0);
        });
        list.addEventListener('dragover', function (e) {
          if (!dragged) { return; }
          e.preventDefault();
          var over = e.target.closest('.guide-card');
          if (!over || over === dragged) { return; }
          var r = over.getBoundingClientRect();
          var after = (e.clientY - r.top) > r.height / 2 || (e.clientX - r.left) > r.width / 2 && Math.abs(e.clientY - (r.top + r.height / 2)) < r.height / 2;
          list.insertBefore(dragged, after ? over.nextSibling : over);
        });
        list.addEventListener('drop', function (e) { if (dragged) { e.preventDefault(); } });
        list.addEventListener('dragend', function () {
          if (!dragged) { return; }
          dragged.classList.remove('dragging');
          dragged.draggable = false;
          var moved = start && start.some(function (c, i) { return cards()[i] !== c; });
          if (moved) { saveOrder(start); }
          dragged = null;
        });
      }

      var dlg = $('g-dialog'), form = $('g-form');
      var BASE = <?= json_encode(epg_base_url() . '/guides/') ?>;
      var cur = null;
      function slugify(s) {
        s = s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40);
        return s || 'guide';
      }
      function addr() {
        var url = BASE + slugify($('g-name').value) + '.xml';
        var out = $('g-addr');
        var changes = cur && cur.id && cur.url && cur.url !== url;
        out.className = 'hint' + (changes ? ' warn-text' : '');
        out.textContent = changes
          ? 'The address changes from ' + cur.url + ' to ' + url + ' with the next build. Update it in your media server.'
          : 'Address: ' + url;
        return changes;
      }
      function open(d) {
        cur = d;
        $('g-title').textContent = d.id ? 'Edit "' + d.name + '"' : 'Add a guide';
        $('g-id').value = d.id || 0;
        $('g-name').value = d.name || '';
        $('g-src').value = d.source_url || '';
        $('g-days').value = d.days || '';
        $('g-hours').value = d.refresh_hours || '';
        $('g-lc').value = d.logo_country || '';
        $('g-enabled').checked = d.enabled !== false;
        form.querySelectorAll('.g-only-new-schema').forEach(function (e) { e.hidden = !!d.legacy; });
        $('g-test-out').className = 'hint';
        $('g-test-out').textContent = 'Plain or gzipped XMLTV, or an absolute file path on the server.';
        addr();
        if (dlg.showModal) { dlg.showModal(); } else { dlg.setAttribute('open', ''); }
        $('g-name').focus();
      }
      document.querySelectorAll('[data-add]').forEach(function (b) { b.addEventListener('click', function () { open({}); }); });
      document.querySelectorAll('[data-edit]').forEach(function (b) {
        b.addEventListener('click', function () { open(JSON.parse(b.getAttribute('data-edit'))); });
      });
      dlg.querySelectorAll('[data-close]').forEach(function (b) { b.addEventListener('click', function () { dlg.close ? dlg.close() : dlg.removeAttribute('open'); }); });
      $('g-name').addEventListener('input', addr);
      form.addEventListener('submit', function (e) {
        if (addr() && !confirm($('g-addr').textContent + ' Save anyway?')) { e.preventDefault(); }
      });
      $('g-test').addEventListener('click', function () {
        var out = $('g-test-out'), btn = this, fd = new FormData(form);
        fd.set('action', 'test_source');
        out.className = 'hint';
        out.textContent = 'Testing the source…';
        btn.disabled = true;
        var tries = 0;
        var done = function (cls, text) { out.className = 'hint ' + cls; out.textContent = text; btn.disabled = false; };
        var poll = function () {
          fetch('guides.php?check=1', { credentials: 'same-origin', cache: 'no-store' }).then(function (r) { return r.json(); }).then(function (j) {
            if (j.result && !j.running) { done(j.result.ok ? 'ok-text' : 'bad-text', j.result.detail); return; }
            if (++tries > 80) { done('bad-text', 'The test is taking too long. Try again later.'); return; }
            setTimeout(poll, 1500);
          }).catch(function () { done('bad-text', 'Could not get the test result.'); });
        };
        fetch('guides.php', { method: 'POST', body: fd, credentials: 'same-origin' }).then(function (r) { return r.json(); }).then(function (j) {
          if (j.error) { done('bad-text', j.error); return; }
          setTimeout(poll, 1500);
        }).catch(function () { done('bad-text', 'Could not start the test.'); });
      });
      <?php if ($formGuide !== null): ?>open(<?= json_encode(array_merge(guide_form_data($p, $formGuide + ['slug' => '']), ['name' => (string)$formGuide['name'], 'source_url' => (string)$formGuide['source_url']]), JSON_HEX_TAG | JSON_HEX_AMP) ?>);<?php endif; ?>
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
      document.querySelectorAll('button.copy').forEach(function (btn) {
        var input = document.getElementById(btn.getAttribute('data-for'));
        btn.addEventListener('click', function () {
          var label = btn.querySelector('span');
          var done = function () { label.textContent = 'Copied'; setTimeout(function () { label.textContent = 'Copy'; }, 1800); };
          if (navigator.clipboard && window.isSecureContext) {
            navigator.clipboard.writeText(input.value).then(done, function () { input.select(); document.execCommand('copy'); done(); });
          } else {
            input.select(); document.execCommand('copy'); done();
          }
        });
      });
    })();
    </script>
<?php endif; ?>
<?php epg_layout_end();
