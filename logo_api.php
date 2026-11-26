<?php
/*
 * Channel logos set from the TV Guide page. JSON only, login and CSRF required.
 *
 *   GET  ?channel=<id>                    current logo, where it comes from, limits
 *   POST action=save, channel, file=<upload> | url=<http(s) URL>
 *   POST action=reset, channel            back to the automatic logo
 *
 * Uploads and URLs pass the same checks: PNG, JPEG, WebP or GIF by their
 * magic bytes (never SVG), at most 2 MB (or less if PHP allows less),
 * 32x32 to 4096x4096. With GD the image is stored as PNG, at most 512 px on
 * its longest side. The file goes to <web folder>/logos/custom/ because media
 * servers fetch logos directly, and is recorded as a channel override.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
$p = epg_paths();
header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-store');

const LOGO_MAX_BYTES = 2 * 1024 * 1024;
const LOGO_MIN_SIDE = 32;
const LOGO_MAX_SIDE = 4096;
const LOGO_STORE_SIDE = 512;

function out(array $data, int $code = 200): never
{
    http_response_code($code);
    echo json_encode($data, JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}

if (!epg_authed($p)) {
    out(['error' => 'Log in first.'], 401);
}

/** "2M" -> bytes. */
function ini_bytes(string $v): int
{
    $v = trim($v);
    if ($v === '') {
        return PHP_INT_MAX;
    }
    $n = (float)$v;
    $u = strtolower(substr($v, -1));
    return (int)($u === 'g' ? $n * 1073741824 : ($u === 'm' ? $n * 1048576 : ($u === 'k' ? $n * 1024 : $n)));
}

/** The upload limit in effect: ours, or PHP's if that is lower. */
function logo_limit(): int
{
    $l = LOGO_MAX_BYTES;
    foreach (['upload_max_filesize', 'post_max_size'] as $k) {
        $b = ini_bytes((string)ini_get($k));
        if ($b > 0) {
            $l = min($l, $b);
        }
    }
    return $l;
}

function mb(int $b): string
{
    return rtrim(rtrim(number_format($b / 1048576, 1), '0'), '.') . ' MB';
}

/** The image type from its first bytes, or '' (SVG and anything else is refused). */
function logo_type(string $bytes): string
{
    if (str_starts_with($bytes, "\x89PNG\r\n\x1a\n")) {
        return 'png';
    }
    if (str_starts_with($bytes, "\xFF\xD8\xFF")) {
        return 'jpg';
    }
    if (str_starts_with($bytes, 'GIF87a') || str_starts_with($bytes, 'GIF89a')) {
        return 'gif';
    }
    if (strlen($bytes) > 12 && substr($bytes, 0, 4) === 'RIFF' && substr($bytes, 8, 4) === 'WEBP') {
        return 'webp';
    }
    return '';
}

/** Check an image. Returns [type, width, height] or throws with a message for the dialog. */
function logo_check(string $bytes, int $limit): array
{
    if ($bytes === '') {
        throw new RuntimeException('The file is empty.');
    }
    if (strlen($bytes) > $limit) {
        throw new RuntimeException('The image is too large: ' . mb(strlen($bytes)) . ' (the limit is ' . mb($limit) . ').');
    }
    $type = logo_type($bytes);
    if ($type === '') {
        throw new RuntimeException('That is not a PNG, JPEG, WebP or GIF image. SVG is not allowed.');
    }
    $info = @getimagesizefromstring($bytes);
    if (!is_array($info) || ($info[0] ?? 0) <= 0) {
        throw new RuntimeException('The image could not be read; it may be damaged.');
    }
    [$w, $h] = [(int)$info[0], (int)$info[1]];
    if ($w < LOGO_MIN_SIDE || $h < LOGO_MIN_SIDE) {
        throw new RuntimeException("The image is too small: {$w}x{$h} (at least " . LOGO_MIN_SIDE . 'x' . LOGO_MIN_SIDE . ').');
    }
    if ($w > LOGO_MAX_SIDE || $h > LOGO_MAX_SIDE) {
        throw new RuntimeException("The image is too big: {$w}x{$h} (at most " . LOGO_MAX_SIDE . 'x' . LOGO_MAX_SIDE . ').');
    }
    return [$type, $w, $h];
}

