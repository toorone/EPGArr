<?php
/*
 * Shared code for settings.php and setup.php.
 *
 * Nothing here is hard-coded: the web folder is __DIR__, the base URL comes
 * from the request, the PHP user and home from posix, and the data dir,
 * source dir, binary and cargo are detected at runtime. Choices made on the
 * setup page are saved in setup-paths.php next to this file. If a saved path
 * stops working (folder moved, new server), detection takes over again and
 * the setup page says so.
 */
declare(strict_types=1);

if (realpath((string)($_SERVER['SCRIPT_FILENAME'] ?? '')) === __FILE__) {
    http_response_code(404);
    exit;
}

// The name people see. Internal names (the epg-loader program and folder, the crontab tag) stay as they are.
const EPG_APP_NAME = 'EPGArr';
const EPG_GROUPS = ['Movie', 'Series', 'News', 'Sports', 'Kids', 'Music', 'Documentary'];
const EPG_PATHS_FILE = __DIR__ . '/setup-paths.php';
const EPG_DATA_DIR_NAME = 'epg-loader';
const EPG_WEB_BASE_NAMES = ['public_html', 'www', 'htdocs', 'html', 'web', 'webroot'];

function h(?string $s): string
{
    return htmlspecialchars((string)$s, ENT_QUOTES | ENT_SUBSTITUTE, 'UTF-8');
}

// ---------------------------------------------------------------- request basics

/** Use the server's own time zone when php.ini sets none (PHP then falls back to UTC). */
function epg_set_local_tz(): void
{
    if ((string)ini_get('date.timezone') !== '') {
        return;
    }
    $tz = trim((string)@file_get_contents('/etc/timezone'));
    if ($tz === '' && is_link('/etc/localtime')) {
        $t = (string)@readlink('/etc/localtime');
        if (preg_match('~zoneinfo/(.+)$~', $t, $m)) {
            $tz = $m[1];
        }
    }
    if ($tz === '') {
        $tz = (string)getenv('TZ');
    }
    if ($tz !== '' && preg_match('~^[A-Za-z0-9_+\-/]+$~', $tz)) {
        try {
            new DateTimeZone($tz);
            date_default_timezone_set($tz);
        } catch (Throwable $e) {
            // unknown name: keep PHP's default
        }
    }
}

function epg_boot(): void
{
    epg_set_local_tz();
    header('X-Frame-Options: DENY');
    header('X-Content-Type-Options: nosniff');
    header('Referrer-Policy: no-referrer');
    header('Cache-Control: no-store');
    header("Content-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'unsafe-inline' 'self'; img-src 'self' https: data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'");
    session_name('epgadmin');
    session_set_cookie_params([
        'path' => epg_base_path() . '/',
        'httponly' => true,
        'samesite' => 'Strict',
        'secure' => epg_scheme() === 'https',
    ]);
    session_start();
    if (empty($_SESSION['csrf'])) {
        $_SESSION['csrf'] = bin2hex(random_bytes(32));
    }
}

function epg_csrf_ok(): bool
{
    return $_SERVER['REQUEST_METHOD'] === 'POST'
        && hash_equals((string)($_SESSION['csrf'] ?? ''), (string)($_POST['csrf'] ?? ''));
}

function epg_csrf_field(): string
{
    return '<input type="hidden" name="csrf" value="' . h((string)$_SESSION['csrf']) . '">';
}

function epg_flash(string $type, string $msg): void
{
    $_SESSION['flash'][] = [$type, $msg];
}

function epg_take_flashes(): array
{
    $f = $_SESSION['flash'] ?? [];
    unset($_SESSION['flash']);
    return $f;
}

function epg_redirect_self(): never
{
    header('Location: ' . strtok((string)($_SERVER['REQUEST_URI'] ?? '/'), '?'), true, 303);
    exit;
}

function epg_scheme(): string
{
    $https = $_SERVER['HTTPS'] ?? '';
    if ($https !== '' && $https !== 'off') {
        return 'https';
    }
    return strtolower((string)($_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '')) === 'https' ? 'https' : 'http';
}

/** URL path of this folder, e.g. "/epg" (no trailing slash; "" at the site root). */
function epg_base_path(): string
{
    $dir = str_replace('\\', '/', dirname((string)($_SERVER['SCRIPT_NAME'] ?? '/')));
    return rtrim($dir, '/');
}

function epg_host(): string
{
    $host = (string)($_SERVER['HTTP_HOST'] ?? ($_SERVER['SERVER_NAME'] ?? 'localhost'));
    return preg_match('/^[A-Za-z0-9.\-\[\]:]+$/', $host) ? $host : 'localhost';
}

function epg_base_url(): string
{
    return epg_scheme() . '://' . epg_host() . epg_base_path();
}

// ---------------------------------------------------------------- environment

function epg_fpm_user(): array
{
    if (function_exists('posix_geteuid') && function_exists('posix_getpwuid')) {
        $pw = posix_getpwuid(posix_geteuid());
        if (is_array($pw)) {
            return ['name' => (string)$pw['name'], 'uid' => (int)$pw['uid'], 'home' => (string)$pw['dir'], 'known' => true];
        }
    }
    return ['name' => (string)(getenv('USER') ?: 'unknown'), 'uid' => -1, 'home' => (string)(getenv('HOME') ?: ''), 'known' => false];
}

function epg_user_of(string $path): array
{
    if (function_exists('posix_getpwuid') && file_exists($path)) {
        $pw = posix_getpwuid((int)fileowner($path));
        if (is_array($pw)) {
            return ['name' => (string)$pw['name'], 'home' => (string)$pw['dir']];
        }
    }
    return ['name' => '', 'home' => ''];
}

function epg_disabled_functions(): array
{
    $list = array_map('trim', explode(',', (string)ini_get('disable_functions')));
    return array_values(array_filter($list));
}

function epg_can_exec(): bool
{
    $off = epg_disabled_functions();
    foreach (['exec', 'proc_open'] as $f) {
        if (!function_exists($f) || in_array($f, $off, true)) {
            return false;
        }
    }
    return true;
}

/** Path of a program on PATH, or "". */
function epg_which(string $prog): string
{
    if (!epg_can_exec() || !preg_match('/^[A-Za-z0-9._\-]+$/', $prog)) {
        return '';
    }
    $out = [];
    $rc = 1;
    @exec('command -v ' . escapeshellarg($prog) . ' 2>/dev/null', $out, $rc);
    return $rc === 0 && isset($out[0]) ? trim($out[0]) : '';
}

/** Run a short command and return [exit code, combined output]. */
function epg_run(string $cmd, array $env = [], int $timeout = 20): array
{
    if (!epg_can_exec()) {
        return [127, 'exec is disabled in PHP'];
    }
    $full = epg_env_prefix($env) . $cmd;
    if (epg_which('timeout') !== '') {
        $full = 'timeout ' . $timeout . ' sh -c ' . escapeshellarg($full);
    }
    $out = [];
    $rc = 1;
    @exec($full . ' 2>&1', $out, $rc);
    return [$rc, trim(implode("\n", $out))];
}

function epg_env_prefix(array $env): string
{
    if (!$env) {
        return '';
    }
    $parts = ['env'];
    foreach ($env as $k => $v) {
        if (!preg_match('/^[A-Z_][A-Z0-9_]*$/', (string)$k)) {
            continue;
        }
        $parts[] = escapeshellarg($k . '=' . $v);
    }
    return implode(' ', $parts) . ' ';
}

// ---------------------------------------------------------------- path helpers

function epg_norm(string $p): string
{
    $p = rtrim(str_replace('\\', '/', $p), '/');
    return $p === '' ? '/' : $p;
}

function epg_inside(string $path, string $dir): bool
{
    $path = epg_norm($path);
    $dir = epg_norm($dir);
    return $path === $dir || str_starts_with($path, $dir === '/' ? '/' : $dir . '/');
}

/** Folders that are (or are likely to be) served by the web server. */
function epg_web_roots(): array
{
    $roots = [];
    $doc = realpath((string)($_SERVER['DOCUMENT_ROOT'] ?? ''));
    if ($doc) {
        $roots[] = $doc;
    }
    $roots[] = __DIR__;
    // An ancestor called public_html, www, ... usually holds several sites.
    $d = __DIR__;
    while ($d !== '/' && $d !== '' && $d !== '.') {
        if (in_array(basename($d), EPG_WEB_BASE_NAMES, true)) {
            $roots[] = $d;
        }
        $parent = dirname($d);
        if ($parent === $d) {
            break;
        }
        $d = $parent;
    }
    return array_values(array_unique($roots));
}

function epg_is_web_path(string $path): bool
{
    foreach (epg_web_roots() as $r) {
        if (epg_inside($path, $r)) {
            return true;
        }
    }
    return false;
}

/** Could this process create $dir (or does it exist and is writable)? */
function epg_creatable(string $dir): bool
{
    if (is_dir($dir)) {
        return is_writable($dir);
    }
    $d = dirname($dir);
    while (!is_dir($d)) {
        $p = dirname($d);
        if ($p === $d) {
            return false;
        }
        $d = $p;
    }
    return is_writable($d);
}

// ---------------------------------------------------------------- saved paths

