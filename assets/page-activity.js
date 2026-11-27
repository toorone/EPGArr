/* EPGArr Activity page script. */
    (function () {
      var data = window.EPG_ACTIVITY;
      var supported = !!window.EPG_SUPPORTED;
      var $ = function (id) { return document.getElementById(id); };
      var nf = new Intl.NumberFormat();
      var labels = { running: 'Running', paused: 'Paused', idle: 'Idle', finished: 'Finished', failed: 'Stopped with an error' };
      function dur(s) {
        if (s == null) return '–';
        if (s < 90) return Math.max(1, Math.round(s)) + ' s';
        if (s < 5400) return Math.round(s / 60) + ' min';
        return (s / 3600).toFixed(1) + ' h';
      }
      function ago(t, now) {
        if (!t) return '';
        var d = Math.max(0, now - t);
        return 'updated ' + (d < 60 ? d + ' s' : d < 3600 ? Math.round(d / 60) + ' min' : (d / 3600).toFixed(1) + ' h') + ' ago';
      }
      function render(d) {
        var state = supported ? d.state : 'unavailable';
        var st = $('act-state');
        st.textContent = supported ? (labels[state] || state) : 'Not available yet';
        st.className = 'act-state s-' + state;
        $('act-msg').textContent = d.message || (state === 'idle' ? 'Nothing is running. Press Start to look up the remaining titles.' : '');
        $('f-start').hidden = state === 'running';
        $('f-pause').hidden = state !== 'running';
        $('act-bar').style.width = Math.min(100, d.percent) + '%';
        $('act-done').textContent = nf.format(d.done);
        $('act-titles').textContent = nf.format(d.titles);
        $('act-pct').textContent = d.percent;
        $('act-rate').textContent = state === 'running' && d.rate > 0 ? d.rate : '–';
        $('act-eta').textContent = state === 'running' ? dur(d.eta_secs) : '–';
        $('act-updated').textContent = ago(d.updated, d.now);
        $('c-cached').textContent = nf.format(d.cached);
        $('c-matched').textContent = nf.format(d.matched);
        $('c-notfound').textContent = nf.format(d.not_found);
        $('c-pending').textContent = nf.format(d.pending);
        $('c-tvmaze').textContent = nf.format(d.tvmaze || 0);
        $('c-sports').textContent = nf.format(d.sports || 0);
        $('c-skipped').textContent = nf.format(d.skipped);

        var box = $('act-recent');
        if (d.recent.length) {
          box.textContent = '';
          d.recent.forEach(function (r) {
            var el = document.createElement('div');
            el.className = 'act-title' + (r.found ? '' : ' miss');
            var img = document.createElement(r.found && r.poster ? 'img' : 'div');
            img.className = 'act-poster';
            if (r.found && r.poster) { img.src = r.poster; img.alt = ''; img.loading = 'lazy'; }
            var t = document.createElement('div');
            t.className = 'act-t';
            t.textContent = r.title;
            var m = document.createElement('div');
            m.className = 'act-m';
            m.textContent = r.found ? [r.year, r.rating].filter(Boolean).join(' · ') || 'matched' : 'no match';
            el.title = r.found && r.tmdb_name ? 'TMDB: ' + r.tmdb_name : r.title;
            el.appendChild(img); el.appendChild(t); el.appendChild(m);
            box.appendChild(el);
          });
        }

        var log = $('act-log');
        var atEnd = log.scrollTop + log.clientHeight >= log.scrollHeight - 30;
        log.textContent = d.log || 'No log yet.';
        if (atEnd) log.scrollTop = log.scrollHeight;

        var busy = d.build_running || d.job !== '';
        $('act-build-text').textContent = d.build_running ? 'A guide build is running now.' : (d.job ? d.job + ' is running.' : 'No guide build is running.');
        $('act-build-cancel').hidden = !busy;
        var blog = $('act-build-log');
        var bEnd = blog.scrollTop + blog.clientHeight >= blog.scrollHeight - 30;
        blog.textContent = d.build_log || 'No guide build has been started from these pages yet.';
        if (bEnd) blog.scrollTop = blog.scrollHeight;
        $('act-build-start').hidden = busy;
        document.querySelectorAll('.act-gbuild button').forEach(function (b) { if (!b.dataset.off) { b.dataset.off = b.disabled ? '1' : '0'; } b.disabled = busy || b.dataset.off === '1'; });
      }
      render(data);
      $('act-log').scrollTop = $('act-log').scrollHeight;
      function poll() {
        if (document.hidden) { setTimeout(poll, 2500); return; }
        fetch('activity?json=1', { credentials: 'same-origin', cache: 'no-store' })
          .then(function (r) { if (r.status === 401) { location.reload(); throw new Error('login'); } return r.json(); })
          .then(function (d) { render(d); })
          .catch(function () {})
          .then(function () { setTimeout(poll, 2500); });
      }
      setTimeout(poll, 2500);
    })();
    