<?php
/*
 * EPGArr setup: checks everything and does the setup from the browser.
 *
 * Nothing needs root. Everything runs as the PHP user. When a check can only
 * be fixed by an administrator, the page shows the exact command in an
 * orange box instead of trying sudo.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
$p = epg_paths();
epg_require_login($p);

// Setup is for the first-time setup. Once it was completed it is only reachable through
// "Re-run setup" (Settings › Advanced), which passes ?rerun=1. Its JSON and actions stay
// available because Settings › Advanced uses them.
if (isset($_GET['rerun'])) {
    $_SESSION['setup_rerun'] = true;
}
if ($_SERVER['REQUEST_METHOD'] === 'GET' && !isset($_GET['ajax']) && empty($_SESSION['setup_rerun']) && epg_setup_done($p) && !epg_program_outdated($p)) {
    header('Location: index.php', true, 302);
    exit;
}

// ---------------------------------------------------------------- helpers

function page_start(string $title, array $flashes): void
{
    global $p;
    epg_layout_start($p, 'setup', 'Setup', 'Checks everything and finishes the setup for you', $flashes,
        '');
}

function page_end(): void
{
    epg_layout_end();
}

function fpm_group(): string
{
    if (function_exists('posix_getegid') && function_exists('posix_getgrgid')) {
        $g = posix_getgrgid(posix_getegid());
        if (is_array($g)) {
            return (string)$g['name'];
        }
    }
    return epg_fpm_user()['name'];
}

/** Ask our own web server for a URL path; returns [status code or 0, detail]. */
function self_http_status(string $path): array
{
    $addr = (string)($_SERVER['SERVER_ADDR'] ?? '127.0.0.1');
    $port = (int)($_SERVER['SERVER_PORT'] ?? 80);
    $tls = !empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off';
    $target = ($tls ? 'tls://' : 'tcp://') . (str_contains($addr, ':') ? '[' . $addr . ']' : $addr) . ':' . $port;
    $ctx = stream_context_create(['ssl' => ['verify_peer' => false, 'verify_peer_name' => false, 'peer_name' => epg_host()]]);
    $errno = 0;
    $errstr = '';
    $fp = @stream_socket_client($target, $errno, $errstr, 5, STREAM_CLIENT_CONNECT, $ctx);
    if (!$fp) {
        return [0, 'could not connect to the web server: ' . $errstr];
    }
    stream_set_timeout($fp, 5);
    fwrite($fp, "GET $path HTTP/1.0\r\nHost: " . epg_host() . "\r\nUser-Agent: epg-loader-setup\r\nConnection: close\r\n\r\n");
    $line = (string)fgets($fp);
    fclose($fp);
    if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m)) {
        return [(int)$m[1], trim($line)];
    }
    return [0, 'no answer from the web server'];
}

/** URL path of a folder inside the web folder, or "" if it is outside. */
function url_path_of(array $p, string $dir): string
{
    if ($dir === '' || !epg_inside($dir, $p['web_dir'])) {
        return '';
    }
    $rel = trim(substr(epg_norm($dir), strlen(epg_norm($p['web_dir']))), '/');
    $segs = array_map('rawurlencode', $rel === '' ? [] : explode('/', $rel));
    return $p['base_path'] . '/' . implode('/', $segs);
}

/** Files under $dir that PHP cannot read (skips target/ and .git/). */
function unreadable_files(string $dir, int $max = 10): array
{
    $bad = [];
    $total = 0;
    if (!is_dir($dir) || !is_readable($dir) || !is_executable($dir)) {
        return [[$dir], 1];
    }
    $it = new RecursiveIteratorIterator(
        new RecursiveCallbackFilterIterator(
            new RecursiveDirectoryIterator($dir, FilesystemIterator::SKIP_DOTS),
            function ($f) use (&$bad, &$total, $max) {
                $name = $f->getFilename();
                if ($f->isDir() && in_array($name, ['target', '.git'], true)) {
                    return false;
                }
                if ($f->isDir() && (!$f->isReadable() || !$f->isExecutable())) {
                    $total++;
                    if (count($bad) < $max) {
                        $bad[] = $f->getPathname() . '/';
                    }
                    return false;
                }
                return true;
            }
        ),
        RecursiveIteratorIterator::SELF_FIRST,
        RecursiveIteratorIterator::CATCH_GET_CHILD
    );
    foreach ($it as $f) {
        if ($f->isFile() && !$f->isReadable()) {
            $total++;
            if (count($bad) < $max) {
                $bad[] = $f->getPathname();
            }
        }
    }
    return [$bad, $total];
}

function newest_source_mtime(string $src): int
{
    $newest = 0;
    foreach (['Cargo.toml', 'Cargo.lock', 'rules.toml'] as $f) {
        if (is_file("$src/$f")) {
            $newest = max($newest, (int)filemtime("$src/$f"));
        }
    }
    if (is_dir("$src/src")) {
        foreach (new RecursiveIteratorIterator(new RecursiveDirectoryIterator("$src/src", FilesystemIterator::SKIP_DOTS)) as $f) {
            if ($f->isFile()) {
                $newest = max($newest, (int)$f->getMTime());
            }
        }
    }
    return $newest;
}