function epg_saved_paths(): array
{
    if (!is_file(EPG_PATHS_FILE)) {
        return [];
    }
    $v = @include EPG_PATHS_FILE;
    return is_array($v) ? $v : [];
}

function epg_save_paths(array $paths): bool
{
    $clean = [];
    foreach (['data_dir', 'src_dir', 'bin', 'admin_hash'] as $k) {
        if (isset($paths[$k]) && is_string($paths[$k]) && $paths[$k] !== '') {
            $clean[$k] = $paths[$k];
        }
    }
    $php = "<?php\n// Written by setup.php. Paths chosen on the setup page; empty = detect automatically.\n"
        . 'return ' . var_export($clean, true) . ";\n";
    $tmp = EPG_PATHS_FILE . '.tmp';
    if (@file_put_contents($tmp, $php) === false) {
        return false;
    }
    @chmod($tmp, 0640);
    if (!@rename($tmp, EPG_PATHS_FILE)) {
        return false;
    }
    if (function_exists('opcache_invalidate')) {
        @opcache_invalidate(EPG_PATHS_FILE, true);
    }
    return true;
}

// ---------------------------------------------------------------- detection

/**
 * Everything the pages need to know about where things are. Each path has a
 * "*_from" telling where it came from: saved, detected, existing.
 */
function epg_paths(bool $fresh = false): array
{
    static $cache = null;
    if ($cache !== null && !$fresh) {
        return $cache;
    }
    $saved = epg_saved_paths();
    $user = epg_fpm_user();
    $notes = [];

    // Source folder: saved, else ./epg-loader next to this file.
    $src = '';
    $srcFrom = 'detected';
    if (!empty($saved['src_dir'])) {
        if (is_file($saved['src_dir'] . '/Cargo.toml')) {
            $src = $saved['src_dir'];
            $srcFrom = 'saved';
        } else {
            $notes[] = 'The saved source folder ' . $saved['src_dir'] . ' has no Cargo.toml any more, so it was detected again.';
        }
    }
    if ($src === '' && is_file(__DIR__ . '/epg-loader/Cargo.toml')) {
        $src = __DIR__ . '/epg-loader';
    }

    // Data dir.
    $candidates = epg_data_dir_candidates($user, $src);
    $data = '';
    $dataFrom = 'detected';
    if (!empty($saved['data_dir'])) {
        $d = $saved['data_dir'];
        if (is_dir($d) && is_writable($d) && !epg_is_web_path($d)) {
            $data = $d;
            $dataFrom = 'saved';
        } else {
            $notes[] = 'The saved data folder ' . $d . ' is missing, not writable or inside the web root, so it was detected again.';
        }
    }
    if ($data === '') {
        foreach ($candidates as $c) {
            if (is_file($c . '/epg.db') && is_writable($c) && is_writable($c . '/epg.db')) {
                $data = $c;
                $dataFrom = 'existing install';
                break;
            }
        }
    }
    if ($data === '') {
        foreach ($candidates as $c) {
            if (epg_creatable($c)) {
                $data = $c;
                break;
            }
        }
    }

    // Binary: saved, else the newest working build.
    $bin = '';
    $binFrom = 'detected';
    if (!empty($saved['bin'])) {
        if (is_file($saved['bin']) && is_executable($saved['bin'])) {
            $bin = $saved['bin'];
            $binFrom = 'saved';
        } else {
            $notes[] = 'The saved program path ' . $saved['bin'] . ' no longer works, so it was detected again.';
        }
    }
    if ($bin === '') {
        $best = 0;
        foreach (epg_bin_candidates($data, $src) as $b) {
            if (is_file($b) && is_executable($b) && filemtime($b) > $best) {
                $best = (int)filemtime($b);
                $bin = $b;
            }
        }
    }

    $cargo = epg_find_cargo($user, $data, $src);

    $cache = [
        'web_dir' => __DIR__,
        'base_url' => epg_base_url(),
        'base_path' => epg_base_path(),
        'doc_root' => (string)realpath((string)($_SERVER['DOCUMENT_ROOT'] ?? '')),
        'user' => $user,
        'src_dir' => $src,
        'src_from' => $srcFrom,
        'data_dir' => $data,
        'data_from' => $dataFrom,
        'data_candidates' => $candidates,
        'bin' => $bin,
        'bin_from' => $binFrom,
        'cargo' => $cargo,
        'notes' => $notes,
        'saved' => $saved,
    ];
    return $cache;
}

function epg_data_dir_candidates(array $user, string $src): array
{
    $c = [];
    $home = $user['home'];
    if ($home !== '' && $home !== '/' && !epg_is_web_path($home)) {
        $c[] = epg_norm($home) . '/.local/share/' . EPG_DATA_DIR_NAME;
    }
    // Beside the web folders: the parent of the top-most public_html/www/...
    $roots = epg_web_roots();
    $top = '';
    foreach ($roots as $r) {
        if ($top === '' || strlen($r) < strlen($top)) {
            $top = $r;
        }
    }
    if ($top !== '' && dirname($top) !== $top) {
        $c[] = epg_norm(dirname($top)) . '/' . EPG_DATA_DIR_NAME . '-data';
    }
    // An install made earlier with the console steps.
    $c[] = '/var/lib/' . EPG_DATA_DIR_NAME;
    $c = array_values(array_unique(array_filter($c, fn($p) => !epg_is_web_path($p))));
    return $c;
}

function epg_bin_candidates(string $data, string $src): array
{
    $c = [];
    if ($data !== '') {
        $c[] = $data . '/target/release/epg-loader';
    }
    if ($src !== '') {
        $c[] = $src . '/target/release/epg-loader';
    }
    $c[] = '/usr/local/bin/epg-loader';
    $w = epg_which('epg-loader');
    if ($w !== '') {
        $c[] = $w;
    }
    return array_values(array_unique($c));
}

/**
 * Find cargo and the environment to run it with. Looks in the PHP user's
 * ~/.cargo, a toolchain installed into the data dir, the source folder
 * owner's ~/.cargo, then PATH.
 */
function epg_find_cargo(array $user, string $data, string $src): array
{
    $tries = [];
    if ($user['home'] !== '') {
        $tries[] = [epg_norm($user['home']) . '/.cargo', epg_norm($user['home']) . '/.rustup', 'your PHP user\'s ~/.cargo'];
    }
    if ($data !== '') {
        $tries[] = [$data . '/rust/cargo', $data . '/rust/rustup', 'the data folder'];
    }
    $owner = $src !== '' ? epg_user_of($src) : ['home' => ''];
    if ($owner['home'] !== '' && $owner['home'] !== $user['home']) {
        $tries[] = [epg_norm($owner['home']) . '/.cargo', epg_norm($owner['home']) . '/.rustup', 'the source folder owner\'s ~/.cargo'];
    }
    foreach ($tries as [$cargoHome, $rustupHome, $label]) {
        $bin = $cargoHome . '/bin/cargo';
        if (is_file($bin) && is_executable($bin)) {
            return epg_cargo_info($bin, $cargoHome, $rustupHome, $label, $data, $user);
        }
    }
    $w = epg_which('cargo');
    if ($w !== '') {
        return epg_cargo_info($w, '', '', 'PATH', $data, $user);
    }
    return ['cargo' => '', 'label' => '', 'env' => []];
}

function epg_cargo_info(string $bin, string $cargoHome, string $rustupHome, string $label, string $data, array $user): array
{
    $home = ($user['home'] !== '' && is_writable($user['home'])) ? $user['home'] : $data;
    $env = [
        'HOME' => $home,
        'PATH' => dirname($bin) . ':/usr/local/bin:/usr/bin:/bin',
    ];
    if ($cargoHome !== '') {
        // Cargo needs a writable CARGO_HOME for its download cache.
        $env['CARGO_HOME'] = is_writable($cargoHome) ? $cargoHome : $data . '/cargo-home';
        $env['RUSTUP_HOME'] = $rustupHome;
    }
    if ($data !== '') {
        $env['CARGO_TARGET_DIR'] = $data . '/target';
    }
    return ['cargo' => $bin, 'label' => $label, 'env' => $env];
}

/** Where "Install Rust" puts the toolchain. */
function epg_rust_install_env(array $p): array
{
    $home = $p['user']['home'];
    if ($home !== '' && is_writable($home) && !epg_is_web_path($home)) {
        $cargoHome = epg_norm($home) . '/.cargo';
        $rustupHome = epg_norm($home) . '/.rustup';
        $h = $home;
    } else {
        $cargoHome = $p['data_dir'] . '/rust/cargo';
        $rustupHome = $p['data_dir'] . '/rust/rustup';
        $h = $p['data_dir'];
    }
    return ['HOME' => $h, 'CARGO_HOME' => $cargoHome, 'RUSTUP_HOME' => $rustupHome, 'PATH' => '/usr/local/bin:/usr/bin:/bin'];
}

/** Environment for running the epg-loader binary. */
function epg_bin_env(array $p): array
{
    return ['EPG_LOADER_DIR' => $p['data_dir'], 'EPG_OUTPUT_DIR' => $p['web_dir']];
}

// ---------------------------------------------------------------- database + auth

