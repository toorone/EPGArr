<?php
/*
 * Interactive TV guide. Reads the guide tables through guide_api.php.
 * Shows only programme information, so it needs no login.
 */
declare(strict_types=1);
require __DIR__ . '/epg_lib.php';

epg_boot();
$p = epg_paths();
epg_require_login($p);

$empty = '';
$guides = [];
$sel = null;
try {
    $db = epg_db($p);
    $guides = epg_guides($db);
    $on = array_values(array_filter($guides, fn($g) => $g['enabled'])) ?: $guides;
    $sel = epg_guide_by_slug($guides, (string)($_GET['guide'] ?? '')) ?? ($on[0] ?? null);
    $perGuide = epg_has_table($db, 'guide_programmes') && epg_has_column($db, 'guide_programmes', 'guide_id');
    $has = (int)$db->query("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('guide_channels', 'guide_programmes')")->fetchColumn();
    if ($has < 2) {
        $empty = 'The guide tables have not been created yet. On the Setup page, build the program again, then build the guide.';
    } elseif ((int)$db->query('SELECT COUNT(*) FROM (SELECT 1 FROM guide_programmes' . ($perGuide && $sel ? ' WHERE guide_id = ' . (int)$sel['id'] : '') . ' LIMIT 1)')->fetchColumn() === 0) {
        $empty = $sel && count($guides) > 1 ? 'The guide "' . $sel['name'] . '" has not been built yet. Build it on the Guides page.' : 'No guide has been stored yet. Build it on the Guides page and it will show up here.';
    }
} catch (Throwable $e) {
    $empty = $e->getMessage();
}

$picker = '';
if ($guides && $sel) {
    $picker = '<label class="g-pick">' . epg_icon('list') . '<span class="sr-only">Guide</span><select id="g-guide" class="input" aria-label="Choose a guide">';
    foreach ($guides as $g) {
        $picker .= '<option value="' . h($g['slug']) . '"' . ($g['slug'] === $sel['slug'] ? ' selected' : '') . '>' . h($g['name'] . ($g['enabled'] ? '' : ' (off)')) . '</option>';
    }
    $picker .= '<option value="__add__">+ Add guide…</option>';
    $picker .= '</select></label>';
}
$search = '<div class="g-tools-top">' . $picker . '<div class="search search-field">'
    . epg_icon('search')
    . '<input type="search" id="g-search" class="input field" placeholder="Search channels" aria-label="Search channels" autocomplete="off"' . ($empty ? ' disabled' : '') . '>'
    . '<button type="button" class="search-clear" id="g-search-clear" aria-label="Clear search" title="Clear search" hidden>' . epg_icon('x') . '</button>'
    . '</div></div>';
// The guide picker and search sit on the same line as the day buttons and groups (below).
epg_layout_start($p, 'guide', 'TV Guide', '', epg_take_flashes(), $empty ? '<div class="g-tools-top">' . $picker . '</div>' : '');
?>
  <script>
  window.EPG_GUIDE = <?= json_encode($sel ? $sel['slug'] : '') ?>;
  (function () {
    var known = <?= json_encode(array_column($guides, 'slug')) ?>;
    var saved = null;
    try { saved = localStorage.getItem('epg-guide'); } catch (e) {}
    if (!/[?&]guide=/.test(location.search) && saved && saved !== window.EPG_GUIDE && known.indexOf(saved) >= 0) {
      location.replace('guide.php?guide=' + encodeURIComponent(saved));
      return;
    }
    try { if (window.EPG_GUIDE) localStorage.setItem('epg-guide', window.EPG_GUIDE); } catch (e) {}
    // The picker is drawn further down the page, so listen on the document.
    document.addEventListener('change', function (e) {
      var s = e.target;
      if (!s || s.id !== 'g-guide') return;
      location.href = s.value === '__add__' ? 'guides.php' : 'guide.php?guide=' + encodeURIComponent(s.value);
    });
  })();
  </script>
<?php

if ($empty !== ''): ?>
  <div class="card g-empty">
    <h2><?= epg_icon('grid') ?>Nothing to show yet</h2>
    <p class="muted"><?= h($empty) ?></p>
    <a class="button" href="guides.php"><?= epg_icon('list') ?>Open Guides</a>
  </div>