/** Copy the source tree (without target/ and .git/) so it can be built when the original is read-only. */
function copy_tree(string $from, string $to): void
{
    if (!is_dir($to) && !mkdir($to, 02775, true)) {
        throw new RuntimeException("cannot create $to");
    }
    foreach (scandir($from) ?: [] as $name) {
        if ($name === '.' || $name === '..' || $name === 'target' || $name === '.git') {
            continue;
        }
        $a = "$from/$name";
        $b = "$to/$name";
        if (is_dir($a)) {
            copy_tree($a, $b);
        } elseif (is_file($a)) {
            if (!copy($a, $b)) {
                throw new RuntimeException("cannot copy $a");
            }
        }
    }
}

function rustc_version(array $cargo): array
{
    if ($cargo['cargo'] === '') {
        return ['', ''];
    }
    $dir = dirname($cargo['cargo']);
    [, $c] = epg_run(escapeshellarg($cargo['cargo']) . ' --version', $cargo['env']);
    $rustc = is_file("$dir/rustc") ? "$dir/rustc" : 'rustc';
    [, $r] = epg_run(escapeshellarg($rustc) . ' --version', $cargo['env']);
    return [$c, $r];
}

function bin_info(array $p): ?array
{
    if ($p['bin'] === '') {
        return null;
    }
    [$rc, $out] = epg_run(escapeshellarg($p['bin']) . ' info');
    $j = $rc === 0 ? json_decode($out, true) : null;
    return is_array($j) ? $j : null;
}

function db_state(array $p): array
{
    try {
        $db = epg_db($p);
        $v = (int)$db->query('SELECT COALESCE(MAX(version), 0) FROM schema_version')->fetchColumn();
        $s = [];
        foreach ($db->query('SELECT key, value FROM settings') as $r) {
            $s[$r['key']] = $r['value'];
        }
        $run = $db->query('SELECT started_at, finished_at, ok, error, programmes FROM runs ORDER BY id DESC LIMIT 1')->fetch();
        $okRun = $db->query('SELECT finished_at FROM runs WHERE ok = 1 ORDER BY id DESC LIMIT 1')->fetchColumn();
        return ['ok' => true, 'version' => $v, 'settings' => $s, 'run' => $run ?: null, 'last_ok' => $okRun ? (int)$okRun : 0, 'error' => ''];
    } catch (Throwable $e) {
        return ['ok' => false, 'version' => 0, 'settings' => [], 'run' => null, 'last_ok' => 0, 'error' => $e->getMessage()];
    }
}

function when(int $t): string
{
    return $t > 0 ? date('Y-m-d H:i T', $t) : 'never';
}

// ---------------------------------------------------------------- actions

epg_run_pending_check($p);
$job = epg_job_status($p);