function epg_db(array $p): PDO
{
    static $pdo = null;
    if ($pdo instanceof PDO) {
        return $pdo;
    }
    if (!class_exists('PDO') || !in_array('sqlite', PDO::getAvailableDrivers(), true)) {
        throw new RuntimeException('PHP has no PDO SQLite driver. Open setup.php for the fix.');
    }
    if ($p['data_dir'] === '' || !is_file($p['data_dir'] . '/epg.db')) {
        throw new RuntimeException('The database (epg.db) does not exist yet. Open setup.php to finish the setup.');
    }
    $pdo = new PDO('sqlite:' . $p['data_dir'] . '/epg.db', null, null, [
        PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION,
        PDO::ATTR_DEFAULT_FETCH_MODE => PDO::FETCH_ASSOC,
        PDO::ATTR_TIMEOUT => 15,
    ]);
    $pdo->exec('PRAGMA busy_timeout = 15000');
    $has = $pdo->query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'")->fetchColumn();
    if ($has === false) {
        throw new RuntimeException('epg.db has no tables yet. Open setup.php and run "Initialise or upgrade database".');
    }
    return $pdo;
}

/** The admin password hash: data dir admin.json, else the paths file, else an older DB setting. */
function epg_admin_hash(array $p): string
{
    if ($p['data_dir'] !== '' && is_readable($p['data_dir'] . '/admin.json')) {
        $j = json_decode((string)file_get_contents($p['data_dir'] . '/admin.json'), true);
        if (is_array($j) && !empty($j['hash'])) {
            return (string)$j['hash'];
        }
    }
    if (!empty($p['saved']['admin_hash'])) {
        return (string)$p['saved']['admin_hash'];
    }
    try {
        $v = epg_db($p)->query("SELECT value FROM settings WHERE key = 'admin_password_hash'")->fetchColumn();
        return is_string($v) ? $v : '';
    } catch (Throwable $e) {
        return '';
    }
}

/** Store the hash in the data dir when possible, else in the paths file. */
function epg_set_admin_hash(array $p, string $hash): bool
{
    if ($p['data_dir'] !== '' && (is_dir($p['data_dir']) || @mkdir($p['data_dir'], 02770, true))) {
        $f = $p['data_dir'] . '/admin.json';
        if (@file_put_contents($f . '.tmp', json_encode(['hash' => $hash])) !== false && @rename($f . '.tmp', $f)) {
            @chmod($f, 0660);
            $saved = $p['saved'];
            unset($saved['admin_hash']);
            epg_save_paths($saved);
            return true;
        }
    }
    $saved = $p['saved'];
    $saved['admin_hash'] = $hash;
    return epg_save_paths($saved);
}

// ---------------------------------------------------------------- users
//
// Users live in <data dir>/users.json (outside the web root): a list of
// {"username", "hash", "created"}. The old single admin password becomes the
// user "admin" the first time this runs. A session remembers the username and
// a fingerprint of that user's password hash, so changing a password or
// removing a user ends their other sessions.

/** All users, in the order they were added. */
function epg_users(array $p): array
{
    if (isset($GLOBALS['epg_users_cache'])) {
        return $GLOBALS['epg_users_cache'];
    }
    $users = null;
    if ($p['data_dir'] !== '' && is_readable($p['data_dir'] . '/users.json')) {
        $users = json_decode((string)file_get_contents($p['data_dir'] . '/users.json'), true);
    } elseif (!empty($p['saved']['users']) && is_array($p['saved']['users'])) {
        $users = $p['saved']['users'];
    }
    if (!is_array($users)) {
        $users = [];
        $old = epg_admin_hash($p);
        if ($old !== '') {
            $users = [['username' => 'admin', 'hash' => $old, 'created' => time()]];
        }
    }
    $out = [];
    foreach ($users as $u) {
        if (is_array($u) && ($u['username'] ?? '') !== '' && ($u['hash'] ?? '') !== '') {
            $out[] = [
                'username' => (string)$u['username'],
                'hash' => (string)$u['hash'],
                'role' => (string)($u['role'] ?? 'admin'),
                'disabled' => !empty($u['disabled']),
                'created' => (int)($u['created'] ?? 0),
                'last_login' => (int)($u['last_login'] ?? 0),
            ];
        }
    }
    $GLOBALS['epg_users_cache'] = $out;
    return $out;
}

/** Save the user list. Returns false when neither the data folder nor the web folder is writable. */
function epg_save_users(array $p, array $users): bool
{
    $users = array_values($users);
    $GLOBALS['epg_users_cache'] = $users;
    if ($p['data_dir'] !== '' && (is_dir($p['data_dir']) || @mkdir($p['data_dir'], 02770, true))) {
        $f = $p['data_dir'] . '/users.json';
        if (@file_put_contents($f . '.tmp', json_encode($users, JSON_PRETTY_PRINT)) !== false && @rename($f . '.tmp', $f)) {
            @chmod($f, 0660);
            $saved = $p['saved'];
            unset($saved['users'], $saved['admin_hash']);
            epg_save_paths($saved);
            return true;
        }
    }
    $saved = $p['saved'];
    $saved['users'] = $users;
    return epg_save_paths($saved);
}

function epg_find_user(array $p, string $username): ?array
{
    foreach (epg_users($p) as $u) {
        if (strcasecmp($u['username'], $username) === 0) {
            return $u;
        }
    }
    return null;
}

// Failed logins: at most 5 per 15 minutes per address and per username.
const EPG_LOGIN_MAX_FAILS = 5;
const EPG_LOGIN_WINDOW = 900;

function epg_login_attempts_file(array $p): string
{
    return $p['data_dir'] !== '' ? $p['data_dir'] . '/login-attempts.json' : '';
}

/** Recent failures: key -> [unix times]. Keys are "ip:<address>" and "user:<name>". */
function epg_login_attempts(array $p): array
{
    $f = epg_login_attempts_file($p);
    $a = $f !== '' && is_readable($f) ? json_decode((string)file_get_contents($f), true) : [];
    $a = is_array($a) ? $a : [];
    $cut = time() - EPG_LOGIN_WINDOW;
    foreach ($a as $k => $times) {
        $a[$k] = array_values(array_filter((array)$times, fn($t) => (int)$t > $cut));
        if (!$a[$k]) {
            unset($a[$k]);
        }
    }
    return $a;
}

function epg_login_keys(string $username): array
{
    return ['ip:' . (string)($_SERVER['REMOTE_ADDR'] ?? ''), 'user:' . strtolower($username)];
}

function epg_login_blocked(array $p, string $username): bool
{
    $a = epg_login_attempts($p);
    foreach (epg_login_keys($username) as $k) {
        if (count($a[$k] ?? []) >= EPG_LOGIN_MAX_FAILS) {
            return true;
        }
    }
    return false;
}

function epg_login_failed(array $p, string $username): void
{
    $f = epg_login_attempts_file($p);
    if ($f === '') {
        return;
    }
    $a = epg_login_attempts($p);
    foreach (epg_login_keys($username) as $k) {
        $a[$k][] = time();
    }
    @file_put_contents($f, json_encode($a), LOCK_EX);
}

/** A successful login: clear the failures for this username and remember when it was. */
function epg_note_login(array $p, string $username): void
{
    $f = epg_login_attempts_file($p);
    if ($f !== '') {
        $a = epg_login_attempts($p);
        unset($a['user:' . strtolower($username)]);
        @file_put_contents($f, json_encode($a), LOCK_EX);
    }
    $users = epg_users($p);
    foreach ($users as $i => $u) {
        if (strcasecmp($u['username'], $username) === 0) {
            $users[$i]['last_login'] = time();
        }
    }
    epg_save_users($p, $users);
}

/** Letters, digits, dot, dash and underscore, 2 to 32 characters. Returns '' or an error. */
function epg_check_username(string $name): string
{
    return preg_match('/^[A-Za-z0-9._-]{2,32}$/', $name) ? '' : 'Usernames are 2 to 32 letters, digits, dots, dashes or underscores.';
}

/** Returns '' or an error. */
function epg_check_password(string $pw, string $repeat): string
{
    if (strlen($pw) < 8) {
        return 'Use at least 8 characters for the password.';
    }
    if (!hash_equals($pw, $repeat)) {
        return 'The two passwords do not match.';
    }
    return '';
}

function epg_login_as(array $u): void
{
    session_regenerate_id(true);
    $_SESSION['user'] = $u['username'];
    $_SESSION['authed'] = hash('sha256', $u['hash']);
}

/** The logged-in username, or ''. */
function epg_current_user(array $p): string
{
    $name = (string)($_SESSION['user'] ?? '');
    if ($name === '' && !empty($_SESSION['authed'])) {
        $name = 'admin'; // a session from before users existed
    }
    $u = $name !== '' ? epg_find_user($p, $name) : null;
    return $u !== null && !$u['disabled'] && hash_equals(hash('sha256', $u['hash']), (string)($_SESSION['authed'] ?? '')) ? $u['username'] : '';
}

function epg_authed(array $p): bool
{
    return epg_current_user($p) !== '';
}

/**
 * Handle the login / first-user forms. Returns true when the visitor
 * may continue; otherwise the caller shows epg_auth_form().
 */