<?php else: ?>
  <div class="g-bar">
    <div class="g-days">
      <button type="button" class="secondary" id="g-prev" aria-label="Previous day">&#8249;</button>
      <span id="g-day" class="g-day-label">Today</span>
      <button type="button" class="secondary" id="g-next" aria-label="Next day">&#8250;</button>
      <button type="button" id="g-now"><?= epg_icon('clock') ?>Now</button>
    </div>
    <div class="g-chips-wrap" id="g-chips-wrap">
      <button type="button" class="g-scroll-btn" data-dir="-1" aria-label="Scroll groups left" hidden>&#8249;</button>
      <div class="g-chips" id="g-chips" role="group" aria-label="Filter by group"></div>
      <button type="button" class="g-scroll-btn" data-dir="1" aria-label="Scroll groups right" hidden>&#8250;</button>
    </div>
    <?= $search ?>
  </div>
  <div class="g-wrap" id="g-wrap">
    <div class="g-scroll" id="g-scroll" tabindex="0" aria-label="Programme guide">
      <div class="g-inner" id="g-inner">
        <div class="g-head" id="g-head"><div class="g-corner" id="g-count"></div></div>
        <div class="g-body" id="g-body"></div>
        <div class="g-nowline" id="g-nowline" hidden></div>
      </div>
    </div>
    <div class="g-status" id="g-status">Loading the guide…</div>
  </div>
  <aside class="g-panel" id="g-panel" hidden aria-live="polite">
    <button type="button" class="ghost g-close" id="g-close" aria-label="Close">&times;</button>
    <div id="g-detail"></div>
  </aside>
  <script>
  (function () {
    var s = document.getElementById('g-search'), c = document.getElementById('g-search-clear');
    if (!s || !c) return;
    var sync = function () { c.hidden = s.value === ''; };
    s.addEventListener('input', sync);
    c.addEventListener('click', function () { s.value = ''; sync(); s.dispatchEvent(new Event('input')); s.focus(); });
    s.addEventListener('keydown', function (e) { if (e.key === 'Escape' && s.value !== '') { e.preventDefault(); c.click(); } });
  })();
  </script>
  <dialog class="g-dialog logo-dialog" id="logo-dialog" aria-labelledby="logo-title">
    <form class="guide-form" id="logo-form" method="post" enctype="multipart/form-data" data-csrf="<?= h((string)($_SESSION['csrf'] ?? '')) ?>">
      <div class="hero-head">
        <h2 id="logo-title">Channel logo</h2>
        <button type="button" class="ghost" data-close aria-label="Close">&times;</button>
      </div>
      <div class="logo-now">
        <div class="logo-preview" id="logo-preview"><span class="g-logo-none" id="logo-initials"></span></div>
        <div>
          <strong id="logo-channel"></strong>
          <p class="hint" id="logo-source">Loading…</p>
        </div>
      </div>
      <div class="logo-tabs" role="tablist">
        <button type="button" role="tab" class="on" data-tab="upload" aria-selected="true">Upload</button>
        <button type="button" role="tab" data-tab="url" aria-selected="false">From URL</button>
      </div>
      <div class="logo-pane" data-pane="upload">
        <label class="logo-drop" id="logo-drop">
          <input type="file" id="logo-file" accept="image/png,image/jpeg,image/webp,image/gif">
          <span id="logo-drop-text">Drop an image here, or click to choose one</span>
        </label>
      </div>
      <div class="logo-pane" data-pane="url" hidden>
        <div class="url-box">
          <input type="text" id="logo-url" placeholder="https://example.com/logo.png">
          <button type="button" class="secondary" id="logo-url-preview">Preview</button>
        </div>
      </div>
      <p class="hint" id="logo-rules">PNG, JPEG, WebP or GIF, 32 to 4096 pixels per side.</p>
      <p class="hint" id="logo-msg" role="status"></p>
      <div class="form-actions">
        <button type="submit" id="logo-save">Save</button>
        <button type="button" class="ghost" id="logo-reset">Reset to automatic</button>
        <button type="button" class="ghost" data-close>Cancel</button>
        <button type="button" class="secondary" id="logo-build" hidden>Build now</button>
      </div>
    </form>
  </dialog>
  <script>
  (function () {
    var $ = function (id) { return document.getElementById(id); };
    var dlg = $('logo-dialog'), form = $('logo-form'), msg = $('logo-msg');
    var chan = null, tab = 'upload', file = null;
    var names = { custom: 'set by you', feed: 'from the guide source', override: 'from an override', 'tv-logos': 'from tv-logos', 'iptv-org': 'from iptv-org', none: 'none yet' };
    function say(text, cls) { msg.className = 'hint ' + (cls || ''); msg.textContent = text || ''; }
    function preview(src) {
      var box = $('logo-preview');
      box.textContent = '';
      if (src) {
        var img = document.createElement('img');
        img.alt = '';
        img.referrerPolicy = 'no-referrer';
        img.src = src;
        img.onerror = function () { box.textContent = ''; say('That image could not be shown.', 'bad-text'); };
        box.appendChild(img);
      } else {
        var s = document.createElement('span');
        s.className = 'g-logo-none';
        s.textContent = chan ? chan.name.slice(0, 2) : '';
        box.appendChild(s);
      }
    }
    function setTab(t) {
      tab = t;
      dlg.querySelectorAll('.logo-tabs button').forEach(function (b) { var on = b.dataset.tab === t; b.classList.toggle('on', on); b.setAttribute('aria-selected', on ? 'true' : 'false'); });
      dlg.querySelectorAll('.logo-pane').forEach(function (p) { p.hidden = p.dataset.pane !== t; });
    }
    function post(fd) {
      fd.append('csrf', form.dataset.csrf);
      fd.append('channel', chan.id);
      return fetch('logo_api.php', { method: 'POST', body: fd, credentials: 'same-origin' }).then(function (r) { return r.json(); });
    }
    function done(j) {
      $('logo-save').disabled = false;
      if (j.error) { say(j.error, 'bad-text'); return; }
      chan.logo = j.logo || '';
      preview(chan.logo);
      $('logo-source').textContent = 'Logo: ' + (names[j.source] || j.source) + ' · applies to ' + j.guides + (j.guides === 1 ? ' guide' : ' guides');
      say(j.message + ' Rebuild to publish it.', 'ok-text');
      $('logo-build').hidden = false;
      window.dispatchEvent(new CustomEvent('epg-logo-updated', { detail: { id: chan.id, logo: chan.logo } }));
    }
    window.addEventListener('epg-channel', function (e) {
      chan = e.detail;
      file = null;
      $('logo-file').value = '';
      $('logo-url').value = '';
      $('logo-drop-text').textContent = 'Drop an image here, or click to choose one';
      $('logo-channel').textContent = chan.name;
      $('logo-source').textContent = 'Loading…';
      $('logo-build').hidden = true;
      say('');
      setTab('upload');
      preview(chan.logo);
      if (dlg.showModal) { dlg.showModal(); } else { dlg.setAttribute('open', ''); }
      fetch('logo_api.php?channel=' + encodeURIComponent(chan.id), { credentials: 'same-origin', cache: 'no-store' })
        .then(function (r) { return r.json(); })
        .then(function (j) {
          if (j.error) { $('logo-source').textContent = j.error; return; }
          $('logo-source').textContent = 'Logo: ' + (names[j.source] || j.source) + ' · applies to ' + j.guides + (j.guides === 1 ? ' guide' : ' guides');
          $('logo-rules').textContent = 'PNG, JPEG, WebP or GIF, up to ' + j.limit_text + ', 32 to 4096 pixels per side.'
            + (j.converts ? ' It is stored as PNG, at most 512 px.' : '');
          $('logo-reset').hidden = j.source !== 'custom';
        });
    });
    dlg.querySelectorAll('[data-close]').forEach(function (b) { b.addEventListener('click', function () { dlg.close ? dlg.close() : dlg.removeAttribute('open'); }); });
    dlg.querySelectorAll('.logo-tabs button').forEach(function (b) { b.addEventListener('click', function () { setTab(b.dataset.tab); }); });
    function pickFile(f) {
      file = f || null;
      if (!file) { return; }
      $('logo-drop-text').textContent = file.name + ' (' + Math.round(file.size / 1024) + ' KB)';
      preview(URL.createObjectURL(file));
      say('');
    }
    $('logo-file').addEventListener('change', function () { pickFile(this.files[0]); });
    var drop = $('logo-drop');
    ['dragenter', 'dragover'].forEach(function (t) { drop.addEventListener(t, function (e) { e.preventDefault(); drop.classList.add('over'); }); });
    ['dragleave', 'drop'].forEach(function (t) { drop.addEventListener(t, function (e) { e.preventDefault(); drop.classList.remove('over'); }); });
    drop.addEventListener('drop', function (e) { if (e.dataTransfer.files.length) { pickFile(e.dataTransfer.files[0]); } });
    $('logo-url-preview').addEventListener('click', function () { var u = $('logo-url').value.trim(); if (u) { say(''); preview(u); } });
    form.addEventListener('submit', function (e) {
      e.preventDefault();
      var fd = new FormData();
      fd.append('action', 'save');
      if (tab === 'upload') {
        if (!file) { say('Choose an image first.', 'bad-text'); return; }
        fd.append('file', file);
      } else {
        var u = $('logo-url').value.trim();
        if (!u) { say('Enter the address of an image first.', 'bad-text'); return; }
        fd.append('url', u);
      }
      $('logo-save').disabled = true;
      say('Checking and saving…');
      post(fd).then(done).catch(function () { $('logo-save').disabled = false; say('Saving failed. Try again.', 'bad-text'); });
    });
    $('logo-reset').addEventListener('click', function () {
      if (!confirm('Remove the logo you set for ' + chan.name + ' and go back to the automatic one?')) { return; }
      var fd = new FormData();
      fd.append('action', 'reset');
      post(fd).then(function (j) { done(j); $('logo-reset').hidden = true; }).catch(function () { say('That did not work. Try again.', 'bad-text'); });
    });
    $('logo-build').addEventListener('click', function () {
      var fd = new FormData();
      fd.append('action', 'build');
      $('logo-build').disabled = true;
      post(fd).then(function (j) {
        $('logo-build').disabled = false;
        say(j.error ? j.error : 'Building. Follow it on the Activity page.', j.error ? 'bad-text' : 'ok-text');
      });
    });
  })();
  </script>
  <script>
  (function () {
    // Group chips: one scrollable strip with arrows, wheel scrolling and fading edges.
    var strip = document.getElementById('g-chips');
    if (!strip) return;
    var btns = document.querySelectorAll('.g-scroll-btn');
    function update() {
      var max = strip.scrollWidth - strip.clientWidth;
      var left = strip.scrollLeft > 2, right = strip.scrollLeft < max - 2;
      strip.classList.toggle('fade-l', left);
      strip.classList.toggle('fade-r', right);
      btns[0].hidden = !left;
      btns[1].hidden = !right;
    }
    btns.forEach(function (b) {
      b.addEventListener('click', function () { strip.scrollBy({ left: +b.dataset.dir * strip.clientWidth * 0.8, behavior: 'smooth' }); });
    });
    strip.addEventListener('wheel', function (e) {
      if (Math.abs(e.deltaY) > Math.abs(e.deltaX) && strip.scrollWidth > strip.clientWidth) { e.preventDefault(); strip.scrollLeft += e.deltaY; }
    }, { passive: false });
    strip.addEventListener('scroll', update, { passive: true });
    window.addEventListener('resize', update);
    // Chips are added by guide.js: keep the arrows right and the chosen chip in view.
    new MutationObserver(update).observe(strip, { childList: true });
    strip.addEventListener('click', function (e) {
      var chip = e.target.closest('button');
      if (chip) chip.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'smooth' });
    });
    update();
  })();
  </script>
  <script src="assets/guide.js?v=<?= (int)@filemtime(__DIR__ . '/assets/guide.js') ?>"></script>
<?php endif;
epg_layout_end();
