/* EPGArr guide grid: virtualised rows, programmes loaded per channel and day. */
(function () {
  'use strict';

  var API = 'api/guide';
  // The guide chosen on the page (tv sets window.EPG_GUIDE).
  var GUIDE_QS = window.EPG_GUIDE ? '&guide=' + encodeURIComponent(window.EPG_GUIDE) : '';
  var GROUPS = ['movie', 'series', 'news', 'sports', 'kids', 'music', 'documentary'];
  var BATCH = 80;        // channels per programme request
  var OVERSCAN = 6;      // rows rendered above and below the view
  var PREFETCH = 30;     // rows fetched ahead of the view

  var el = function (id) { return document.getElementById(id); };
  var scroller = el('g-scroll'), inner = el('g-inner'), head = el('g-head'), body = el('g-body');
  var nowLine = el('g-nowline'), status = el('g-status'), wrap = el('g-wrap');
  var panel = el('g-panel'), detail = el('g-detail'), chipsBox = el('g-chips'), search = el('g-search');
  if (!scroller) return;

  var S = {
    channels: [],        // [pos, id, name, logo, group]
    rows: [],            // filtered channels
    min: 0, max: 0, now: Math.floor(Date.now() / 1000),
    dayStart: 0, dayEnd: 0,
    group: '', query: '',
    cache: new Map(),    // pos -> programmes for the current day
    pending: new Set(),
    rendered: new Map(), // row index -> {el, loaded}
    dims: null
  };

  function dims() {
    var phone = window.matchMedia('(max-width: 640px)').matches;
    var chW = phone ? 104 : 210;
    // Two hours fill the visible programme area (pixels per minute), never too cramped.
    var view = Math.max(240, (scroller.clientWidth || window.innerWidth) - chW);
    return { rowH: phone ? 58 : 66, chW: chW, px: Math.max(2, view / 120), headH: 38 };
  }

  // ---------- helpers

  function make(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }
  function slug(g) {
    var s = String(g || '').toLowerCase().replace(/[^a-z]/g, '');
    return GROUPS.indexOf(s) >= 0 ? s : (s ? 'other' : 'nogroup');
  }
  function hm(t) { return new Date(t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); }
  function localMidnight(t) { var d = new Date(t * 1000); d.setHours(0, 0, 0, 0); return Math.floor(d.getTime() / 1000); }
  function nextMidnight(t) { var d = new Date(t * 1000); d.setHours(24, 0, 0, 0); return Math.floor(d.getTime() / 1000); }
  function dayLabel(t) {
    var diff = Math.round((t - localMidnight(Date.now() / 1000)) / 86400);
    if (diff === 0) return 'Today';
    if (diff === 1) return 'Tomorrow';
    if (diff === -1) return 'Yesterday';
    return new Date(t * 1000).toLocaleDateString([], { weekday: 'short', day: 'numeric', month: 'short' });
  }
  function duration(s) {
    var m = Math.round(s / 60);
    return m < 60 ? m + ' min' : Math.floor(m / 60) + ' h' + (m % 60 ? ' ' + (m % 60) + ' min' : '');
  }
  function getJSON(qs) {
    return fetch(API + '?' + qs + GUIDE_QS, { credentials: 'same-origin' }).then(function (r) {
      return r.json().then(function (j) {
        if (!r.ok || j.error) throw new Error(j.error || ('HTTP ' + r.status));
        return j;
      });
    });
  }
  function x(t) { return (t - S.dayStart) / 60 * S.dims.px; }

  // ---------- layout

  // The guide scrolls inside itself; it fills the rest of the window so the page has no scroll bar.
  function fitHeight() {
    var top = wrap.getBoundingClientRect().top + window.scrollY;
    var main = wrap.closest('main');
    var below = main ? parseFloat(getComputedStyle(main).paddingBottom) || 0 : 0;
    wrap.style.height = Math.max(320, Math.floor(window.innerHeight - top - below - 2)) + 'px';
  }

  function buildFrame() {
    var d = S.dims = dims();
    var width = d.chW + x(S.dayEnd);
    inner.style.width = width + 'px';
    inner.style.setProperty('--ch-w', d.chW + 'px');
    inner.style.setProperty('--row-h', d.rowH + 'px');
    inner.style.setProperty('--head-h', d.headH + 'px');
    head.querySelectorAll('.g-slot').forEach(function (n) { n.remove(); });
    for (var t = S.dayStart; t < S.dayEnd; t += 1800) {
      var s = make('div', 'g-slot' + ((t - S.dayStart) % 3600 === 0 ? ' hour' : ''), hm(t));
      s.style.left = (d.chW + x(t)) + 'px';
      s.style.width = (1800 / 60 * d.px) + 'px';
      head.appendChild(s);
    }
    el('g-day').textContent = dayLabel(S.dayStart);
    el('g-prev').disabled = S.dayStart <= localMidnight(S.min);
    el('g-next').disabled = S.dayEnd >= S.max;
    resetRows();
    placeNowLine();
  }

  function resetRows() {
    S.rendered.forEach(function (r) { r.el.remove(); });
    S.rendered.clear();
    body.style.height = (S.rows.length * S.dims.rowH) + 'px';
    countEl.textContent = S.rows.length.toLocaleString() + (S.rows.length === 1 ? ' channel' : ' channels');
    status.hidden = S.rows.length > 0;
    if (!S.rows.length) status.textContent = 'No channels match.';
    render();
  }

  function placeNowLine() {
    S.now = Math.floor(Date.now() / 1000);
    var on = S.now >= S.dayStart && S.now < S.dayEnd;
    nowLine.hidden = !on;
    if (on) nowLine.style.left = (S.dims.chW + x(S.now)) + 'px';
    body.querySelectorAll('.g-p').forEach(function (n) {
      n.classList.toggle('on', +n.dataset.start <= S.now && +n.dataset.stop > S.now);
      n.classList.toggle('past', +n.dataset.stop <= S.now);
    });
  }

  // ---------- rows

  function rowRange() {
    var d = S.dims;
    var first = Math.floor((scroller.scrollTop - d.headH) / d.rowH);
    var last = Math.ceil((scroller.scrollTop + scroller.clientHeight) / d.rowH);
    return [Math.max(0, first), Math.min(S.rows.length - 1, last)];
  }

  // Programmes that started left of the visible area keep their title in view:
  // the text is pushed to the visible edge and marked with "◂".
  function stickLabels() {
    var edge = scroller.scrollLeft + S.dims.chW + 1;
    body.querySelectorAll('.g-p').forEach(function (b) {
      var l = parseFloat(b.style.left), w = parseFloat(b.style.width);
      var cut = edge - l;
      if (cut > 0 && cut < w - 48) {
        b.style.paddingLeft = (cut + 9) + 'px';
        b.classList.add('cont');
      } else if (b.classList.contains('cont')) {
        b.style.paddingLeft = '';
        b.classList.remove('cont');
      }
    });
  }

  function render() {
    if (!S.rows.length) return;
    var r = rowRange(), from = Math.max(0, r[0] - OVERSCAN), to = Math.min(S.rows.length - 1, r[1] + OVERSCAN);
    S.rendered.forEach(function (v, i) {
      if (i < from || i > to) { v.el.remove(); S.rendered.delete(i); }
    });
    for (var i = from; i <= to; i++) {
      var have = S.rendered.get(i), pos = S.rows[i][0], loaded = S.cache.has(pos);
      if (have && have.loaded === loaded) continue;
      var row = buildRow(i, loaded);
      if (have) have.el.replaceWith(row); else body.appendChild(row);
      S.rendered.set(i, { el: row, loaded: loaded });
    }
    want(Math.max(0, r[0] - PREFETCH), Math.min(S.rows.length - 1, r[1] + PREFETCH));
    stickLabels();
  }

  function buildRow(i, loaded) {
    var c = S.rows[i], d = S.dims;
    var row = make('div', 'g-row');
    row.style.top = (i * d.rowH) + 'px';
    var ch = make('div', 'g-ch g-' + slug(c[4]));
    ch.title = c[2] + (c[4] ? ' · ' + c[4] : '');
    if (c[3]) {
      var img = make('img');
      img.loading = 'lazy';
      img.decoding = 'async';
      img.alt = '';
      img.referrerPolicy = 'no-referrer';
      img.src = c[3];
      img.onerror = function () { this.replaceWith(make('span', 'g-logo-none', c[2].slice(0, 2))); };
      ch.appendChild(img);
    } else {
      ch.appendChild(make('span', 'g-logo-none', c[2].slice(0, 2)));
    }
    ch.appendChild(make('span', 'g-ch-name', c[2]));
    // Click (or Enter) opens the "Channel logo" dialog on tv.
    if (!c[3]) ch.classList.add('no-logo');
    ch.tabIndex = 0;
    ch.setAttribute('role', 'button');
    ch.setAttribute('aria-label', 'Channel logo for ' + c[2]);
    var pick = function () { window.dispatchEvent(new CustomEvent('epg-channel', { detail: { id: c[1], name: c[2], logo: c[3] || '' } })); };
    ch.addEventListener('click', pick);
    ch.addEventListener('keydown', function (e) { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pick(); } });
    row.appendChild(ch);
    if (!loaded) {
      row.appendChild(make('div', 'g-loading'));
      return row;
    }
    var progs = S.cache.get(c[0]);
    if (!progs.length) {
      var none = make('div', 'g-none', 'No programme information');
      none.style.left = (d.chW + 6) + 'px';
      row.appendChild(none);
    }
    progs.forEach(function (p) {
      var s = Math.max(p[2], S.dayStart), e = Math.min(p[3], S.dayEnd);
      if (e <= s) return;
      var b = make('div', 'g-p g-' + slug(p[6]));
      b.style.left = (d.chW + x(s) + 1) + 'px';
      b.style.width = Math.max(4, (e - s) / 60 * d.px - 3) + 'px';
      b.tabIndex = 0;
      b.setAttribute('role', 'button');
      b.dataset.id = p[0];
      b.dataset.start = p[2];
      b.dataset.stop = p[3];
      if (p[2] <= S.now && p[3] > S.now) b.classList.add('on');
      if (p[3] <= S.now) b.classList.add('past');
      b.appendChild(make('span', 'g-t', p[4] || 'Untitled'));
      b.appendChild(make('span', 'g-m', hm(p[2]) + '–' + hm(p[3]) + (p[5] ? ' · ' + p[5] : '')));
      b.title = (p[4] || '') + '\n' + hm(p[2]) + '–' + hm(p[3]);
      row.appendChild(b);
    });
    return row;
  }

  // ---------- data

  var wantTimer = 0;
  function want(from, to) {
    clearTimeout(wantTimer);
    wantTimer = setTimeout(function () {
      var need = [];
      for (var i = from; i <= to; i++) {
        var pos = S.rows[i][0];
        if (!S.cache.has(pos) && !S.pending.has(pos)) need.push(pos);
      }
      for (var k = 0; k < need.length; k += BATCH) load(need.slice(k, k + BATCH));
    }, 60);
  }

  function load(list) {
    var day = S.dayStart;
    list.forEach(function (p) { S.pending.add(p); });
    getJSON('progs=1&ch=' + list.join(',') + '&from=' + S.dayStart + '&to=' + S.dayEnd).then(function (j) {
      if (day !== S.dayStart) return;
      list.forEach(function (p) { S.cache.set(p, []); });
      j.progs.forEach(function (p) { S.cache.get(p[1]).push(p); });
      render();
    }).catch(function (e) {
      status.hidden = false;
      status.textContent = 'Could not load programmes: ' + e.message;
    }).then(function () {
      list.forEach(function (p) { S.pending.delete(p); });
    });
  }

  // ---------- filters

  // ---------- sorting: channel name (default), the source's order, or group

  var SORTS = [['name', 'Channel name'], ['source', 'Source order'], ['group', 'Group']];
  S.sort = 'name';
  try { if (SORTS.some(function (s) { return s[0] === localStorage.getItem('epg-guide-sort'); })) S.sort = localStorage.getItem('epg-guide-sort'); } catch (e) {}
  var byName = function (a, b) { return a[2].localeCompare(b[2], undefined, { numeric: true, sensitivity: 'base' }) || a[0] - b[0]; };
  function sortRows(rows) {
    if (S.sort === 'source') return rows.sort(function (a, b) { return a[0] - b[0]; });
    if (S.sort === 'group') {
      return rows.sort(function (a, b) {
        return (a[4] || '\uffff').localeCompare(b[4] || '\uffff') || byName(a, b);
      });
    }
    return rows.sort(byName);
  }

  // The channel count in the guide's top-left corner, with the sort button next to it.
  var corner = el('g-count');
  corner.textContent = '';
  var countEl = make('span', 'g-count-n');
  corner.appendChild(countEl);
  var sortWrap = make('div', 'g-sort');
  var sortBtn = make('button', 'g-sort-btn');
  sortBtn.type = 'button';
  sortBtn.setAttribute('aria-haspopup', 'menu');
  sortBtn.setAttribute('aria-expanded', 'false');
  sortBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M7 4v16M3 16l4 4 4-4M14 6h7M14 12h5M14 18h3"/></svg><span></span>';
  var sortMenu = make('div', 'g-sort-menu');
  sortMenu.setAttribute('role', 'menu');
  sortMenu.hidden = true;
  SORTS.forEach(function (s) {
    var b = make('button', 'g-sort-item', s[1]);
    b.type = 'button';
    b.setAttribute('role', 'menuitemradio');
    b.dataset.sort = s[0];
    b.addEventListener('click', function () {
      S.sort = s[0];
      try { localStorage.setItem('epg-guide-sort', S.sort); } catch (e) {}
      sortMenu.hidden = true;
      sortBtn.setAttribute('aria-expanded', 'false');
      showSort();
      applyFilter();
    });
    sortMenu.appendChild(b);
  });
  function showSort() {
    var label = SORTS.filter(function (s) { return s[0] === S.sort; })[0][1];
    sortBtn.querySelector('span').textContent = { name: 'A–Z', source: 'Source', group: 'Group' }[S.sort];
    sortBtn.title = 'Sort channels: ' + label;
    sortBtn.setAttribute('aria-label', 'Sort channels by ' + label);
    sortMenu.querySelectorAll('button').forEach(function (b) { b.setAttribute('aria-checked', b.dataset.sort === S.sort ? 'true' : 'false'); });
  }
  sortBtn.addEventListener('click', function (e) {
    e.stopPropagation();
    sortMenu.hidden = !sortMenu.hidden;
    sortBtn.setAttribute('aria-expanded', sortMenu.hidden ? 'false' : 'true');
  });
  document.addEventListener('click', function (e) { if (!sortMenu.hidden && !sortWrap.contains(e.target)) { sortMenu.hidden = true; sortBtn.setAttribute('aria-expanded', 'false'); } });
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape' && !sortMenu.hidden) { sortMenu.hidden = true; sortBtn.focus(); } });
  sortWrap.appendChild(sortBtn);
  sortWrap.appendChild(sortMenu);
  corner.appendChild(sortWrap);
  showSort();

  function applyFilter() {
    var q = S.query.toLowerCase();
    S.rows = sortRows(S.channels.filter(function (c) {
      if (S.group && c[4] !== S.group) return false;
      return !q || c[2].toLowerCase().indexOf(q) >= 0 || c[1].toLowerCase().indexOf(q) >= 0;
    }));
    scroller.scrollTop = 0;
    resetRows();
  }

  function buildChips(groups) {
    var all = [['', S.channels.length]].concat(groups);
    all.forEach(function (g) {
      var b = make('button', 'g-chip g-' + (g[0] ? slug(g[0]) : 'all'));
      b.type = 'button';
      b.appendChild(make('span', 'g-chip-dot'));
      b.appendChild(document.createTextNode(g[0] || 'All'));
      b.appendChild(make('span', 'g-chip-n', g[1].toLocaleString()));
      b.setAttribute('aria-pressed', g[0] === S.group ? 'true' : 'false');
      b.addEventListener('click', function () {
        S.group = S.group === g[0] ? '' : g[0];
        chipsBox.querySelectorAll('.g-chip').forEach(function (n, k) {
          n.setAttribute('aria-pressed', all[k][0] === S.group ? 'true' : 'false');
        });
        applyFilter();
      });
      chipsBox.appendChild(b);
    });
  }

  // ---------- days

  function setDay(start, keepScroll) {
    var left = scroller.scrollLeft;
    S.dayStart = start;
    S.dayEnd = nextMidnight(start);
    S.cache.clear();
    S.pending.clear();
    buildFrame();
    if (keepScroll) scroller.scrollLeft = left;
  }

  function goNow() {
    S.now = Math.floor(Date.now() / 1000);
    var today = localMidnight(S.now);
    if (today !== S.dayStart) setDay(today, false);
    // Start the view at the current half hour.
    scroller.scrollLeft = Math.max(0, x(Math.floor(S.now / 1800) * 1800));
  }

  // ---------- details

  function openDetail(id) {
    panel.hidden = false;
    detail.textContent = '';
    detail.appendChild(make('p', 'muted', 'Loading…'));
    getJSON('id=' + encodeURIComponent(id)).then(function (p) {
      detail.textContent = '';
      if (p.icon) {
        var img = make('img', 'g-poster');
        img.alt = '';
        img.referrerPolicy = 'no-referrer';
        img.src = p.icon;
        img.onerror = function () { this.remove(); };
        detail.appendChild(img);
      }
      var g = make('div', 'g-d-group g-' + slug(p.group));
      g.appendChild(make('span', 'g-chip-dot'));
      g.appendChild(document.createTextNode(p.group || 'Programme'));
      detail.appendChild(g);
      detail.appendChild(make('h3', 'g-d-title', p.title || 'Untitled'));
      var ep = [p.episode, p.sub_title].filter(Boolean).join(' · ');
      if (ep) detail.appendChild(make('p', 'g-d-ep', ep));
      var when = new Date(p.start * 1000).toLocaleDateString([], { weekday: 'short', day: 'numeric', month: 'short' })
        + ' · ' + hm(p.start) + '–' + hm(p.stop) + ' · ' + duration(p.stop - p.start);
      detail.appendChild(make('p', 'g-d-when', when));
      var ch = make('p', 'g-d-ch');
      if (p.logo) {
        var l = make('img');
        l.alt = '';
        l.referrerPolicy = 'no-referrer';
        l.src = p.logo;
        l.onerror = function () { this.remove(); };
        ch.appendChild(l);
      }
      ch.appendChild(document.createTextNode(p.channel || ''));
      detail.appendChild(ch);
      var tags = make('div', 'g-d-tags');
      if (p.rating) tags.appendChild(make('span', 'g-tag rating', p.rating));
      if (p.star) tags.appendChild(make('span', 'g-tag star', '\u2605 IMDb ' + p.star));
      (p.categories || []).forEach(function (c) { tags.appendChild(make('span', 'g-tag', c)); });
      if (tags.childNodes.length) detail.appendChild(tags);
      detail.appendChild(make('p', 'g-d-desc', p.desc || 'No description.'));
      detail.appendChild(tmdbBox(p));
    }).catch(function (e) {
      detail.textContent = '';
      detail.appendChild(make('p', 'bad-text', 'Could not load the details: ' + e.message));
    });
  }
  // ---------- Identify on TMDB

  function tmdbBox(p) {
    var box = make('div', 'g-tmdb');
    var m = p.tmdb;
    var line = make('p', 'g-tmdb-now');
    if (m) {
      line.appendChild(document.createTextNode('TMDB: '));
      var a = make('a', '', m.name + (m.media_type === 'movie' ? ' (movie)' : ' (series)'));
      a.href = m.url; a.target = '_blank'; a.rel = 'noopener';
      line.appendChild(a);
      line.appendChild(make('span', 'muted', m.manual ? ' · set by you' : ' · matched automatically'));
    } else {
      line.appendChild(make('span', 'muted', 'Not matched on TMDB.'));
    }
    box.appendChild(line);
    var b = make('button', 'secondary', m ? 'Change TMDB match' : 'Identify on TMDB');
    b.type = 'button';
    b.addEventListener('click', function () { identify(p); });
    box.appendChild(b);
    return box;
  }

  var idDlg = null;
  function csrf() { var f = el('logo-form'); return f ? f.dataset.csrf : ''; }
  function identify(p) {
    if (!idDlg) {
      idDlg = make('dialog', 'g-dialog tmdb-dialog');
      idDlg.innerHTML = '<form class="guide-form" method="dialog"><div class="hero-head"><h2>Identify on TMDB</h2><button type="button" class="ghost" data-close aria-label="Close">&times;</button></div>'
        + '<p class="hint" id="tm-for"></p>'
        + '<div class="url-box"><input type="text" id="tm-q" placeholder="Title to search for" autocomplete="off">'
        + '<span class="tm-kind" role="group" aria-label="Kind"><button type="button" data-k="multi" class="on">All</button><button type="button" data-k="movie">Movies</button><button type="button" data-k="tv">Series</button></span>'
        + '<button type="submit" id="tm-go">Search</button></div>'
        + '<p class="hint" id="tm-msg" role="status"></p><div class="tm-results" id="tm-results"></div>'
        + '<div class="form-actions"><button type="button" class="ghost" id="tm-clear" hidden>Back to automatic</button><button type="button" class="ghost" data-close>Close</button></div></form>';
      document.body.appendChild(idDlg);
      idDlg.querySelectorAll('[data-close]').forEach(function (x) { x.addEventListener('click', function () { idDlg.close(); }); });
      idDlg.querySelectorAll('.tm-kind button').forEach(function (k) {
        k.addEventListener('click', function () {
          idDlg.querySelectorAll('.tm-kind button').forEach(function (o) { o.classList.toggle('on', o === k); });
          runSearch();
        });
      });
      idDlg.querySelector('form').addEventListener('submit', function (e) { e.preventDefault(); runSearch(); });
      idDlg.querySelector('#tm-clear').addEventListener('click', function () { post({ action: 'clear', title: idDlg.dataset.title }); });
    }
    idDlg.dataset.title = p.title || '';
    idDlg.dataset.id = p.id;
    idDlg.querySelector('#tm-for').textContent = 'Pick the right title for "' + (p.title || '') + '". It is used for every programme with this title, now and in future builds.';
    idDlg.querySelector('#tm-q').value = p.title || '';
    idDlg.querySelector('#tm-clear').hidden = !(p.tmdb && p.tmdb.manual);
    idDlg.querySelector('#tm-results').textContent = '';
    idDlg.querySelector('#tm-msg').textContent = '';
    idDlg.showModal();
    runSearch();
  }
  function msg(text, cls) { var m = idDlg.querySelector('#tm-msg'); m.className = 'hint ' + (cls || ''); m.textContent = text; }
  function runSearch() {
    var q = idDlg.querySelector('#tm-q').value.trim();
    var k = idDlg.querySelector('.tm-kind .on').dataset.k;
    var out = idDlg.querySelector('#tm-results');
    if (!q) { msg('Type a title to search for.'); return; }
    msg('Searching TMDB…');
    fetch('api/tmdb?q=' + encodeURIComponent(q) + '&type=' + k, { credentials: 'same-origin', cache: 'no-store' })
      .then(function (r) { return r.json(); })
      .then(function (j) {
        out.textContent = '';
        if (j.error) { msg(j.error, 'bad-text'); return; }
        if (!j.results.length) { msg('Nothing found. Try another spelling, or the original title.'); return; }
        msg(j.results.length + ' result' + (j.results.length > 1 ? 's' : '') + '. Pick the right one.');
        j.results.forEach(function (r) {
          var b = make('button', 'tm-item');
          b.type = 'button';
          if (r.poster) { var i = make('img'); i.src = r.poster; i.alt = ''; i.referrerPolicy = 'no-referrer'; i.loading = 'lazy'; b.appendChild(i); }
          else { b.appendChild(make('span', 'tm-noposter', '?')); }
          var t = make('span', 'tm-text');
          var h = make('strong', '', r.name);
          t.appendChild(h);
          t.appendChild(make('span', 'tm-meta', [r.media_type === 'movie' ? 'Movie' : 'Series', r.year].filter(Boolean).join(' · ')));
          if (r.overview) t.appendChild(make('span', 'tm-ov', r.overview));
          b.appendChild(t);
          b.addEventListener('click', function () {
            post({ action: 'set', title: idDlg.dataset.title, media_type: r.media_type, id: r.id });
          });
          out.appendChild(b);
        });
      })
      .catch(function () { msg('Could not reach EPGArr. Try again.', 'bad-text'); });
  }
  function post(fields) {
    var fd = new FormData();
    fd.append('csrf', csrf());
    Object.keys(fields).forEach(function (k) { fd.append(k, fields[k]); });
    msg('Saving…');
    fetch('api/tmdb', { method: 'POST', body: fd, credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (j) {
        if (j.error) { msg(j.error, 'bad-text'); return; }
        idDlg.close();
        var id = idDlg.dataset.id;
        openDetail(id);
        setTimeout(function () {
          var n = detail.querySelector('.g-tmdb');
          if (n) n.appendChild(make('p', 'ok-text g-tmdb-msg', j.message));
        }, 400);
      })
      .catch(function () { msg('Could not save. Try again.', 'bad-text'); });
  }

  function closeDetail() {
    panel.hidden = true;
    var a = body.querySelector('.g-p.sel');
    if (a) a.classList.remove('sel');
  }

  body.addEventListener('click', function (e) {
    var b = e.target.closest('.g-p');
    if (!b) return;
    closeDetail();
    b.classList.add('sel');
    openDetail(b.dataset.id);
  });
  body.addEventListener('keydown', function (e) {
    if ((e.key === 'Enter' || e.key === ' ') && e.target.classList.contains('g-p')) {
      e.preventDefault();
      e.target.click();
    }
  });
  el('g-close').addEventListener('click', closeDetail);
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape' && !panel.hidden) closeDetail(); });

  // ---------- wiring

  // Where you were in the guide is remembered (for 30 minutes) when you go to another page and back.
  var POS_KEY = 'epg-guide-pos:' + (window.EPG_GUIDE || '');
  var saveTimer = 0;
  function savePos() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(function () {
      try {
        sessionStorage.setItem(POS_KEY, JSON.stringify({
          top: scroller.scrollTop, left: scroller.scrollLeft, day: S.dayStart, group: S.group, at: Date.now()
        }));
      } catch (e) {}
    }, 150);
  }
  function savedPos() {
    try {
      var p = JSON.parse(sessionStorage.getItem(POS_KEY) || 'null');
      return p && Date.now() - p.at < 30 * 60 * 1000 ? p : null;
    } catch (e) { return null; }
  }

  // A small "back to the top" button in the guide's corner, shown once you have scrolled down.
  var topBtn = make('button', 'g-top');
  topBtn.type = 'button';
  topBtn.title = 'Back to the top';
  topBtn.setAttribute('aria-label', 'Back to the top');
  topBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 15 6-6 6 6"/></svg>';
  topBtn.hidden = true;
  topBtn.addEventListener('click', function () { scroller.scrollTop = 0; topBtn.hidden = true; scroller.focus({ preventScroll: true }); });
  wrap.appendChild(topBtn);

  var raf = 0;
  scroller.addEventListener('scroll', function () {
    if (!raf) raf = requestAnimationFrame(function () { raf = 0; render(); });
    topBtn.hidden = scroller.scrollTop < 300;
    savePos();
  }, { passive: true });
  el('g-prev').addEventListener('click', function () { setDay(localMidnight(S.dayStart - 3600), true); });
  el('g-next').addEventListener('click', function () { setDay(S.dayEnd, true); });
  el('g-now').addEventListener('click', goNow);
  var searchTimer = 0;
  if (search) {
    search.addEventListener('input', function () {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(function () { if (search.dataset.mode === 'progs') return; S.query = search.value.trim(); applyFilter(); }, 150);
    });
  }
  var lastPhone = null;
  window.addEventListener('resize', function () {
    fitHeight();
    var phone = window.matchMedia('(max-width: 640px)').matches;
    // Rebuild when the layout or the width (and so the 2-hour scale) changes.
    var px = S.dayStart ? dims().px : 0;
    if (S.dayStart && (phone !== lastPhone || Math.abs(px - S.dims.px) > 0.05)) {
      lastPhone = phone;
      var t = S.dayStart + scroller.scrollLeft / S.dims.px * 60;
      buildFrame();
      scroller.scrollLeft = x(t);
    }
  });
  // Programme search picked a result: show that channel at that time and open the details.
  window.addEventListener('epg-goto', function (e) {
    var d = e.detail;
    var idx = S.rows.findIndex(function (c) { return c[0] === d.ch; });
    if (idx < 0) {
      // Filtered out by a group or channel search: show all channels again.
      S.group = '';
      S.query = '';
      chipsBox.querySelectorAll('button').forEach(function (n, k) { n.setAttribute('aria-pressed', k === 0 ? 'true' : 'false'); });
      applyFilter();
      idx = S.rows.findIndex(function (c) { return c[0] === d.ch; });
    }
    var day = localMidnight(d.start);
    if (day !== S.dayStart && d.start >= S.min) setDay(day, false);
    if (idx >= 0) scroller.scrollTop = Math.max(0, idx * S.dims.rowH - S.dims.rowH);
    scroller.scrollLeft = Math.max(0, x(Math.max(d.start, S.dayStart)) - 15 * S.dims.px);
    render();
    openDetail(d.id);
  });

  // A logo was saved or reset in the dialog: show it right away.
  window.addEventListener('epg-logo-updated', function (e) {
    S.channels.forEach(function (c) { if (c[1] === e.detail.id) c[3] = e.detail.logo || null; });
    resetRows();
  });

  setInterval(placeNowLine, 30000);

  fitHeight();
  getJSON('channels=1').then(function (j) {
    if (j.empty) {
      status.textContent = j.reason || 'No guide has been stored yet.';
      return;
    }
    S.channels = j.channels;
    S.rows = sortRows(j.channels.slice());
    S.min = j.min;
    S.max = j.max;
    lastPhone = window.matchMedia('(max-width: 640px)').matches;
    var back = savedPos();
    // Back from another page: the same group filter, day and place as before.
    if (back && back.group && (j.groups || []).some(function (g) { return g[0] === back.group; })) {
      S.group = back.group;
      S.rows = sortRows(S.channels.filter(function (c) { return c[4] === S.group; }));
    }
    buildChips(j.groups || []);
    var today = localMidnight(Date.now() / 1000);
    var start = (today < localMidnight(S.min) || today >= S.max) ? localMidnight(S.min) : today;
    if (back && back.day && back.day >= localMidnight(S.min) && back.day < S.max) {
      setDay(back.day, false);
      scroller.scrollLeft = back.left;
      scroller.scrollTop = back.top;
      topBtn.hidden = scroller.scrollTop < 300;
      render();
    } else {
      setDay(start, false);
      if (start === today) goNow();
    }
  }).catch(function (e) {
    status.textContent = 'Could not load the guide: ' + e.message;
  });
})();