function epg_handle_auth(array $p): bool
{
    if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'logout' && epg_csrf_ok()) {
        $_SESSION = [];
        session_regenerate_id(true);
        header('Location: ./', true, 303);
        exit;
    }
    if ($_SERVER['REQUEST_METHOD'] === 'POST' && in_array($_POST['action'] ?? '', ['login', 'first_password'], true)) {
        if (!epg_csrf_ok()) {
            epg_flash('error', 'The form expired. Please try again.');
            epg_redirect_self();
        }
        $username = trim((string)($_POST['username'] ?? ''));
        if ($_POST['action'] === 'login') {
            $u = epg_find_user($p, $username);
            if (epg_login_blocked($p, $username)) {
                epg_flash('error', 'Too many failed attempts. Wait 15 minutes and try again.');
            } elseif ($u !== null && !$u['disabled'] && password_verify((string)($_POST['password'] ?? ''), $u['hash'])) {
                epg_login_as($u);
                epg_note_login($p, $u['username']);
            } else {
                epg_login_failed($p, $username);
                sleep(1);
                epg_flash('error', 'Invalid username or password.');
            }
        } elseif (!epg_users($p)) {
            $new = (string)($_POST['new_password'] ?? '');
            $err = epg_check_username($username) ?: epg_check_password($new, (string)($_POST['confirm_password'] ?? ''));
            if ($err !== '') {
                epg_flash('error', $err);
            } else {
                $u = ['username' => $username, 'hash' => password_hash($new, PASSWORD_DEFAULT), 'role' => 'admin', 'disabled' => false, 'created' => time(), 'last_login' => time()];
                if (epg_save_users($p, [$u])) {
                    epg_login_as($u);
                    epg_flash('ok', 'Welcome, ' . $username . '. Your account is ready.');
                } else {
                    epg_flash('error', 'Could not save the account: neither the data folder nor this web folder is writable by PHP (' . epg_fpm_user()['name'] . ').');
                }
            }
        }
        $next = epg_safe_next((string)($_POST['next'] ?? ''));
        if ($next !== '' && epg_authed($p)) {
            header('Location: ' . $next, true, 303);
            exit;
        }
        if ($next !== '') {
            header('Location: ' . strtok((string)($_SERVER['REQUEST_URI'] ?? '/'), '?') . '?next=' . rawurlencode($next), true, 303);
            exit;
        }
        epg_redirect_self();
    }
    return epg_authed($p);
}

/** A local page to return to after login ("guide.php", "index.php#history"), or '' if not safe. */
function epg_safe_next(string $next): string
{
    return preg_match('/^[a-z0-9_-]+\.php(\?[a-z0-9_=&-]*)?(#[a-z0-9_-]*)?$/i', $next) ? $next : '';
}

/** Gate for every page: logged-out visitors get the full-page login, returning here afterwards. */
function epg_require_login(array $p): void
{
    if (epg_handle_auth($p)) {
        return;
    }
    if (!isset($_GET['next'])) {
        $_GET['next'] = basename((string)($_SERVER['SCRIPT_NAME'] ?? ''));
    }
    epg_login_page($p);
}

/** The app icon as a data: URL for the browser tab. */
function epg_favicon(): string
{
    $svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#3b7bd6"/>'
        . '<g fill="none" stroke="#fff" stroke-width="2.2" stroke-linecap="round" transform="translate(4 4)">'
        . '<circle cx="12" cy="12" r="2" fill="#fff"/><path d="M16.24 7.76a6 6 0 0 1 0 8.48M7.76 16.24a6 6 0 0 1 0-8.48M19.07 4.93a10 10 0 0 1 0 14.14M4.93 19.07a10 10 0 0 1 0-14.14"/></g></svg>';
    return 'data:image/svg+xml,' . rawurlencode($svg);
}

/** Standalone login / first-user page (no sidebar). Ends the request. */
function epg_login_page(array $p): never
{
    $flashes = epg_take_flashes();
    echo '<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">'
        . '<meta name="viewport" content="width=device-width, initial-scale=1">'
        . '<title>' . EPG_APP_NAME . ' · ' . (epg_users($p) ? 'Log in' : 'Create your account') . '</title>'
        . '<link rel="icon" type="image/svg+xml" href="assets/favicon.svg">'
        . '<link rel="icon" href="' . h(epg_favicon()) . '">'
        . '<link rel="stylesheet" href="assets/app.css?v=' . (int)@filemtime(__DIR__ . '/assets/app.css') . '">'
        . '</head><body class="auth-page"><main class="auth-wrap">'
        . '<div class="auth-brand"><span class="brand-mark">' . epg_icon('logo') . '</span><span>' . EPG_APP_NAME . '</span></div>';
    foreach ($flashes as [$type, $msg]) {
        echo '<div class="flash ' . h((string)$type) . '" role="alert">' . h((string)$msg) . '</div>';
    }
    echo epg_auth_form($p)
        . '</main></body></html>';
    exit;
}

function epg_auth_form(array $p): string
{
    $f = epg_csrf_field();
    $next = epg_safe_next((string)($_GET['next'] ?? ''));
    if ($next !== '') {
        $f .= '<input type="hidden" name="next" value="' . h($next) . '">';
    }
    if (!epg_users($p)) {
        return '<div class="card"><h2>Create your account</h2>'
            . '<p>This account protects every page. The first person to open this page creates it; more users can be added in Settings.</p>'
            . '<form method="post">' . $f . '<input type="hidden" name="action" value="first_password">'
            . '<label for="un">Username</label><input type="text" id="un" name="username" autocomplete="username" required autofocus maxlength="32">'
            . '<label for="np">Password (at least 8 characters)</label><input type="password" id="np" name="new_password" minlength="8" autocomplete="new-password" required>'
            . '<label for="cp">Repeat it</label><input type="password" id="cp" name="confirm_password" autocomplete="new-password" required>'
            . '<button>Create account</button></form></div>';
    }
    return '<div class="card"><h2>Log in</h2><form method="post">' . $f . '<input type="hidden" name="action" value="login">'
        . '<label for="un">Username</label><input type="text" id="un" name="username" autocomplete="username" required autofocus maxlength="32">'
        . '<label for="pw">Password</label><input type="password" id="pw" name="password" autocomplete="current-password" required>'
        . '<button>Log in</button></form></div>';
}

// ---------------------------------------------------------------- locks + background jobs

function epg_lock_held(string $file): bool
{
    if (!is_file($file)) {
        return false;
    }
    $fh = @fopen($file, 'r');
    if ($fh === false) {
        return false;
    }
    $free = flock($fh, LOCK_EX | LOCK_NB);
    if ($free) {
        flock($fh, LOCK_UN);
    }
    fclose($fh);
    return !$free;
}

function epg_build_running(array $p): bool
{
    return $p['data_dir'] !== '' && epg_lock_held($p['data_dir'] . '/build.lock');
}

function epg_logs_dir(array $p): string
{
    return $p['data_dir'] . '/logs';
}

function epg_job_running(array $p): bool
{
    return $p['data_dir'] !== '' && epg_lock_held(epg_logs_dir($p) . '/job.lock');
}

/**
 * Start a background job. $cmd is a complete shell command whose parts were
 * already escaped with escapeshellarg. Its output goes to logs/<name>.log in
 * the data dir. Returns "" or an error message.
 */
function epg_start_job(array $p, string $name, string $label, string $cmd, string $cwd = ''): string
{
    if (!epg_can_exec()) {
        return 'PHP cannot run programs here (exec/proc_open are disabled).';
    }
    if ($p['data_dir'] === '' || !is_dir($p['data_dir'])) {
        return 'Create the data folder first.';
    }
    $logs = epg_logs_dir($p);
    if (!is_dir($logs) && !@mkdir($logs, 02775, true)) {
        return 'Cannot create ' . $logs . '.';
    }
    if (epg_job_running($p)) {
        return 'Another setup task is still running. Wait for it to finish.';
    }
    $log = $logs . '/' . $name . '.log';
    $lock = $logs . '/job.lock';
    $pidFile = $logs . '/job.pid';
    @touch($lock);
    @chmod($lock, 0664);
    @unlink($pidFile);
    @unlink(epg_cancel_flag($p));
    // With setsid the job gets its own process group, so Cancel can stop it and everything it started.
    $setsid = epg_which('setsid') !== '';
    file_put_contents($logs . '/job.json', json_encode(['name' => $name, 'label' => $label, 'started' => time(), 'log' => $name . '.log', 'group' => $setsid]));
    $inner = ($cwd !== '' ? 'cd ' . escapeshellarg($cwd) . ' && ' : '') . $cmd
        . '; echo; echo "[finished with exit code $?]"';
    $runner = epg_which('flock') !== ''
        ? 'flock -n ' . escapeshellarg($lock) . ' sh -c ' . escapeshellarg($inner)
        : 'sh -c ' . escapeshellarg($inner);
    // The wrapper writes its PID, then becomes the runner (exec keeps the PID).
    $wrapped = 'sh -c ' . escapeshellarg('echo $$ > ' . escapeshellarg($pidFile) . '; exec ' . $runner);
    $full = 'umask 002; nohup ' . ($setsid ? 'setsid ' : '') . $wrapped . ' > ' . escapeshellarg($log) . ' 2>&1 < /dev/null &';
    $out = [];
    $rc = 0;
    @exec($full, $out, $rc);
    return $rc === 0 ? '' : 'Could not start the task (exit code ' . $rc . ').';
}