if (($_GET['ajax'] ?? '') === 'job') {
    header('Content-Type: application/json');
    echo json_encode($job, JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}

if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    if (!epg_csrf_ok()) {
        epg_flash('error', 'The form expired. Please try again.');
        epg_redirect_self();
    }
    $action = (string)($_POST['action'] ?? '');
    $err = '';
    $okMsg = '';
    try {
        switch ($action) {
            case 'unlock':
                $_SESSION['setup_unlocked'] = true;
                break;
            case 'lock':
                unset($_SESSION['setup_unlocked']);
                unset($_SESSION['setup_rerun']);
                break;

            case 'save_paths':
                $saved = $p['saved'];
                $d = trim((string)($_POST['data_dir'] ?? ''));
                $s = trim((string)($_POST['src_dir'] ?? ''));
                $b = trim((string)($_POST['bin'] ?? ''));
                foreach ([$d, $s, $b] as $x) {
                    if ($x !== '' && (!str_starts_with($x, '/') || preg_match('/[\x00-\x1f]/', $x) || str_contains($x, '/../'))) {
                        $err = 'Paths must be absolute (start with /) and must not contain "..".';
                    }
                }
                if ($err === '' && $d !== '' && epg_is_web_path($d)) {
                    $err = 'The data folder must be outside the web root: it holds the TMDB key.';
                }
                if ($err === '' && $s !== '' && !is_file($s . '/Cargo.toml')) {
                    $err = 'There is no Cargo.toml in ' . $s . '.';
                }
                if ($err === '' && $b !== '' && !(is_file($b) && is_executable($b))) {
                    $err = $b . ' is not an executable file.';
                }
                if ($err === '') {
                    foreach (['data_dir' => $d, 'src_dir' => $s, 'bin' => $b] as $k => $val) {
                        if ($val === '') {
                            unset($saved[$k]);
                        } else {
                            $saved[$k] = epg_norm($val);
                        }
                    }
                    if (!epg_save_paths($saved)) {
                        $err = 'Could not write ' . EPG_PATHS_FILE . ': the web folder is not writable by PHP.';
                    } else {
                        $okMsg = 'Paths saved.';
                    }
                }
                break;

            case 'redetect':
                $saved = array_intersect_key($p['saved'], ['admin_hash' => 1]);
                $err = epg_save_paths($saved) ? '' : 'Could not write ' . EPG_PATHS_FILE . '.';
                $okMsg = 'Saved paths cleared; everything is detected again.';
                break;

            case 'create_data_dir':
                $d = $p['data_dir'];
                if ($d === '') {
                    $err = 'No usable data folder was found. Enter one under "Detected paths".';
                } elseif (!is_dir($d) && !@mkdir($d, 02770, true)) {
                    $err = 'Could not create ' . $d . '.';
                } else {
                    @mkdir($d . '/logs', 02770, true);
                    $okMsg = 'Data folder ready: ' . $d;
                    // Remember it, so a later detection change does not move it.
                    $saved = $p['saved'];
                    $saved['data_dir'] = $d;
                    epg_save_paths($saved);
                }
                break;

            case 'install_rust':
                if (epg_which('curl') === '') {
                    $err = 'curl is not installed, so Rust cannot be downloaded.';
                    break;
                }
                $env = epg_rust_install_env($p);
                $cmd = epg_env_prefix($env) . "sh -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path'";
                $err = epg_start_job($p, 'install-rust', 'Install Rust', $cmd);
                break;

            case 'update_rust':
                $rustup = $p['cargo']['cargo'] !== '' ? dirname($p['cargo']['cargo']) . '/rustup' : '';
                if ($rustup === '' || !is_executable($rustup)) {
                    $err = 'rustup was not found next to cargo.';
                    break;
                }
                $err = epg_start_job($p, 'update-rust', 'Update Rust', epg_env_prefix($p['cargo']['env']) . escapeshellarg($rustup) . ' update stable');
                break;

            case 'build':
            case 'test':
                if ($p['cargo']['cargo'] === '') {
                    $err = 'Rust (cargo) was not found. Use "Install Rust" first.';
                    break;
                }
                if ($p['src_dir'] === '') {
                    $err = 'The source folder was not found.';
                    break;
                }
                $src = $p['src_dir'];
                $lockOk = !is_file("$src/Cargo.lock") || is_writable("$src/Cargo.lock");
                if (!is_writable($src) || !$lockOk) {
                    // Build a copy in the data dir when the source tree is read-only for PHP.
                    $copy = $p['data_dir'] . '/src';
                    copy_tree($src, $copy);
                    $src = $copy;
                }
                @mkdir($p['data_dir'] . '/cargo-home', 02770, true);
                $sub = $action === 'build' ? 'build --release' : 'test --release';
                $cmd = epg_env_prefix($p['cargo']['env']) . escapeshellarg($p['cargo']['cargo']) . ' ' . $sub;
                $err = epg_start_job($p, $action === 'build' ? 'build' : 'tests', $action === 'build' ? 'Build the program' : 'Run tests', $cmd, $src);
                break;

            case 'migrate':
                if ($p['bin'] === '') {
                    $err = 'Build the program first.';
                    break;
                }
                $err = epg_start_job($p, 'migrate', 'Initialise or upgrade database', epg_env_prefix(epg_bin_env($p)) . escapeshellarg($p['bin']) . ' migrate');
                break;

            case 'check':
                $err = epg_start_check($p);
                break;

            case 'cancel':
                $err = epg_cancel($p);
                $okMsg = 'Cancelled.';
                break;

            case 'first_build':
                $err = epg_start_build($p);
                break;

            case 'install_schedule':
                $err = epg_cron_install($p);
                $okMsg = 'Schedule installed: the guide is rebuilt every refresh interval (checked every 15 minutes).';
                break;

            case 'remove_schedule':
                $err = epg_cron_remove();
                $okMsg = 'Schedule removed.';
                break;

            default:
                $err = 'Unknown action.';
        }
    } catch (Throwable $e) {
        $err = 'Error: ' . $e->getMessage();
    }
    if ($err !== '') {
        epg_flash('error', $err);
    } elseif ($okMsg !== '') {
        epg_flash('ok', $okMsg);
    }
    if (($_POST['back'] ?? '') === 'settings') {
        header('Location: settings.php#advanced', true, 303);
        exit;
    }
    epg_redirect_self();
}

// ---------------------------------------------------------------- checks

$checks = [];
$add = function (string $group, string $title, string $status, string $detail, array $extra = []) use (&$checks) {
    $checks[] = ['group' => $group, 'title' => $title, 'status' => $status, 'detail' => $detail] + $extra;
};

$user = $p['user'];
$group = fpm_group();
$phpMm = PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION;
$canExec = epg_can_exec();

// PHP
$add('PHP', 'PHP version', PHP_VERSION_ID >= 80100 ? 'ok' : 'fail', 'PHP ' . PHP_VERSION . ' (' . PHP_SAPI . ')',
    PHP_VERSION_ID >= 80100 ? [] : ['admin' => "# PHP 8.1 or newer is needed: install a newer php-fpm package and point the web server at it"]);
$hasSqlite = class_exists('PDO') && in_array('sqlite', PDO::getAvailableDrivers(), true);
$add('PHP', 'SQLite driver for PHP (pdo_sqlite)', $hasSqlite ? 'ok' : 'fail',
    $hasSqlite ? 'available' : 'missing in the PHP that runs this page',
    $hasSqlite ? [] : ['admin' => "sudo apt install php{$phpMm}-sqlite3 && sudo systemctl restart php{$phpMm}-fpm"]);
