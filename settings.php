<?php
/*
 * EPGArr settings page: a thin bridge to the Rust program.
 *
 * - Reads and writes rows in epg.db (settings). It never
 *   creates or migrates tables; the program does that when the container starts.
 * - "Build now" starts "epg-loader build" in the background. The program
 *   locks build.lock itself, so two builds can never run at once.
 * - Paths are fixed by the Docker image (see epg_lib.php). The TMDB
 *   key is never sent back to the browser in full.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';


epg_boot();
$p = epg_paths();

function key_hint(string $key): string
{
    return $key === '' ? '' : '••••' . substr($key, -4);
}

function save_settings(PDO $pdo, array $kv): void
{
    $pdo->beginTransaction();
    $st = $pdo->prepare('INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value');
    foreach ($kv as $k => $v) {
        $st->execute([$k, (string)$v]);
    }
    $pdo->commit();
}

epg_require_login($p);
$authed = true;

// Download a backup (Settings › Backups). Only names from the list are accepted.
if (isset($_GET['backup'])) {
    $f = epg_backup_path($p, (string)$_GET['backup']);
    if ($f === '') {
        http_response_code(404);
        exit('Backup not found.');
    }
    header('Content-Type: application/gzip');
    header('Content-Disposition: attachment; filename="' . basename($f) . '"');
    header('Content-Length: ' . filesize($f));
    readfile($f);
    exit;
}
if ($_SERVER['REQUEST_METHOD'] === 'GET') {
    epg_backup_daily($p);
}

$fatal = null;
$pdo = null;
$s = [];
if ($authed) {
    try {
        $pdo = epg_db($p);
        foreach ($pdo->query('SELECT key, value FROM settings') as $r) {
            $s[$r['key']] = $r['value'];
        }
    } catch (Throwable $e) {
        $fatal = $e->getMessage();
    }
}

// ---------------------------------------------------------------- status JSON

if (($_GET['ajax'] ?? '') === 'status') {
    header('Content-Type: application/json');
    if (!$authed || $fatal !== null) {
        http_response_code(!$authed ? 403 : 500);
        echo json_encode(['error' => !$authed ? 'login required' : $fatal]);
        exit;
    }
    $runs = $pdo->query('SELECT id, started_at, finished_at, ok, duration_secs, channels, programmes, logos_missing,
                                posters_added, ratings_added, errors_total, error
                         FROM runs ORDER BY id DESC LIMIT 15')->fetchAll();
    $summary = $pdo->query('SELECT summary FROM runs WHERE finished_at IS NOT NULL AND ok = 1 ORDER BY id DESC LIMIT 1')->fetchColumn();
    $job = epg_job_status($p);
    $log = $job['name'] === 'guide-build' ? $job['log'] : epg_tail(epg_logs_dir($p) . '/guide-build.log');
    if ($log === '') {
        $log = epg_tail(epg_logs_dir($p) . '/cron.log');
    }
    echo json_encode([
        'running' => epg_build_running($p),
        'busy' => epg_anything_running($p),
        'task' => $job['running'] ? $job['label'] : '',
        'runs' => $runs,
        'last' => $summary ? json_decode((string)$summary, true) : null,
        'log' => $log,
    ], JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}

// ---------------------------------------------------------------- POST actions

if ($_SERVER['REQUEST_METHOD'] === 'POST' && $authed && $fatal === null) {
    if (!epg_csrf_ok()) {
        epg_flash('error', 'The form expired. Please try again.');
        epg_redirect_self();
    }
    $action = (string)($_POST['action'] ?? '');
    try {
        switch ($action) {
            case 'logout':
                $_SESSION = [];
                session_regenerate_id(true);
                epg_redirect_self();

            case 'save_settings':
                $errors = [];
                $kv = [];

                $key = trim((string)($_POST['tmdb_api_key'] ?? ''));
                if ($key !== '') {
                    if (!preg_match('/^[A-Za-z0-9._\-]{16,2048}$/', $key)) {
                        $errors[] = 'That does not look like a TMDB API key or read access token.';
                    }
                    $kv['tmdb_api_key'] = $key;
                } elseif (!empty($_POST['clear_key'])) {
                    $kv['tmdb_api_key'] = '';
                }

                $lang = trim((string)($_POST['language'] ?? ''));
                if (!preg_match('/^[a-z]{2}(-[A-Z]{2})?$/', $lang)) {
                    $errors[] = 'Language must look like en-US.';
                }
                $kv['language'] = $lang;

                $country = strtoupper(trim((string)($_POST['rating_country'] ?? '')));
                if (!preg_match('/^[A-Z]{2}$/', $country)) {
                    $errors[] = 'Rating country must be a two-letter code such as US.';
                }
                $kv['rating_country'] = $country;


                $hours = trim((string)($_POST['refresh_hours'] ?? ''));
                if (!is_numeric($hours) || (float)$hours < 0.5 || (float)$hours > 168) {
                    $errors[] = 'Refresh interval must be between 0.5 and 168 hours.';
                }
                $kv['refresh_hours'] = is_numeric($hours) ? (string)(float)$hours : '6';

                $kv['logo_source'] = !empty($_POST['logo_source']) ? 'iptv-org' : 'off';
                $kv['tmdb_background'] = !empty($_POST['tmdb_background']) ? '1' : '0';
                $kv['imdb_ratings'] = !empty($_POST['imdb_ratings']) ? '1' : '0';
                $kv['tvmaze'] = !empty($_POST['tvmaze']) ? '1' : '0';
                $kv['sportsdb'] = !empty($_POST['sportsdb']) ? '1' : '0';
                $sk = trim((string)($_POST['sportsdb_key'] ?? ''));
                if ($sk !== '' && !preg_match('/^[A-Za-z0-9]{1,64}$/', $sk)) {
                    $errors[] = 'The TheSportsDB key should be letters and digits only.';
                }
                $kv['sportsdb_key'] = $sk;
                $lc = strtolower(trim((string)($_POST['logo_country'] ?? 'us')));
                if (!preg_match('/^[a-z]{2}$/', $lc)) {
                    $errors[] = 'Logo country must be a two-letter code such as us or uk.';
                }
                $kv['logo_country'] = $lc;
                // The guide is written next to this page.
                $kv['output_dir'] = $p['web_dir'];

                if ($errors) {
                    foreach ($errors as $e) {
                        epg_flash('error', $e);
                    }
                    unset($kv['tmdb_api_key']);
                    $_SESSION['old'] = $kv;
                } else {
                    $kv['settings_saved_at'] = (string)time();
                    save_settings($pdo, $kv);
                    // Old test results no longer apply: test the new settings.
                    epg_queue_check($p);
                    epg_flash('ok', 'Settings saved. They apply from the next build. The TMDB key and source URL are being tested; see the setup page for the result.');
                }
                epg_redirect_self();

            case 'add_user':
            case 'set_password':
            case 'toggle_user':
            case 'delete_user':
                $me = epg_current_user($p);
                $users = epg_users($p);
                $name = trim((string)($_POST['username'] ?? ''));
                $idx = null;
                foreach ($users as $i => $u) {
                    if (strcasecmp($u['username'], $name) === 0) {
                        $idx = $i;
                    }
                }
                $activeAdmins = count(array_filter($users, fn($u) => !$u['disabled'] && $u['role'] === 'admin'));
                $err = '';
                $msg = '';
                if ($action === 'add_user') {
                    $err = epg_check_username($name);
                    if ($err === '' && $idx !== null) {
                        $err = 'There is already a user called ' . $users[$idx]['username'] . '.';
                    }
                    $err = $err ?: epg_check_password((string)($_POST['new_password'] ?? ''), (string)($_POST['confirm_password'] ?? ''));
                    if ($err === '') {
                        $users[] = ['username' => $name, 'hash' => password_hash((string)$_POST['new_password'], PASSWORD_DEFAULT),
                                    'role' => 'admin', 'disabled' => false, 'created' => time(), 'last_login' => 0];
                        $msg = 'User ' . $name . ' added.';
                    }
                } elseif ($idx === null) {
                    $err = 'That user no longer exists.';
                } elseif ($action === 'set_password') {
                    $err = epg_check_password((string)($_POST['new_password'] ?? ''), (string)($_POST['confirm_password'] ?? ''));
                    if ($err === '') {
                        $users[$idx]['hash'] = password_hash((string)$_POST['new_password'], PASSWORD_DEFAULT);
                        $msg = 'Password changed for ' . $users[$idx]['username'] . '.';
                    }
                } elseif (strcasecmp($users[$idx]['username'], $me) === 0) {
                    $err = 'You cannot ' . ($action === 'delete_user' ? 'delete' : 'disable') . ' yourself.';
                } elseif ($action === 'toggle_user') {
                    if (!$users[$idx]['disabled'] && $activeAdmins <= 1) {
                        $err = 'This is the last active admin.';
                    } else {
                        $users[$idx]['disabled'] = !$users[$idx]['disabled'];
                        $msg = $users[$idx]['username'] . ($users[$idx]['disabled'] ? ' is disabled and cannot log in.' : ' is enabled again.');
                    }
                } else {
                    if (!$users[$idx]['disabled'] && $activeAdmins <= 1) {
                        $err = 'This is the last active admin.';
                    } else {
                        $msg = 'User ' . $users[$idx]['username'] . ' deleted.';
                        array_splice($users, $idx, 1);
                    }
                }
                if ($err === '' && !epg_save_users($p, $users)) {
                    $err = 'Could not save the users: the data folder is not writable.';
                }
                if ($err !== '') {
                    epg_flash('error', $err);
                } else {
                    // Changing your own password keeps you logged in; other sessions of that user end.
                    if ($action === 'set_password' && strcasecmp($name, $me) === 0) {
                        $u = epg_find_user($p, $me);
                        if ($u !== null) {
                            epg_login_as($u);
                        }
                    }
                    epg_flash('ok', $msg);
                }
                header('Location: settings.php#users', true, 303);
                exit;

            case 'backup_create':
            case 'backup_restore':
            case 'backup_delete':
            case 'backup_upload':
                $bn = (string)($_POST['name'] ?? '');
                if ($action === 'backup_create') {
                    [$made, $err] = epg_backup_create($p);
                    $err === '' ? epg_flash('ok', 'Backup made: ' . $made) : epg_flash('error', $err);
                } elseif ($action === 'backup_delete') {
                    $f = epg_backup_path($p, $bn);
                    $f !== '' && ($f) ? epg_flash('ok', 'Backup deleted.') : epg_flash('error', 'That backup could not be deleted.');
                } else {
                    $gz = '';
                    if ($action === 'backup_restore') {
                        $f = epg_backup_path($p, $bn);
                        $gz = $f !== '' ? (string)file_get_contents($f) : '';
                    } elseif (isset($_FILES['file']) && ($_FILES['file']['error'] ?? 1) === UPLOAD_ERR_OK && is_uploaded_file((string)$_FILES['file']['tmp_name'])) {
                        $gz = (string)file_get_contents((string)$_FILES['file']['tmp_name']);
                    }
                    $err = $gz === '' ? 'Choose a backup to restore.' : epg_backup_restore($p, $gz);
                    $err === ''
                        ? epg_flash('ok', 'Backup restored. The state before it was saved as a backup too. Rebuild the guides to use it.')
                        : epg_flash('error', $err);
                }
                header('Location: settings.php#backups', true, 303);
                exit;

            case 'cancel':
                $err = epg_cancel($p);
                $err === '' ? epg_flash('ok', 'Cancelled.') : epg_flash('error', $err);
                epg_redirect_self();

            case 'build':
                $err = epg_start_build($p);
                $err === '' ? epg_flash('ok', 'Build started.') : epg_flash('error', $err);
                epg_redirect_self();

            default:
                epg_flash('error', 'Unknown action.');
                epg_redirect_self();
        }
    } catch (Throwable $e) {
        if ($pdo instanceof PDO && $pdo->inTransaction()) {
            $pdo->rollBack();
        }
        epg_flash('error', 'Error: ' . $e->getMessage());
        epg_redirect_self();
    }
}

// ---------------------------------------------------------------- page data

$flashes = epg_take_flashes();
$old = $_SESSION['old'] ?? [];
unset($_SESSION['old']);
$v = fn(string $k, string $default = '') => (string)($old[$k] ?? ($s[$k] ?? $default));

$csrf = epg_csrf_field();
$savedKey = (string)($s['tmdb_api_key'] ?? '');
epg_layout_start($p, 'settings', 'Settings', 'Options shared by all your guides', $flashes, '');
?>
<?php if (!$authed): ?>
    <?= epg_auth_form($p) ?>

<?php elseif ($fatal !== null): ?>
    <div class="card">
      <h2>Not set up yet</h2>
      <p class="bad-text"><?= h($fatal) ?></p>
      <p>Check that the container is running and that the /data volume is mounted and writable.</p>
    </div>

<?php else: ?>
    <form method="post" id="user-add" autocomplete="off"><?= $csrf ?><input type="hidden" name="action" value="add_user"></form>
    <form method="post" id="user-setpw" autocomplete="off"><?= $csrf ?><input type="hidden" name="action" value="set_password"></form>
    <form method="post" id="user-toggle"><?= $csrf ?><input type="hidden" name="action" value="toggle_user"></form>
    <form method="post" id="user-delete"><?= $csrf ?><input type="hidden" name="action" value="delete_user"></form>
    <form method="post" id="bk-create"><?= $csrf ?><input type="hidden" name="action" value="backup_create"></form>
    <form method="post" id="bk-restore" onsubmit="return confirm('Restore this backup? Your current settings, guides, users and custom logos are backed up first, then replaced.')"><?= $csrf ?><input type="hidden" name="action" value="backup_restore"></form>
    <form method="post" id="bk-delete" onsubmit="return confirm('Delete this backup?')"><?= $csrf ?><input type="hidden" name="action" value="backup_delete"></form>
    <form method="post" id="bk-upload" enctype="multipart/form-data" onsubmit="return confirm('Restore from this file? Your current state is backed up first.')"><?= $csrf ?><input type="hidden" name="action" value="backup_upload"></form>

    <form method="post" autocomplete="off" id="settings-form" class="set-layout">
      <?= $csrf ?>
      <input type="hidden" name="action" value="save_settings">

      <nav class="set-tabs" aria-label="Settings sections">
        <a href="#artwork" data-tab="artwork"><?= epg_icon('grid') ?>Artwork &amp; info</a>
        <a href="#logos" data-tab="logos"><?= epg_icon('tv') ?>Channel logos</a>
        <a href="#schedule" data-tab="schedule"><?= epg_icon('history') ?>Schedule</a>
        <a href="#users" data-tab="users"><?= epg_icon('user') ?>Users</a>
        <a href="#backups" data-tab="backups"><?= epg_icon('history') ?>Backups</a>
        <a href="#advanced" data-tab="advanced"><?= epg_icon('settings') ?>Advanced</a>
      </nav>

      <div class="set-panels">
        <p class="hint set-pointer">XMLTV sources, days to keep and guide addresses are on the <a href="guides.php">Guides</a> page.</p>
        <section class="card set-panel" id="artwork">
          <h2>Artwork &amp; info</h2>
          <label for="tmdb_api_key">TMDB API key or read access token</label>
          <input type="password" id="tmdb_api_key" name="tmdb_api_key" value="" autocomplete="new-password"
                 placeholder="<?= $savedKey !== '' ? h('Saved key ' . key_hint($savedKey) . ' · leave blank to keep it') : 'Paste a v3 API key or v4 read access token' ?>">
          <?php if ($savedKey !== ''): ?>
            <label class="check"><input type="checkbox" name="clear_key" value="1"> Remove the saved key (turns TMDB lookups off)</label>
          <?php else: ?>
            <p class="hint">Free at themoviedb.org → Settings → API. Without it, no posters or ratings are added.</p>
          <?php endif; ?>
          <div class="row">
            <div>
              <label for="language">Language</label>
              <input type="text" id="language" name="language" value="<?= h($v('language', 'en-US')) ?>" pattern="[a-z]{2}(-[A-Z]{2})?" required>
              <p class="hint">For titles and descriptions, e.g. en-US.</p>
            </div>
            <div>
              <label for="rating_country">Rating country</label>
              <input type="text" id="rating_country" name="rating_country" value="<?= h($v('rating_country', 'US')) ?>" maxlength="2" required>
              <p class="hint">Whose age ratings to use, e.g. US.</p>
            </div>
          </div>
          <label class="check">
            <input type="checkbox" name="tmdb_background" value="1" <?= $v('tmdb_background', '1') !== '0' ? 'checked' : '' ?>>
            Keep looking up titles in the background until all are done
          </label>
          <p class="hint">Watch progress, Start or Pause on the <a href="activity.php">Activity</a> page.</p>
          <label class="check">
            <input type="checkbox" name="imdb_ratings" value="1" <?= $v('imdb_ratings', '1') !== '0' ? 'checked' : '' ?>>
            Add IMDb star ratings (e.g. 7.8/10) to matched programmes
          </label>
          <p class="hint">IMDb data is for personal, non-commercial use.</p>
          <label class="check">
            <input type="checkbox" name="tvmaze" value="1" <?= $v('tvmaze', '1') !== '0' ? 'checked' : '' ?>>
            Use TVmaze for series TMDB has no match or poster for
          </label>
          <p class="hint">Free, no key needed.</p>
          <label class="check">
            <input type="checkbox" name="sportsdb" value="1" <?= $v('sportsdb', '1') !== '0' ? 'checked' : '' ?>>
            Use TheSportsDB for sports matches without a poster
          </label>
          <label for="sportsdb_key">TheSportsDB API key</label>
          <input type="text" id="sportsdb_key" name="sportsdb_key" value="<?= h($v('sportsdb_key')) ?>" placeholder="3 (free test key)" class="short" autocomplete="off">
          <p class="hint">Leave empty to use the free key; enter your own for higher limits.</p>
        </section>

        <section class="card set-panel" id="logos">
          <h2>Channel logos</h2>
          <p class="hint" style="margin-top:0">Logos already in your source are always kept.</p>
          <label class="check">
            <input type="checkbox" name="logo_source" value="1" <?= $v('logo_source', 'iptv-org') !== 'off' ? 'checked' : '' ?>>
            Fill in missing logos from tv-logos, then iptv-org
          </label>
          <label for="logo_country">Default logo country</label>
          <input type="text" id="logo_country" name="logo_country" value="<?= h($v('logo_country', 'us')) ?>" maxlength="2" pattern="[A-Za-z]{2}" required class="short">
          <p class="hint">Used when a channel id has no country, e.g. us or uk.</p>
        </section>

        <section class="card set-panel" id="schedule">
          <h2>Schedule</h2>
          <label for="refresh_hours">Rebuild every (hours)</label>
          <input type="number" id="refresh_hours" name="refresh_hours" min="0.5" max="168" step="0.5" value="<?= h($v('refresh_hours', '6')) ?>" required class="short">
          <p class="hint">Default for every guide; a guide can set its own on the <a href="guides.php">Guides</a> page.</p>
        </section>

        <section class="card set-panel" id="users">
          <h2>Users</h2>
          <?php $me = epg_current_user($p); ?>
          <table class="compact users-table">
            <thead><tr><th>Username</th><th>Role</th><th>Last login</th><th>Status</th><th></th></tr></thead>
            <tbody>
            <?php foreach (epg_users($p) as $u): $self = strcasecmp($u['username'], $me) === 0; ?>
              <tr>
                <td><strong><?= h($u['username']) ?></strong><?= $self ? ' <span class="muted">(you)</span>' : '' ?></td>
                <td><?= h($u['role']) ?></td>
                <td><?= $u['last_login'] ? h(date('D j M Y, H:i', $u['last_login'])) : '<span class="muted">never</span>' ?></td>
                <td><?= $u['disabled'] ? '<span class="warn-text">disabled</span>' : '<span class="ok-text">active</span>' ?></td>
                <td class="user-actions">
                  <button type="button" class="ghost" data-pw="<?= h($u['username']) ?>">Change password</button>
                  <?php if (!$self): ?>
                  <button class="ghost" form="user-toggle" name="username" value="<?= h($u['username']) ?>"><?= $u['disabled'] ? 'Enable' : 'Disable' ?></button>
                  <button class="ghost danger-text" form="user-delete" name="username" value="<?= h($u['username']) ?>" onclick="return confirm(<?= h(json_encode('Delete the user ' . $u['username'] . '?')) ?>)">Delete</button>
                  <?php endif; ?>
                </td>
              </tr>
            <?php endforeach; ?>
            </tbody>
          </table>

          <div class="user-pw" id="user-pw" hidden>
            <h3 class="adv-h">Change password for <span id="user-pw-name"></span></h3>
            <input form="user-setpw" type="hidden" name="username" id="user-pw-user">
            <div class="row">
              <div>
                <label for="upw1">New password</label>
                <input form="user-setpw" type="password" id="upw1" name="new_password" autocomplete="new-password" minlength="8">
                <p class="hint">At least 8 characters.</p>
              </div>
              <div>
                <label for="upw2">Repeat it</label>
                <input form="user-setpw" type="password" id="upw2" name="confirm_password" autocomplete="new-password">
              </div>
            </div>
            <div class="form-actions"><button form="user-setpw">Save password</button><button type="button" class="ghost" id="user-pw-cancel">Cancel</button></div>
          </div>

          <h3 class="adv-h">Add a user</h3>
          <div class="row">
            <div>
              <label for="nu-name">Username</label>
              <input form="user-add" type="text" id="nu-name" name="username" maxlength="32" autocomplete="off">
              <p class="hint">Letters, digits, dots, dashes or underscores.</p>
            </div>
            <div>
              <label for="nu-pw1">Password</label>
              <input form="user-add" type="password" id="nu-pw1" name="new_password" autocomplete="new-password" minlength="8">
              <p class="hint">At least 8 characters.</p>
            </div>
            <div>
              <label for="nu-pw2">Repeat it</label>
              <input form="user-add" type="password" id="nu-pw2" name="confirm_password" autocomplete="new-password">
            </div>
          </div>
          <button form="user-add">Add user</button>
        </section>

        <section class="card set-panel" id="backups">
          <h2>Backups</h2>
          <p class="hint" style="margin-top:0">Settings, guides, channel overrides, users and custom logos. One is made every day automatically and before every restore; the newest <?= EPG_BACKUP_KEEP ?> are kept in the data folder.</p>
          <div class="form-actions" style="margin-top:0"><button form="bk-create">Back up now</button>
            <label class="button secondary bk-upload">Restore from a file…<input form="bk-upload" type="file" name="file" accept=".gz,application/gzip" onchange="if (this.files.length) this.form.requestSubmit()"></label></div>
          <?php $bks = epg_backups($p); ?>
          <?php if (!$bks): ?>
            <p class="muted">No backups yet.</p>
          <?php else: ?>
          <table class="compact bk-table">
            <thead><tr><th>When</th><th>Made</th><th>Contains</th><th class="num">Size</th><th></th></tr></thead>
            <tbody>
            <?php foreach ($bks as $b): ?>
              <tr>
                <td><?= h(date('D j M Y, H:i', $b['time'])) ?></td>
                <td><?= h($b['reason'] === 'before-restore' ? 'before a restore' : $b['reason']) ?></td>
                <td class="muted"><?= h(epg_backup_summary($p, $b['name'])) ?></td>
                <td class="num"><?= h(number_format($b['size'] / 1024, 0) . ' KB') ?></td>
                <td class="user-actions">
                  <a class="button ghost" href="settings.php?backup=<?= h(rawurlencode($b['name'])) ?>">Download</a>
                  <button class="ghost" form="bk-restore" name="name" value="<?= h($b['name']) ?>">Restore</button>
                  <button class="ghost danger-text" form="bk-delete" name="name" value="<?= h($b['name']) ?>">Delete</button>
                </td>
              </tr>
            <?php endforeach; ?>
            </tbody>
          </table>
          <?php endif; ?>
        </section>

        <section class="card set-panel" id="advanced">
          <h2>Advanced</h2>
          <?php
          $checks = epg_system_checks($p);
          $bad = count(array_filter($checks, fn($c) => $c[1] === 'fail'));
          $info = epg_program_info($p);
          ?>
          <h3 class="adv-h">Program</h3>
          <dl>
            <dt>Version</dt><dd>EPGArr <?= h((string)($info['version'] ?? '?')) ?> (Docker image)</dd>
            <dt>Database schema</dt><dd><?= h((string)($info['schema_version'] ?? '?')) ?></dd>
          </dl>
          <p class="hint">To update, pull or build the new image and recreate the container; your data in /data stays.</p>

          <details class="adv-checks" id="adv-checks"<?= $bad ? ' open' : '' ?>>
            <summary>System checks · <span class="<?= $bad ? 'bad-text' : 'ok-text' ?>"><?= $bad ? h($bad . ' problem' . ($bad > 1 ? 's' : '')) : 'Running normally' ?></span></summary>
            <ul class="adv-check-list">
            <?php foreach ($checks as [$title, $status, $detail]): ?>
              <li><span class="dot <?= h($status) ?>"></span><strong><?= h($title) ?></strong> <span class="muted"><?= h($detail) ?></span></li>
            <?php endforeach; ?>
            </ul>
          </details>

          <h3 class="adv-h">Folders in the container</h3>
          <dl>
            <dt>Data volume</dt><dd><code><?= h($p['data_dir'] !== '' ? $p['data_dir'] : '/data (not mounted)') ?></code></dd>
            <dt>Guide files</dt><dd><code><?= h(($p['data_dir'] !== '' ? $p['data_dir'] : '/data') . '/guides/') ?></code></dd>
            <dt>Program</dt><dd><code><?= h($p['bin'] !== '' ? $p['bin'] : 'missing') ?></code></dd>
          </dl>
        </section>
      </div>

      <div class="save-bar" id="save-bar" hidden>
        <span>You have unsaved changes.</span>
        <button type="button" class="ghost" id="discard">Discard</button>
        <button type="submit">Save settings</button>
      </div>
    </form>

    <script>
    (function () {
      var form = document.getElementById('settings-form');
      var tabs = form.querySelectorAll('.set-tabs a');
      var panels = form.querySelectorAll('.set-panel');
      function show(name) {
        var found = false;
        panels.forEach(function (p) { var on = p.id === name; p.hidden = !on; found = found || on; });
        if (!found) { return show('artwork'); }
        tabs.forEach(function (t) { t.classList.toggle('active', t.dataset.tab === name); t.setAttribute('aria-current', t.dataset.tab === name ? 'page' : 'false'); });
        try { localStorage.setItem('epg-settings-tab', name); } catch (e) {}
      }
      tabs.forEach(function (t) {
        t.addEventListener('click', function (e) { e.preventDefault(); show(t.dataset.tab); history.replaceState(null, '', '#' + t.dataset.tab); });
      });
      var start = location.hash.slice(1);
      if (!start) { try { start = localStorage.getItem('epg-settings-tab') || ''; } catch (e) {} }
      show(start || 'artwork');

      // The Save bar appears once a setting changes.
      var bar = document.getElementById('save-bar');
      var dirty = function (e) { if (e.target.form === form) { bar.hidden = false; } };
      form.addEventListener('input', dirty);
      form.addEventListener('change', dirty);
      document.getElementById('discard').addEventListener('click', function () { form.reset(); bar.hidden = true; });
      form.addEventListener('submit', function () { bar.hidden = true; });

      // Users: "Change password" opens the small form for that user.
      document.querySelectorAll('[data-pw]').forEach(function (b) {
        b.addEventListener('click', function () {
          var box = document.getElementById('user-pw');
          document.getElementById('user-pw-name').textContent = b.dataset.pw;
          document.getElementById('user-pw-user').value = b.dataset.pw;
          box.hidden = false;
          document.getElementById('upw1').focus();
        });
      });
      document.getElementById('user-pw-cancel').addEventListener('click', function () { document.getElementById('user-pw').hidden = true; });

    })();
    </script>
<?php endif; ?>
<?php epg_layout_end();