function epg_job_status(array $p): array
{
    $logs = epg_logs_dir($p);
    $meta = is_file($logs . '/job.json') ? json_decode((string)file_get_contents($logs . '/job.json'), true) : null;
    if (!is_array($meta)) {
        return ['running' => false, 'cancelled' => false, 'name' => '', 'label' => '', 'log' => '', 'exit' => null, 'started' => 0];
    }
    $log = epg_tail($logs . '/' . basename((string)$meta['log']), 200);
    $exit = null;
    if (preg_match('/\[finished with exit code (\d+)\]\s*$/', $log, $m)) {
        $exit = (int)$m[1];
    }
    $locked = epg_which('flock') !== '' ? epg_job_running($p) : (time() - (int)$meta['started'] < 7200);
    // A recorded PID that is gone means the job is over, whatever the lock says.
    $pid = epg_job_pid($p);
    if ($locked && $pid > 0 && !epg_pid_alive($pid)) {
        $locked = false;
    }
    return [
        'running' => $locked && $exit === null,
        'cancelled' => !empty($meta['cancelled']) || str_contains($log, '[cancelled'),
        'name' => (string)$meta['name'],
        'label' => (string)$meta['label'],
        'started' => (int)$meta['started'],
        'log' => $log,
        'exit' => $exit,
    ];
}

// ---------------------------------------------------------------- cancel

/** When this file exists, a running build stops cleanly (the program checks it every second). */
function epg_cancel_flag(array $p): string
{
    return $p['data_dir'] !== '' ? $p['data_dir'] . '/cancel' : '';
}

function epg_job_pid(array $p): int
{
    $f = epg_logs_dir($p) . '/job.pid';
    return is_readable($f) ? (int)trim((string)file_get_contents($f)) : 0;
}

function epg_pid_alive(int $pid): bool
{
    if ($pid <= 1) {
        return false;
    }
    if (@is_dir('/proc/self')) {
        $st = @file_get_contents("/proc/$pid/stat");
        if (!is_string($st)) {
            return false;
        }
        return !preg_match('/\)\s+Z\s/', $st); // a zombie has already exited
    }
    return function_exists('posix_kill') && posix_kill($pid, 0);
}

function epg_proc_cmdline(int $pid): string
{
    $c = $pid > 1 ? @file_get_contents("/proc/$pid/cmdline") : false;
    return is_string($c) ? str_replace("\0", ' ', $c) : '';
}

/** Send a signal to a process, or to its whole process group. */
function epg_signal(int $pid, bool $group, int $sig): bool
{
    $target = $group ? -$pid : $pid;
    if (function_exists('posix_kill')) {
        return posix_kill($target, $sig);
    }
    $out = [];
    $rc = 1;
    @exec('kill -' . $sig . ' -- ' . $target . ' 2>/dev/null', $out, $rc);
    return $rc === 0;
}

/** Wait until none of the PIDs is alive; true when they are all gone. */
function epg_wait_gone(array $pids, float $secs): bool
{
    $end = microtime(true) + $secs;
    while (true) {
        $alive = array_filter($pids, 'epg_pid_alive');
        if (!$alive) {
            return true;
        }
        if (microtime(true) >= $end) {
            return false;
        }
        usleep(200000);
    }
}

/** Is there anything Cancel could stop? */
function epg_anything_running(array $p): bool
{
    return $p['data_dir'] !== '' && (epg_job_status($p)['running'] || epg_build_running($p));
}

/**
 * Stop the running background task and/or a guide build started by the
 * schedule. A build is first asked to stop through the cancel file (it then
 * saves its TMDB results and leaves epg.xml as it was); anything still
 * running gets SIGTERM, then SIGKILL. Only processes whose command line shows
 * they are ours are signalled. Returns "" or an error message.
 */
function epg_cancel(array $p): string
{
    if ($p['data_dir'] === '') {
        return 'There is no data folder, so nothing can be running.';
    }
    $logs = epg_logs_dir($p);
    $meta = json_decode((string)@file_get_contents($logs . '/job.json'), true);
    $meta = is_array($meta) ? $meta : [];
    $job = epg_job_status($p);
    $targets = []; // pid => [process group?, is a guide build?]
    $jobPid = 0;
    if ($job['running']) {
        $pid = epg_job_pid($p);
        if ($pid > 0 && str_contains(epg_proc_cmdline($pid), '[finished with exit code')) {
            $targets[$pid] = [!empty($meta['group']), $job['name'] === 'guide-build'];
            $jobPid = $pid;
        } elseif ($pid > 0 && epg_pid_alive($pid)) {
            return 'PID ' . $pid . ' does not look like a task started by these pages, so it was not stopped.';
        }
    }
    // A build started by the schedule (or inside the job) holds build.lock with its PID.
    if (epg_build_running($p)) {
        $bpid = (int)trim((string)@file_get_contents($p['data_dir'] . '/build.lock'));
        $cl = epg_proc_cmdline($bpid);
        if ($bpid > 0 && !isset($targets[$bpid]) && str_contains($cl, 'epg-loader') && preg_match('/\s(build|enrich)(\s|$)/', $cl)) {
            $targets[$bpid] = [false, true];
        }
    }
    if (!$targets) {
        @unlink(epg_cancel_flag($p));
        return epg_anything_running($p) ? 'The running process could not be identified, so it was not stopped.' : 'Nothing is running.';
    }
    $pids = array_keys($targets);
    if (array_filter($targets, fn($t) => $t[1])) {
        @file_put_contents(epg_cancel_flag($p), (string)time());
        epg_wait_gone($pids, 8);
    }
    foreach ($targets as $pid => [$group]) {
        if (epg_pid_alive($pid)) {
            epg_signal($pid, $group, 15);
        }
    }
    if (!epg_wait_gone($pids, 5)) {
        foreach ($targets as $pid => [$group]) {
            if (epg_pid_alive($pid)) {
                epg_signal($pid, $group, 9);
            }
        }
        epg_wait_gone($pids, 2);
    }
    @unlink(epg_cancel_flag($p));
    if ($jobPid > 0) {
        $meta['cancelled'] = true;
        @file_put_contents($logs . '/job.json', json_encode($meta));
        $log = $logs . '/' . basename((string)($meta['log'] ?? ''));
        if (is_file($log) && !preg_match('/\[finished with exit code \d+\]\s*$/', epg_tail($log, 3))) {
            @file_put_contents($log, "\n[cancelled at " . date('Y-m-d H:i:s T') . "]\n[finished with exit code 130]\n", FILE_APPEND);
        }
    }
    return array_filter($pids, 'epg_pid_alive') ? 'Some processes did not stop: ' . implode(', ', array_filter($pids, 'epg_pid_alive')) : '';
}

/** The Cancel button. Pages handle action=cancel with epg_cancel(). */
function epg_cancel_form(bool $hidden = false): string
{
    return '<form method="post" class="inline cancel-form"' . ($hidden ? ' hidden' : '') . ' onsubmit="return confirm(\'Stop the running task?\')">'
        . epg_csrf_field() . '<input type="hidden" name="action" value="cancel">'
        . '<button class="danger">Cancel</button></form>';
}

function epg_tail(string $file, int $lines = 40): string
{
    if (!is_readable($file)) {
        return '';
    }
    $size = (int)(filesize($file) ?: 0);
    $fh = fopen($file, 'r');
    if ($fh === false) {
        return '';
    }
    fseek($fh, max(0, $size - 65536));
    $data = (string)stream_get_contents($fh);
    fclose($fh);
    $rows = preg_split('/\r?\n/', rtrim($data)) ?: [];
    return implode("\n", array_slice($rows, -$lines));
}

/** Start "epg-loader build" in the background (used by both pages). */
/** Build every guide, or only $slug when the program supports --guide. */
function epg_start_build(array $p, string $slug = '', string $name = ''): string
{
    if ($p['bin'] === '') {
        return 'The epg-loader program has not been built yet. Use the setup page.';
    }
    if (epg_build_running($p)) {
        return 'A build is already running.';
    }
    $cmd = epg_env_prefix(epg_bin_env($p)) . escapeshellarg($p['bin']) . ' build';
    $label = 'Guide build';
    if ($slug !== '' && epg_bin_supports_flag($p, '--guide')) {
        $cmd .= ' --guide ' . escapeshellarg($slug);
        $label = 'Guide build: ' . ($name !== '' ? $name : $slug);
    }
    return epg_start_job($p, 'guide-build', $label, $cmd);
}

/** Does the installed program's --help mention this option? (Same cache as epg_bin_supports.) */
function epg_bin_supports_flag(array $p, string $flag): bool
{
    epg_bin_supports($p, 'build');
    $cache = $p['data_dir'] !== '' ? epg_logs_dir($p) . '/bin-help.json' : '';
    $c = $cache !== '' && is_readable($cache) ? json_decode((string)file_get_contents($cache), true) : null;
    return is_array($c) && str_contains((string)($c['help'] ?? ''), $flag);
}