/** Download a logo: http(s) only, 10 s, at most 3 redirects, never more than $limit bytes. */
function logo_fetch(string $url, int $limit): string
{
    if (!preg_match('~^https?://[^\s/$.?#].[^\s]*$~i', $url) || strlen($url) > 2000) {
        throw new RuntimeException('Enter a full http:// or https:// address.');
    }
    $ctx = stream_context_create([
        'http' => ['timeout' => 10, 'follow_location' => 1, 'max_redirects' => 3, 'ignore_errors' => true,
                   'user_agent' => 'EPGArr logo fetch', 'header' => "Accept: image/*\r\n"],
        'ssl' => ['verify_peer' => true, 'verify_peer_name' => true],
    ]);
    $fh = @fopen($url, 'rb', false, $ctx);
    if ($fh === false) {
        throw new RuntimeException('Could not fetch that address.');
    }
    $status = 0;
    foreach (stream_get_meta_data($fh)['wrapper_data'] ?? [] as $line) {
        if (preg_match('~^HTTP/\S+\s+(\d{3})~', (string)$line, $m)) {
            $status = (int)$m[1]; // the last one wins (after redirects)
        }
    }
    if ($status !== 0 && ($status < 200 || $status >= 300)) {
        fclose($fh);
        throw new RuntimeException("Could not fetch that address (HTTP $status).");
    }
    $data = '';
    $start = microtime(true);
    while (!feof($fh)) {
        $chunk = fread($fh, 65536);
        if ($chunk === false) {
            break;
        }
        $data .= $chunk;
        if (strlen($data) > $limit) {
            fclose($fh);
            throw new RuntimeException('The image is larger than ' . mb($limit) . '.');
        }
        if (microtime(true) - $start > 10) {
            fclose($fh);
            throw new RuntimeException('Fetching the image took too long.');
        }
    }
    fclose($fh);
    return $data;
}

/** Store the checked image; with GD as PNG scaled to at most 512 px. Returns the file name. */
function logo_store(array $p, string $channel, string $bytes, string $type): array
{
    $dir = $p['web_dir'] . '/logos/custom';
    if (!is_dir($dir) && !@mkdir($dir, 0775, true)) {
        throw new RuntimeException('Could not create the logos folder in the web folder.');
    }
    $base = substr(hash('sha256', $channel), 0, 20);
    $out = $bytes;
    $ext = $type;
    $converted = false;
    if (function_exists('imagecreatefromstring')) {
        $img = @imagecreatefromstring($bytes);
        if ($img !== false) {
            $w = imagesx($img);
            $h = imagesy($img);
            $scale = min(1, LOGO_STORE_SIDE / max($w, $h));
            $nw = max(1, (int)round($w * $scale));
            $nh = max(1, (int)round($h * $scale));
            $dst = imagecreatetruecolor($nw, $nh);
            imagealphablending($dst, false);
            imagesavealpha($dst, true);
            imagefill($dst, 0, 0, imagecolorallocatealpha($dst, 0, 0, 0, 127));
            imagecopyresampled($dst, $img, 0, 0, 0, 0, $nw, $nh, $w, $h);
            ob_start();
            $ok = imagepng($dst, null, 9);
            $png = (string)ob_get_clean();
            if ($ok && $png !== '') {
                $out = $png;
                $ext = 'png';
                $converted = true;
            }
        }
    }
    foreach (glob($dir . '/' . $base . '.*') ?: [] as $old) {
        @unlink($old);
    }
    $tmp = $dir . '/.' . bin2hex(random_bytes(8)) . '.tmp';
    if (@file_put_contents($tmp, $out) === false || !@rename($tmp, $dir . '/' . $base . '.' . $ext)) {
        @unlink($tmp);
        throw new RuntimeException('Could not save the logo in the web folder.');
    }
    @chmod($dir . '/' . $base . '.' . $ext, 0644);
    return [$base . '.' . $ext, $converted];
}

/** How many guides have this channel, and where its logo comes from now. */
function logo_info(PDO $db, string $channel): array
{
    $guides = 0;
    $logo = '';
    $st = $db->prepare('SELECT logo FROM guide_channels WHERE channel_id = ?');
    $st->execute([$channel]);
    foreach ($st->fetchAll(PDO::FETCH_COLUMN) as $l) {
        $guides++;
        if ($logo === '' && is_string($l)) {
            $logo = $l;
        }
    }
    $o = $db->prepare('SELECT logo_url FROM channel_overrides WHERE channel_id = ?');
    $o->execute([$channel]);
    $ov = (string)($o->fetchColumn() ?: '');
    $source = '';
    if (str_contains($ov, '/logos/custom/')) {
        $source = 'custom';
    } elseif (epg_has_table($db, 'channel_logo_sources')) {
        $s = $db->prepare('SELECT source FROM channel_logo_sources WHERE channel_id = ? LIMIT 1');
        $s->execute([$channel]);
        $source = (string)($s->fetchColumn() ?: '');
    }
    return ['guides' => max(1, $guides), 'logo' => $logo, 'source' => $source !== '' ? $source : ($logo !== '' ? 'feed' : 'none')];
}

