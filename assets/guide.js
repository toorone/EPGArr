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

  function fitHeight() {
    var top = wrap.getBoundingClientRect().top + window.scrollY;
    wrap.style.height = Math.max(360, window.innerHeight - top - 18) + 'px';
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
    el('g-count').textContent = S.rows.length.toLocaleString() + (S.rows.length === 1 ? ' channel' : ' channels');
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

  function applyFilter() {
    var q = S.query.toLowerCase();
    S.rows = S.channels.filter(function (c) {
      if (S.group && c[4] !== S.group) return false;
      return !q || c[2].toLowerCase().indexOf(q) >= 0 || c[1].toLowerCase().indexOf(q) >= 0;
    });
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
    }).catch(function (e) {
      detail.textContent = '';
      detail.appendChild(make('p', 'bad-text', 'Could not load the details: ' + e.message));
    });
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

  var raf = 0;
  scroller.addEventListener('scroll', function () {
    if (!raf) raf = requestAnimationFrame(function () { raf = 0; render(); });
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
    S.rows = j.channels;
    S.min = j.min;
    S.max = j.max;
    lastPhone = window.matchMedia('(max-width: 640px)').matches;
    buildChips(j.groups || []);
    var today = localMidnight(Date.now() / 1000);
    var start = (today < localMidnight(S.min) || today >= S.max) ? localMidnight(S.min) : today;
    setDay(start, false);
    if (start === today) goNow();
  }).catch(function (e) {
    status.textContent = 'Could not load the guide: ' + e.message;
  });
})();