// ---------------------------------------------------------------- guides (several sources, one output guide each)

function epg_has_table(PDO $db, string $table): bool
{
    $st = $db->prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?");
    $st->execute([$table]);
    return (bool)$st->fetchColumn();
}

function epg_has_column(PDO $db, string $table, string $column): bool
{
    foreach ($db->query('PRAGMA table_info(' . $db->quote($table) . ')') as $r) {
        if ((string)$r['name'] === $column) {
            return true;
        }
    }
    return false;
}

/** True once the program has created the guides table, so guides can be added and removed. */
function epg_guides_editable(PDO $db): bool
{
    return epg_has_table($db, 'guides');
}

/**
 * All guides in display order. Until the program creates the guides table,
 * the single source in the settings is shown as one guide called "Main".
 */
function epg_guides(PDO $db): array
{
    if (epg_guides_editable($db)) {
        $out = [];
        foreach ($db->query('SELECT * FROM guides ORDER BY sort_order, id') as $r) {
            $out[] = [
                'id' => (int)$r['id'],
                'name' => (string)$r['name'],
                'slug' => (string)$r['slug'],
                'source_url' => (string)$r['source_url'],
                'days' => $r['days'] === null ? '' : (string)$r['days'],
                'refresh_hours' => $r['refresh_hours'] === null ? '' : (string)$r['refresh_hours'],
                'logo_country' => (string)($r['logo_country'] ?? ''),
                'enabled' => (int)$r['enabled'] === 1,
                'output_slug' => (string)($r['output_slug'] ?? ''),
                'legacy' => false,
            ];
        }
        return $out;
    }
    $s = [];
    foreach ($db->query("SELECT key, value FROM settings WHERE key IN ('source_url', 'days', 'refresh_hours', 'logo_country', 'main_guide_name')") as $r) {
        $s[$r['key']] = (string)$r['value'];
    }
    return [[
        'id' => 1,
        'name' => trim($s['main_guide_name'] ?? '') !== '' ? trim($s['main_guide_name']) : 'Main',
        'slug' => epg_slugify(trim($s['main_guide_name'] ?? '') !== '' ? trim($s['main_guide_name']) : 'Main'),
        'source_url' => $s['source_url'] ?? '',
        'days' => $s['days'] ?? '',
        'refresh_hours' => '',
        'logo_country' => '',
        'enabled' => true,
        'legacy' => true,
    ]];
}

function epg_guide_by_slug(array $guides, string $slug): ?array
{
    foreach ($guides as $g) {
        if ($g['slug'] === $slug) {
            return $g;
        }
    }
    return null;
}

/** "US Locals!" -> "us-locals". Never empty. */
function epg_slugify(string $name): string
{
    $s = trim((string)preg_replace('/[^a-z0-9]+/', '-', strtolower($name)), '-');
    return $s !== '' ? substr($s, 0, 40) : 'guide';
}

/** Where a guide is written and served: guides/<slug>.xml, named after the guide. Before the program
 * has the guides table, the one guide is still written as epg.xml. */
function epg_guide_files(array $p, array $g): array
{
    if (!empty($g['legacy'])) {
        $rel = 'epg.xml';
        $status = 'status.json';
    } else {
        $rel = 'guides/' . $g['slug'] . '.xml';
        $status = 'guides/status-' . (int)$g['id'] . '.json';
    }
    return [
        'xml' => $p['web_dir'] . '/' . $rel,
        'gz' => $p['web_dir'] . '/' . $rel . '.gz',
        'status' => $p['web_dir'] . '/' . $status,
        'url' => epg_base_url() . '/' . $rel,
        'gz_url' => epg_base_url() . '/' . $rel . '.gz',
    ];
}

/** The guide's last status.json (written by the program), or []. */
function epg_guide_status(array $p, array $g): array
{
    $f = epg_guide_files($p, $g)['status'];
    $s = is_readable($f) ? json_decode((string)file_get_contents($f), true) : null;
    return is_array($s) ? $s : [];
}

/** The host of a source URL for display (never the full URL: it may hold a password). */
function epg_source_label(string $src): string
{
    if ($src === '') {
        return 'no source set';
    }
    $host = parse_url($src, PHP_URL_HOST);
    return is_string($host) && $host !== '' ? $host : 'file on the server';
}

// ---------------------------------------------------------------- background TMDB lookups (epg-loader enrich)

/** Progress written by "epg-loader enrich", plus whether it is running right now. */
function epg_enrich_status(array $p): array
{
    $st = [];
    if ($p['data_dir'] !== '' && is_readable($p['data_dir'] . '/enrich_status.json')) {
        $st = json_decode((string)file_get_contents($p['data_dir'] . '/enrich_status.json'), true);
        $st = is_array($st) ? $st : [];
    }
    $st['active'] = $p['data_dir'] !== '' && epg_lock_held($p['data_dir'] . '/enrich.lock');
    return $st;
}

/**
 * Does the installed epg-loader know this subcommand? Reads its --help once
 * and caches the answer in the data folder, keyed by the binary's mtime.
 */
function epg_bin_supports(array $p, string $cmd): bool
{
    if ($p['bin'] === '' || !is_file($p['bin'])) {
        return false;
    }
    $mtime = (int)@filemtime($p['bin']);
    $cache = $p['data_dir'] !== '' ? epg_logs_dir($p) . '/bin-help.json' : '';
    $c = $cache !== '' && is_readable($cache) ? json_decode((string)file_get_contents($cache), true) : null;
    if (!is_array($c) || (int)($c['mtime'] ?? -1) !== $mtime || (string)($c['bin'] ?? '') !== $p['bin']) {
        if (!epg_can_exec()) {
            return false;
        }
        $out = [];
        $rc = 1;
        @exec(escapeshellarg($p['bin']) . ' --help 2>&1', $out, $rc);
        $c = ['bin' => $p['bin'], 'mtime' => $mtime, 'help' => implode("\n", $out)];
        if ($cache !== '' && (is_dir(dirname($cache)) || @mkdir(dirname($cache), 02775, true))) {
            @file_put_contents($cache, json_encode($c));
        }
    }
    return (bool)preg_match('/^\s*' . preg_quote($cmd, '/') . '\b/m', (string)$c['help']);
}

/** Start background TMDB lookups. Returns "" or an error message. */
function epg_start_enrich(array $p): string
{
    if ($p['bin'] === '') {
        return 'The epg-loader program has not been built yet. Use the setup page.';
    }
    if (!epg_bin_supports($p, 'enrich')) {
        return 'The installed program is older than the background lookups. Open Setup and press Build the program first.';
    }
    if (!epg_can_exec()) {
        return 'PHP cannot run programs here (exec/proc_open are disabled).';
    }
    if (epg_enrich_status($p)['active']) {
        return 'Background TMDB lookups are already running.';
    }
    $logs = epg_logs_dir($p);
    if (!is_dir($logs) && !@mkdir($logs, 02775, true)) {
        return 'Cannot create ' . $logs . '.';
    }
    @unlink($p['data_dir'] . '/enrich-cancel');
    $cmd = epg_env_prefix(epg_bin_env($p)) . escapeshellarg($p['bin']) . ' enrich';
    $full = 'umask 002; nohup ' . (epg_which('setsid') !== '' ? 'setsid ' : '') . 'sh -c ' . escapeshellarg($cmd)
        . ' > ' . escapeshellarg($logs . '/enrich.log') . ' 2>&1 < /dev/null &';
    $out = [];
    $rc = 0;
    @exec($full, $out, $rc);
    return $rc === 0 ? '' : 'Could not start the lookups (exit code ' . $rc . ').';
}

/** Ask background TMDB lookups to stop (results so far are kept). Returns "" or an error message. */
function epg_stop_enrich(array $p): string
{
    if (!epg_enrich_status($p)['active']) {
        return 'Background TMDB lookups are not running.';
    }
    @file_put_contents($p['data_dir'] . '/enrich-cancel', (string)time());
    $end = microtime(true) + 8;
    while (microtime(true) < $end) {
        if (!epg_enrich_status($p)['active']) {
            return '';
        }
        usleep(250000);
    }
    $pid = (int)trim((string)@file_get_contents($p['data_dir'] . '/enrich.lock'));
    $cl = epg_proc_cmdline($pid);
    if ($pid > 1 && str_contains($cl, 'epg-loader') && preg_match('/\senrich(\s|$)/', $cl)) {
        epg_signal($pid, false, 15);
        return '';
    }
    return 'The lookups were asked to stop; they stop within a few seconds.';
}

// ---------------------------------------------------------------- connection tests

function epg_check_file(array $p): string
{
    return $p['data_dir'] !== '' ? $p['data_dir'] . '/check.json' : '';
}

/**
 * Start "epg-loader check" in the background. The old result is deleted
 * first, so a failed or unfinished test can never show an old result.
 */
function epg_start_check(array $p): string
{
    if ($p['bin'] === '') {
        return 'Build the program first.';
    }
    $out = epg_check_file($p);
    if (epg_job_status($p)['running']) {
        return 'Another task is still running. Wait for it to finish.';
    }
    @unlink($out);
    @unlink($p['data_dir'] . '/check.pending');
    $cmd = epg_env_prefix(epg_bin_env($p)) . escapeshellarg($p['bin']) . ' check > ' . escapeshellarg($out . '.tmp')
        . ' && mv ' . escapeshellarg($out . '.tmp') . ' ' . escapeshellarg($out) . ' && cat ' . escapeshellarg($out);
    return epg_start_job($p, 'check', 'Test internet, TMDB key and source URL', $cmd);
}