$off = array_intersect(epg_disabled_functions(), ['exec', 'proc_open']);
$add('PHP', 'PHP may run programs (exec, proc_open)', $canExec ? 'ok' : 'fail',
    $canExec ? 'allowed' : 'disabled by disable_functions: ' . implode(', ', $off),
    $canExec ? [] : ['admin' => 'Remove ' . implode(', ', $off) . ' from disable_functions in ' . (php_ini_loaded_file() ?: 'php.ini') . ", then: sudo systemctl restart php{$phpMm}-fpm"]);
$add('PHP', 'Image support for channel logos', function_exists('imagecreatefromstring') ? 'ok' : 'warn',
    function_exists('imagecreatefromstring')
        ? 'GD is available: uploaded logos are stored as PNG, at most 512 px'
        : 'GD is not installed: uploaded logos are checked but stored as they are (install php-gd to resize them)');
$add('PHP', 'PHP runs as', $user['known'] ? 'info' : 'warn',
    $user['name'] . ($user['uid'] >= 0 ? ' (uid ' . $user['uid'] . ')' : '') . ', group ' . $group . ', home ' . ($user['home'] ?: 'unknown')
    . ($user['known'] ? '' : ' (the posix extension is missing, so this is a guess)'));

// Tools
$tools = [];
foreach (['sh', 'flock', 'crontab', 'curl', 'timeout'] as $t) {
    $tools[$t] = epg_which($t);
}
$missingTools = array_keys(array_filter($tools, fn($v) => $v === ''));
$add('Build tools', 'Helper programs (sh, flock, crontab, curl, timeout)', $missingTools ? 'warn' : 'ok',
    $missingTools ? 'missing: ' . implode(', ', $missingTools) . '. crontab is needed for the schedule, curl to install Rust.' : 'all present',
    $missingTools ? ['admin' => 'sudo apt install ' . implode(' ', array_unique(array_map(
        fn($t) => ['flock' => 'util-linux', 'crontab' => 'cron', 'curl' => 'curl', 'timeout' => 'coreutils', 'sh' => 'dash'][$t], $missingTools)))] : []);
$cc = epg_which('cc');
$add('Build tools', 'C compiler and linker (cc)', $cc !== '' ? 'ok' : 'fail',
    $cc !== '' ? $cc : 'not found. Rust needs it to link the program.',
    $cc !== '' ? [] : ['admin' => 'sudo apt install build-essential pkg-config']);
[$cargoV, $rustcV] = rustc_version($p['cargo']);
$rustOk = false;
if ($p['cargo']['cargo'] !== '') {
    $rustOk = preg_match('/rustc (\d+)\.(\d+)/', $rustcV, $m) && ((int)$m[1] > 1 || (int)$m[2] >= 89);
    $add('Build tools', 'Rust (cargo, rustc)', $rustOk ? 'ok' : 'warn',
        ($cargoV ?: 'cargo found') . ' · ' . ($rustcV ?: 'rustc version unknown') . ' · from ' . $p['cargo']['label'] . ($rustOk ? '' : '. Version 1.89 or newer is needed.'),
        $rustOk ? [] : ['action' => 'update_rust', 'action_label' => 'Update Rust']);
} else {
    $add('Build tools', 'Rust (cargo, rustc)', 'fail', 'not found for ' . $user['name'] . ' (looked in ~/.cargo/bin, the data folder and PATH)',
        ['action' => 'install_rust', 'action_label' => 'Install Rust (no admin rights needed)']);
}

// Files
if ($p['src_dir'] === '') {
    $add('Files', 'Source folder', 'fail', 'No epg-loader folder with a Cargo.toml was found next to this page. Enter its location under "Detected paths".');
} else {
    [$bad, $total] = unreadable_files($p['src_dir']);
    $owner = epg_user_of($p['src_dir'])['name'];
    $add('Files', 'Source folder readable by PHP', $total === 0 ? 'ok' : 'fail',
        $total === 0 ? $p['src_dir'] . ($p['src_from'] === 'saved' ? ' (saved)' : '')
            : "$total file(s) or folder(s) cannot be read by {$user['name']}, for example: " . implode(', ', $bad)
              . '. Files copied in over SMB often arrive readable only by their owner.',
        $total === 0 ? [] : ['admin' => ($owner !== '' && $owner !== $user['name'] ? "# as $owner (the owner), or with sudo:\n" : '')
            . 'chmod -R a+rX ' . escapeshellarg($p['src_dir'])]);
}
$webFiles = ['index.html', 'settings.php', 'setup.php', 'epg_lib.php'];
$badWeb = array_values(array_filter($webFiles, fn($f) => !is_readable($p['web_dir'] . '/' . $f)));
$add('Files', 'Pages readable by PHP', $badWeb ? 'fail' : 'ok',
    $badWeb ? 'cannot read: ' . implode(', ', $badWeb) : implode(', ', $webFiles),
    $badWeb ? ['admin' => 'chmod a+r ' . implode(' ', array_map(fn($f) => escapeshellarg($p['web_dir'] . '/' . $f), $badWeb))] : []);

