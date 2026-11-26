<?php
/*
 * Read-only JSON for guide.php. Reads the guide tables the Rust build fills.
 *
 *   ?channels=1                       channel list, groups and the time range
 *   ?progs=1&ch=1,2,3&from=T&to=T     programmes on those channels overlapping [from, to)
 *   ?id=N                             full details of one programme
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
if (!ini_get('zlib.output_compression') && extension_loaded('zlib')) {
    ob_start('ob_gzhandler');
}
header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-store');
header('X-Content-Type-Options: nosniff');

const GUIDE_MAX_CHANNELS = 200;
const GUIDE_MAX_SPAN = 2 * 86400;

function out(array $data, int $code = 200): never
{
    http_response_code($code);
    echo json_encode($data, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}

if (!epg_authed(epg_paths())) {
    out(['error' => 'Log in first.'], 401);
}

try {
    $db = epg_db(epg_paths());
} catch (Throwable $e) {
    out(['empty' => true, 'reason' => $e->getMessage()]);
}
$has = $db->query("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('guide_channels', 'guide_programmes')")->fetchColumn();
if ((int)$has < 2) {
    out(['empty' => true, 'reason' => 'The guide tables do not exist yet. Build the program again, then build the guide.']);
}

// Which guide: ?guide=slug once the program stores several guides; otherwise the only one.
$gid = null;
if (epg_has_column($db, 'guide_channels', 'guide_id')) {
    $all = epg_guides($db);
    $on = array_values(array_filter($all, fn($g) => $g['enabled'])) ?: $all;
    // Any guide can be viewed (also one that is turned off); the default is the first one that is on.
    $g = epg_guide_by_slug($all, (string)($_GET['guide'] ?? '')) ?? ($on[0] ?? null);
    $gid = $g ? $g['id'] : 0;
}
$gAnd = $gid === null ? '' : ' AND guide_id = ' . (int)$gid;
$gWhere = $gid === null ? '' : ' WHERE guide_id = ' . (int)$gid;

try {
    if (isset($_GET['id'])) {
        $star = epg_has_column($db, 'guide_programmes', 'star') ? 'p.star' : 'NULL';
        $st = $db->prepare('SELECT p.id, p.channel_pos, p.start, p.stop, p.title, p.sub_title, p.episode, p.descr, p.icon, p.rating,
                                   p.categories, p.group_name, c.name AS channel, c.logo, ' . $star . ' AS star
                            FROM guide_programmes p JOIN guide_channels c ON c.pos = p.channel_pos' . ($gid === null ? '' : ' AND c.guide_id = p.guide_id') . ' WHERE p.id = ?');
        $st->execute([(int)$_GET['id']]);
        $r = $st->fetch();
        if (!$r) {
            out(['error' => 'not found'], 404);
        }
        out([
            'id' => (int)$r['id'], 'ch' => (int)$r['channel_pos'], 'start' => (int)$r['start'], 'stop' => (int)$r['stop'],
            'title' => $r['title'], 'sub_title' => $r['sub_title'], 'episode' => $r['episode'], 'desc' => $r['descr'],
            'icon' => $r['icon'], 'rating' => $r['rating'], 'star' => $r['star'], 'categories' => json_decode((string)$r['categories'], true) ?: [],
            'group' => $r['group_name'], 'channel' => $r['channel'], 'logo' => $r['logo'],
        ]);
    }

    if (isset($_GET['progs'])) {
        $ch = array_values(array_unique(array_filter(
            array_map('intval', explode(',', (string)($_GET['ch'] ?? ''))),
            fn ($v) => $v >= 0
        )));
        $ch = array_slice($ch, 0, GUIDE_MAX_CHANNELS);
        $from = (int)($_GET['from'] ?? 0);
        $to = (int)($_GET['to'] ?? 0);
        if (!$ch || $to <= $from || $to - $from > GUIDE_MAX_SPAN) {
            out(['error' => 'bad request'], 400);
        }
        $in = implode(',', array_fill(0, count($ch), '?'));
        $st = $db->prepare("SELECT id, channel_pos, start, stop, title, episode, group_name FROM guide_programmes
                            WHERE channel_pos IN ($in) AND start < ? AND stop > ?$gAnd ORDER BY channel_pos, start");
        $st->execute([...$ch, $to, $from]);
        $rows = [];
        foreach ($st->fetchAll(PDO::FETCH_NUM) as $r) {
            $rows[] = [(int)$r[0], (int)$r[1], (int)$r[2], (int)$r[3], $r[4], $r[5], $r[6]];
        }
        out(['ch' => $ch, 'from' => $from, 'to' => $to, 'progs' => $rows]);
    }

    // Programme search: titles and sub-titles from now on, soonest first.
    if (isset($_GET['search'])) {
        $q = trim((string)$_GET['search']);
        if (strlen($q) < 2) {
            out(['results' => []]);
        }
        // "!" escapes LIKE wildcards typed by the user.
        $like = '%' . strtr(substr($q, 0, 120), ['!' => '!!', '%' => '!%', '_' => '!_']) . '%';
        $star = epg_has_column($db, 'guide_programmes', 'star') ? 'p.star' : 'NULL';
        $st = $db->prepare('SELECT p.id, p.channel_pos, p.start, p.stop, p.title, p.episode, p.icon, p.group_name, ' . $star . ' AS star, c.name, c.logo
                             FROM guide_programmes p JOIN guide_channels c ON c.pos = p.channel_pos' . ($gid === null ? '' : ' AND c.guide_id = p.guide_id') . '
                             WHERE p.stop > ?' . ($gid === null ? '' : ' AND p.guide_id = ' . (int)$gid) . '
                               AND (p.title LIKE ? ESCAPE \'!\' OR p.sub_title LIKE ? ESCAPE \'!\')
                             ORDER BY p.start, c.pos LIMIT 50');
        $st->execute([time(), $like, $like]);
        $rows = [];
        foreach ($st->fetchAll(PDO::FETCH_ASSOC) as $r) {
            $rows[] = ['id' => (int)$r['id'], 'ch' => (int)$r['channel_pos'], 'start' => (int)$r['start'], 'stop' => (int)$r['stop'],
                       'title' => $r['title'], 'episode' => $r['episode'], 'icon' => $r['icon'], 'group' => $r['group_name'],
                       'star' => $r['star'], 'channel' => $r['name'], 'logo' => $r['logo']];
        }
        out(['now' => time(), 'results' => $rows]);
    }

    // Channel list (default).
    $channels = [];
    foreach ($db->query('SELECT pos, channel_id, name, logo, group_name FROM guide_channels' . $gWhere . ' ORDER BY pos')->fetchAll(PDO::FETCH_NUM) as $r) {
        $channels[] = [(int)$r[0], $r[1], $r[2], $r[3], $r[4]];
    }
    if (!$channels) {
        out(['empty' => true, 'reason' => 'No guide has been stored yet. Run a guide build.']);
    }
    $groups = [];
    foreach ($db->query("SELECT group_name, COUNT(*) FROM guide_channels WHERE group_name IS NOT NULL AND group_name <> ''$gAnd
                         GROUP BY group_name ORDER BY COUNT(*) DESC") as $r) {
        $groups[] = [$r['group_name'], (int)$r['COUNT(*)']];
    }
    $range = $db->query('SELECT MIN(start), MAX(stop) FROM guide_programmes' . $gWhere)->fetch(PDO::FETCH_NUM);
    out([
        'now' => time(),
        'min' => (int)$range[0],
        'max' => (int)$range[1],
        'groups' => $groups,
        'channels' => $channels,
    ]);
} catch (Throwable $e) {
    out(['error' => 'The guide could not be read: ' . $e->getMessage()], 500);
}