/** Where the source test of one guide is written. */
function epg_guide_check_file(array $p, string $slug): string
{
    return $p['data_dir'] !== '' ? $p['data_dir'] . '/check-' . epg_slugify($slug) . '.json' : '';
}

/** Test one guide's XMLTV source ("epg-loader check --guide slug") in the background. */
function epg_start_guide_check(array $p, string $slug): string
{
    if ($p['bin'] === '') {
        return 'Build the program first.';
    }
    if (!epg_bin_supports_flag($p, '--guide')) {
        return 'Testing a single guide needs the new program: open Setup and press Build the program.';
    }
    if (epg_job_status($p)['running']) {
        return 'Another task is still running. Wait for it to finish.';
    }
    $out = epg_guide_check_file($p, $slug);
    @unlink($out);
    $cmd = epg_env_prefix(epg_bin_env($p)) . escapeshellarg($p['bin']) . ' check --guide ' . escapeshellarg($slug)
        . ' > ' . escapeshellarg($out . '.tmp') . ' && mv ' . escapeshellarg($out . '.tmp') . ' ' . escapeshellarg($out);
    return epg_start_job($p, 'check-guide', 'Test guide source', $cmd);
}

/** The last source test of one guide: ['ok' => bool, 'detail' => string, 'at' => unix] or null. */
function epg_guide_check(array $p, string $slug): ?array
{
    $f = epg_guide_check_file($p, $slug);
    $j = $f !== '' && is_readable($f) ? json_decode((string)file_get_contents($f), true) : null;
    if (!is_array($j) || !isset($j['source']) || !is_array($j['source'])) {
        return null;
    }
    return ['ok' => !empty($j['source']['ok']), 'detail' => (string)($j['source']['detail'] ?? ''), 'at' => (int)($j['checked_at'] ?? 0)];
}

/**
 * Settings changed: drop the old test result and test again now, or as soon
 * as the running task has finished (see epg_run_pending_check).
 */
function epg_queue_check(array $p): void
{
    if ($p['data_dir'] === '' || !is_dir($p['data_dir'])) {
        return;
    }
    @unlink(epg_check_file($p));
    if ($p['bin'] === '' || epg_start_check($p) !== '') {
        @touch($p['data_dir'] . '/check.pending');
    }
}

/** Start a queued test once nothing else is running. */
function epg_run_pending_check(array $p): void
{
    if ($p['data_dir'] !== '' && is_file($p['data_dir'] . '/check.pending') && $p['bin'] !== '' && !epg_job_status($p)['running']) {
        epg_start_check($p);
    }
}

// ---------------------------------------------------------------- schedule (crontab)

function epg_cron_tag(): string
{
    return '# epg-loader ' . substr(sha1(__DIR__), 0, 10);
}

function epg_cron_lines(): ?array
{
    if (epg_which('crontab') === '') {
        return null;
    }
    $out = [];
    $rc = 0;
    @exec('crontab -l 2>/dev/null', $out, $rc);
    return $rc === 0 ? $out : [];
}

function epg_cron_installed(): bool
{
    $lines = epg_cron_lines();
    if (!$lines) {
        return false;
    }
    foreach ($lines as $l) {
        if (str_contains($l, epg_cron_tag())) {
            return true;
        }
    }
    return false;
}