// Program
$info = bin_info($p);
if ($p['bin'] === '' || $info === null) {
    $add('Program', 'epg-loader program', 'fail', $p['bin'] === '' ? 'not built yet' : $p['bin'] . ' does not run',
        ['action' => 'build', 'action_label' => 'Build the program']);
} else {
    $age = (int)filemtime($p['bin']);
    $newest = $p['src_dir'] !== '' ? newest_source_mtime($p['src_dir']) : 0;
    $stale = $newest > $age;
    $add('Program', 'epg-loader program', $stale ? 'warn' : 'ok',
        $p['bin'] . ' · version ' . ($info['version'] ?? '?') . ' · built ' . when($age)
            . ($stale ? ' · the source changed on ' . when($newest) . ', rebuild to use it' : ''),
        $stale ? ['action' => 'build', 'action_label' => 'Rebuild the program'] : []);
}

// Data
$d = $p['data_dir'];
if ($d === '') {
    $add('Data', 'Data folder', 'fail', 'No place outside the web root that ' . $user['name'] . ' can write was found. Tried: ' . implode(', ', $p['data_candidates']),
        ['admin' => 'sudo install -d -o ' . escapeshellarg($user['name']) . ' -g ' . escapeshellarg($group) . ' -m 2770 ' . ($p['data_candidates'] ? escapeshellarg($p['data_candidates'][0]) : '<a folder outside the web root>')]);
} elseif (!is_dir($d)) {
    $add('Data', 'Data folder', 'warn', "$d does not exist yet (outside the web root, writable by {$user['name']})",
        ['action' => 'create_data_dir', 'action_label' => 'Create data folder']);
} else {
    $add('Data', 'Data folder', is_writable($d) ? 'ok' : 'fail', $d . ' (' . $p['data_from'] . ')' . (is_writable($d) ? '' : ' is not writable by ' . $user['name']),
        is_writable($d) ? [] : ['admin' => 'sudo chown -R ' . escapeshellarg($user['name'] . ':' . $group) . ' ' . escapeshellarg($d)]);
}
$db = db_state($p);
$expected = (int)($info['schema_version'] ?? 0);
if ($db['ok']) {
    $current = $expected === 0 || $db['version'] >= $expected;
    $add('Data', 'Database (epg.db)', $current ? 'ok' : 'warn', 'schema version ' . $db['version'] . ($expected ? " (program expects $expected)" : ''),
        $current ? [] : ['action' => 'migrate', 'action_label' => 'Upgrade database']);
} else {
    $add('Data', 'Database (epg.db)', 'fail', $db['error'], ['action' => 'migrate', 'action_label' => 'Initialise database']);
}
$probe = $p['web_dir'] . '/.epg-write-test-' . bin2hex(random_bytes(4));
$canWriteWeb = @file_put_contents($probe, 'x') !== false;
if ($canWriteWeb) {
    @unlink($probe);
}
$add('Data', 'Output folder (epg.xml, status.json)', $canWriteWeb ? 'ok' : 'fail',
    $p['web_dir'] . ($canWriteWeb ? ' is writable' : ' is not writable by ' . $user['name']),
    $canWriteWeb ? [] : ['admin' => 'sudo chgrp ' . escapeshellarg($group) . ' ' . escapeshellarg($p['web_dir']) . ' && sudo chmod 2775 ' . escapeshellarg($p['web_dir'])]);

// Web
$srcUrl = url_path_of($p, $p['src_dir']);
$server = (string)($_SERVER['SERVER_SOFTWARE'] ?? '');
if ($p['src_dir'] === '' || $srcUrl === '') {
    $add('Web', 'Source folder hidden from the web', 'ok', 'the source folder is outside this web folder');
} else {
    [$code, $line] = self_http_status($srcUrl . '/Cargo.toml');
    $blocked = in_array($code, [403, 404], true);
    $snippet = stripos($server, 'apache') !== false
        ? "# Apache: the .htaccess in the source folder already does this; make sure AllowOverride permits it."
        : "# nginx: add inside the server { } block for " . epg_host() . ", then: sudo nginx -t && sudo systemctl reload nginx\n"
          . "location ^~ $srcUrl/ { deny all; return 404; }";
    $add('Web', 'Source folder hidden from the web', $blocked ? 'ok' : ($code === 0 ? 'warn' : 'fail'),
        $blocked ? epg_base_url() . substr($srcUrl, strlen($p['base_path'])) . "/ answers $code"
            : ($code === 0 ? "could not test it: $line" : epg_base_url() . substr($srcUrl, strlen($p['base_path'])) . "/Cargo.toml is publicly readable (HTTP $code)"),
        $blocked ? [] : ['admin' => $snippet]);
}
$add('Web', 'Web server', 'info', ($server ?: 'unknown') . ' · this page: ' . epg_base_url() . '/setup.php');

