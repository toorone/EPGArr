/* EPGArr Settings page script. */
    (function () {
      var form = document.getElementById('settings-form');
      var tabs = form.querySelectorAll('.set-tabs a[data-tab]');
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
    