try {
    $db = epg_db($p);
    $limit = logo_limit();

    if ($_SERVER['REQUEST_METHOD'] === 'GET') {
        $channel = (string)($_GET['channel'] ?? '');
        if ($channel === '' || strlen($channel) > 200) {
            out(['error' => 'Which channel?'], 400);
        }
        out(logo_info($db, $channel) + [
            'limit' => $limit, 'limit_text' => mb($limit),
            'converts' => function_exists('imagecreatefromstring'),
        ]);
    }

    if (!epg_csrf_ok()) {
        out(['error' => 'The page expired. Reload it and try again.'], 403);
    }
    $action = (string)($_POST['action'] ?? '');
    $channel = trim((string)($_POST['channel'] ?? ''));
    if ($channel === '' || strlen($channel) > 200 || preg_match('/[\x00-\x1f]/', $channel)) {
        out(['error' => 'Which channel?'], 400);
    }

    if ($action === 'build') {
        $err = epg_start_build($p);
        out($err === '' ? ['ok' => true] : ['error' => $err]);
    }

    if ($action === 'reset') {
        $dir = $p['web_dir'] . '/logos/custom';
        foreach (glob($dir . '/' . substr(hash('sha256', $channel), 0, 20) . '.*') ?: [] as $old) {
            @unlink($old);
        }
        $db->prepare("UPDATE channel_overrides SET logo_url = '', updated_at = ? WHERE channel_id = ?")->execute([time(), $channel]);
        $db->prepare("DELETE FROM channel_overrides WHERE channel_id = ? AND logo_url = '' AND group_name = ''")->execute([$channel]);
        $db->prepare("UPDATE guide_channels SET logo = NULL WHERE channel_id = ? AND logo LIKE '%/logos/custom/%'")->execute([$channel]);
        out(['ok' => true, 'logo' => '', 'message' => 'Back to the automatic logo from the next build.'] + logo_info($db, $channel));
    }

    if ($action !== 'save') {
        out(['error' => 'Unknown action.'], 400);
    }

    if (isset($_FILES['file']) && is_array($_FILES['file']) && ($_FILES['file']['error'] ?? UPLOAD_ERR_NO_FILE) !== UPLOAD_ERR_NO_FILE) {
        $f = $_FILES['file'];
        $code = (int)$f['error'];
        if ($code === UPLOAD_ERR_INI_SIZE || $code === UPLOAD_ERR_FORM_SIZE) {
            out(['error' => 'The image is too large (the limit is ' . mb($limit) . ').']);
        }
        if ($code !== UPLOAD_ERR_OK || !is_uploaded_file((string)$f['tmp_name'])) {
            out(['error' => 'The upload did not arrive. Try again.']);
        }
        if ((int)$f['size'] > $limit) {
            out(['error' => 'The image is too large: ' . mb((int)$f['size']) . ' (the limit is ' . mb($limit) . ').']);
        }
        $bytes = (string)file_get_contents((string)$f['tmp_name']);
    } else {
        $url = trim((string)($_POST['url'] ?? ''));
        if ($url === '') {
            out(['error' => 'Choose an image or enter an address.']);
        }
        $bytes = logo_fetch($url, $limit);
    }

    [$type] = logo_check($bytes, $limit);
    [$file, $converted] = logo_store($p, $channel, $bytes, $type);
    $url = epg_base_url() . '/logos/custom/' . $file . '?v=' . time();

    $db->prepare('INSERT INTO channel_overrides (channel_id, logo_url, group_name, updated_at) VALUES (?, ?, \'\', ?)
                  ON CONFLICT(channel_id) DO UPDATE SET logo_url = excluded.logo_url, updated_at = excluded.updated_at')
        ->execute([$channel, $url, time()]);
    $db->prepare('UPDATE guide_channels SET logo = ? WHERE channel_id = ?')->execute([$url, $channel]);

    out(['ok' => true, 'logo' => $url, 'converted' => $converted,
         'message' => 'Saved. It is in the TV Guide now; your media server gets it with the next build.'] + logo_info($db, $channel));
} catch (RuntimeException $e) {
    out(['error' => $e->getMessage()]);
} catch (Throwable $e) {
    out(['error' => 'Something went wrong: ' . $e->getMessage()], 500);
}