// Connections. What is saved comes from epg.db right now; the test result
// from the last "epg-loader check" (check.json), and only if it is newer
// than the last settings change.
$set = $db['settings'];
$hasSrc = trim((string)($set['source_url'] ?? '')) !== '';
try {
    foreach (epg_guides(epg_db($p)) as $gg) {
        $hasSrc = $hasSrc || ($gg['enabled'] && trim($gg['source_url']) !== '');
    }
} catch (Throwable $t) {
}
$hasKey = trim((string)($set['tmdb_api_key'] ?? '')) !== '';
$savedAt = (int)($set['settings_saved_at'] ?? 0);
$checkFile = epg_check_file($p);
$cj = ($checkFile !== '' && is_file($checkFile)) ? json_decode((string)file_get_contents($checkFile), true) : null;
if (is_array($cj) && (int)($cj['checked_at'] ?? 0) < $savedAt) {
    $cj = null; // tested before the settings were last saved: not valid any more
}
$checkJobRunning = $job['name'] === 'check' && $job['running'];
$checkJobFailed = $job['name'] === 'check' && !$job['running'] && $job['exit'] !== null && $job['exit'] !== 0;
$checkPending = $d !== '' && is_file($d . '/check.pending');
$checkExtra = ['action' => 'check', 'action_label' => 'Run connection tests'];
$keyHint = $hasKey ? 'saved (••••' . substr(trim((string)$set['tmdb_api_key']), -4) . ')' : 'not saved';
$conn = [
    'internet' => ['Internet access (TMDB API reachable)', ''],
    'tmdb' => ['TMDB key accepted', 'Key ' . $keyHint],
    'source' => ['XMLTV source reachable and valid', 'Source URL ' . ($hasSrc ? 'saved' : 'not saved')],
];
foreach ($conn as $k => [$t, $savedNote]) {
    $pre = $savedNote !== '' ? $savedNote . ' · ' : '';
    if ($p['bin'] === '') {
        $add('Connections', $t, 'info', $pre . 'test: build the program first');
    } elseif ($checkJobRunning) {
        $add('Connections', $t, 'info', $pre . 'test: running now…');
    } elseif ($checkJobFailed) {
        $lines = array_values(array_filter(explode("\n", $job['log']), fn($l) => trim($l) !== '' && !str_starts_with($l, '[finished')));
        $add('Connections', $t, 'fail', $pre . 'test could not run (exit code ' . $job['exit'] . '): ' . (end($lines) ?: 'see the task log'), $checkExtra);
    } elseif (!is_array($cj)) {
        $add('Connections', $t, 'info', $pre . ($checkPending ? 'test: queued, starts when the running task finishes' : 'test: not run since the settings were saved'), $checkExtra);
    } else {
        $r = $cj[$k] ?? ['ok' => false, 'detail' => 'no result'];
        $add('Connections', $t, !empty($r['ok']) ? 'ok' : 'fail',
            $pre . 'test: ' . (string)($r['detail'] ?? '') . ' · tested ' . when((int)($cj['checked_at'] ?? 0)), $checkExtra);
    }
}

// Settings + schedule + runs
$add('Guide', 'Settings', $hasSrc ? ($hasKey ? 'ok' : 'warn') : 'fail',
    ($hasSrc ? 'XMLTV source saved' : 'no XMLTV source yet: add or edit guides on the Guides page') . ' · ' . ($hasKey ? 'TMDB key saved' : 'no TMDB key (posters and ratings are skipped)'),
    ['link' => $hasSrc ? 'settings.php' : 'guides.php', 'link_label' => $hasSrc ? 'Open settings' : 'Open Guides']);
$cronOk = $canExec && epg_cron_installed();
$svc = epg_which('systemctl') !== '' ? epg_run('systemctl is-active epg-loader')[1] : '';
$watchOk = trim($svc) === 'active';
$add('Guide', 'Automatic rebuilds', ($cronOk || $watchOk) ? 'ok' : 'warn',
    $cronOk ? 'crontab entry for ' . $user['name'] . ' installed (runs every 15 minutes, builds when refresh_hours have passed)'
        : ($watchOk ? 'the epg-loader systemd service (watch mode) is running' : 'not installed'),
    $cronOk ? ['action' => 'remove_schedule', 'action_label' => 'Remove schedule', 'danger' => true]
        : ($p['bin'] !== '' && $tools['crontab'] !== '' ? ['action' => 'install_schedule', 'action_label' => 'Install schedule'] : []));
$run = $db['run'];
if ($run === null) {
    $add('Guide', 'Last guide build', $db['ok'] ? 'warn' : 'info', 'no build has run yet',
        $p['bin'] !== '' && $hasSrc ? ['action' => 'first_build', 'action_label' => 'Run first guide build'] : []);
} elseif ($run['finished_at'] === null) {
    $add('Guide', 'Last guide build', 'info', 'running since ' . when((int)$run['started_at']));
} elseif ((int)$run['ok'] === 1) {
    $add('Guide', 'Last guide build', 'ok', 'finished ' . when((int)$run['finished_at']) . ' · ' . (int)$run['programmes'] . ' programmes · ' . epg_base_url() . '/epg.xml',
        ['action' => 'first_build', 'action_label' => 'Build the guide now']);
} else {
    $add('Guide', 'Last guide build', 'fail', 'failed ' . when((int)$run['finished_at']) . ': ' . (string)$run['error'],
        ['action' => 'first_build', 'action_label' => 'Try again']);
}

