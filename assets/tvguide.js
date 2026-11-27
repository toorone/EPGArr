/* EPGArr TV Guide page: guide picker, search box, logo dialog, group strip and programme search.
   The page sets window.EPG_GUIDE (selected guide) and window.EPG_GUIDES (all guide slugs). */
  (function () {
    var known = window.EPG_GUIDES || [];
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
  
  (function () {
    var s = document.getElementById('g-search'), c = document.getElementById('g-search-clear');
    if (!s || !c) return;
    var sync = function () { c.hidden = s.value === ''; };
    s.addEventListener('input', sync);
    c.addEventListener('click', function () { s.value = ''; sync(); s.dispatchEvent(new Event('input')); s.focus(); });
    s.addEventListener('keydown', function (e) { if (e.key === 'Escape' && s.value !== '') { e.preventDefault(); c.click(); } });
  })();
  
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
  
  (function () {
    // Search box: "Channels" filters the channel list (guide.js), "Programmes" finds programmes.
    var input = document.getElementById('g-search'), box = document.getElementById('g-sresults');
    if (!input || !box) return;
    var modeBtns = document.querySelectorAll('.s-mode button');
    var timer = 0, seq = 0, items = [], active = -1;
    function setMode(m, keep) {
      input.dataset.mode = m;
      modeBtns.forEach(function (b) { var on = b.dataset.mode === m; b.classList.toggle('on', on); b.setAttribute('aria-pressed', on ? 'true' : 'false'); });
      input.placeholder = m === 'progs' ? 'Search programmes' : 'Search channels';
      input.setAttribute('aria-label', input.placeholder);
      try { localStorage.setItem('epg-search-mode', m); } catch (e) {}
      close();
      if (!keep) { input.value = ''; input.dispatchEvent(new Event('input')); }
    }
    modeBtns.forEach(function (b) { b.addEventListener('click', function () { setMode(b.dataset.mode); input.focus(); }); });
    var saved = 'chans';
    try { saved = localStorage.getItem('epg-search-mode') || 'chans'; } catch (e) {}
    setMode(saved === 'progs' ? 'progs' : 'chans', true);

    function close() { box.hidden = true; box.textContent = ''; items = []; active = -1; }
    function hm(t) { return new Date(t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); }
    function dayName(t) { return new Date(t * 1000).toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'short' }); }
    function slug(g) { return (g || 'other').toLowerCase().replace(/[^a-z]+/g, ''); }
    function mk(tag, cls, text) { var e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
    function pick(r) {
      close();
      window.dispatchEvent(new CustomEvent('epg-goto', { detail: { ch: r.ch, start: r.start, id: r.id } }));
    }
    function highlight(i) {
      items.forEach(function (el, k) { el.classList.toggle('active', k === i); el.setAttribute('aria-selected', k === i ? 'true' : 'false'); });
      active = i;
      if (items[i]) items[i].scrollIntoView({ block: 'nearest' });
    }
    function show(rows, now) {
      box.textContent = '';
      items = [];
      active = -1;
      if (!rows.length) { box.appendChild(mk('p', 'muted s-none', 'No programmes found.')); box.hidden = false; return; }
      var endToday = new Date(); endToday.setHours(24, 0, 0, 0); endToday = endToday.getTime() / 1000;
      var last = '';
      rows.forEach(function (r) {
        var head = r.start <= now ? 'On now' : (r.start < endToday ? 'Later today' : dayName(r.start));
        if (head !== last) { box.appendChild(mk('div', 's-head', head)); last = head; }
        var row = mk('div', 's-row g-' + slug(r.group));
        row.setAttribute('role', 'option');
        var thumb = mk('div', 's-thumb');
        if (r.icon) { var im = mk('img'); im.alt = ''; im.loading = 'lazy'; im.referrerPolicy = 'no-referrer'; im.src = r.icon; im.onerror = function () { this.remove(); }; thumb.appendChild(im); }
        row.appendChild(thumb);
        var txt = mk('div', 's-txt');
        txt.appendChild(mk('div', 's-title', r.title + (r.episode ? ' · ' + r.episode : '')));
        var meta = mk('div', 's-meta');
        meta.appendChild(mk('span', 's-dot'));
        meta.appendChild(document.createTextNode(r.channel + ' · ' + hm(r.start) + '–' + hm(r.stop)));
        if (r.star) meta.appendChild(mk('span', 's-star', ' ★ ' + r.star));
        txt.appendChild(meta);
        row.appendChild(txt);
        if (r.logo) { var lg = mk('img', 's-logo'); lg.alt = ''; lg.loading = 'lazy'; lg.referrerPolicy = 'no-referrer'; lg.src = r.logo; lg.onerror = function () { this.remove(); }; row.appendChild(lg); }
        row.addEventListener('mousedown', function (e) { e.preventDefault(); pick(r); });
        row.addEventListener('mouseenter', function () { highlight(items.indexOf(row)); });
        box.appendChild(row);
        items.push(row);
      });
      box.hidden = false;
    }
    input.addEventListener('input', function () {
      if (input.dataset.mode !== 'progs') return;
      clearTimeout(timer);
      var q = input.value.trim();
      if (q.length < 2) { close(); return; }
      timer = setTimeout(function () {
        var mine = ++seq;
        var g = window.EPG_GUIDE ? '&guide=' + encodeURIComponent(window.EPG_GUIDE) : '';
        fetch('guide_api.php?search=' + encodeURIComponent(q) + g, { credentials: 'same-origin' })
          .then(function (r) { return r.json(); })
          .then(function (j) { if (mine === seq) show(j.results || [], j.now || Math.floor(Date.now() / 1000)); })
          .catch(function () {});
      }, 250);
    });
    input.addEventListener('keydown', function (e) {
      if (input.dataset.mode !== 'progs' || box.hidden) return;
      if (e.key === 'ArrowDown') { e.preventDefault(); highlight(Math.min(items.length - 1, active + 1)); }
      else if (e.key === 'ArrowUp') { e.preventDefault(); highlight(Math.max(0, active - 1)); }
      else if (e.key === 'Enter' && active >= 0) { e.preventDefault(); items[active].dispatchEvent(new MouseEvent('mousedown')); }
      else if (e.key === 'Escape') { e.preventDefault(); close(); }
    });
    input.addEventListener('blur', function () { setTimeout(close, 150); });
  })();
  