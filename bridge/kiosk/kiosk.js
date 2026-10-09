// DeskWatch kiosk page. Plain JavaScript, no libraries, no build step.
//
// The bridge sends one JSON snapshot (see docs/kiosk.md) over a WebSocket
// whenever something changes, and the page redraws from it. If the WebSocket
// cannot connect, the page polls the snapshot endpoint instead. If nothing
// arrives for a while (bridge restarting, network down) the page keeps the
// last data on screen, dims it and says how old it is, then recovers on its own.
//
// `?static` stops all network use; the screenshots and tests call
// `DeskWatchKiosk.render(snapshot)` themselves.
(function () {
  'use strict';

  var STALE_AFTER_S = 15;      // dim the page when no snapshot has come in this long
  var POLL_MS = 5000;          // polling interval when the WebSocket is down
  var HISTORY = 60;            // samples kept for the CPU sparklines
  var params = new URLSearchParams(location.search);
  var token = params.get('token');

  var snap = null;             // latest snapshot
  var lastMsg = 0;             // client time (ms) of the latest snapshot
  var skew = 0;                // server clock minus client clock, in seconds
  var build = null;            // bridge version, to reload the page after an upgrade
  var history = {};            // host name -> recent CPU readings
  var historyAt = {};          // host name -> client time of the last reading
  var cards = {};              // card key -> { el, html }

  var $ = function (id) { return document.getElementById(id); };

  // ---- small helpers ------------------------------------------------------

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function nowS() { return Date.now() / 1000 + skew; }
  function pct(v) { return v == null ? '--' : Math.round(v) + '%'; }
  function level(v, warn, crit) { return v == null ? 'var(--grey)' : v >= crit ? 'var(--red)' : v >= warn ? 'var(--amber)' : 'var(--green)'; }
  function clock(s) {
    s = Math.max(0, Math.floor(s));
    var h = Math.floor(s / 3600), m = Math.floor(s % 3600 / 60), r = s % 60;
    var two = function (n) { return n < 10 ? '0' + n : '' + n; };
    return h ? h + ':' + two(m) + ':' + two(r) : m + ':' + two(r);
  }
  function ago(s) {
    s = Math.max(0, Math.floor(s));
    if (s < 60) return s + 's ago';
    if (s < 3600) return Math.floor(s / 60) + 'm ago';
    if (s < 86400) return Math.floor(s / 3600) + 'h ago';
    return Math.floor(s / 86400) + 'd ago';
  }
  function uptime(s) {
    if (s == null) return '--';
    var d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60);
    return d ? d + 'd ' + h + 'h' : h ? h + 'h ' + m + 'm' : m + 'm';
  }
  function rate(bps) {
    if (bps == null) return '--';
    var u = ['B/s', 'kB/s', 'MB/s', 'GB/s'], i = 0, v = bps;
    while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
    return (v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)) + ' ' + u[i];
  }
  // The CI system a job, run or PR comes from, as a small tag.
  var SOURCES = { gitea: 'Gitea', github: 'GitHub', azure_devops: 'Azure DevOps' };
  function src(kind) {
    return kind ? '<span class="src ' + esc(kind) + '">' + esc(SOURCES[kind] || kind) + '</span>' : '';
  }
  function dot(status) { return '<span class="dot ' + esc(status) + '"></span>'; }

  // ---- widgets ------------------------------------------------------------

  function findHost(name) {
    var hosts = snap.hosts || [];
    for (var i = 0; i < hosts.length; i++) if (hosts[i].name === name) return hosts[i];
    return name ? null : hosts[0] || null;
  }

  function ciStale() {
    // Only the CI sources: a Prometheus outage does not make the PR list old.
    return (snap.sources || []).some(function (s) { return s.health !== 'ok' && /^(gitea|github|azure_devops):/.test(s.id); });
  }

  function gauge(label, value, warn, crit) {
    var w = value == null ? 0 : Math.max(0, Math.min(100, value));
    return '<div class="gauge"><span class="l">' + label + '</span>' +
      '<div class="bar"><i style="--p:' + w + '%;--c:' + level(value, warn, crit) + '"></i></div>' +
      '<span class="v">' + pct(value) + '</span></div>';
  }

  function sparkline(values) {
    if (!values || values.length < 2) return '<svg class="spark"></svg>';
    var w = 100, h = 30, n = HISTORY;
    var step = w / (n - 1), off = (n - values.length) * step;
    var pts = values.map(function (v, i) {
      return (off + i * step).toFixed(1) + ',' + (h - Math.max(0, Math.min(100, v)) / 100 * h).toFixed(1);
    });
    return '<svg class="spark" viewBox="0 0 ' + w + ' ' + h + '" preserveAspectRatio="none">' +
      '<path d="M' + pts.join(' L') + ' L' + (off + (values.length - 1) * step).toFixed(1) + ',' + h + ' L' + off.toFixed(1) + ',' + h + ' Z"/>' +
      '<polyline points="' + pts.join(' ') + '"/></svg>';
  }

  var widgets = {
    stats: function (p) {
      var host = findHost(p.host);
      if (!host) return { title: p.host || 'Server', stale: true, body: '<div class="empty">No data yet</div>' };
      var s = host.stats || {};
      var load = s.load ? s.load.map(function (x) { return x.toFixed(1); }).join(' ') : '--';
      var html = '<div class="gauges">' + gauge('CPU', s.cpu_pct, 70, 90) + gauge('RAM', s.ram_pct, 80, 90) + gauge('Disk', s.disk_pct, 80, 90) + '</div>' +
        '<div class="stat-grid">' +
        '<div><span class="l">Temp</span><span>' + (s.cpu_temp_c == null ? '--' : Math.round(s.cpu_temp_c) + ' &deg;C') + '</span></div>' +
        '<div><span class="l">Load</span><span>' + load + '</span></div>' +
        '<div><span class="l">Net in</span><span>' + (s.net ? rate(s.net.rx_bps) : '--') + '</span></div>' +
        '<div><span class="l">Net out</span><span>' + (s.net ? rate(s.net.tx_bps) : '--') + '</span></div>' +
        '<div><span class="l">Uptime</span><span>' + uptime(s.uptime_s) + '</span></div>' +
        '<div><span class="l">Cores</span><span>' + (s.cpu_count == null ? '--' : s.cpu_count) + '</span></div>' +
        '</div>' + sparkline(history[host.name]);
      return { title: p.title || host.name, tag: host.up ? '' : 'not answering', stale: host.stale || !host.up, body: html };
    },

    hosts: function () {
      var hosts = snap.hosts || [];
      var html;
      if (!hosts.length) html = '<div class="empty">No hosts</div>';
      else {
        html = '<div class="hosts"><span class="h">Host</span><span class="h">CPU</span><span class="h">RAM</span><span class="h">Disk</span>';
        hosts.forEach(function (h) {
          var s = h.stats || {};
          var cell = function (v, w, c) { return '<span class="v ' + (v >= c ? 'crit-text' : v >= w ? 'warn-text' : '') + '">' + pct(v) + '</span>'; };
          html += '<span class="n' + (h.up ? '' : ' off') + '">' + esc(h.name) + '</span>' + cell(s.cpu_pct, 70, 90) + cell(s.ram_pct, 80, 90) + cell(s.disk_pct, 80, 90);
        });
        html += '</div>';
      }
      var down = (snap.down || []).length;
      return { title: 'Hosts', count: hosts.length, tag: down ? down + ' down' : '', body: html };
    },

    containers: function (p) {
      var down = snap.down || [];
      var rows = down.map(function (d) {
        return '<div class="row compact">' + dot('failed') + '<span class="main">' + esc(d.text) + '</span><span class="when">' + esc(d.sub) + '</span></div>';
      });
      return { title: 'Containers', count: down.length || '', body: list(rows, p.rows, '<div class="empty good">Everything is running</div>') };
    },

    jobs: function (p) {
      var jobs = snap.jobs || [];
      if (!jobs.length) return { title: 'Running now', body: '<div class="empty">No jobs running</div>', stale: false, plain: true };
      var max = p.rows || 3, shown = jobs.slice(0, max);
      var html = shown.map(function (j) {
        var step = j.step ? esc(j.step) + (j.step_no && j.step_count ? ' <span>(' + j.step_no + '/' + j.step_count + ')</span>' : '') : 'starting';
        var bar = j.progress == null ? '<div class="pbar spin"><i></i></div>' :
          '<div class="pbar"><i style="--p:' + Math.round(j.progress * 100) + '%"></i></div>';
        return '<div class="job"><div class="l1"><span class="kind ' + esc(j.kind) + '">' + esc(j.kind) + '</span>' +
          '<span class="name">' + esc(j.project) + ' / ' + esc(j.pipeline) + '</span>' +
          '<span class="el" data-since="' + j.started + '">' + clock(nowS() - j.started) + '</span></div>' +
          '<div class="l2"><span>' + esc(j.ref) + (j.commit ? ' @ ' + esc(j.commit) : '') + '</span>' + src(j.source) + '<span class="step">' + step + '</span></div>' +
          bar + (j.progress == null ? '' : '<div class="pct">' + Math.round(j.progress * 100) + '%</div>') + '</div>';
      }).join('');
      if (jobs.length > max) html += '<div class="more">+' + (jobs.length - max) + ' more running</div>';
      return { title: 'Running now', count: jobs.length, stale: ciStale(), body: html };
    },

    pipelines: function (p) {
      var runs = snap.runs || [];
      var rows = runs.map(function (r) {
        var when = r.status === 'running' ? 'running' : ago(nowS() - (r.finished || r.started));
        return '<div class="row">' + dot(runStatus(r.status)) + '<span class="main">' + esc(r.project) + ' / ' + esc(r.pipeline) + '</span>' +
          '<span class="when">' + when + '</span><span class="sub">' + esc(r.ref) + src(r.source) + (r.step ? ' &middot; failed at ' + esc(r.step) : '') + '</span></div>';
      });
      return { title: 'Pipelines', count: runs.length, stale: ciStale(), body: list(rows, p.rows, '<div class="empty">No runs yet</div>') };
    },

    prs: function (p) {
      var repos = (snap.pulls || []).filter(function (r) { return r.total > 0; });
      var total = repos.reduce(function (n, r) { return n + r.total; }, 0);
      var rows = [];
      repos.forEach(function (r) {
        rows.push('<div class="group"><b>' + esc(r.project) + '</b>' + src(r.source) + '<span>' + r.total + ' open</span></div>');
        (r.items || []).forEach(function (pr) {
          rows.push('<div class="row pr compact">' + dot('open') + '<span class="main"><span class="num">#' + pr.number + '</span>' + esc(pr.title) + '</span>' +
            '<span class="when">' + ago(nowS() - pr.created) + '</span></div>');
        });
      });
      return { title: 'Open PRs', count: total, stale: ciStale(), body: list(rows, p.rows, '<div class="empty good">No open PRs</div>') };
    },

    alerts: function (p) {
      var alerts = snap.alerts || [];
      var rows = alerts.map(function (a) {
        return '<div class="row">' + dot(a.severity) + '<span class="main">' + esc(a.title) + '</span><span class="when">' + ago(nowS() - a.since) + '</span>' +
          (a.message ? '<span class="sub">' + esc(a.message) + '</span>' : '') + '</div>';
      });
      return { title: 'Alerts', count: alerts.length || '', body: list(rows, p.rows, '<div class="empty good">All quiet</div>') };
    },

    health: function () {
      var sources = snap.sources || [];
      var bad = sources.filter(function (s) { return s.health !== 'ok'; }).length;
      var names = { ok: 'working', auth_failed: 'token refused', unreachable: 'not reachable' };
      var rows = sources.map(function (s) {
        return '<div class="row compact">' + dot(s.health === 'ok' ? 'ok' : 'failed') + '<span class="main">' + esc(s.id) + '</span><span class="when">' + (names[s.health] || esc(s.health)) + '</span></div>';
      });
      return { title: 'Sources', count: sources.length, tag: bad ? bad + ' with a problem' : '', body: list(rows, null, '<div class="empty">No sources configured</div>') };
    }
  };

  function runStatus(s) { return s === 'success' ? 'ok' : s === 'waiting' ? 'review' : s; }

  // Rows that do not fit are cut off; `rows` caps the list from the config.
  function list(rows, cap, empty) {
    if (!rows.length) return empty;
    var shown = cap ? rows.slice(0, cap) : rows;
    return '<div class="rows">' + shown.join('') + '</div>' +
      (cap && rows.length > cap ? '<div class="more">+' + (rows.length - cap) + ' more</div>' : '');
  }

  // ---- drawing ------------------------------------------------------------

  function renderCard(key, panel, grid) {
    var make = widgets[panel.widget];
    var out = make ? make(panel) : { title: panel.widget, body: '<div class="empty">Unknown widget</div>' };
    var html = '<header><h2>' + esc(panel.title || out.title) + '</h2>' +
      (out.tag ? '<span class="tag">' + esc(out.tag) + '</span>' : '') +
      (out.count !== undefined && out.count !== '' ? '<span class="count">' + esc(out.count) + '</span>' : '') +
      '</header><div class="body">' + out.body + '</div>';
    var entry = cards[key];
    if (!entry) {
      var el = document.createElement('section');
      var span = panel.span || [1, 1];
      el.style.gridColumn = 'span ' + span[0];
      el.style.gridRow = 'span ' + span[1];
      grid.appendChild(el);
      entry = cards[key] = { el: el, html: '' };
    }
    entry.el.className = 'card w-' + panel.widget + (out.stale ? ' stale' : '');
    if (entry.html !== html) { entry.el.innerHTML = html; entry.html = html; }
  }

  function renderBadges() {
    var el = $('badges'), html = '';
    var names = { pr: 'open PRs', pipeline: 'failed', server: 'down', warn: 'with a problem', home: 'alerts' };
    (snap.badges || []).forEach(function (b) {
      html += '<span class="chip ' + esc(b.status) + '">' + b.count + ' ' + (names[b.icon] || '') + '</span>';
    });
    if (el.innerHTML !== html) el.innerHTML = html;
  }

  function renderBanner() {
    var el = $('banner'), bar = $('progress');
    var s = snap.screen, html = '', cls = '';
    bar.hidden = true;
    if (s && s.level === 1 && s.data) {
      // A running job: the jobs widget has the detail, the header shows progress.
      bar.hidden = false;
      bar.className = 'topbar' + (s.data.progress == null ? ' spin' : '');
      bar.style.setProperty('--p', Math.round((s.data.progress || 0) * 100) + '%');
    } else if (s && s.level < 5 && s.data) {
      var d = s.data;
      if (s.template === 'alert') {
        var tag = d.status === 'success' ? 'Success' : d.status === 'warn' ? 'Warning' : s.level === 0 ? 'Critical' : 'Failed';
        cls = s.level === 0 ? 'critical' : d.status === 'success' ? 'success' : d.status === 'warn' ? 'warn' : '';
        var title = d.title || ((d.project || '') + (d.pipeline ? ' / ' + d.pipeline : ''));
        var msg = d.message || (d.step ? 'failed at ' + d.step : '');
        html = '<span class="tag">' + tag + '</span><span>' + esc(title) + '</span><span class="msg">' + esc(msg) + '</span>' +
          (d.others ? '<span class="meta">+' + d.others + ' more</span>' : '') + '<span class="meta">' + esc(SOURCES[d.source] || d.source) + '</span>';
      } else if (s.template === 'notice') {
        cls = 'notice';
        html = '<span class="tag">Notice</span><span>' + esc(d.text) + '</span><span class="msg">' + esc(d.sub) + '</span><span class="meta">' + esc(SOURCES[d.source] || d.source) + '</span>';
      }
    }
    el.hidden = !html;
    el.className = 'banner ' + cls;
    if (el.innerHTML !== html) el.innerHTML = html;
  }

  function render(next) {
    snap = next;
    skew = (snap.now || 0) - Date.now() / 1000;
    lastMsg = Date.now();
    if (build && snap.build && snap.build !== build) { location.reload(); return; }
    build = snap.build || build;

    // CPU history for the sparklines, one reading per 5 s at most.
    (snap.hosts || []).forEach(function (h) {
      var v = h.stats && h.stats.cpu_pct;
      if (v == null || Date.now() - (historyAt[h.name] || 0) < 4500) return;
      historyAt[h.name] = Date.now();
      var list = history[h.name] = history[h.name] || [];
      list.push(v);
      if (list.length > HISTORY) list.shift();
    });

    var layout = snap.layout || { columns: 4, rows: 3, panels: [] };
    var grid = $('grid');
    grid.style.setProperty('--cols', layout.columns);
    grid.style.setProperty('--rows', layout.rows);
    var keep = {};
    (layout.panels || []).forEach(function (panel, i) {
      var key = i + ':' + panel.widget + ':' + (panel.host || '');
      keep[key] = true;
      renderCard(key, panel, grid);
    });
    Object.keys(cards).forEach(function (key) {
      if (!keep[key]) { grid.removeChild(cards[key].el); delete cards[key]; }
    });
    renderBadges();
    renderBanner();
    tick();
  }

  // ---- once-a-second work: clock, elapsed times, connection state --------

  function tick() {
    var d = new Date();
    var two = function (n) { return n < 10 ? '0' + n : '' + n; };
    $('time').textContent = two(d.getHours()) + ':' + two(d.getMinutes());
    $('date').textContent = d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' });
    Array.prototype.forEach.call(document.querySelectorAll('[data-since]'), function (el) {
      el.textContent = clock(nowS() - Number(el.getAttribute('data-since')));
    });
    var age = lastMsg ? (Date.now() - lastMsg) / 1000 : Infinity;
    var lost = !snap || age > STALE_AFTER_S;
    document.body.classList.toggle('lost', !!snap && lost);
    var off = $('offline');
    off.hidden = !(snap && lost);
    if (snap && lost) off.textContent = 'No data from the bridge for ' + clock(age) + '. Showing the last known state, reconnecting.';
    var conn = $('conn');
    var ok = snap && !lost;
    conn.className = 'chip ' + (ok ? 'ok' : snap ? 'bad' : '');
    var problems = ok && (snap.sources || []).filter(function (s) { return s.health !== 'ok'; }).length;
    conn.textContent = !snap ? 'connecting' : lost ? 'offline' : problems ? problems + (problems === 1 ? ' source has' : ' sources have') + ' a problem' : 'all sources ok';
    if (problems) conn.className = 'chip bad';
  }

  // ---- network ------------------------------------------------------------

  function withToken(url) { return token ? url + (url.indexOf('?') < 0 ? '?' : '&') + 'token=' + encodeURIComponent(token) : url; }

  var ws = null, retry = 1000, pollTimer = null;

  function poll() {
    fetch(withToken('api/kiosk'), { cache: 'no-store' })
      .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
      .then(render)
      .catch(function () {});
  }

  function connect() {
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + location.pathname.replace(/[^/]*$/, '') + withToken('api/kiosk/ws');
    try { ws = new WebSocket(url); } catch (e) { ws = null; }
    if (!ws) { later(); return; }
    ws.onopen = function () { retry = 1000; if (pollTimer) { clearInterval(pollTimer); pollTimer = null; } };
    ws.onmessage = function (ev) { try { render(JSON.parse(ev.data)); } catch (e) {} };
    ws.onclose = ws.onerror = function () {
      if (!ws) return;
      ws = null;
      // While the socket is down, keep fetching snapshots so a proxy that
      // blocks WebSockets still works.
      if (!pollTimer) { poll(); pollTimer = setInterval(poll, POLL_MS); }
      later();
    };
  }
  function later() { setTimeout(connect, retry); retry = Math.min(retry * 2, 10000); }

  window.DeskWatchKiosk = { render: render, tick: tick, seed: function (name, values) { history[name] = values.slice(-HISTORY); } };
  setInterval(tick, 1000);
  tick();
  if (!params.has('static')) connect();
})();
