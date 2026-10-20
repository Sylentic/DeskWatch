// DeskWatch kiosk page. Plain JavaScript, no libraries, no build step.
//
// The bridge sends one JSON snapshot (see docs/kiosk.md) over a WebSocket
// whenever something changes, and the page redraws from it. If the WebSocket
// cannot connect, the page polls the snapshot endpoint instead. If nothing
// arrives for a while (bridge restarting, network down) the page keeps the
// last data on screen, dims it and says how old it is, then recovers on its own.
//
// When the page cannot get data it says why, in the header chip and in a box
// in the page body: the bridge cannot be reached, the bridge answered with an
// error, the WebSocket is blocked (the page then polls), or the bridge has no
// data yet. There is no login: the page is for a trusted network.
//
// `?static` stops all network use; the screenshots and tests call
// `DeskWatchKiosk.render(snapshot)` themselves.
(function () {
  'use strict';

  // The login (docs/auth.md): every write carries X-DeskWatch (a page on
  // another site cannot add it), and a refused session goes to the sign-in page.
  var plainFetch = window.fetch;
  window.fetch = function (url, opts) {
    opts = opts || {};
    if (opts.method && opts.method !== 'GET') {
      opts.headers = Object.assign({ 'X-DeskWatch': '1' }, opts.headers || {});
    }
    return plainFetch.call(this, url, opts).then(function (r) {
      if (r.status === 401) location.href = '/login?next=' + encodeURIComponent(location.pathname);
      return r;
    });
  };

  var STALE_AFTER_S = 15;      // dim the page when no snapshot has come in this long
  var POLL_MS = 5000;          // polling interval when the WebSocket is down
  var HISTORY = 60;            // samples kept for the CPU sparklines
  var params = new URLSearchParams(location.search);

  var snap = null;             // latest snapshot
  var lastMsg = 0;             // client time (ms) of the latest snapshot
  var skew = 0;                // server clock minus client clock, in seconds
  var build = null;            // bridge version, to reload the page after an upgrade
  var history = {};            // host name -> recent CPU readings
  var historyAt = {};          // host name -> client time of the last reading
  var cards = {};              // card key -> { el, html }
  var problem = null;          // why no data arrives, see describe(); null when all is well
  var wsFailed = false;        // the WebSocket is down, the page polls instead
  var HOLD_MS = 8000;          // a card someone is scrolling is not redrawn for this long
  var RETURN_MS = 30000;       // a scrolled card goes back to the top after this long untouched
  var noticeKey = null;        // which notice is on screen, to redraw it only on a change

  var $ = function (id) { return document.getElementById(id); };

  // ---- small helpers ------------------------------------------------------

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function nowS() { return Date.now() / 1000 + skew; }
  function pct(v) { return v == null ? '--' : Math.round(v) + '%'; }
  function level(v, warn, crit) { return v == null ? 'var(--grey)' : v >= crit ? 'var(--red)' : v >= warn ? 'var(--amber)' : 'var(--blue)'; }
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
  // The CI system a job, run or PR comes from, as a two-letter mark. Every
  // row carries one, so a glance tells where something runs.
  var SOURCES = { gitea: 'Gitea', github: 'GitHub', gitlab: 'GitLab', bitbucket: 'Bitbucket', azure_devops: 'Azure DevOps',
    prometheus: 'Prometheus', home_assistant: 'Home Assistant', local: 'this machine' };
  var MARKS = { gitea: 'Gt', github: 'GH', gitlab: 'GL', bitbucket: 'BB', azure_devops: 'AZ', prometheus: 'PM', home_assistant: 'HA', local: 'LO' };
  function src(kind) {
    // An empty mark keeps the row's columns in line.
    if (!kind) return '<span class="mark blank" aria-hidden="true"></span>';
    var k = MARKS[kind] ? kind : 'other';
    return '<span class="mark ' + esc(k) + '" title="' + esc(SOURCES[kind] || kind) + '">' +
      esc(MARKS[kind] || String(kind).slice(0, 2).toUpperCase()) + '</span>';
  }
  // A source id is "<kind>:<name>"; github-enterprise counts as github.
  function kindOf(id) {
    var k = String(id || '').split(':')[0].replace(/[-_]?enterprise$/, '');
    return k === 'stats' || k === 'proc' ? 'local' : k;
  }
  // Where an alert comes from, for its mark: Home Assistant or anything else.
  function alertKind(a) {
    return /home.?assistant|^ha\b/i.test(a.source || '') ? 'home_assistant' : 'local';
  }
  // A row or card that opens its page on the source. The bridge only sends
  // http(s) addresses, and this checks again, so nothing else can become a
  // link. It opens in a new tab: the dashboard itself stays where it is.
  function href(url) { return typeof url === 'string' && /^https?:\/\//i.test(url) ? url : null; }
  function linkOpen(url, cls) {
    var h = href(url);
    return h ? '<a class="' + cls + ' link" href="' + esc(h) + '" target="_blank" rel="noopener noreferrer">' : '<div class="' + cls + '">';
  }
  function linkClose(url) { return href(url) ? '</a>' : '</div>'; }
  function dot(status) { return '<span class="dot ' + esc(status) + '"></span>'; }

  // ---- widgets ------------------------------------------------------------

  function findHost(name) {
    var hosts = snap.hosts || [];
    for (var i = 0; i < hosts.length; i++) if (hosts[i].name === name) return hosts[i];
    return name ? null : hosts[0] || null;
  }

  // A stats panel without a fixed `host` has arrows to step through the hosts.
  // The choice is kept in this browser, so a reload keeps it. A host that is
  // gone (or never chosen) falls back to the first one.
  var HOST_KEY = 'deskwatch.kiosk.host';
  function chosenHost() {
    try { return localStorage.getItem(HOST_KEY) || ''; } catch (e) { return ''; }
  }
  function stepHost(step) {
    var hosts = snap.hosts || [];
    if (hosts.length < 2) return;
    var at = 0;
    hosts.forEach(function (h, i) { if (h.name === chosenHost()) at = i; });
    chooseHost(hosts[(at + step + hosts.length) % hosts.length].name);
  }
  function chooseHost(name) {
    try { localStorage.setItem(HOST_KEY, name); } catch (e) { /* private mode: the choice lasts until reload */ }
    // The tap itself held the card still (see HOLD_MS); redraw it now.
    Object.keys(cards).forEach(function (key) { cards[key].touched = 0; });
    render(snap);
  }

  // One host in detail: CPU, RAM and disk bars, a few numbers and the CPU graph.
  // A host that was started less than an hour ago: it rebooted, worth a look.
  var REBOOT_S = 3600;
  function hostBody(host) {
    var s = host.stats || {};
    var load = s.load ? s.load.map(function (x) { return x.toFixed(1); }).join(' ') : '--';
    var vals = history[host.name] || [];
    var peak = vals.length ? Math.round(Math.max.apply(null, vals)) + '%' : '--';
    var fresh = s.uptime_s != null && s.uptime_s < REBOOT_S;
    return '<div class="gauges">' + gauge('CPU', s.cpu_pct, 70, 90) + gauge('MEM', s.ram_pct, 80, 90) + gauge('DISK', s.disk_pct, 80, 90) + '</div>' +
      '<div class="stat-grid">' +
      '<div><span class="l">Uptime</span><span' + (fresh ? ' class="warn-text"' : '') + '>' + uptime(s.uptime_s) + '</span></div>' +
      '<div><span class="l">Temp</span><span>' + (s.cpu_temp_c == null ? '--' : Math.round(s.cpu_temp_c) + ' &deg;C') + '</span></div>' +
      '<div><span class="l">Load</span><span>' + load + '</span></div>' +
      '<div><span class="l">Net in</span><span>' + (s.net ? rate(s.net.rx_bps) : '--') + '</span></div>' +
      '<div><span class="l">Net out</span><span>' + (s.net ? rate(s.net.tx_bps) : '--') + '</span></div>' +
      '<div><span class="l">Cores</span><span>' + (s.cpu_count == null ? '--' : s.cpu_count) + '</span></div>' +
      '</div><div class="sparkhead"><span>CPU &middot; ' + Math.round(HISTORY * 5 / 60) + ' MIN</span><span>PEAK ' + peak + '</span></div>' +
      sparkline(vals);
  }

  // Does a host need a look: down, a value over its warning line, or rebooted.
  function hostLook(h) {
    var s = h.stats || {};
    if (!h.up) return 'off';
    if (s.cpu_pct >= 70 || s.ram_pct >= 80 || s.disk_pct >= 80) return 'look';
    if (s.uptime_s != null && s.uptime_s < REBOOT_S) return 'look';
    return '';
  }
  // The highest of CPU, MEM and DISK, as "DISK 88", for the host list.
  function hostKey(h) {
    var s = h.stats || {};
    var pick = [['CPU', s.cpu_pct, 70, 90], ['MEM', s.ram_pct, 80, 90], ['DISK', s.disk_pct, 80, 90]]
      .filter(function (x) { return x[1] != null; })
      .sort(function (a, b) { return b[1] - a[1]; })[0];
    if (!pick) return '<span class="k">--</span>';
    var cls = pick[1] >= pick[3] ? ' crit-text' : pick[1] >= pick[2] ? ' warn-text' : '';
    return '<span class="k' + cls + '">' + pick[0] + ' ' + Math.round(pick[1]) + '</span>';
  }

  // The detailed hosts panel: a list to pick from next to the chosen host (the
  // host above the list when the card is narrow). Hosts that need a look come
  // first. Tapping a host chooses it, kept in this browser. The chosen host
  // never changes by itself: a host going down gets a red pip in the list.
  function hostsDetailed(hosts) {
    var host = findHost(chosenHost()) || hosts[0];
    var rank = { off: 0, look: 1, '': 2 };
    var order = hosts.map(function (h, i) { return { h: h, i: i, r: rank[hostLook(h)] }; })
      .sort(function (a, b) { return a.r - b.r || a.i - b.i; });
    var list = '<div class="hostlist" role="listbox" aria-label="Hosts">';
    order.forEach(function (o) {
      var h = o.h, look = hostLook(h);
      list += '<button type="button" role="option" class="hostrow' + (h === host ? ' sel' : '') + (look ? ' ' + look : '') +
        '" data-host-pick="' + esc(h.name) + '" aria-selected="' + (h === host ? 'true' : 'false') + '">' +
        '<span class="pip"></span><span class="n">' + esc(h.name) + '</span>' + (h.up ? hostKey(h) : '<span class="k crit-text">down</span>') + '</button>';
    });
    list += '</div>';
    var down = hosts.filter(function (h) { return !h.up; }).length;
    var look = hosts.filter(function (h) { return hostLook(h) === 'look'; }).length;
    var tag = down ? down + ' down' : look ? look + (look === 1 ? ' needs' : ' need') + ' a look' : '';
    return {
      title: 'Hosts', count: (hosts.length - down) + '/' + hosts.length + ' up', tag: tag,
      stale: host.stale || !host.up,
      body: '<div class="hsplit">' + list + '<div class="hostdetail"><div class="hn">' + esc(host.name) +
        (host.up ? '' : '<span class="meta crit-text">not answering</span>') + '</div>' + hostBody(host) + '</div></div>'
    };
  }

  function ciStale() {
    // Only the CI sources: a Prometheus outage does not make the PR list old.
    return (snap.sources || []).some(function (s) { return s.health !== 'ok' && /^(gitea|github|gitlab|bitbucket|azure_devops):/.test(s.id); });
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

  // ---- filters ------------------------------------------------------------

  // `source` and `repo` of a panel (jobs, prs and pipelines widgets): keep only
  // what comes from that CI system and that repository or project. `repo`
  // ignores case and may end in * to match a prefix. Without either, all stays.
  function matches(panel, source, project) {
    if (panel.source && panel.source !== source) return false;
    var repo = (panel.repo || '').toLowerCase(), name = String(project || '').toLowerCase();
    if (!repo) return true;
    return repo.slice(-1) === '*' ? name.indexOf(repo.slice(0, -1)) === 0 : name === repo;
  }

  // ---- widgets ------------------------------------------------------------
  // One entry per widget kind: name -> function(panel) returning
  // { title, body, count?, tag?, stale? }. A new widget is one more entry here,
  // plus its name in config.rs.

  // Seconds after which a runner list counts as old (a few missed polls).
  var RUNNERS_OLD = 150;
  // How long a finished run stays in Running now as "Done", a just-finished
  // row stays tinted, and an open PR carries NEW.
  var DONE_S = 30, FRESH_S = 30, NEW_PR_S = 3600;

  var widgets = {
    stats: function (p) {
      var hosts = snap.hosts || [], nav = '';
      var host = p.host ? findHost(p.host) : findHost(chosenHost()) || findHost('');
      if (!p.host && hosts.length > 1 && host) {
        nav = '<span class="hostnav"><button type="button" data-host-step="-1" aria-label="Previous host">&lsaquo;</button>' +
          '<span class="pos">' + (hosts.indexOf(host) + 1) + '/' + hosts.length + '</span>' +
          '<button type="button" data-host-step="1" aria-label="Next host">&rsaquo;</button></span>';
      }
      if (!host) return { title: p.host || 'Server', stale: true, body: '<div class="empty">No data yet</div>' };
      var html = hostBody(host);
      return { title: p.title || host.name, nav: nav, tag: host.up ? '' : 'not answering', stale: host.stale || !host.up, body: html };
    },

    hosts: function (p) {
      if (p && p.view === 'detailed' && (snap.hosts || []).length) return hostsDetailed(snap.hosts);
      // The compact host view is a taller table: it adds the network rates, as
      // columns when it is two cells wide, else as a small line under each host.
      var span = (p && p.span) || [1, 1];
      var wide = span[0] >= 2, tall = !wide && span[1] >= 2;
      var hosts = snap.hosts || [];
      var html;
      if (!hosts.length) html = '<div class="empty">No hosts</div>';
      else {
        // Hosts that need a look come first, like in the detailed view.
        var rank = { off: 0, look: 1, '': 2 };
        var order = hosts.map(function (h, i) { return { h: h, i: i, r: rank[hostLook(h)] }; })
          .sort(function (a, b) { return a.r - b.r || a.i - b.i; }).map(function (o) { return o.h; });
        html = '<div class="hosts' + (wide ? ' wide' : '') + '"><span class="h">Host</span><span class="h">Uptime</span><span class="h">CPU</span><span class="h">Mem</span><span class="h">Disk</span>' +
          (wide ? '<span class="h">&darr; In</span><span class="h">&uarr; Out</span>' : '');
        order.forEach(function (h) {
          var s = h.stats || {};
          var cell = function (v, w, c) { return '<span class="v ' + (v >= c ? 'crit-text' : v >= w ? 'warn-text' : '') + '">' + (v == null ? '--' : Math.round(v)) + '</span>'; };
          var fresh = s.uptime_s != null && s.uptime_s < REBOOT_S;
          var net = tall ? '<span class="net">&darr; ' + (s.net ? rate(s.net.rx_bps) : '--') + ' &nbsp; &uarr; ' + (s.net ? rate(s.net.tx_bps) : '--') + '</span>' : '';
          html += '<span class="n' + (h.up ? '' : ' off') + '">' + esc(h.name) + '</span>' +
            '<span class="u' + (fresh ? ' warn-text' : '') + '">' + (h.up ? uptime(s.uptime_s) : 'down') + '</span>' +
            cell(s.cpu_pct, 70, 90) + cell(s.ram_pct, 80, 90) + cell(s.disk_pct, 80, 90) +
            (wide ? '<span class="r">' + (s.net ? rate(s.net.rx_bps) : '--') + '</span><span class="r">' + (s.net ? rate(s.net.tx_bps) : '--') + '</span>' : '') + net;
        });
        html += '</div>';
      }
      var down = hosts.filter(function (h) { return !h.up; }).length;
      var look = hosts.filter(function (h) { return hostLook(h) === 'look'; }).length;
      return { title: 'Hosts', count: (hosts.length - down) + '/' + hosts.length + ' up',
        tag: down ? down + ' down' : look ? look + (look === 1 ? ' needs' : ' need') + ' a look' : '', body: html };
    },

    containers: function (p) {
      var down = snap.down || [];
      var rows = down.map(function (d) {
        return '<div class="row compact nomark">' + dot('failed') + '<span class="main">' + esc(d.text) + '</span><span class="when">' + esc(d.sub) + '</span></div>';
      });
      return { title: 'Containers', count: down.length || '', body: list(rows, p.rows, '<div class="empty good">Everything is running</div>') };
    },

    jobs: function (p) {
      var jobs = (snap.jobs || []).filter(function (j) { return matches(p, j.source, j.project); });
      // A run that just finished well stays here for DONE_S as "Done", then
      // only Pipelines has it. A failed run goes straight to the red pill.
      var done = (snap.runs || []).filter(function (r) {
        return r.status === 'success' && r.finished && nowS() - r.finished < DONE_S && matches(p, r.source, r.project) &&
          !jobs.some(function (j) { return j.project === r.project && j.pipeline === r.pipeline; });
      });
      if (!jobs.length && !done.length) return { title: 'Running now', body: '<div class="empty">No jobs running</div>', stale: false, plain: true };
      var max = p.rows;
      var items = done.map(function (r) { return { done: r }; }).concat(jobs.map(function (j) { return { job: j }; }));
      var shown = max ? items.slice(0, max) : items;
      var html = shown.map(function (it) {
        if (it.done) {
          var r = it.done;
          return linkOpen(r.url, 'job done fresh ok') + '<div class="l1">' + src(r.source) + '<span class="name">' + esc(r.project) + '</span>' +
            '<span class="ref">' + esc(r.pipeline) + (r.ref ? ' &middot; ' + esc(r.ref) : '') + '</span></div>' +
            '<div class="l2"><span class="steps"><i class="d"></i></span><span class="step">Done</span>' +
            '<span class="el">' + clock((r.finished || r.started) - r.started) + '</span></div>' + linkClose(r.url);
        }
        var j = it.job;
        var step = j.step ? esc(j.step) : 'starting';
        var bar;
        if (j.step_no && j.step_count && j.step_count <= 12) {
          bar = '<span class="steps" title="step ' + j.step_no + ' of ' + j.step_count + '">';
          for (var i = 1; i <= j.step_count; i++) bar += '<i class="' + (i < j.step_no ? 'd' : i === j.step_no ? 'c' : '') + '"></i>';
          bar += '</span>';
        } else {
          bar = j.progress == null ? '<span class="pbar spin"><i></i></span>' :
            '<span class="pbar"><i style="--p:' + Math.round(j.progress * 100) + '%"></i></span>';
        }
        return linkOpen(j.url, 'job') + '<div class="l1">' + src(j.source) + '<span class="name">' + esc(j.project) + '</span>' +
          '<span class="kind ' + esc(j.kind) + '">' + esc(j.kind) + '</span>' +
          '<span class="ref">' + esc(j.pipeline) + (j.ref ? ' &middot; ' + esc(j.ref) : '') + (j.commit ? ' @ ' + esc(j.commit) : '') + '</span></div>' +
          '<div class="l2">' + bar + '<span class="step">' + step + '</span>' +
          '<span class="el" data-since="' + j.started + '">' + clock(nowS() - j.started) + '</span></div>' + linkClose(j.url);
      }).join('');
      if (max && items.length > max) html += '<div class="more">+' + (items.length - max) + ' more running</div>';
      return { title: 'Running now', count: jobs.length + (done.length ? ' · ' + done.length + ' done' : ''), stale: ciStale(), body: html };
    },

    pipelines: function (p) {
      var runs = (snap.runs || []).filter(function (r) { return matches(p, r.source, r.project); });
      var rows = runs.map(function (r) {
        var when = r.status === 'running' ? 'running' : ago(nowS() - (r.finished || r.started));
        var status = runStatus(r.status);
        var fresh = r.finished && nowS() - r.finished < FRESH_S && (status === 'failed' || status === 'ok') ? ' fresh ' + status : '';
        return linkOpen(r.url, 'row' + fresh) + dot(status) + src(r.source) +
          '<span class="main">' + esc(r.project) + ' <span class="wf">&middot; ' + esc(r.pipeline) + '</span></span>' +
          '<span class="when">' + when + '</span>' +
          (r.ref || r.step ? '<span class="sub">' + esc(r.ref) + (r.step ? ' &middot; failed at ' + esc(r.step) : '') + '</span>' : '') + linkClose(r.url);
      });
      return { title: 'Pipelines', count: runs.length, stale: ciStale(), body: list(rows, p.rows, '<div class="empty">No runs yet</div>') };
    },

    prs: function (p) {
      var repos = (snap.pulls || []).filter(function (r) { return r.total > 0 && matches(p, r.source, r.project); });
      var total = repos.reduce(function (n, r) { return n + r.total; }, 0);
      // One flat list, newest first; the repository goes in the line under the title.
      var items = [];
      repos.forEach(function (r) { (r.items || []).forEach(function (pr) { items.push({ pr: pr, repo: r }); }); });
      items.sort(function (a, b) { return (b.pr.created || 0) - (a.pr.created || 0); });
      var rows = items.map(function (it) {
        var pr = it.pr, isNew = pr.created && nowS() - pr.created < NEW_PR_S;
        return linkOpen(pr.url, 'row pr') + dot('open') + src(it.repo.source) +
          '<span class="main"><span class="num">#' + pr.number + '</span>' + esc(pr.title) + (isNew ? '<span class="tagnew">NEW</span>' : '') + '</span>' +
          '<span class="when">' + ago(nowS() - pr.created) + '</span><span class="sub">' + esc(it.repo.project) + '</span>' + linkClose(pr.url);
      });
      // A repository whose items the bridge does not send still counts.
      repos.forEach(function (r) {
        var shown = (r.items || []).length;
        if (r.total > shown) rows.push('<div class="row">' + dot('open') + src(r.source) + '<span class="main">' + esc(r.project) + '</span>' +
          '<span class="when">' + (r.total - shown) + ' more</span></div>');
      });
      return { title: 'Open PRs', count: total, stale: ciStale(), body: list(rows, p.rows, '<div class="empty good">No open PRs</div>') };
    },

    // Each alert is a tinted box (red, amber or blue) with a Dismiss button when
    // `[alerts] dismiss` allows it; dismissing clears it on every screen.
    alerts: function (p) {
      var alerts = snap.alerts || [];
      var rows = alerts.map(function (a) {
        var sub = [a.message ? esc(a.message) : '', ago(nowS() - a.since)].filter(Boolean).join(' &middot; ');
        return '<div class="abox ' + esc(a.severity) + '">' + src(alertKind(a)) +
          '<span class="txt"><span class="main">' + esc(a.title) + '</span><span class="sub">' + sub + '</span></span>' +
          (canDismiss() ? '<button type="button" class="ax" data-dismiss-alert="' + esc(a.id) + '" aria-label="Dismiss ' + esc(a.title) + '">&times;</button>' : '') +
          '</div>';
      });
      return { title: 'Alerts', count: alerts.length || '', body: list(rows, p.rows, '<div class="empty good">All quiet</div>') };
    },

    runners: function (p) {
      var runners = snap.runners || [];
      var offline = runners.filter(function (r) { return r.status === 'offline' && !r.disabled; }).length;
      // The bridge polls every 30 s or so; a list this old stopped updating.
      var old = runners.some(function (r) { return nowS() - r.updated > RUNNERS_OLD; });
      var rows = runners.map(function (r) {
        var state = r.disabled ? 'disabled' : r.status;
        var dotState = r.disabled ? 'neutral' : r.status === 'busy' ? 'running' : r.status === 'idle' ? 'ok' : 'failed';
        var labels = (r.labels || []).join(', ');
        return linkOpen(r.url, 'row' + (r.disabled ? ' off' : '')) + dot(dotState) + src(r.source) + '<span class="main">' + esc(r.name) + '</span>' +
          '<span class="when ' + (r.status === 'offline' && !r.disabled ? 'crit-text' : '') + '">' + esc(state) + '</span>' +
          '<span class="sub">' + (labels ? esc(labels) : 'no labels') + '</span>' + linkClose(r.url);
      });
      // Jobs waiting for an agent (Azure DevOps pools), busiest pool first.
      var queued = snap.queued_jobs || [];
      var waiting = queued.reduce(function (n, q) { return n + q.queued; }, 0);
      queued.forEach(function (q) {
        if (nowS() - q.updated > RUNNERS_OLD) { old = true; }
        rows.unshift('<div class="row">' + dot('waiting') + src(q.source) + '<span class="main">' + esc(q.pool) + '</span>' +
          '<span class="when">' + q.queued + ' waiting</span>' +
          '<span class="sub">for an agent</span></div>');
      });
      var tags = [];
      if (offline) { tags.push(offline + ' offline'); }
      if (waiting) { tags.push(waiting + ' waiting'); }
      return { title: 'Runners', count: runners.length, tag: tags.join(', '), stale: ciStale() || old, body: list(rows, p.rows, '<div class="empty">No runners configured</div>') };
    },

    health: function () {
      var sources = snap.sources || [];
      var bad = sources.filter(function (s) { return s.health !== 'ok'; }).length;
      var names = { ok: 'working', auth_failed: 'token refused', unreachable: 'not reachable' };
      var rows = sources.map(function (s) {
        return '<div class="row compact">' + dot(s.health === 'ok' ? 'ok' : 'failed') + src(kindOf(s.id)) + '<span class="main">' + esc(s.id) + '</span><span class="when">' + (names[s.health] || esc(s.health)) + '</span></div>';
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
    var html = '<header><h2>' + esc(panel.title || out.title) + '</h2>' + (out.nav || '') +
      (out.tag ? '<span class="tag">' + esc(out.tag) + '</span>' : '') +
      (out.count !== undefined && out.count !== '' ? '<span class="count">' + esc(out.count) + '</span>' : '') +
      '</header><div class="body">' + out.body + '</div>';
    var entry = cards[key];
    if (!entry) {
      var el = document.createElement('section');
      el.setAttribute('data-key', key);
      var span = panel.span || [1, 1];
      el.style.gridColumn = 'span ' + span[0];
      el.style.gridRow = 'span ' + span[1];
      grid.appendChild(el);
      entry = cards[key] = { el: el, html: '', touched: 0 };
    }
    entry.el.className = 'card w-' + panel.widget + (out.stale ? ' stale' : '');
    // A redraw replaces the card's contents. It keeps the scroll position, and
    // waits while someone is scrolling the card (the next snapshot, at most
    // 5 s later, catches up) so a touch gesture is not cut off.
    if (entry.html !== html && Date.now() - entry.touched >= HOLD_MS) {
      var old = entry.el.querySelector('.body');
      var top = old ? old.scrollTop : 0;
      entry.el.innerHTML = html;
      entry.html = html;
      var fresh = entry.el.querySelector('.body');
      if (fresh && top) fresh.scrollTop = top;
    }
    hint(entry.el);
  }

  // A card with more below shows a fade at its bottom edge (there is no
  // scrollbar). Wheel, touch and keys scroll it.
  function hint(el) {
    var body = el.querySelector('.body');
    var more = !!body && body.scrollHeight - body.clientHeight - body.scrollTop > 4;
    if (el.classList.contains('more-below') !== more) el.classList.toggle('more-below', more);
  }

  // Remember when a card was last touched, wheeled or keyed.
  function touched(ev) {
    var el = ev.target && ev.target.closest ? ev.target.closest('.card') : null;
    var entry = el && cards[el.getAttribute('data-key')];
    if (entry) entry.touched = Date.now();
  }
  ['wheel', 'touchstart', 'touchmove', 'pointerdown', 'keydown'].forEach(function (name) {
    document.addEventListener(name, touched, { passive: true, capture: true });
  });
  // Scroll events do not bubble; listen on the way down to update the fade.
  document.addEventListener('scroll', function (ev) {
    var el = ev.target && ev.target.closest ? ev.target.closest('.card') : null;
    if (el) hint(el);
  }, { passive: true, capture: true });

  // A card that was scrolled and then left alone goes back to the top, so a
  // wall display shows the newest items again.
  function returnToTop() {
    Object.keys(cards).forEach(function (key) {
      var entry = cards[key], body = entry.el.querySelector('.body');
      if (entry.touched && Date.now() - entry.touched > RETURN_MS) {
        entry.touched = 0;
        if (body && body.scrollTop > 0) body.scrollTo({ top: 0, behavior: 'smooth' });
      }
    });
  }

  // One listener for the arrows: cards are rebuilt on every snapshot.
  $('grid').addEventListener('click', function (e) {
    var b = e.target.closest ? e.target.closest('[data-host-step]') : null;
    if (b) stepHost(Number(b.getAttribute('data-host-step')));
    var pick = e.target.closest ? e.target.closest('[data-host-pick]') : null;
    if (pick) chooseHost(pick.getAttribute('data-host-pick'));
    // The Alerts card's dismiss buttons work like the ones in the Alerts menu.
    var gone = e.target.closest ? e.target.closest('[data-dismiss-alert]') : null;
    if (gone) {
      var id = gone.getAttribute('data-dismiss-alert');
      snap.alerts = alertList().filter(function (a) { return a.id !== id; });
      Object.keys(cards).forEach(function (key) { cards[key].touched = 0; });
      drawGrid(); renderBadges(); renderTicker();
      dismissAlerts([id]);
    }
  });

  // ---- failed runs: badge, list and dismiss --------------------------------

  // A dismissed failure is kept on the bridge, so it goes away on every
  // screen at once (like an alert). The snapshot no longer lists it; until
  // the next snapshot arrives the page hides it itself. A new failure of the
  // same pipeline has another start time, so it shows up again.
  var failedOpen = false;
  function failId(f) { return f.key + '@' + f.started; }
  // A newer release is a quiet chip next to Setup. The x hides that version
  // in this browser only; the next release shows again.
  function renderUpdate(u) {
    var chip = $('update-chip'), seen = null;
    try { seen = localStorage.getItem('deskwatch.update.hidden'); } catch (e) {}
    var ok = u && /^[0-9][0-9A-Za-z.+-]*$/.test(u.version) && /^https:\/\//.test(u.url || '') && seen !== u.version;
    chip.hidden = !ok;
    if (!ok) return;
    $('update-link').textContent = 'update available: v' + u.version;
    $('update-link').href = u.url;
    $('update-link').title = 'Open the release notes';
    $('update-x').onclick = function () {
      try { localStorage.setItem('deskwatch.update.hidden', u.version); } catch (e) {}
      chip.hidden = true;
    };
  }
  function visibleFailed() {
    return snap.failed || [];
  }
  function dismissFailed(id) {
    // id null: dismiss everything that is listed now.
    var ids = id ? [id] : visibleFailed().map(failId);
    if (!ids.length) return;
    snap.failed = visibleFailed().filter(function (f) { return ids.indexOf(failId(f)) < 0; });
    renderBadges();
    fetch('api/kiosk/failed/dismiss', {
      method: 'POST', cache: 'no-store', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ids: ids })
    }).catch(function () { /* the next snapshot shows what is left */ });
  }

  function failedRow(f) {
    var what = esc(f.project || '') + (f.pipeline ? ' / ' + esc(f.pipeline) : '');
    var sub = [f.ref ? esc(f.ref) : '', f.step ? 'step ' + esc(f.step) : '', f.finished ? ago(nowS() - f.finished) : '']
      .filter(Boolean).join(' · ');
    var open = linkOpen(f.url, 'what') ;
    return '<li>' + open + src(f.source) + ' <b>' + what + '</b><span class="sub">' + sub + '</span>' + linkClose(f.url) +
      '<button type="button" class="x" data-dismiss="' + esc(failId(f)) + '" aria-label="Dismiss">Dismiss</button></li>';
  }

  function renderFailedPop() {
    var pop = $('failed-pop'), list = visibleFailed();
    if (!failedOpen || !list.length) { failedOpen = failedOpen && list.length > 0; pop.hidden = true; pop.innerHTML = ''; return; }
    var html = '<div class="head"><b>Failed runs</b>' +
      '<button type="button" class="x all" data-dismiss-all="1">Dismiss all</button></div><ul>' + list.map(failedRow).join('') + '</ul>' +
      '<div class="foot">Dismissing clears the run on every screen. A new failure shows up again.</div>';
    if (pop.innerHTML !== html) pop.innerHTML = html;
    pop.hidden = false;
    var chip = document.querySelector('[data-failed-toggle]');
    if (chip) pop.style.left = Math.max(8, Math.min(chip.getBoundingClientRect().left, window.innerWidth - pop.offsetWidth - 8)) + 'px';
  }

  // ---- alerts: badge, list and dismiss -------------------------------------

  // The Alerts badge works like the Failed badge: click for the list. Dismiss
  // is different in one way: it asks the bridge to clear the alert, so it goes
  // away on every screen (the same as a `clear` message on MQTT). With
  // `[alerts] dismiss = false` the list is read-only.
  var alertsOpen = false;
  var SEV_NAME = { critical: 'Critical', warn: 'Warning', info: 'Info' };
  function alertList() {
    return snap.alerts || [];
  }
  function canDismiss() {
    var cfg = (snap.layout && snap.layout.alerts) || {};
    return !!cfg.dismiss;
  }
  function dismissAlerts(ids) {
    // One after the other, so the bridge never has more than one clear in flight.
    return ids.reduce(function (chain, id) {
      return chain.then(function () {
        return fetch('api/kiosk/alert/dismiss', {
          method: 'POST', cache: 'no-store', credentials: 'same-origin',
          headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: id })
        });
      });
    }, Promise.resolve()).catch(function () { /* the next snapshot shows what is left */ });
  }
  function alertRow(a) {
    var cfg = (snap.layout && snap.layout.alerts) || {};
    var sub = [a.message ? esc(a.message) : '', a.source && cfg.show_source !== false ? 'via ' + esc(a.source) : '', a.since ? ago(nowS() - a.since) : '']
      .filter(Boolean).join(' &middot; ');
    return '<li><span class="what"><span class="dot ' + esc(a.severity) + '"></span><b>' + esc(a.title) + '</b>' +
      '<span class="sev ' + esc(a.severity) + '">' + esc(SEV_NAME[a.severity] || a.severity) + '</span><span class="sub">' + sub + '</span></span>' +
      (canDismiss() ? '<button type="button" class="x" data-dismiss-alert="' + esc(a.id) + '" aria-label="Dismiss alert">Dismiss</button>' : '') + '</li>';
  }
  function renderAlertsPop() {
    var pop = $('alerts-pop'), list = alertList();
    if (!alertsOpen || !list.length) { alertsOpen = alertsOpen && list.length > 0; pop.hidden = true; pop.innerHTML = ''; return; }
    var html = '<div class="head"><b>Alerts</b>' +
      (canDismiss() ? '<button type="button" class="x all" data-dismiss-all-alerts="1">Dismiss all</button>' : '') + '</div><ul>' +
      list.map(alertRow).join('') + '</ul>' +
      (canDismiss() ? '<div class="foot">Dismissing clears the alert on every screen.</div>' : '');
    if (pop.innerHTML !== html) pop.innerHTML = html;
    pop.hidden = false;
    var chip = document.querySelector('[data-alerts-toggle]');
    if (chip) pop.style.left = Math.max(8, Math.min(chip.getBoundingClientRect().left, window.innerWidth - pop.offsetWidth - 8)) + 'px';
  }

  function renderBadges() {
    var el = $('badges'), html = '';
    var names = { pr: 'open PRs', pipeline: 'failed', server: 'down', warn: 'with a problem', home: 'alerts' };
    // Order: Open PRs, Failed runs, Alerts, then any other badge.
    var rank = { pr: 0, pipeline: 1 };
    var list = (snap.badges || []).slice().sort(function (x, y) {
      return (x.id === 'failed' ? 1 : rank[x.icon] === undefined ? 5 : rank[x.icon]) - (y.id === 'failed' ? 1 : rank[y.icon] === undefined ? 5 : rank[y.icon]);
    });
    list.forEach(function (b) {
      if (b.id === 'alerts') return; // the Alerts chip below replaces it
      if (b.id === 'failed' && snap.failed) {
        // The count follows what is not dismissed; the chip opens the list and is gone at 0.
        var n = visibleFailed().length;
        if (!n) { failedOpen = false; return; }
        html += '<button type="button" class="chip failed action" data-failed-toggle="1" aria-expanded="' + (failedOpen ? 'true' : 'false') +
          '" title="Show the failed runs">' + n + ' failed</button>';
        return;
      }
      if (b.icon === 'pr' && !b.count) return; // no open PRs: no badge
      html += '<span class="chip ' + esc(b.status) + '">' + b.count + ' ' + (names[b.icon] || '') + '</span>';
    });
    // The Alerts chip: every active alert, worst colour wins. Gone at 0, like Failed and Open PRs.
    var alerts = alertList(), worst = alerts.some(function (a) { return a.severity === 'critical'; }) ? 'failed'
      : alerts.some(function (a) { return a.severity === 'warn'; }) ? 'review' : alerts.length ? 'open' : 'ok';
    if (alerts.length) html += '<button type="button" class="chip ' + worst + ' action" data-alerts-toggle="1" aria-expanded="' + (alertsOpen ? 'true' : 'false') +
      '" title="Show the alerts">' + alerts.length + ' alerts</button>';
    if (!alerts.length) alertsOpen = false;
    if (el.innerHTML !== html) el.innerHTML = html;
    renderFailedPop();
    renderAlertsPop();
  }

  // One listener each: the badge row and the popover are rebuilt on every snapshot.
  $('badges').addEventListener('click', function (e) {
    if (e.target.closest && e.target.closest('[data-failed-toggle]')) { failedOpen = !failedOpen; alertsOpen = false; renderBadges(); }
    if (e.target.closest && e.target.closest('[data-alerts-toggle]')) { alertsOpen = !alertsOpen; failedOpen = false; renderBadges(); }
  });
  $('alert-pill').addEventListener('click', function () { alertsOpen = !alertsOpen; failedOpen = false; renderBadges(); });
  $('alerts-pop').addEventListener('click', function (e) {
    var one = e.target.closest ? e.target.closest('[data-dismiss-alert]') : null;
    if (one) { dismissAlerts([one.getAttribute('data-dismiss-alert')]); return; }
    if (e.target.closest && e.target.closest('[data-dismiss-all-alerts]')) dismissAlerts(alertList().map(function (a) { return a.id; }));
  });
  $('failed-pop').addEventListener('click', function (e) {
    var one = e.target.closest ? e.target.closest('[data-dismiss]') : null;
    if (one) { dismissFailed(one.getAttribute('data-dismiss')); return; }
    if (e.target.closest && e.target.closest('[data-dismiss-all]')) dismissFailed(null);
  });
  document.addEventListener('click', function (e) {
    if (failedOpen && document.contains(e.target) && e.target.closest && !e.target.closest('#failed-pop') && !e.target.closest('[data-failed-toggle]')) {
      failedOpen = false; renderBadges();
    }
    if (alertsOpen && document.contains(e.target) && e.target.closest && !e.target.closest('#alerts-pop') && !e.target.closest('[data-alerts-toggle]') && !e.target.closest('#alert-pill')) {
      alertsOpen = false; renderBadges();
    }
  });
  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape' && (failedOpen || alertsOpen)) { failedOpen = false; alertsOpen = false; renderBadges(); }
  });

  // ---- most urgent alert in the header --------------------------------------

  // The most urgent active alert is a coloured pill next to the badges, with
  // "+N" for the others; it opens the Alerts menu. No alerts: no pill. The
  // `ticker` setting switches the pill off.
  var SEV_CLASS = { critical: 'critical', warn: 'warn', info: 'note' };
  function renderTicker() {
    var el = $('alert-pill');
    var cfg = (snap.layout && snap.layout.alerts) || {};
    var alerts = cfg.ticker === false ? [] : snap.alerts || [];
    var a = alerts[0];
    // The header shows one pill: a run result or notice wins over the alert pill.
    if (!$('banner').hidden) a = null;
    el.hidden = !a;
    if (!a) { el.innerHTML = ''; return; }
    var html = '<span class="tag">' + esc(SEV_NAME[a.severity] || a.severity) + '</span><span>' + esc(a.title) + '</span>' +
      (a.message ? '<span class="msg">' + esc(a.message) + '</span>' : '') +
      (alerts.length > 1 ? '<span class="more">+' + (alerts.length - 1) + ' more</span>' : '') +
      (a.source && cfg.show_source !== false ? '<span class="meta">' + esc(a.source) + '</span>' : '');
    el.className = 'banner pill ' + (SEV_CLASS[a.severity] || 'note');
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
      if (s.template === 'alert' && d.title) {
        // An alert from Home Assistant or a script: the alert pill shows it, nothing here.
      } else if (s.template === 'alert') {
        var runKey = (d.project || '') + '/' + (d.pipeline || '');
        // A success right after a failure of the same pipeline is "Fixed": the
        // bridge has already replaced the red result with this green one.
        var fixed = d.status === 'success' && failedSeen[runKey];
        var tag = fixed ? 'Fixed' : d.status === 'success' ? (/deploy/i.test(d.kind || d.pipeline || '') ? 'Deployed' : 'Success')
          : d.status === 'warn' ? 'Warning' : s.level === 0 ? 'Critical' : 'Failed';
        cls = s.level === 0 ? 'critical' : d.status === 'success' ? 'success' : d.status === 'warn' ? 'warn' : '';
        var title = d.title || ((d.project || '') + (d.pipeline ? ' · ' + d.pipeline : ''));
        var msg = d.message || (d.step ? 'step ' + d.step : '');
        html = '<span class="tag">' + tag + '</span>' + src(d.source) + '<span>' + esc(title) + '</span>' + (msg ? '<span class="msg">' + esc(msg) + '</span>' : '') +
          (d.others ? '<span class="meta">+' + d.others + '</span>' : '');
      } else if (s.template === 'notice') {
        cls = 'note';
        html = '<span class="tag">Notice</span>' + src(d.source) + '<span>' + esc(d.text) + '</span><span class="msg">' + esc(d.sub) + '</span>';
      }
    }
    // A failed run that is not dismissed is a red pill until someone dismisses
    // it, whatever the panel's screen shows: a running job, a quiet repository
    // (no `interrupt`) or a failure older than ten minutes all keep it. Only a
    // critical alert (the alert pill) comes before it.
    var cfg = (snap.layout && snap.layout.alerts) || {};
    var critical = cfg.ticker !== false && alertList().some(function (a) { return a.severity === 'critical'; });
    var failed = visibleFailed().slice().sort(function (a, b) { return (b.finished || b.started || 0) - (a.finished || a.started || 0); });
    if (failed.length && !critical && !(s && s.level === 0)) {
      var f = failed[0], fh = href(f.url);
      var fmsg = [f.step ? 'step ' + f.step : '', f.finished ? ago(nowS() - f.finished) : ''].filter(Boolean).join(' · ');
      cls = '';
      html = '<span class="tag">Failed</span>' + src(f.source) +
        '<span>' + esc((f.project || '') + (f.pipeline ? ' · ' + f.pipeline : '')) + '</span>' +
        (fmsg ? '<span class="msg">' + esc(fmsg) + '</span>' : '') +
        (failed.length > 1 ? '<span class="meta">+' + (failed.length - 1) + '</span>' : '') +
        (fh ? '<a class="act" href="' + esc(fh) + '" target="_blank" rel="noopener noreferrer">Open</a>' : '') +
        '<button type="button" class="act x" data-dismiss="' + esc(failId(f)) + '" aria-label="Dismiss this failed run">&times;</button>';
    }
    el.hidden = !html;
    // A new red result pulses once; the same one redrawn does not.
    var fresh = html && cls !== 'success' && cls !== 'note' && html !== bannerSeen;
    if (html) bannerSeen = html;
    el.className = 'banner ' + cls + (fresh ? ' fresh' : '');
    if (el.innerHTML !== html) el.innerHTML = html;
  }
  var bannerSeen = '';
  // The failed pill: x dismisses that run, Open opens it, the rest of the pill
  // opens the list of failed runs.
  $('banner').addEventListener('click', function (e) {
    var x = e.target.closest ? e.target.closest('[data-dismiss]') : null;
    if (x) { dismissFailed(x.getAttribute('data-dismiss')); renderBanner(); renderTicker(); return; }
    if (e.target.closest && e.target.closest('a')) return;
    if (visibleFailed().length && $('banner').querySelector('[data-dismiss]')) { failedOpen = !failedOpen; alertsOpen = false; renderBadges(); }
  });
  // Pipelines that failed recently (project/pipeline -> true), so a success
  // that follows can say Fixed. Kept for a day.
  var failedSeen = {}, failedSeenAt = {};
  function noteFailures() {
    var t = nowS();
    (snap.failed || []).forEach(function (f) {
      var k = (f.project || '') + '/' + (f.pipeline || '');
      failedSeen[k] = true; failedSeenAt[k] = t;
    });
    (snap.runs || []).forEach(function (r) {
      var k = r.project + '/' + r.pipeline;
      if (r.status === 'failed') { failedSeen[k] = true; failedSeenAt[k] = t; }
      // Long after a success the next one is not a fix any more.
      else if (r.status === 'success' && r.finished && t - r.finished > 300) { delete failedSeen[k]; delete failedSeenAt[k]; }
    });
    Object.keys(failedSeenAt).forEach(function (k) {
      if (t - failedSeenAt[k] > 86400) { delete failedSeen[k]; delete failedSeenAt[k]; }
    });
  }

  // A source that is down (not answering, or its login refused) is a pill next
  // to the alert pill, with a Mute button. Mute hides the pill on every screen
  // until the source works again; the sources chip keeps counting the problem.
  // A quiet source is not a down source: only a failed check counts.
  var SRC_WHY = { unreachable: 'not answering', auth_failed: 'sign-in refused' };
  function renderSourcePill() {
    var el = $('src-pill');
    var bad = (snap.sources || []).filter(function (x) { return x.health !== 'ok' && !x.muted; });
    el.hidden = !bad.length;
    if (!bad.length) { el.innerHTML = ''; return; }
    var b = bad[0];
    el.className = 'banner pill ' + (b.health === 'auth_failed' ? 'critical' : 'warn');
    var html = '<span class="tag">Source down</span><span>' + esc(b.id) + '</span><span class="msg">' + esc(SRC_WHY[b.health] || b.health) + '</span>' +
      (bad.length > 1 ? '<span class="more">+' + (bad.length - 1) + ' more</span>' : '') +
      '<button type="button" class="mute" data-mute="' + esc(b.id) + '" title="Hide this until the source works again">Mute</button>';
    if (el.innerHTML !== html) el.innerHTML = html;
  }
  $('src-pill').addEventListener('click', function (e) {
    var b = e.target.closest ? e.target.closest('[data-mute]') : null;
    if (!b) return;
    var id = b.getAttribute('data-mute');
    (snap.sources || []).forEach(function (x) { if (x.id === id) x.muted = true; });
    renderSourcePill();
    fetch('api/kiosk/source/mute', {
      method: 'POST', cache: 'no-store', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: id })
    }).catch(function () { /* the next snapshot shows what is left */ });
  });

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
    $('setup-link').hidden = !layout.setup;
    renderUpdate(snap.update);
    renderSourcePill();
    renderSources();
    // With the setup page on, the sources chip opens it: that is where every
    // source and its status are listed.
    if (layout.setup) { $('conn').setAttribute('href', 'setup'); $('conn').setAttribute('title', 'Sources and their status: Setup'); }
    else { $('conn').removeAttribute('href'); $('conn').removeAttribute('title'); }
    drawGrid();
    renderBadges();
    renderBanner();
    renderTicker();
    noteFailures();
    tick();
  }

  // The cards. Also redrawn every few seconds without a new snapshot, so a
  // "Done" job and a tinted row go away on time.
  function drawGrid() {
    var layout = snap.layout || { columns: 4, rows: 3, panels: [] };
    var grid = $('grid');
    grid.style.setProperty('--cols', layout.columns);
    grid.style.setProperty('--rows', layout.rows);
    var keep = {};
    (layout.panels || []).forEach(function (panel, i) {
      // The span is in the key: a card is placed once, so a new size needs a new card.
      var key = i + ':' + panel.widget + ':' + (panel.host || '') + ':' + (panel.span || [1, 1]).join('x');
      keep[key] = true;
      renderCard(key, panel, grid);
    });
    Object.keys(cards).forEach(function (key) {
      if (!keep[key]) { grid.removeChild(cards[key].el); delete cards[key]; }
    });
  }

  // The sources bar: every source with its mark and state. Running jobs count
  // per CI system; a source with a problem says what. With the setup page on,
  // each opens it.
  var SRC_STATE = { auth_failed: 'sign-in refused', unreachable: 'not answering' };
  function renderSources() {
    var setup = snap.layout && snap.layout.setup;
    var html = (snap.sources || []).map(function (s) {
      var kind = kindOf(s.id), name = String(s.id).split(':').slice(1).join(':') || kind;
      var running = (snap.jobs || []).filter(function (j) { return j.source === kind; }).length;
      var cls = s.health !== 'ok' ? (s.health === 'auth_failed' ? 'bad' : 'warn') : running ? 'busy' : '';
      var st = s.health !== 'ok' ? (SRC_STATE[s.health] || s.health) : running ? running + ' running' : 'ok';
      var tag = setup ? 'a' : 'span';
      return '<' + tag + ' class="rsrc ' + cls + '"' + (setup ? ' href="setup#sources"' : '') + ' title="' + esc(s.id) + '">' +
        src(kind) + '<span>' + esc(name) + '</span><span class="st">' + esc(st) + '</span></' + tag + '>';
    }).join('');
    var el = $('sources');
    if (el.innerHTML !== html) el.innerHTML = html;
  }

  // ---- light or dark ------------------------------------------------------

  // Light or dark is chosen with the button on the setup page and kept in this
  // browser; the dashboard follows it (the same key, the same origin).
  // `?scheme=light|dark` overrides it. The night theme still dims the page at
  // night, whatever this says.
  var SCHEME_KEY = 'deskwatch-theme', OLD_SCHEME_KEY = 'deskwatch-setup-theme';
  function scheme() {
    var p = params.get('scheme');
    if (p === 'light' || p === 'dark') return p;
    try { return (localStorage.getItem(SCHEME_KEY) || localStorage.getItem(OLD_SCHEME_KEY)) === 'light' ? 'light' : 'dark'; }
    catch (e) { return 'dark'; }
  }
  function applyScheme() {
    var light = scheme() === 'light', root = document.documentElement;
    if (light) root.setAttribute('data-scheme', 'light'); else root.removeAttribute('data-scheme');
  }
  // The setup page open in another tab changes it too.
  window.addEventListener('storage', function (e) { if (e.key === SCHEME_KEY || e.key === OLD_SCHEME_KEY) applyScheme(); });
  applyScheme();

  // ---- day and night, pixel shift ----------------------------------------

  // The theme is chosen here, by the clock the page shows, not by the bridge:
  // a Pi in another time zone than the server still dims at its own bedtime.
  // `?theme=day|night|auto` on the address overrides the config (for testing).
  function minutesOf(text, fallback) {
    var m = /^(\d\d):(\d\d)$/.exec(text || '');
    return m ? Number(m[1]) * 60 + Number(m[2]) : fallback;
  }
  function themeAt(date) {
    var cfg = (snap && snap.layout && snap.layout.theme) || {};
    var mode = params.get('theme') || cfg.mode || 'day';
    if (mode !== 'auto') return mode === 'night' ? 'night' : 'day';
    var from = minutesOf(cfg.night_from, 22 * 60), to = minutesOf(cfg.night_to, 7 * 60);
    var now = date.getHours() * 60 + date.getMinutes();
    if (from === to) return 'day';
    return (from < to ? now >= from && now < to : now >= from || now < to) ? 'night' : 'day';
  }
  function applyTheme(date) {
    var theme = themeAt(date), root = document.documentElement;
    if (root.getAttribute('data-theme') !== theme) root.setAttribute('data-theme', theme);
  }

  // Pixel shift: the whole page moves one step around a small ring every
  // `pixel_shift_s` seconds. The step follows the wall clock, so a reload
  // does not restart the walk at the same spot.
  var RING = [[0, 0], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1], [0, -1], [1, -1]];
  var shiftStep = null;
  function shiftAt(ms) {
    var every = (snap && snap.layout && snap.layout.pixel_shift_s) || 0;
    return every ? RING[Math.floor(ms / 1000 / every) % RING.length] : [0, 0];
  }
  function applyShift(ms) {
    var step = shiftAt(ms), key = step.join(',');
    if (key === shiftStep) return;
    shiftStep = key;
    // 0.25rem is 4 px at 1080p and 8 px at 4K.
    document.body.style.transform = key === '0,0' ? '' : 'translate(' + step[0] * 0.25 + 'rem,' + step[1] * 0.25 + 'rem)';
  }

  // ---- why there is no data ----------------------------------------------

  // Turn a failed request into something a person can act on.
  function describe(e) {
    if (e && e.status) {
      return {
        code: 'http', pill: 'bridge error ' + e.status,
        title: 'The bridge answered with an error (HTTP ' + e.status + ')',
        detail: 'A proxy in front of the bridge may be failing, or the bridge has a problem. Its log has details.'
      };
    }
    return {
      code: 'unreachable', pill: 'bridge unreachable',
      title: 'The page cannot reach the bridge',
      detail: 'Check that the bridge is running and that the address and port are right. A firewall or proxy can also block it.'
    };
  }

  // The box in the page body. Rebuilt only when the message changes, so
  // the notice does not flicker on every tick.
  function renderNotice() {
    var el = $('notice'), p = null;
    if (problem && !snap) p = problem;
    else if (snap && !snap.now) {
      p = {
        code: 'starting', title: 'Waiting for the first data',
        detail: 'The bridge is running but has not collected anything yet. This page fills in by itself.'
      };
    } else if (!snap && lastTry && Date.now() - lastTry > 8000) {
      p = { code: 'slow', title: 'Still connecting', detail: 'The bridge has not answered yet. This page keeps trying.' };
    }
    var key = p ? p.code : '';
    if (key === noticeKey) return;
    noticeKey = key;
    el.hidden = !p;
    if (!p) { el.innerHTML = ''; return; }
    el.innerHTML = '<h2>' + esc(p.title) + '</h2><p>' + esc(p.detail).replace(/`([^`]+)`/g, '<code>$1</code>') + '</p>';
  }

  // ---- once-a-second work: clock, elapsed times, connection state --------

  function tick() {
    var d = new Date();
    var two = function (n) { return n < 10 ? '0' + n : '' + n; };
    applyTheme(d);
    applyShift(d.getTime());
    returnToTop();
    // Done jobs and tinted rows run out on the clock, not on a new snapshot.
    if (snap && d.getSeconds() % 5 === 0) drawGrid();
    $('time').textContent = two(d.getHours()) + ':' + two(d.getMinutes());
    $('date').textContent = d.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' });
    Array.prototype.forEach.call(document.querySelectorAll('[data-since]'), function (el) {
      el.textContent = clock(nowS() - Number(el.getAttribute('data-since')));
    });
    var age = lastMsg ? (Date.now() - lastMsg) / 1000 : Infinity;
    var lost = !snap || age > STALE_AFTER_S;
    document.body.classList.toggle('lost', !!snap && lost);
    var off = $('offline');
    off.hidden = !(snap && lost);
    if (snap && lost) off.textContent = 'No data from the bridge for ' + clock(age) + '. Showing the last known state, reconnecting' + (problem ? ' (' + problem.pill + ').' : '.');
    var conn = $('conn');
    var ok = snap && !lost;
    var problems = ok && (snap.sources || []).filter(function (s) { return s.health !== 'ok'; }).length;
    var text, cls;
    if (problem && !ok) { text = problem.pill; cls = 'bad'; }
    else if (!snap) { text = 'connecting'; cls = ''; }
    else if (lost) { text = 'offline'; cls = 'bad'; }
    else if (wsFailed) { text = 'polling, no live socket'; cls = 'review'; }
    else if (problems) { text = problems + (problems === 1 ? ' source has' : ' sources have') + ' a problem'; cls = 'bad'; }
    else { text = 'all sources ok'; cls = 'ok'; }
    conn.className = 'chip end ' + cls;
    conn.textContent = text;
    renderNotice();
  }

  // ---- network ------------------------------------------------------------

  var ws = null, retry = 1000, pollTimer = null, retryTimer = null, lastTry = 0;

  function poll() {
    lastTry = lastTry || Date.now();
    fetch('api/kiosk', { cache: 'no-store' })
      .then(function (r) {
        if (!r.ok) throw { status: r.status };
        return r.json();
      })
      .then(function (data) { problem = null; render(data); })
      .catch(function (e) {
        problem = describe(e);
        tick();
      });
  }

  function connect() {
    retryTimer = null;
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + location.pathname.replace(/[^/]*$/, '') + 'api/kiosk/ws';
    try { ws = new WebSocket(url); } catch (e) { ws = null; }
    if (!ws) { wsFailed = true; later(); return; }
    ws.onopen = function () { retry = 1000; wsFailed = false; if (pollTimer) { clearInterval(pollTimer); pollTimer = null; } };
    ws.onmessage = function (ev) { try { var data = JSON.parse(ev.data); problem = null; render(data); } catch (e) {} };
    ws.onclose = ws.onerror = function () {
      if (!ws) return;
      ws = null;
      wsFailed = true;
      // While the socket is down, keep fetching snapshots so a proxy that
      // blocks WebSockets still works. The fetch also tells why (error).
      if (!pollTimer) { poll(); pollTimer = setInterval(poll, POLL_MS); }
      later();
    };
  }
  function later() { retryTimer = setTimeout(connect, retry); retry = Math.min(retry * 2, 10000); }
  // Hide the mouse pointer after 3 s without movement; any movement brings it back.
  var idleTimer = null;
  function wake() {
    document.body.classList.remove('idle');
    clearTimeout(idleTimer);
    idleTimer = setTimeout(function () { document.body.classList.add('idle'); }, 3000);
  }
  document.addEventListener('mousemove', wake);
  document.addEventListener('mousedown', wake);
  wake();

  window.DeskWatchKiosk = { render: render, tick: tick, themeAt: themeAt, shiftAt: shiftAt, seed: function (name, values) { history[name] = values.slice(-HISTORY); } };
  setInterval(tick, 1000);
  tick();
  if (!params.has('static')) {
    poll();
    connect();
  }
})();
