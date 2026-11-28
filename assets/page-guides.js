/* EPGArr Guides page script. */
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
        fetch('guides', { method: 'POST', body: fd, credentials: 'same-origin' })
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
      var BASE = (window.EPG_GUIDE_BASE || '');
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
          fetch('guides?check=1', { credentials: 'same-origin', cache: 'no-store' }).then(function (r) { return r.json(); }).then(function (j) {
            if (j.result && !j.running) { done(j.result.ok ? 'ok-text' : 'bad-text', j.result.detail); return; }
            if (++tries > 80) { done('bad-text', 'The test is taking too long. Try again later.'); return; }
            setTimeout(poll, 1500);
          }).catch(function () { done('bad-text', 'Could not get the test result.'); });
        };
        fetch('guides', { method: 'POST', body: fd, credentials: 'same-origin' }).then(function (r) {
          if (!r.ok) { throw new Error('the server answered HTTP ' + r.status); }
          return r.json().catch(function () { throw new Error('unexpected answer from the server; reload the page and try again'); });
        }).then(function (j) {
          if (j.error) { done('bad-text', j.error); return; }
          setTimeout(poll, 1500);
        }).catch(function (e) { done('bad-text', 'Could not start the test: ' + (e && e.message ? e.message : 'no connection to EPGArr') + '.'); });
      });
      // Public guides: pick one and its address (and a name) are filled in.
      (function () {
        var openBtn = $('g-presets-open'), box = $('g-presets'), prov = $('g-prov'), list = $('g-plist'), filter = $('g-pfilter'), pmsg = $('g-pmsg');
        if (!openBtn) return;
        var items = [], provName = '';
        function show() {
          var q = filter.value.trim().toLowerCase();
          list.textContent = '';
          var shown = items.filter(function (r) { return !q || r.name.toLowerCase().indexOf(q) >= 0 || r.url.toLowerCase().indexOf(q) >= 0; });
          shown.forEach(function (r) {
            var b = document.createElement('button');
            b.type = 'button';
            b.className = 'g-pitem';
            b.setAttribute('role', 'option');
            var n = document.createElement('strong'); n.textContent = r.name;
            var u = document.createElement('span'); u.textContent = r.url.replace(/^https?:\/\//, '');
            b.appendChild(n); b.appendChild(u);
            b.addEventListener('click', function () {
              $('g-src').value = r.url;
              if (!$('g-name').value.trim()) { $('g-name').value = r.name; $('g-name').dispatchEvent(new Event('input')); }
              box.hidden = true;
              openBtn.setAttribute('aria-expanded', 'false');
              $('g-test-out').className = 'hint';
              $('g-test-out').textContent = 'Filled in from ' + provName + '. Press Test source to check it.';
            });
            list.appendChild(b);
          });
          pmsg.textContent = items.length ? shown.length + ' of ' + items.length + ' guides from ' + provName + '.' : pmsg.textContent;
        }
        function load(p, btn) {
          prov.querySelectorAll('button').forEach(function (x) { x.classList.toggle('on', x === btn); });
          items = []; list.textContent = ''; provName = p.name;
          pmsg.textContent = 'Loading the list from ' + p.name + '…';
          fetch('api/presets?provider=' + encodeURIComponent(p.id), { credentials: 'same-origin', cache: 'no-store' })
            .then(function (r) { return r.json(); })
            .then(function (j) {
              if (j.error) { pmsg.textContent = j.error; return; }
              items = j.results; show();
            })
            .catch(function () { pmsg.textContent = 'Could not load the list. Try again.'; });
        }
        openBtn.addEventListener('click', function () {
          box.hidden = !box.hidden;
          openBtn.setAttribute('aria-expanded', box.hidden ? 'false' : 'true');
          if (box.hidden || prov.childNodes.length) return;
          fetch('api/presets', { credentials: 'same-origin', cache: 'no-store' }).then(function (r) { return r.json(); }).then(function (j) {
            (j.providers || []).forEach(function (p, i) {
              var b = document.createElement('button');
              b.type = 'button';
              b.textContent = p.name;
              b.addEventListener('click', function () { load(p, b); });
              prov.appendChild(b);
              if (i === 0) load(p, b);
            });
          });
        });
        filter.addEventListener('input', show);
        filter.addEventListener('keydown', function (e) { if (e.key === 'Enter') e.preventDefault(); });
      })();
      if (window.EPG_OPEN_GUIDE) { open(window.EPG_OPEN_GUIDE); }
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
    