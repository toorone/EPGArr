/* EPGArr Dashboard page script. */
    (function () {
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
      // Recent builds: a click (or Enter) on a row shows everything recorded for that run.
      document.querySelectorAll('tr.run-row').forEach(function (row) {
        var toggle = function () {
          var d = document.getElementById(row.getAttribute('aria-controls'));
          var open = d.hidden;
          d.hidden = !open;
          row.setAttribute('aria-expanded', open ? 'true' : 'false');
        };
        row.addEventListener('click', toggle);
        row.addEventListener('keydown', function (e) { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); } });
      });
      document.querySelectorAll('button.copy').forEach(function (btn) {
        var input = document.getElementById(btn.getAttribute('data-for'));
        btn.addEventListener('click', function () {
          var label = btn.querySelector('span');
          var done = function () {
            label.textContent = 'Copied';
            setTimeout(function () { label.textContent = 'Copy'; }, 1800);
          };
          if (navigator.clipboard && window.isSecureContext) {
            navigator.clipboard.writeText(input.value).then(done, function () { input.select(); document.execCommand('copy'); done(); });
          } else {
            input.select();
            document.execCommand('copy');
            done();
          }
        });
      });
      if (window.EPG_BUSY) { setTimeout(function () { location.reload(); }, 15000); }
    })();
    