function epg_cron_write(array $lines): string
{
    $proc = proc_open('crontab -', [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']], $pipes);
    if (!is_resource($proc)) {
        return 'Could not run crontab.';
    }
    fwrite($pipes[0], implode("\n", $lines) . "\n");
    fclose($pipes[0]);
    $out = stream_get_contents($pipes[1]) . stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    $rc = proc_close($proc);
    return $rc === 0 ? '' : 'crontab refused the change: ' . trim((string)$out);
}

/** The cron line: every 15 minutes, and the binary itself waits for refresh_hours. */
function epg_cron_line(array $p): string
{
    $cmd = epg_env_prefix(epg_bin_env($p)) . escapeshellarg($p['bin']) . ' build --if-due'
        . ' >> ' . escapeshellarg(epg_logs_dir($p) . '/cron.log') . ' 2>&1';
    // % is special in crontab.
    return '*/15 * * * * ' . str_replace('%', '\%', $cmd) . ' ' . epg_cron_tag();
}

function epg_cron_install(array $p): string
{
    if ($p['bin'] === '') {
        return 'Build the program first.';
    }
    $lines = epg_cron_lines();
    if ($lines === null) {
        return 'The crontab command is not available on this server.';
    }
    $keep = array_values(array_filter($lines, fn($l) => !str_contains($l, epg_cron_tag())));
    $keep[] = epg_cron_line($p);
    return epg_cron_write($keep);
}

function epg_cron_remove(): string
{
    $lines = epg_cron_lines();
    if ($lines === null) {
        return 'The crontab command is not available on this server.';
    }
    $keep = array_values(array_filter($lines, fn($l) => !str_contains($l, epg_cron_tag())));
    return epg_cron_write($keep);
}

// ---------------------------------------------------------------- setup state

/**
 * Cheap "is setup complete?" for every page load: the program exists, the
 * database works, a source URL is saved, a build has succeeded, a schedule
 * exists, and the last full check on setup.php found no failures.
 */
function epg_setup_state(array $p): array
{
    static $cache = null;
    if ($cache !== null) {
        return $cache;
    }
    $saved = [];
    if ($p['data_dir'] !== '' && is_readable($p['data_dir'] . '/setup-state.json')) {
        $saved = json_decode((string)file_get_contents($p['data_dir'] . '/setup-state.json'), true) ?: [];
    }
    $fails = (int)($saved['fails'] ?? 0);
    $warns = (int)($saved['warns'] ?? 0);
    $reason = '';
    if ($p['bin'] === '') {
        $reason = 'the program has not been built';
    } else {
        try {
            $db = epg_db($p);
            $src = epg_has_table($db, 'guides')
                ? $db->query("SELECT source_url FROM guides WHERE enabled = 1 AND source_url <> '' LIMIT 1")->fetchColumn()
                : $db->query("SELECT value FROM settings WHERE key = 'source_url'")->fetchColumn();
            $ok = $db->query('SELECT COUNT(*) FROM runs WHERE ok = 1')->fetchColumn();
            if (!is_string($src) || trim($src) === '') {
                $reason = 'no XMLTV source is saved';
            } elseif ((int)$ok === 0) {
                $reason = 'no guide build has succeeded yet';
            } elseif (!epg_cron_installed() && empty($saved['watch'])) {
                $reason = 'no schedule is installed';
            } elseif ($fails > 0) {
                $reason = $fails . ' check(s) failed';
            }
        } catch (Throwable $e) {
            $reason = 'the database is not ready';
        }
    }
    $cache = ['complete' => $reason === '', 'reason' => $reason, 'fails' => $fails, 'warns' => $warns];
    return $cache;
}

function epg_save_setup_state(array $p, array $state): void
{
    if ($p['data_dir'] === '' || !is_dir($p['data_dir'])) {
        return;
    }
    $f = $p['data_dir'] . '/setup-state.json';
    if (@file_put_contents($f . '.tmp', json_encode($state + ['checked_at' => time()])) !== false) {
        @rename($f . '.tmp', $f);
    }
}

// ---------------------------------------------------------------- layout

function epg_icon(string $name): string
{
    $paths = [
        'logo' => '<circle cx="12" cy="12" r="2"/><path d="M16.24 7.76a6 6 0 0 1 0 8.48M7.76 16.24a6 6 0 0 1 0-8.48M19.07 4.93a10 10 0 0 1 0 14.14M4.93 19.07a10 10 0 0 1 0-14.14"/>',
        'dashboard' => '<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/>',
        'settings' => '<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>',
        'setup' => '<path d="M9 3h6a1 1 0 0 1 1 1v2H8V4a1 1 0 0 1 1-1z"/><path d="M16 5h2a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h2"/><path d="m9 14 2 2 4-4"/>',
        'file' => '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M12 11v6M9 14l3 3 3-3"/>',
        'history' => '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5M12 7v5l3 2"/>',
        'logout' => '<path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4M10 17l5-5-5-5M15 12H3"/>',
        'copy' => '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>',
        'play' => '<path d="M7 4v16l13-8z"/>',
        'eye' => '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
        'edit' => '<path d="M4 20h4L19 9l-4-4L4 16v4z"/><path d="m13.5 6.5 4 4"/>',
        'tv' => '<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 21h8M12 17v4"/>',
        'clock' => '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
        'image' => '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="2"/><path d="m21 16-5-5-9 9"/>',
        'list' => '<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>',
        'check' => '<path d="M20 6 9 17l-5-5"/>',
        'pulse' => '<path d="M3 12h4l3-8 4 16 3-8h4"/>',
        'grid' => '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M8 9v11M14 9v11M8 14.5h13"/>',
        'search' => '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
        'x' => '<path d="M18 6 6 18M6 6l12 12"/>',
        'user' => '<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>',
        'login' => '<path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4M10 17l5-5-5-5M15 12H3"/>',
    ];
    $d = $paths[$name] ?? '';
    return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' . $d . '</svg>';
}

/**
 * Page start: head, sidebar, page heading and flash messages.
 * $active is one of dashboard, guide, settings, setup.
 */
function epg_layout_start(array $p, string $active, string $title, string $sub = '', array $flashes = [], string $headRight = ''): void
{
    $state = epg_setup_state($p);
    // Setup only shows until the first-time setup is done; after that it lives in Settings › Advanced.
    $showSetup = $active === 'setup' || !epg_setup_done($p);
    $item = function (string $key, string $href, string $label, string $icon, string $extra = '') use ($active) {
        $cls = 'item' . ($key === $active ? ' active' : '');
        $cur = $key === $active ? ' aria-current="page"' : '';
        return '<a class="' . $cls . '" href="' . h($href) . '"' . $cur . ' title="' . h($label) . '">'
            . epg_icon($icon) . '<span class="label">' . h($label) . '</span>' . $extra . '</a>';
    };
    $badge = '';
    if ($state['fails'] > 0) {
        $badge = '<span class="badge fail" title="' . h($state['fails'] . ' failing check(s)') . '">' . (int)$state['fails'] . '</span>';
    } elseif (!$state['complete']) {
        $badge = '<span class="badge" title="' . h('Setup is not finished: ' . $state['reason']) . '">!</span>';
    }
    echo '<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">'
        . '<meta name="viewport" content="width=device-width, initial-scale=1">'
        . '<title>' . EPG_APP_NAME . ' · ' . h($title) . '</title>'
        . '<link rel="icon" type="image/svg+xml" href="assets/favicon.svg">'
        . '<link rel="icon" href="' . h(epg_favicon()) . '">'
        . '<link rel="apple-touch-icon" href="assets/favicon.svg">'
        . '<link rel="stylesheet" href="assets/app.css?v=' . (int)@filemtime(__DIR__ . '/assets/app.css') . '">'
        . '</head><body><div class="layout">'
        . '<nav class="nav" aria-label="Main">'
        . '<div class="brand"><span class="brand-mark">' . epg_icon('logo') . '</span><span>' . EPG_APP_NAME . '</span></div>'
        . $item('dashboard', 'index.php', 'Dashboard', 'dashboard')
        . $item('guides', 'guides.php', 'Guides', 'list')
        . $item('guide', 'guide.php', 'TV Guide', 'grid')
        . $item('activity', 'activity.php', 'Activity', 'pulse')
        . $item('settings', 'settings.php', 'Settings', 'settings')
        . ($showSetup ? $item('setup', 'setup.php', 'Setup', 'setup', $badge) : '')
        . '<div class="nav-spacer"></div>'
        . epg_nav_auth($p)
        . '</nav><div class="content">'
        // No page title or description: the sidebar shows where you are. Page actions (search, "+ Add guide") stay.
        . ($headRight !== '' ? '<div class="page-head page-tools">' . $headRight . '</div>' : '<div class="page-top"></div>')
        . '<main class="page-' . h($active) . ($active === 'guide' ? ' wide' : '') . '">';
    if ($active !== 'setup' && epg_authed($p) && epg_program_outdated($p)) {
        echo '<div class="flash warn outdated-banner"><strong>The epg-loader program is out of date.</strong> '
            . 'The source code is newer than the installed program. <a href="settings.php#advanced">Build the program</a> in Settings › Advanced.</div>';
    }
    foreach ($flashes as [$type, $msg]) {
        echo '<div class="flash ' . h((string)$type) . '">' . h((string)$msg) . '</div>';
    }
}

/** True once the first-time setup has been completed (it then stays out of the sidebar). */
function epg_setup_done(array $p): bool
{
    if ($p['data_dir'] === '' || !is_readable($p['data_dir'] . '/setup-state.json')) {
        return false;
    }
    $s = json_decode((string)file_get_contents($p['data_dir'] . '/setup-state.json'), true);
    return is_array($s) && (!empty($s['ever_complete']) || !empty($s['complete']));
}

/** Newest change in the program's source folder (0 when unknown). */
function epg_source_newest(array $p): int
{
    $src = (string)($p['src_dir'] ?? '');
    if ($src === '' || !is_dir($src)) {
        return 0;
    }
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

/** The installed program is older than its source, so "Build the program" is needed. */
function epg_program_outdated(array $p): bool
{
    if ($p['bin'] === '' || !is_file($p['bin'])) {
        return false;
    }
    return epg_source_newest($p) > (int)filemtime($p['bin']);
}

function epg_layout_end(): void
{
    echo '</main></div></div></body></html>';
}

/** A log-out button for the page heading (handled by settings.php). */
/** Sidebar footer: Log out (signed in) or Log in. Icon only with a tooltip in rail mode. */
function epg_nav_auth(array $p): string
{
    if (epg_authed($p)) {
        return '<form method="post" action="settings.php" class="nav-auth">' . epg_csrf_field()
            . '<input type="hidden" name="action" value="logout">'
            . '<button type="submit" class="item" title="Log out ' . h(epg_current_user($p)) . '" aria-label="Log out ' . h(epg_current_user($p)) . '">' . epg_icon('user') . '<span class="label"><span class="nav-user">' . h(epg_current_user($p)) . '</span><small>Log out</small></span></button></form>';
    }
    return ''; // Logged-out visitors only ever see the login page.
}

function epg_logout_button(): string
{
    return '<form method="post" action="settings.php">' . epg_csrf_field()
        . '<input type="hidden" name="action" value="logout"><button class="ghost">' . epg_icon('logout') . 'Log out</button></form>';
}

// ---------------------------------------------------------------- legacy inline CSS (unused)

function epg_css(): string
{
    return <<<CSS
    body { font-family: system-ui, sans-serif; margin: 0; background: #111; color: #eee; }
    header { padding: 1rem 2rem; background: #1c1c1c; border-bottom: 1px solid #333; display: flex; gap: 1.5rem; align-items: baseline; flex-wrap: wrap; }
    main { padding: 2rem; max-width: 960px; }
    h1 { margin: 0; font-size: 1.5rem; }
    h2 { margin-top: 0; }
    .card { background: #1c1c1c; border: 1px solid #333; border-radius: 8px; padding: 1rem 1.5rem; margin-bottom: 1rem; }
    code { background: #222; padding: 0.1rem 0.4rem; border-radius: 4px; word-break: break-all; }
    a { color: #6cf; }
    label { display: block; margin: 0.8rem 0 0.25rem; color: #bbb; font-size: 0.9rem; }
    input[type=text], input[type=password], input[type=number], select {
      width: 100%; box-sizing: border-box; padding: 0.45rem 0.6rem; background: #111; color: #eee;
      border: 1px solid #444; border-radius: 6px; font: inherit; }
    .row { display: grid; grid-template-columns: 1fr 1fr; gap: 0 1rem; }
    .check { display: flex; gap: 0.5rem; align-items: center; margin-top: 0.8rem; color: #ddd; }
    .check input { width: auto; }
    .hint { color: #888; font-size: 0.85rem; margin: 0.2rem 0 0; }
    button { margin-top: 1rem; padding: 0.5rem 1.1rem; background: #2a6; color: #fff; border: 0; border-radius: 6px; font: inherit; cursor: pointer; }
    button.secondary { background: #444; }
    button.danger { background: #933; }
    button:disabled { opacity: 0.5; cursor: default; }
    .flash { padding: 0.6rem 1rem; border-radius: 6px; margin-bottom: 1rem; }
    .flash.ok { background: #143; border: 1px solid #2a6; }
    .flash.error { background: #411; border: 1px solid #a33; }
    .flash.warn { background: #431; border: 1px solid #a73; }
    table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
    th, td { text-align: left; padding: 0.35rem 0.4rem; border-bottom: 1px solid #2a2a2a; vertical-align: top; }
    th { color: #aaa; font-weight: 600; }
    pre { background: #0b0b0b; border: 1px solid #333; border-radius: 6px; padding: 0.6rem; overflow: auto; max-height: 22rem; font-size: 0.8rem; white-space: pre-wrap; }
    dl { display: grid; grid-template-columns: max-content 1fr; gap: 0.25rem 1rem; margin: 0; }
    dt { color: #aaa; }
    dd { margin: 0; word-break: break-all; }
    .ok-text { color: #6d6; } .bad-text { color: #f77; } .warn-text { color: #fb5; } .muted { color: #888; }
    .dot { display: inline-block; width: 0.7rem; height: 0.7rem; border-radius: 50%; margin-right: 0.4rem; vertical-align: baseline; }
    .dot.ok { background: #3c6; } .dot.warn { background: #e93; } .dot.fail { background: #e44; } .dot.info { background: #59f; }
    .admin { border: 1px solid #a73; background: #2a1d10; border-radius: 6px; padding: 0.5rem 0.8rem; margin-top: 0.4rem; }
    .admin strong { color: #fb5; }
    .inline { display: inline; }
    .inline button { margin-top: 0.3rem; }
CSS;
}