$fails = count(array_filter($checks, fn($c) => $c['status'] === 'fail'));
$warns = count(array_filter($checks, fn($c) => $c['status'] === 'warn'));
$complete = $p['bin'] !== '' && $db['ok'] && $hasSrc && ($cronOk || $watchOk) && $db['last_ok'] > 0 && $fails === 0;
epg_save_setup_state($p, ['complete' => $complete, 'fails' => $fails, 'warns' => $warns, 'watch' => $watchOk, 'ever_complete' => $complete || epg_setup_done($p)]);
if (($_GET['ajax'] ?? '') === 'checks') {
    header('Content-Type: application/json');
    header('Cache-Control: no-store');
    $list = array_map(fn($c) => ['group' => $c['group'], 'title' => $c['title'], 'status' => $c['status'], 'detail' => $c['detail']], $checks);
    echo json_encode(['complete' => $complete, 'fails' => $fails, 'warns' => $warns, 'checks' => $list], JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}
$unlocked = !empty($_SESSION['setup_unlocked']);
$showActions = !$complete || $unlocked;

// ---------------------------------------------------------------- page

page_start(EPG_APP_NAME . ' setup', epg_take_flashes());
foreach ($p['notes'] as $n) {
    echo '<div class="flash warn">' . h($n) . '</div>';
}
$csrf = epg_csrf_field();
if ($p['bin'] !== '' && (!empty($stale) || !epg_bin_supports($p, 'enrich'))) {
    echo '<div class="flash warn stale-banner"><strong>The epg-loader program is older than its source code.</strong> '
        . 'New features (background TMDB lookups on the Activity page, tv-logos channel logos) need a rebuild.'
        . '<form method="post" class="inline">' . $csrf . '<input type="hidden" name="action" value="build"><button' . ($job['running'] ? ' disabled' : '') . '>Build the program</button></form></div>';
}
?>
    <div class="card" id="job-card" <?= $job['name'] === '' ? 'hidden' : '' ?>>
      <h2 id="job-title"><?= h($job['label'] ?: 'Task') ?></h2>
      <p id="job-state" class="muted"></p>
      <div id="job-cancel"><?= epg_cancel_form(!epg_anything_running($p)) ?></div>
      <pre id="job-log"><?= h($job['log']) ?></pre>
    </div>

<?php if ($complete && !$unlocked): ?>
    <div class="card">
      <h2><span class="dot ok"></span>Setup complete</h2>
      <dl>
        <dt>Guide address</dt><dd><a href="epg.xml"><?= h(epg_base_url() . '/epg.xml') ?></a></dd>
        <dt>Last good build</dt><dd><?= h(when($db['last_ok'])) ?></dd>
        <dt>Rebuilds</dt><dd><?= h($cronOk ? 'crontab, every ' . ($set['refresh_hours'] ?? '6') . ' hours' : 'systemd watch service') ?></dd>
        <dt>Program</dt><dd><?= h($p['bin']) ?></dd>
        <dt>Data folder</dt><dd><?= h($p['data_dir']) ?></dd>
        <dt>Warnings</dt><dd><?= $warns ? h($warns . ' (see below)') : 'none' ?></dd>
      </dl>
      <form method="post" onsubmit="return confirm('Show the setup actions again?')">
        <?= $csrf ?><input type="hidden" name="action" value="unlock">
        <button class="secondary">Re-run setup</button>
      </form>
    </div>
<?php elseif ($complete): ?>
    <div class="card">
      <h2><span class="dot ok"></span>Setup complete: actions unlocked</h2>
      <form method="post"><?= $csrf ?><input type="hidden" name="action" value="lock"><button class="secondary">Hide actions again</button></form>
    </div>
<?php else: ?>
    <div class="card">
      <h2><span class="dot <?= $fails ? 'fail' : 'warn' ?>"></span>Setup: <?= $fails ? h($fails . ' item(s) to fix') : h('nearly done') ?></h2>
      <p class="hint">Work down the list. Buttons do the step for you. An orange box means the step needs someone with admin rights on the server, and shows the exact command.</p>
    </div>
<?php endif; ?>

    <div class="card">
      <h2>Checks</h2>
      <table>
        <thead><tr><th style="width:30%">Check</th><th>Result</th></tr></thead>
        <tbody>
        <?php $lastGroup = ''; foreach ($checks as $c): ?>
          <?php if ($c['group'] !== $lastGroup): $lastGroup = $c['group']; ?>
            <tr><th colspan="2" style="padding-top:0.9rem"><?= h($c['group']) ?></th></tr>
          <?php endif; ?>
          <tr>
            <td><span class="dot <?= h($c['status']) ?>"></span><?= h($c['title']) ?></td>
            <td>
              <?= h($c['detail']) ?>
              <?php if (!empty($c['admin']) && $c['status'] !== 'ok'): ?>
                <div class="admin"><strong>Needs admin rights on the server.</strong> Copy and run:<pre><?= h($c['admin']) ?></pre></div>
              <?php endif; ?>
              <?php if (!empty($c['action']) && $showActions): ?>
                <form method="post" class="inline"><?= $csrf ?><input type="hidden" name="action" value="<?= h($c['action']) ?>">
                  <button class="<?= !empty($c['danger']) ? 'danger' : '' ?>" <?= $job['running'] ? 'disabled' : '' ?>><?= h($c['action_label']) ?></button></form>
              <?php endif; ?>
              <?php if (!empty($c['link'])): ?>
                <div><a href="<?= h($c['link']) ?>"><?= h($c['link_label']) ?></a></div>
              <?php endif; ?>
            </td>
          </tr>
        <?php endforeach; ?>
        </tbody>
      </table>
    </div>

<?php if ($showActions): ?>
    <div class="card">
      <h2>All actions</h2>
      <p class="hint">Each runs in the background as <?= h($user['name']) ?>; its log appears at the top of the page. Only one runs at a time.</p>
      <?php foreach ([
          'create_data_dir' => 'Create data folder',
          'install_rust' => 'Install Rust',
          'build' => 'Build the program',
          'test' => 'Run tests',
          'migrate' => 'Initialise or upgrade database',
          'check' => 'Test TMDB key and source URL',
          'first_build' => 'Build the guide',
          'install_schedule' => 'Install schedule',
          'remove_schedule' => 'Remove schedule',
      ] as $a => $label): ?>
        <form method="post" class="inline"><?= $csrf ?><input type="hidden" name="action" value="<?= h($a) ?>">
          <button class="<?= $a === 'remove_schedule' ? 'danger' : 'secondary' ?>" <?= $job['running'] ? 'disabled' : '' ?>><?= h($label) ?></button></form>
      <?php endforeach; ?>
    </div>

    <div class="card">
      <h2>Detected paths</h2>
      <dl>
        <dt>Web folder</dt><dd><?= h($p['web_dir']) ?></dd>
        <dt>Base URL</dt><dd><?= h($p['base_url']) ?></dd>
        <dt>Document root</dt><dd><?= h($p['doc_root'] ?: 'unknown') ?></dd>
        <dt>PHP user / home</dt><dd><?= h($user['name'] . ' / ' . ($user['home'] ?: 'unknown')) ?></dd>
        <dt>Cargo</dt><dd><?= h($p['cargo']['cargo'] ?: 'not found') ?></dd>
      </dl>
      <form method="post" autocomplete="off">
        <?= $csrf ?><input type="hidden" name="action" value="save_paths">
        <label for="data_dir">Data folder (<?= h($p['data_from']) ?>). Must be outside the web root</label>
        <input type="text" id="data_dir" name="data_dir" value="<?= h($p['saved']['data_dir'] ?? '') ?>" placeholder="<?= h($p['data_dir'] ?: 'auto') ?>">
        <p class="hint">Leave blank to detect. Candidates: <?= h(implode(', ', $p['data_candidates'])) ?></p>
        <label for="src_dir">Source folder (<?= h($p['src_from']) ?>)</label>
        <input type="text" id="src_dir" name="src_dir" value="<?= h($p['saved']['src_dir'] ?? '') ?>" placeholder="<?= h($p['src_dir'] ?: 'auto') ?>">
        <label for="bin">Program (<?= h($p['bin_from']) ?>)</label>
        <input type="text" id="bin" name="bin" value="<?= h($p['saved']['bin'] ?? '') ?>" placeholder="<?= h($p['bin'] ?: 'auto: the newest build') ?>">
        <button>Save paths</button>
      </form>
      <form method="post" class="inline"><?= $csrf ?><input type="hidden" name="action" value="redetect"><button class="secondary">Forget saved paths and detect again</button></form>
    </div>
<?php endif; ?>

    <script>
    (function () {
      var card = document.getElementById('job-card');
      var title = document.getElementById('job-title');
      var state = document.getElementById('job-state');
      var log = document.getElementById('job-log');
      var wasRunning = <?= $job['running'] ? 'true' : 'false' ?>;
      function show(j) {
        if (!j.name) { return; }
        card.hidden = false;
        title.textContent = j.label || 'Task';
        var atBottom = log.scrollTop + log.clientHeight >= log.scrollHeight - 20;
        log.textContent = j.log || '(no output yet)';
        if (atBottom) { log.scrollTop = log.scrollHeight; }
        var cancelForm = document.querySelector('#job-cancel form');
        if (cancelForm) { cancelForm.hidden = !j.running; }
        if (j.running) {
          state.textContent = 'Running…';
          state.className = 'muted';
        } else if (j.cancelled) {
          state.textContent = 'Cancelled.';
          state.className = 'warn-text';
        } else if (j.exit === 0) {
          state.textContent = 'Finished successfully.';
          state.className = 'ok-text';
        } else if (j.exit !== null) {
          state.textContent = 'Failed (exit code ' + j.exit + '). The log above says why.';
          state.className = 'bad-text';
        } else {
          state.textContent = 'Stopped without a result.';
          state.className = 'warn-text';
        }
      }
      function poll() {
        fetch('?ajax=job', { cache: 'no-store', credentials: 'same-origin' })
          .then(function (r) { return r.json(); })
          .then(function (j) {
            show(j);
            if (j.running) { wasRunning = true; setTimeout(poll, 1500); }
            else if (wasRunning) { setTimeout(function () { location.reload(); }, 1200); }
          })
          .catch(function () { setTimeout(poll, 5000); });
      }
      log.scrollTop = log.scrollHeight;
      if (wasRunning) { poll(); } else if (card.hidden === false) { show(<?= json_encode($job, JSON_HEX_TAG | JSON_HEX_AMP | JSON_HEX_APOS | JSON_HEX_QUOT | JSON_INVALID_UTF8_SUBSTITUTE) ?>); }
    })();
    </script>
<?php page_end();
