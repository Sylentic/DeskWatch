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
  var gpuLines = {};           // host name -> one list of readings per GPU, for a host with several GPUs
  var gpuHistory = {};         // host name -> recent GPU readings, same timing as `history`
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
  // Where an alert comes from, for its mark: a git source (new issue or PR
  // comment), Home Assistant, or anything else.
  function alertKind(a) {
    if (MARKS[a.source] && a.source !== 'local') return a.source;
    return /home.?assistant|^ha\b/i.test(a.source || '') ? 'home_assistant' : 'local';
  }
  // A row or card that opens its page on the source. The bridge only sends
  // http(s) addresses, and this checks again, so nothing else can become a
  // link. It opens in a new tab: the dashboard itself stays where it is.
  function href(url) { return typeof url === 'string' && /^https?:\/\//i.test(url) ? url : null; }
  // A source can name a Firefox container (Setup > Sources). Then, in Firefox,
  // its links go through the "Open external links in a container" add-on:
  // ext+container:name=<container>&url=<address>. Like every link it opens in a
  // new tab, so the dashboard stays. Any other browser, or a source without a
  // container, keeps the plain link.
  var FIREFOX = /\bFirefox\/\d/.test(navigator.userAgent || '');
  function linkAttrs(h, source) {
    var name = FIREFOX && snap && snap.containers && source && snap.containers[source];
    // Both need target=_blank: without it the add-on's handler took over the dashboard tab.
    if (!name) return 'href="' + esc(h) + '" target="_blank" rel="noopener noreferrer"';
    return 'href="' + esc('ext+container:name=' + encodeURIComponent(name) + '&url=' + encodeURIComponent(h)) + '" target="_blank" rel="noopener noreferrer"';
  }
  function linkOpen(url, cls, source) {
    var h = href(url);
    return h ? '<a class="' + cls + ' link" ' + linkAttrs(h, source) + '>' : '<div class="' + cls + '">';
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
    // Parts switched off for this host on the setup page are left out, not shown as dashes.
    var hide = s.hide || [];
    var show = function (part) { return hide.indexOf(part) < 0; };
    var vals = history[host.name] || [];
    var peak = vals.length ? Math.round(Math.max.apply(null, vals)) + '%' : '--';
    var fresh = s.uptime_s != null && s.uptime_s < REBOOT_S;
    var many = s.gpus && s.gpus.length > 1;
    var gauges = (show('cpu') ? gauge('CPU', s.cpu_pct, 70, 90) : '') + (show('memory') ? gauge('MEM', s.ram_pct, 80, 90) : '') +
      (show('disk') ? gauge('DISK', s.disk_pct, 80, 90) : '') +
      // GPU rows only for hosts that report a GPU. With several, each gets its own load bar and a line below it.
      (many ? gpuRows(s.gpus, s.gpu_plain) :
        (s.gpu_pct == null ? '' : gauge('GPU', s.gpu_pct, 80, 95)) +
        (s.gpu_mem_pct == null ? '' : gauge('VRAM', s.gpu_mem_pct, 80, 95)));
    var cells = (show('uptime') ? '<div><span class="l">Uptime</span><span' + (fresh ? ' class="warn-text"' : '') + '>' + uptime(s.uptime_s) + '</span></div>' : '') +
      (show('temperature') ? '<div><span class="l">Temp</span><span>' + (s.cpu_temp_c == null ? '--' : Math.round(s.cpu_temp_c) + ' &deg;C') + '</span></div>' : '') +
      (many || s.gpu_temp_c == null ? '' : '<div><span class="l">GPU temp</span><span>' + Math.round(s.gpu_temp_c) + ' &deg;C</span></div>') +
      (show('network') ? '<div><span class="l">Net in</span><span>' + (s.net ? rate(s.net.rx_bps) : '--') + '</span></div>' +
        '<div><span class="l">Net out</span><span>' + (s.net ? rate(s.net.tx_bps) : '--') + '</span></div>' : '') +
      // Only for hosts that report them (see docs/hosts.md).
      (s.updates == null ? '' : '<div><span class="l">Pending updates</span><span' + (s.updates_security > 0 ? ' class="warn-text"' : '') + '>' + s.updates +
        (s.updates_security == null ? '' : ' (' + s.updates_security + ' security)') + '</span></div>') +
      (s.reboot_needed ? '<div><span class="l">Restart</span><span class="warn-text">needed</span></div>' : '');
    return (gauges ? '<div class="gauges">' + gauges + '</div>' : '') +
      (cells ? '<div class="stat-grid">' + cells + '</div>' : '') +
      (show('cpu') ? '<div class="sparkhead"><span>CPU' + (many ? s.gpus.map(function (g, i) { return ' <span class="gpu-key g' + (i % 4) + '">&amp; ' + esc(g.label) + '</span>'; }).join('') : gpuHistory[host.name] && gpuHistory[host.name].some(function (g) { return g != null; }) ? ' <span class="gpu-key">&amp; GPU</span>' : '') + ' &middot; ' + spanLabel(vals.length) + '</span><span>PEAK ' + peak + '</span></div>' +
        sparkline(vals, many ? null : gpuHistory[host.name], many ? gpuLines[host.name] : null) : '');
  }

  // How much time the graph covers: one reading per 5 s, at most HISTORY.
  function spanLabel(n) {
    var s = Math.max(0, n - 1) * 5;
    return s < 60 ? s + ' S' : Math.round(s / 60) + ' MIN';
  }

  // Does a host need a look: down, a value over its warning line, or rebooted.
  // Pending security updates: a small amber shield takes the place of the dot in the host list.
  var SHIELD = '<svg class="shield" viewBox="0 0 16 16" role="img" aria-label="Security updates pending"><title>Security updates pending</title>' +
    '<path d="M8 1 2.5 3v4.2c0 3.4 2.3 6 5.5 7.8 3.2-1.8 5.5-4.4 5.5-7.8V3z" fill="currentColor"/></svg>';
  function hostLook(h) {
    var s = h.stats || {};
    if (!h.up) return 'off';
    if (s.cpu_pct >= 70 || s.ram_pct >= 80 || s.disk_pct >= 80 || s.gpu_pct >= 95) return 'look';
    if (s.uptime_s != null && s.uptime_s < REBOOT_S) return 'look';
    if (s.reboot_needed) return 'look';
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
  // host above the list when the card is narrow), in the order chosen on the
  // Hosts page of Setup. Tapping a host chooses it, kept in this browser. The chosen host
  // never changes by itself: a host going down gets a red pip in the list.
  function hostsDetailed(hosts) {
    var host = findHost(chosenHost()) || hosts[0];
    var order = hosts.map(function (h) { return { h: h }; });
    var list = '<div class="hostlist" role="listbox" aria-label="Hosts">';
    order.forEach(function (o) {
      var h = o.h, look = hostLook(h);
      list += '<button type="button" role="option" class="hostrow' + (h === host ? ' sel' : '') + (look ? ' ' + look : '') +
        '" data-host-pick="' + esc(h.name) + '" aria-selected="' + (h === host ? 'true' : 'false') + '">' +
        (h.up && h.stats && h.stats.updates_security > 0 ? SHIELD : '<span class="pip"></span>') + '<span class="n">' + esc(h.name) + '</span>' + (h.up ? hostKey(h) : '<span class="k crit-text">down</span>') + '</button>';
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


  // ---- Alerting Kiosk: a wall of small host cards -------------------------
  // How bad is a host: 0 fine, 1 yellow (a warning line crossed, restarted,
  // restart needed), 2 red (a critical line crossed), 3 down. `why` names the
  // first thing that put it there, for the tag on the card.
  function hostLevel(h) {
    var s = h.stats || {};
    if (!h.up) return { lvl: 3, why: 'down' };
    var out = { lvl: 0, why: '', kind: '' };
    function check(label, v, warn, crit) {
      if (v == null) return;
      var lvl = v >= crit ? 2 : v >= warn ? 1 : 0;
      if (lvl > out.lvl) out = { lvl: lvl, why: label + ' ' + Math.round(v), kind: '' };
    }
    check('CPU', s.cpu_pct, 70, 90);
    check('RAM', s.ram_pct, 80, 90);
    check('DISK', s.disk_pct, 80, 90);
    check('GPU', s.gpu_pct, 80, 95);
    // Security updates are a warning like a full disk. A restart that is needed has its own colour (purple).
    if (!out.lvl && s.updates_security > 0) out = { lvl: 1, why: s.updates_security + ' security', kind: 'sec' };
    if (!out.lvl && s.reboot_needed) out = { lvl: 1, why: 'restart needed', kind: 'restart' };
    if (!out.lvl && s.uptime_s != null && s.uptime_s < REBOOT_S) out = { lvl: 1, why: 'restarted', kind: '' };
    return out;
  }

  // A host that gets worse blinks once: yellow for 10 s, red (or down) for
  // 30 s, then it stays lit and steady. The first snapshot after the page
  // opens never blinks: what was already wrong is not new.
  var BLINK_MS = { 1: 10000, 2: 30000, 3: 30000 };
  var hostSeen = null;   // host name -> level, null until the first snapshot
  var hostBlink = {};    // host name -> when the blinking stops (ms)
  function trackHostLevels() {
    var first = hostSeen === null;
    if (first) hostSeen = {};
    var now = Date.now();
    (snap.hosts || []).forEach(function (h) {
      var lvl = hostLevel(h).lvl, was = hostSeen[h.name] || 0;
      if (!first && lvl > was) {
        hostBlink[h.name] = now + BLINK_MS[lvl];
        // The blink class goes off by itself: redraw when it ends.
        setTimeout(function () { if (snap) drawGrid(); }, BLINK_MS[lvl] + 100);
      }
      hostSeen[h.name] = lvl;
    });
  }

  function hostsAlerting(hosts) {
    var now = Date.now();
    var rows = hosts.map(function (h, i) { return { h: h, i: i, v: hostLevel(h) }; });
    // Hosts that need a look come first, the worst first; the rest keep the order from Setup.
    rows.sort(function (a, b) { return (b.v.lvl - a.v.lvl) || (a.i - b.i); });
    var html = '<div class="wall">';
    rows.forEach(function (r) {
      var h = r.h, s = h.stats || {}, hide = s.hide || [];
      var show = function (part) { return hide.indexOf(part) < 0; };
      var cls = r.v.kind === 'restart' ? 'restart' : ['ok', 'warn', 'crit', 'down'][r.v.lvl];
      var blink = (hostBlink[h.name] || 0) > now && r.v.lvl > 0 ? ' blink' : '';
      var cell = function (label, v, warn, crit) {
        if (v == null) return '';
        var c = v >= crit ? 'var(--red)' : v >= warn ? 'var(--amber)' : 'var(--green)';
        return '<div class="ac"><span class="l">' + label + '</span><span class="v' + (v >= crit ? ' crit-text' : v >= warn ? ' warn-text' : '') + '">' + Math.round(v) + '<small>%</small></span>' +
          '<div class="bar"><i style="--p:' + Math.max(0, Math.min(100, v)) + '%;--c:' + c + '"></i></div></div>';
      };
      var cells = h.up ? (show('cpu') ? cell('CPU', s.cpu_pct, 70, 90) : '') + (show('memory') ? cell('RAM', s.ram_pct, 80, 90) : '') +
        (show('disk') ? cell('DISK', s.disk_pct, 80, 90) : '') + cell('GPU', s.gpu_pct, 80, 95) : '';
      var foot = !h.up ? '' : (s.updates ? s.updates + ' updates' + (s.updates_security > 0 ? ' (' + s.updates_security + ' security)' : '') : '') + (s.reboot_needed ? (s.updates ? ' &middot; ' : '') + 'restart needed' : '');
      html += '<div class="ah ' + cls + blink + '"><div class="an"><span class="dot"></span><span class="nm">' + esc(h.name) + '</span>' +
        (r.v.why ? '<span class="why">' + esc(r.v.why) + '</span>' : '') + '</div>' +
        (h.up ? '<div class="acs">' + cells + '</div>' + (foot ? '<div class="af">' + foot + '</div>' : '') : '<div class="gone">not answering</div>') + '</div>';
    });
    html += '</div>';
    var down = rows.filter(function (r) { return r.v.lvl === 3; }).length;
    var look = rows.filter(function (r) { return r.v.lvl > 0 && r.v.lvl < 3; }).length;
    return { title: 'Hosts', count: (hosts.length - down) + '/' + hosts.length + ' up',
      tag: down ? down + ' down' : look ? look + (look === 1 ? ' needs' : ' need') + ' a look' : '', body: html };
  }

  function ciStale() {
    // Only the CI sources: a Prometheus outage does not make the PR list old.
    return (snap.sources || []).some(function (s) { return s.health !== 'ok' && /^(gitea|github|gitlab|bitbucket|azure_devops):/.test(s.id); });
  }

  function gauge(label, value, warn, crit, labelClass) {
    var w = value == null ? 0 : Math.max(0, Math.min(100, value));
    return '<div class="gauge"><span class="l' + (labelClass ? ' ' + labelClass : '') + '">' + esc(label) + '</span>' +
      '<div class="bar"><i style="--p:' + w + '%;--c:' + level(value, warn, crit) + '"></i></div>' +
      '<span class="v">' + pct(value) + '</span></div>';
  }

  // One GPU of a host with several: load bar and VRAM bar, then model and temperature in small print.
  // `plain` (set on the setup page) leaves only the load bar.
  function gpuRows(gpus, plain) {
    return gpus.map(function (g, i) {
      if (plain) return gauge(g.label, g.pct, 80, 95, 'g' + (i % 4));
      var meta = [g.name, g.temp_c == null ? null : Math.round(g.temp_c) + ' &deg;C']
        .filter(function (m) { return m; }).join(' &middot; ');
      return '<div class="gpu-group">' + gauge(g.label, g.pct, 80, 95, 'g' + (i % 4)) +
        (g.mem_pct == null ? '' : gauge('VRAM', g.mem_pct, 80, 95)) +
        (meta ? '<div class="gpu-meta">' + meta + '</div>' : '') + '</div>';
    }).join('');
  }

  function sparkline(values, gpuValues, lines) {
    if (!values || values.length < 2) return '<svg class="spark"></svg>';
    // The readings so far fill the whole width, so the graph is not a small
    // line at the right edge for the first minutes after a reload.
    var w = 100, h = 30;
    var step = w / (values.length - 1), off = 0;
    var y = function (v) { return (h - Math.max(0, Math.min(100, v)) / 100 * h).toFixed(1); };
    var pts = values.map(function (v, i) {
      return (off + i * step).toFixed(1) + ',' + y(v);
    });
    // A red GPU line on the same scale, when the host reports a GPU.
    var gpuLine = '';
    if (gpuValues && gpuValues.length === values.length && gpuValues.some(function (g) { return g != null; })) {
      var gpts = [];
      gpuValues.forEach(function (g, i) { if (g != null) gpts.push((off + i * step).toFixed(1) + ',' + y(g)); });
      if (gpts.length > 1) gpuLine = '<polyline class="gpu" points="' + gpts.join(' ') + '"/>';
    }
    // A host with several GPUs: a line per GPU, each in its own colour.
    if (lines) lines.forEach(function (l, n) {
      var lp = [];
      l.forEach(function (g, i) { if (g != null) lp.push((off + i * step).toFixed(1) + ',' + y(g)); });
      if (l.length === values.length && lp.length > 1) gpuLine += '<polyline class="gpu g' + (n % 4) + '" points="' + lp.join(' ') + '"/>';
    });
    return '<svg class="spark" viewBox="0 0 ' + w + ' ' + h + '" preserveAspectRatio="none">' +
      '<path d="M' + pts.join(' L') + ' L' + (off + (values.length - 1) * step).toFixed(1) + ',' + h + ' L' + off.toFixed(1) + ',' + h + ' Z"/>' +
      '<polyline points="' + pts.join(' ') + '"/>' + gpuLine + '</svg>';
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
  // Runs Running now shows at once when the panel sets no `rows`.
  var RUNS_MAX = 3;

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
      if (p && p.view === 'alerting' && (snap.hosts || []).length) return hostsAlerting(snap.hosts);
      if (p && p.view === 'detailed' && (snap.hosts || []).length) return hostsDetailed(snap.hosts);
      // The compact host view is a taller table: it adds the network rates, as
      // columns when it is two cells wide, else as a small line under each host.
      var span = (p && p.span) || [1, 1];
      var wide = span[0] >= 2, tall = !wide && span[1] >= 2;
      var hosts = snap.hosts || [];
      var html;
      if (!hosts.length) html = '<div class="empty">No hosts</div>';
      else {
        // In the order chosen on the Hosts page of Setup.
        var order = hosts;
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
      // One heading per run. Jobs of the same run (parallel jobs) share a
      // `group` and are listed under it from `run_jobs`.
      var lanes = snap.run_jobs || {};
      var groups = [], byKey = {};
      done.forEach(function (r) { groups.push({ done: r }); });
      jobs.forEach(function (j) {
        var g = j.group && lanes[j.group] ? j.group : j.key;
        if (byKey[g]) { byKey[g].jobs.push(j); return; }
        groups.push(byKey[g] = { jobs: [j], lanes: j.group ? lanes[j.group] : null });
      });
      // A newer run of the same pipeline and branch replaces the older one: a
      // new push does not stack a second set of jobs under the first.
      var newest = {}, replaced = [];
      groups.forEach(function (g) {
        if (g.done) return;
        var j = g.jobs[0];
        if (!j.ref) return;
        var k = j.source + '\n' + j.project + '\n' + j.pipeline + '\n' + j.ref, t = groupStart(g);
        if (!newest[k] || t > groupStart(newest[k])) newest[k] = g;
      });
      groups = groups.filter(function (g) {
        if (g.done) return true;
        var j = g.jobs[0];
        if (!j.ref || newest[j.source + '\n' + j.project + '\n' + j.pipeline + '\n' + j.ref] === g) return true;
        replaced.push(g);
        return false;
      });
      var max = p.rows || RUNS_MAX;
      var shown = groups.slice(0, max);
      var html = shown.map(function (g) { return g.done ? doneGroup(g.done) : runGroup(g); }).join('');
      if (groups.length > max) html += '<div class="more">+' + (groups.length - max) + ' more running</div>';
      if (replaced.length) html += replacedNote(replaced);
      var running = groups.length - done.length;
      return { title: 'Running now', count: running + (done.length ? ' · ' + done.length + ' done' : ''), stale: ciStale(), body: html };
    },

    pipelines: function (p) {
      var runs = (snap.runs || []).filter(function (r) { return matches(p, r.source, r.project); });
      var rows = runs.map(function (r) {
        var when = r.status === 'running' ? 'running' : ago(nowS() - (r.finished || r.started));
        var status = runStatus(r.status);
        // Every run sits in a box tinted by its status, like the alert boxes.
        return linkOpen(r.url, 'row rbox ' + status, r.source) + dot(status) + src(r.source) +
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
        return linkOpen(pr.url, 'row pr', it.repo.source) + dot('open') + src(it.repo.source) +
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
      var alerts = alertList();
      var rows = alerts.map(function (a) {
        var sub = [a.message ? esc(a.message) : '', ago(nowS() - a.since)].filter(Boolean).join(' &middot; ');
        // An alert from a git source links to its issue or comment.
        var txt = '<span class="main">' + esc(a.title) + '</span><span class="sub">' + sub + '</span>';
        var h = href(a.url);
        return '<div class="abox ' + esc(a.severity) + '">' + src(alertKind(a)) +
          (h ? '<a class="txt link" ' + linkAttrs(h, a.source) + '>' + txt + '</a>' : '<span class="txt">' + txt + '</span>') +
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
        return linkOpen(r.url, 'row' + (r.disabled ? ' off' : ''), r.source) + dot(dotState) + src(r.source) + '<span class="main">' + esc(r.name) + '</span>' +
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

  // ---- Running now: a heading per run, a row per job under it ----------

  function groupHead(source, project, kind, ref, sum, url) {
    return linkOpen(url, 'gh', source) + src(source) + '<span class="name">' + esc(project) + '</span>' +
      (kind ? '<span class="kind ' + esc(kind) + '">' + esc(kind) + '</span>' : '') +
      '<span class="ref">' + ref + '</span><span class="sum">' + sum + '</span>' + linkClose(url);
  }

  function jobRow(url, status, name, text, bar, time, source) {
    return linkOpen(url, 'jrow ' + status, source) + dot(status) + '<span class="n">' + esc(name) + '</span>' +
      '<span class="mid"><span class="s">' + text + '</span>' + bar + '</span><span class="t">' + time + '</span>' + linkClose(url);
  }

  // What a job of a run is doing, in a few words.
  function laneText(l) {
    var st = runStatus(l.status);
    if (st === 'running') return esc(l.step || 'Running');
    if (st === 'ok') return 'Passed';
    if (st === 'failed') return l.step ? 'Failed at ' + esc(l.step) : 'Failed';
    if (st === 'review') return 'Waiting for approval';
    if (st === 'queued') return 'Queued';
    return 'Skipped';
  }

  function laneTime(l) {
    if (l.status === 'running' && l.started) return '<span data-since="' + l.started + '">' + clock(nowS() - l.started) + '</span>';
    if (l.started && l.finished) return clock(l.finished - l.started);
    return '';
  }

  // A passed or skipped job of a run that is still going leaves the list DONE_S
  // after it ended, so the running and failed ones stand out. Skipped jobs often
  // have no end time, so the first time this page saw the job ended counts then.
  var laneSeen = {};
  function laneGone(group, l) {
    var st = runStatus(l.status);
    if (st !== 'ok' && st !== 'neutral') return false;
    var key = group + '\n' + l.name, now = nowS();
    if (laneSeen[key] === undefined) laneSeen[key] = l.finished || now;
    return now - laneSeen[key] >= DONE_S;
  }

  function runGroup(g) {
    var j = g.jobs[0];
    var started = Math.min.apply(null, g.jobs.map(function (x) { return x.started; }));
    var ref = esc(j.pipeline) + (j.ref ? ' &middot; ' + esc(j.ref) : '') + (j.commit ? ' @ ' + esc(j.commit) : '');
    var since = '<span data-since="' + started + '">' + clock(nowS() - started) + '</span>';
    var rows, sum;
    if (g.lanes && g.lanes.length) {
      var finished = g.lanes.filter(function (l) { return l.status === 'success' || l.status === 'failed' || l.status === 'neutral'; }).length;
      sum = (g.lanes.length > 1 ? finished + ' of ' + g.lanes.length + ' done &middot; ' : '') + since;
      rows = g.lanes.filter(function (l) { return !laneGone(j.group, l); }).map(function (l) {
        var st = runStatus(l.status);
        var bar = st !== 'running' ? '' : l.progress == null ? '<span class="pbar spin"><i style="animation-delay:-' + (Date.now() % 3600) + 'ms"></i></span>' :
          '<span class="pbar"><i style="--p:' + Math.round(l.progress * 100) + '%"></i></span>';
        return jobRow(l.url || j.url, st, l.name, laneText(l), bar, laneTime(l), j.source);
      });
    } else {
      // A source that only knows the run as one job: one row with its steps.
      sum = since;
      rows = g.jobs.map(function (x) {
        var text = esc(x.step || 'Starting') + (x.step_no && x.step_count ? ' &middot; step ' + x.step_no + ' of ' + x.step_count : '');
        var bar = x.progress == null ? '<span class="pbar spin"><i style="animation-delay:-' + (Date.now() % 3600) + 'ms"></i></span>' :
          '<span class="pbar"><i style="--p:' + Math.round(x.progress * 100) + '%"></i></span>';
        return jobRow(x.url, 'running', x.kind, text, bar, '<span data-since="' + x.started + '">' + clock(nowS() - x.started) + '</span>', x.source);
      });
    }
    return '<div class="jgroup">' + groupHead(j.source, j.project, j.kind, ref, sum, j.url) + '<div class="jrows">' + rows.join('') + '</div></div>';
  }

  function groupStart(g) { return Math.min.apply(null, g.jobs.map(function (x) { return x.started; })); }

  // One dashed line for the runs a newer run of the same branch replaced, with
  // how many of their jobs had failed or were skipped.
  function replacedNote(gs) {
    var bad = 0, skipped = 0;
    gs.forEach(function (g) {
      (g.lanes || []).forEach(function (l) {
        var st = runStatus(l.status);
        if (st === 'failed') bad++; else if (st === 'neutral') skipped++;
      });
    });
    var parts = [];
    if (bad) parts.push(bad + ' failed');
    if (skipped) parts.push(skipped + ' skipped');
    return '<div class="replaced">&#8635; ' + (gs.length === 1 ? '1 older run replaced' : gs.length + ' older runs replaced') +
      ' by a newer one on the same branch' + (parts.length ? ' (' + parts.join(', ') + ')' : '') + '</div>';
  }

  function doneGroup(r) {
    var ref = esc(r.pipeline) + (r.ref ? ' &middot; ' + esc(r.ref) : '');
    var took = clock((r.finished || r.started) - r.started);
    return '<div class="jgroup">' + groupHead(r.source, r.project, '', ref, 'Done', r.url) +
      '<div class="jrows">' + jobRow(r.url, 'ok', 'Done', 'Passed', '', took, r.source) + '</div></div>';
  }

  // Rows that do not fit are cut off; `rows` caps the list from the config.
  function list(rows, cap, empty) {
    if (!rows.length) return empty;
    var shown = cap ? rows.slice(0, cap) : rows;
    return '<div class="rows">' + shown.join('') + '</div>' +
      (cap && rows.length > cap ? '<div class="more">+' + (rows.length - cap) + ' more</div>' : '');
  }

  // ---- drawing ------------------------------------------------------------

  function renderCard(key, panel, grid, pos) {
    var make = widgets[panel.widget];
    var out = make ? make(panel) : { title: panel.widget, body: '<div class="empty">Unknown widget</div>' };
    var html = '<header><h2>' + esc(panel.title || out.title) + '</h2>' + (out.nav || '') +
      (out.tag ? '<span class="tag">' + esc(out.tag) + '</span>' : '') +
      (out.count !== undefined && out.count !== '' ? '<span class="count">' + esc(out.count) + '</span>' : '') +
      '</header><div class="body">' + out.body + '</div>' + (editing ? editControls(panel, pos) : '');
    var entry = cards[key];
    if (!entry) {
      var el = document.createElement('section');
      el.setAttribute('data-key', key);
      el.setAttribute('data-id', panel.id);
      var span = panel.span || [1, 1];
      // A panel with a saved place sits on its own cell; the others flow around it.
      el.style.gridColumn = panel.pos ? panel.pos[0] + ' / span ' + span[0] : 'span ' + span[0];
      el.style.gridRow = panel.pos ? panel.pos[1] + ' / span ' + span[1] : 'span ' + span[1];
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
      // Progress bars are rebuilt too; remember where each one was so it can
      // grow from there instead of jumping.
      var was = [].map.call(entry.el.querySelectorAll('.pbar:not(.spin) i'), function (i) { return i.style.getPropertyValue('--p'); });
      entry.el.innerHTML = html;
      [].forEach.call(entry.el.querySelectorAll('.pbar:not(.spin) i'), function (i, n) {
        var to = i.style.getPropertyValue('--p');
        if (!was[n] || was[n] === to) return;
        i.style.setProperty('--p', was[n]);
        void i.offsetWidth;
        i.style.setProperty('--p', to);
      });
      entry.html = html;
      var fresh = entry.el.querySelector('.body');
      if (fresh && top) fresh.scrollTop = top;
    }
    hint(entry.el);
  }

  // A card with more below shows a small "N more" pill at its bottom edge
  // (there is no scrollbar). Wheel, touch and keys scroll it; tapping the
  // pill scrolls down a screenful. N counts the items not fully in view.
  function hint(el) {
    var body = el.querySelector('.body');
    var n = 0;
    if (body && body.scrollHeight - body.clientHeight - body.scrollTop > 4) {
      var rows = body.querySelector('.rows');
      var items = (rows || body).children, bottom = body.getBoundingClientRect().bottom;
      for (var i = 0; i < items.length; i++) {
        if (!items[i].classList.contains('more') && items[i].getBoundingClientRect().bottom > bottom + 1) n++;
      }
    }
    var pill = el.querySelector('.scroll-more');
    if (!n) { if (pill) pill.remove(); return; }
    if (!pill) {
      pill = document.createElement('button');
      pill.type = 'button';
      pill.className = 'scroll-more';
      pill.addEventListener('click', function () {
        var b = el.querySelector('.body');
        if (b) b.scrollBy({ top: b.clientHeight * 0.8, behavior: 'smooth' });
      });
      el.appendChild(pill);
    }
    var text = '\u2193 ' + n + ' more';
    if (pill.textContent !== text) pill.textContent = text;
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
  // Scroll events do not bubble; listen on the way down to update the pill.
  document.addEventListener('scroll', function (ev) {
    var el = ev.target && ev.target.closest ? ev.target.closest('.card') : null;
    if (el) hint(el);
  }, { passive: true, capture: true });
  // A new window size or panel size can make a list fit, or stop fitting.
  window.addEventListener('resize', function () {
    Object.keys(cards).forEach(function (key) { hint(cards[key].el); });
  });

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
      snap.alerts = realAlerts().filter(function (a) { return a.id !== id; });
      if (isFailedAlert(id)) dismissFailed(id.slice(FAILED_ALERT.length));
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
  var failedOpen = false, failedFrom = 'chip';
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
  // The Alerting Kiosk is about hosts: pipeline failures are not shown there.
  function alerting() { return !!(snap.layout && snap.layout.alerting); }
  function visibleFailed() {
    return alerting() ? [] : snap.failed || [];
  }
  function dismissFailed(id) {
    // id null: dismiss everything that is listed now.
    dismissFailedRuns(id ? [id] : visibleFailed().map(failId));
  }
  function dismissFailedRuns(ids) {
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
    var open = linkOpen(f.url, 'what', f.source);
    return '<li>' + open + src(f.source) + ' <b>' + what + '</b><span class="sub">' + sub + '</span>' + linkClose(f.url) +
      '<button type="button" class="x" data-dismiss="' + esc(failId(f)) + '" aria-label="Dismiss">Dismiss</button></li>';
  }

  function renderFailedPop() {
    var pop = $('failed-pop'), list = visibleFailed();
    if (!failedOpen || !list.length) { failedOpen = failedOpen && list.length > 0; pop.hidden = true; pop.innerHTML = ''; return; }
    var fromBanner = failedFrom === 'banner' && !$('banner').hidden;
    var html = '<div class="head"><b>' + (fromBanner ? list.length + ' runs failed' : 'Failed runs') + '</b>' +
      '<span><button type="button" class="x all" data-dismiss-all="1">Dismiss all</button>' +
      (fromBanner ? ' <button type="button" class="x" data-failed-close="1" aria-label="Collapse">&#9652;</button>' : '') + '</span></div><ul>' + list.map(failedRow).join('') + '</ul>' +
      '<div class="foot">Dismissing clears the run on every screen. A new failure shows up again.</div>';
    if (pop.innerHTML !== html) pop.innerHTML = html;
    pop.hidden = false;
    pop.classList.toggle('from-banner', fromBanner);
    var chip = document.querySelector('[data-failed-toggle]');
    if (fromBanner) {
      // Under the red notice, with the same right edge.
      var bb = $('banner').getBoundingClientRect();
      pop.style.left = Math.max(8, Math.min(bb.right - pop.offsetWidth, window.innerWidth - pop.offsetWidth - 8)) + 'px';
      pop.style.top = (bb.bottom + 6) + 'px';
    } else {
      pop.style.top = '';
      if (chip) pop.style.left = Math.max(8, Math.min(chip.getBoundingClientRect().left, window.innerWidth - pop.offsetWidth - 8)) + 'px';
    }
  }

  // ---- alerts: badge, list and dismiss -------------------------------------

  // The Alerts badge works like the Failed badge: click for the list. Dismiss
  // is different in one way: it asks the bridge to clear the alert, so it goes
  // away on every screen (the same as a `clear` message on MQTT). With
  // `[alerts] dismiss = false` the list is read-only.
  var alertsOpen = false;
  var SEV_NAME = { critical: 'Critical', warn: 'Warning', info: 'Info' };
  // Real alerts only: the ticker and the "is there a critical alert" check use these.
  function realAlerts() {
    return snap.alerts || [];
  }
  // With `[alerts] failed_as_alerts` every failed run that is not dismissed is
  // also a critical alert in the Alerts box and menu. Dismissing one dismisses
  // the run itself (the same list as the Failed chip), so there is one list.
  var FAILED_ALERT = 'failed:';
  function isFailedAlert(id) { return String(id).indexOf(FAILED_ALERT) === 0; }
  function failedAsAlert(f) {
    return {
      id: FAILED_ALERT + failId(f), severity: 'critical', source: f.source, url: f.url, since: f.finished || f.started || nowS(),
      title: (f.project || '') + (f.pipeline ? ' · ' + f.pipeline : ''),
      message: 'Failed' + (f.step ? ' at ' + f.step : '') + (f.ref ? ' on ' + f.ref : '')
    };
  }
  function alertList() {
    var cfg = (snap.layout && snap.layout.alerts) || {};
    var runs = cfg.failed_as_alerts ? visibleFailed().map(failedAsAlert) : [];
    return runs.concat(realAlerts());
  }
  function canDismiss() {
    var cfg = (snap.layout && snap.layout.alerts) || {};
    return !!cfg.dismiss && myRole !== 'viewer';
  }
  function dismissAlerts(ids) {
    // A failed run shown as an alert is dismissed as a run.
    var runs = ids.filter(isFailedAlert).map(function (id) { return id.slice(FAILED_ALERT.length); });
    ids = ids.filter(function (id) { return !isFailedAlert(id); });
    if (runs.length) { dismissFailedRuns(runs); }
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
    var sub = [a.message ? esc(a.message) : '', a.source && cfg.show_source !== false ? 'via ' + esc(SOURCES[a.source] || a.source) : '', a.since ? ago(nowS() - a.since) : '']
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
        // `[alerts] failed_pill = false` hides this chip; the red notice stays.
        if (((snap.layout && snap.layout.alerts) || {}).failed_pill === false) return;
        html += '<button type="button" class="chip failed action" data-failed-toggle="1" aria-expanded="' + (failedOpen ? 'true' : 'false') +
          '" title="Show the failed runs">' + n + ' failed</button>';
        return;
      }
      if (alerting() && (b.icon === 'pipeline' || b.icon === 'pr')) return; // the Alerting Kiosk is about hosts: no PR or pipeline badges
      if (b.icon === 'pr' && !b.count) return; // no open PRs: no badge; the Alerting Kiosk has no PR badge at all
      html += '<span class="chip ' + esc(b.status) + '">' + b.count + ' ' + (names[b.icon] || '') + '</span>';
    });
    // The Alerts chip: every active alert, worst colour wins. Gone at 0, like Failed and Open PRs.
    var alerts = alertList(), worst = alerts.some(function (a) { return a.severity === 'critical'; }) ? 'failed'
      : alerts.some(function (a) { return a.severity === 'warn'; }) ? 'warn' : alerts.length ? 'open' : 'ok';
    if (alerts.length) html += '<button type="button" class="chip ' + worst + ' action" data-alerts-toggle="1" aria-expanded="' + (alertsOpen ? 'true' : 'false') +
      '" title="Show the alerts">' + alerts.length + ' alerts</button>';
    if (!alerts.length) alertsOpen = false;
    if (el.innerHTML !== html) el.innerHTML = html;
    renderFailedPop();
    renderAlertsPop();
  }

  // One listener each: the badge row and the popover are rebuilt on every snapshot.
  $('badges').addEventListener('click', function (e) {
    if (e.target.closest && e.target.closest('[data-failed-toggle]')) { failedFrom = 'chip'; failedOpen = !failedOpen; alertsOpen = false; renderBadges(); }
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
    if (e.target.closest && e.target.closest('[data-failed-close]')) { failedOpen = false; renderBadges(); }
  });
  document.addEventListener('click', function (e) {
    if (failedOpen && document.contains(e.target) && e.target.closest && !e.target.closest('#failed-pop') && !e.target.closest('[data-failed-toggle]') && !e.target.closest('#banner')) {
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
    var alerts = cfg.ticker === false ? [] : realAlerts();
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
    var s = alerting() ? null : snap.screen, html = '', cls = '';
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
    var critical = cfg.ticker !== false && realAlerts().some(function (a) { return a.severity === 'critical'; });
    var failed = visibleFailed().slice().sort(function (a, b) { return (b.finished || b.started || 0) - (a.finished || a.started || 0); });
    if (failed.length && !critical && !(s && s.level === 0)) {
      var f = failed[0], fh = href(f.url);
      var fmsg = [f.step ? 'step ' + f.step : '', f.finished ? ago(nowS() - f.finished) : ''].filter(Boolean).join(' · ');
      cls = '';
      html = '<span class="tag">Failed</span>' + src(f.source) +
        (fh ? '<a class="name link" ' + linkAttrs(fh, f.source) + ' title="Open this run">' : '<span>') +
        esc((f.project || '') + (f.pipeline ? ' · ' + f.pipeline : '')) + (fh ? '</a>' : '</span>') +
        (fmsg ? '<span class="msg">' + esc(fmsg) + '</span>' : '') +
        (failed.length > 1 ? '<span class="meta">+' + (failed.length - 1) + '</span>' : '') +
        (fh ? '<a class="act" ' + linkAttrs(fh, f.source) + '>Open</a>' : '') +
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
  // The failed pill: x dismisses that run, the pipeline name and Open open it, the rest of the pill
  // opens the list of failed runs.
  $('banner').addEventListener('click', function (e) {
    var x = e.target.closest ? e.target.closest('[data-dismiss]') : null;
    if (x) { dismissFailed(x.getAttribute('data-dismiss')); renderBanner(); renderTicker(); return; }
    if (e.target.closest && e.target.closest('a')) return;
    // One failed run is a plain line; with more the notice unfolds as a window.
    if (visibleFailed().length > 1 && $('banner').querySelector('[data-dismiss]')) { failedFrom = 'banner'; failedOpen = !failedOpen; alertsOpen = false; renderBadges(); }
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
      // GPU line, only for hosts that report a GPU. Pad with nulls so both lines stay aligned.
      var g = h.stats.gpu_pct;
      var glist = gpuHistory[h.name] = gpuHistory[h.name] || [];
      glist.push(g == null ? null : g);
      if (glist.length > HISTORY) glist.shift();
      // A host with several GPUs: one line each, kept the same way.
      var many = h.stats.gpus && h.stats.gpus.length > 1;
      if (!many) { delete gpuLines[h.name]; return; }
      var lines = gpuLines[h.name] = gpuLines[h.name] || [];
      h.stats.gpus.forEach(function (gpu, i) {
        var l = lines[i] = lines[i] || [];
        // A GPU that appears later is padded so every line ends at the same sample.
        while (l.length < list.length - 1) l.push(null);
        l.push(gpu.pct == null ? null : gpu.pct);
        if (l.length > HISTORY) l.shift();
      });
    });

    trackHostLevels();
    var layout = snap.layout || { columns: 4, rows: 3, panels: [] };
    var setupOk = layout.setup && (myRole == null || myRole === 'admin');
    $('setup-link').hidden = !setupOk;
    renderUpdate(snap.update);
    renderSourcePill();
    renderSources();
    // With the setup page on, the sources chip opens it: that is where every
    // source and its status are listed.
    if (setupOk) { $('conn').setAttribute('href', 'setup'); $('conn').setAttribute('title', 'Sources and their status: Setup'); }
    else { $('conn').removeAttribute('href'); $('conn').removeAttribute('title'); }
    drawGrid();
    renderBadges();
    renderBanner();
    renderTicker();
    noteFailures();
    tick();
  }

  // ---- customizing: hide, move and resize panels ---------------------------

  // The administrator's layout (the bridge's panels) is the default. A person
  // can hide panels, move them and change their size; only that difference is
  // kept (docs/kiosk.md), so new panels in the default still show up. A signed-in
  // user's choice is kept by the bridge, per user; without a login it stays in
  // this browser.
  var TITLES = { jobs: 'Running now', hosts: 'Hosts', stats: 'Host', pipelines: 'Pipelines', prs: 'Open PRs', alerts: 'Alerts', containers: 'Containers', health: 'Health', runners: 'Runners' };
  var editing = false;
  var MAX_ROWS = 12;           // the grid grows downwards when panels are put below the last row
  var custom = null;           // { order: [id], hidden: [id], span: { id: [c, r] }, pos: { id: [col, row] } } or null
  var CUSTOM_KEY = 'deskwatch.layout';
  // The signed-in role (viewer, operator, admin), or null without a login. A
  // viewer gets no Dismiss or Mute buttons and Setup is for administrators:
  // the bridge refuses them anyway, this only keeps buttons that do nothing
  // off the page.
  var myRole = null;
  fetch('api/auth/status').then(function (r) { return r.ok ? r.json() : null; }).then(function (j) {
    myRole = (j && j.state === 'signed_in' && j.role) || null;
    document.documentElement.classList.toggle('ro', myRole === 'viewer');
    if (snap) render(snap);
  }).catch(function () {});
  var onServer = false;        // a signed-in person: the bridge keeps the layout, per user
  try { custom = JSON.parse(localStorage.getItem(CUSTOM_KEY) || 'null'); } catch (e) { custom = null; }
  // A signed-in person's layout lives on the bridge. Without a login, or with a
  // screen key, the answer is an error and the layout stays in this browser.
  // (Relative addresses, like the other calls, so a proxy under a sub path works.)
  var edited = false;          // the person changed the layout here before the bridge's copy arrived
  fetch('api/auth/layout').then(function (r) { return r.ok ? r.json() : null; }).then(function (j) {
    if (!j) return;
    onServer = true;
    if (!edited) custom = j.layout || null;
    if (snap) drawGrid();
  }).catch(function () {});

  function withIds(panels) {
    var seen = {};
    return (panels || []).map(function (p) {
      var base = p.widget + (p.host ? ':' + p.host : '');
      seen[base] = (seen[base] || 0) + 1;
      return Object.assign({}, p, { id: base + '#' + seen[base] });
    });
  }
  function customPanels(layout) {
    var all = withIds(layout.panels);
    if (!custom) return all;
    var hidden = custom.hidden || [], order = custom.order || [];
    var out = all.filter(function (p) { return hidden.indexOf(p.id) < 0; });
    out.sort(function (a, b) {
      var x = order.indexOf(a.id), y = order.indexOf(b.id);
      return (x < 0 ? 999 : x) - (y < 0 ? 999 : y);
    });
    return out.map(function (p) {
      var sp = custom.span && custom.span[p.id];
      if (sp) p.span = [Math.min(sp[0], layout.columns), Math.min(sp[1], MAX_ROWS)];
      var at = custom.pos && custom.pos[p.id], s = p.span || [1, 1];
      if (at) p.pos = [Math.max(1, Math.min(at[0], layout.columns - s[0] + 1)), Math.max(1, Math.min(at[1], MAX_ROWS - s[1] + 1))];
      p.hidden = hidden.indexOf(p.id) >= 0;
      return p;
    });
  }
  // Saves go one after the other, so an older answer can never overwrite a newer one.
  // When the bridge cannot keep it (signed out, no data folder) the bar says so; before, the
  // change was lost on the next reload without a word.
  var saveChain = Promise.resolve(), saveNote = '';
  function saveLocal() {
    try { localStorage.setItem(CUSTOM_KEY, JSON.stringify(custom)); } catch (e) {}
  }
  function saveCustom() {
    edited = true;
    if (!onServer) { saveLocal(); return; }
    var body = JSON.stringify(custom);
    saveChain = saveChain.then(function () {
      return fetch('api/auth/layout', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: body })
        .then(function (r) { if (!r.ok) throw new Error(String(r.status)); saveNote = ''; })
        .catch(function () { saveNote = 'Could not save to your account. Sign in again and retry.'; })
        .then(function () { if (snap && editing && !gesture) renderEditBar(snap.layout); });
    });
  }
  function resetCustom() {
    custom = null;
    edited = true;
    if (onServer) saveChain = saveChain.then(function () { return fetch('api/auth/layout/reset', { method: 'POST' }); }).catch(function () {});
    else try { localStorage.removeItem(CUSTOM_KEY); } catch (x) {}
  }
  function ensureCustom() {
    if (!custom) custom = { order: customPanels(snap.layout).map(function (p) { return p.id; }), hidden: [], span: {}, pos: {} };
    custom.hidden = custom.hidden || []; custom.span = custom.span || {}; custom.pos = custom.pos || {};
    return custom;
  }
  function usedCells(layout) {
    return customPanels(layout).filter(function (p) { return !p.hidden; })
      .reduce(function (n, p) { var s = p.span || [1, 1]; return n + s[0] * s[1]; }, 0);
  }
  // Each panel in edit mode: a grip on top to move it, a cross to hide it and a
  // handle in the corner to resize it. Both gestures also work from the keyboard
  // (arrow keys on the grip and on the handle).
  var RESIZE_ICON = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true"><path d="M13 5L5 13M13 9L9 13"/></svg>';
  function editControls(panel) {
    var id = esc(panel.id);
    return '<button type="button" class="edit-grip" data-grip="' + id + '" title="Drag to move (arrow keys too)" aria-label="Move this panel">&#10303; Drag to move</button>' +
      '<button type="button" class="edit-x" data-edit="hide" data-id="' + id + '" title="Hide this panel" aria-label="Hide this panel">&times;</button>' +
      '<button type="button" class="edit-resize" data-resize="' + id + '" title="Drag to resize (arrow keys too)" aria-label="Resize this panel">' + RESIZE_ICON + '</button>';
  }
  function renderEditBar(layout) {
    var bar = $('edit-bar');
    bar.hidden = !editing;
    $('edit-btn').classList.toggle('on', editing);
    if (!editing) return;
    var cap = layout.columns * layout.rows, used = usedCells(layout);
    var gone = withIds(layout.panels).filter(function (p) { return custom && (custom.hidden || []).indexOf(p.id) >= 0; });
    var tray = gone.length ? gone.map(function (p) {
      return '<button type="button" class="add" data-edit="show" data-id="' + esc(p.id) + '" title="Show this panel again">+ ' + esc(p.title || TITLES[p.widget] || p.widget) + '</button>';
    }).join('') : '<span class="none">none</span>';
    bar.innerHTML = '<span class="t">Customize</span>' +
      '<span class="hint">Drag a panel by its handle onto any free cell, drag the corner to resize it.' + (saveNote ? ' <b class="warn">' + saveNote + '</b>' : ' Saved ' + (onServer ? 'for your account.' : 'in this browser.')) + '</span>' +
      '<span class="tray"><span class="lbl">Hidden panels</span>' + tray + '</span>' +
      '<span class="cells' + (used > cap ? ' over' : '') + '">' + used + ' of ' + cap + ' cells' + (used > cap ? ' &middot; too big, the grid will scroll' : '') + '</span>' +
      '<button type="button" data-edit="reset"' + (custom ? '' : ' disabled') + '>Reset to default</button>' +
      '<button type="button" data-edit="done" class="primary">Done</button>';
  }
  function editAct(act, id) {
    var c = ensureCustom();
    if (act === 'hide') {
      // The others stay where they are; the hidden panel leaves a free gap.
      var cells = readCells(gridGeom());
      delete cells[id];
      commitCells(cells);
    }
    if (act === 'hide' && c.hidden.indexOf(id) < 0) c.hidden.push(id);
    if (act === 'show') c.hidden = c.hidden.filter(function (x) { return x !== id; });
    saveCustom(); drawGrid();
  }
  $('edit-btn').addEventListener('click', function () { editing = !editing; drawGrid(); });
  document.addEventListener('click', function (e) {
    var b = e.target.closest ? e.target.closest('[data-edit]') : null;
    if (!b) return;
    var act = b.getAttribute('data-edit');
    if (act === 'done') { editing = false; drawGrid(); }
    else if (act === 'reset') { resetCustom(); drawGrid(); }
    else editAct(act, b.getAttribute('data-id'));
  });
  // ---- moving and resizing on the grid (mouse, pen and touch) -------------------

  // In edit mode the grid is drawn behind the panels, cell by cell. A panel can be put on
  // any free cell and be any size that fits; panels never overlap. The grid grows
  // downwards (one extra row is always offered, up to MAX_ROWS), so a user who wants
  // everything under each other can have that.
  var gesture = null;

  // Where the cells are on screen.
  function gridGeom() {
    var grid = $('grid'), layout = snap.layout, gr = grid.getBoundingClientRect(), cs = getComputedStyle(grid);
    var pl = parseFloat(cs.paddingLeft) || 0, pr = parseFloat(cs.paddingRight) || 0;
    var pt = parseFloat(cs.paddingTop) || 0, pb = parseFloat(cs.paddingBottom) || 0;
    var gx = parseFloat(cs.columnGap) || 0, gy = parseFloat(cs.rowGap) || 0;
    var cols = layout.columns, rows = layout.rows;
    return {
      x0: gr.left + pl, y0: gr.top + pt, gx: gx, gy: gy, cols: cols, rows: rows,
      w: (grid.clientWidth - pl - pr - gx * (cols - 1)) / cols,
      h: (grid.clientHeight - pt - pb - gy * (rows - 1)) / rows
    };
  }
  function rectOfCell(g, c) {
    return {
      left: g.x0 + (c.col - 1) * (g.w + g.gx), top: g.y0 - $('grid').scrollTop + (c.row - 1) * (g.h + g.gy),
      width: c.w * (g.w + g.gx) - g.gx, height: c.h * (g.h + g.gy) - g.gy
    };
  }
  function cellOfRect(g, r) {
    return {
      col: Math.round((r.left - g.x0) / (g.w + g.gx)) + 1, row: Math.round((r.top - g.y0 + $('grid').scrollTop) / (g.h + g.gy)) + 1,
      w: Math.max(1, Math.round((r.width + g.gx) / (g.w + g.gx))), h: Math.max(1, Math.round((r.height + g.gy) / (g.h + g.gy)))
    };
  }
  // Every card's cell, read from where it is on screen (so it holds for panels that flow too).
  function readCells(g) {
    var cells = {};
    [].forEach.call($('grid').querySelectorAll('.card[data-id]'), function (el) {
      cells[el.getAttribute('data-id')] = cellOfRect(g, el.getBoundingClientRect());
    });
    return cells;
  }
  function fits(g, cells, id, c) {
    if (c.col < 1 || c.row < 1 || c.col + c.w - 1 > g.cols || c.row + c.h - 1 > MAX_ROWS) return false;
    return Object.keys(cells).every(function (o) {
      if (o === id) return true;
      var d = cells[o];
      return c.col + c.w <= d.col || d.col + d.w <= c.col || c.row + c.h <= d.row || d.row + d.h <= c.row;
    });
  }
  function bottomRow(cells) {
    return Object.keys(cells).reduce(function (n, id) { return Math.max(n, cells[id].row + cells[id].h - 1); }, 0);
  }

  // The cells behind the panels, and the layer above them for the drop target.
  var bgEl = null, fxEl = null;
  function layer(cls) {
    var el = document.createElement('div');
    el.className = cls;
    el.setAttribute('aria-hidden', 'true');
    return el;
  }
  function placeIn(el, c) {
    el.style.gridColumn = c.col + ' / span ' + c.w;
    el.style.gridRow = c.row + ' / span ' + c.h;
  }
  function drawBackground(layout) {
    var grid = $('grid'), low = 0;
    customPanels(layout).forEach(function (p) { if (p.pos) low = Math.max(low, p.pos[1] + (p.span || [1, 1])[1] - 1); });
    // Panels put below the last row make the grid longer: it scrolls, rows keep the size of the others.
    grid.classList.toggle('tall', low > layout.rows || editing);
    if (low > layout.rows || editing) grid.style.setProperty('--auto-row', gridGeom().h + 'px'); else grid.style.removeProperty('--auto-row');
    if (!editing) {
      if (bgEl) { bgEl.remove(); fxEl.remove(); bgEl = fxEl = null; }
      return;
    }
    if (!bgEl) { bgEl = layer('grid-bg'); fxEl = layer('grid-fx'); }
    grid.insertBefore(bgEl, grid.firstChild); grid.appendChild(fxEl);
    var g = gridGeom(), cells = readCells(g);
    var rows = Math.min(MAX_ROWS, Math.max(layout.rows, bottomRow(cells) + 1));   // one spare row below
    bgEl.style.gridTemplateRows = 'repeat(' + rows + ', ' + g.h + 'px)';
    fxEl.style.gridTemplateRows = bgEl.style.gridTemplateRows;
    var used = {};
    Object.keys(cells).forEach(function (id) {
      var c = cells[id];
      for (var i = 0; i < c.w; i++) for (var j = 0; j < c.h; j++) used[(c.col + i) + ',' + (c.row + j)] = true;
    });
    var html = '';
    for (var r = 1; r <= rows; r++) for (var q = 1; q <= layout.columns; q++) {
      html += '<div class="bg-cell' + (used[q + ',' + r] ? '' : ' free') + '" style="grid-column:' + q + ';grid-row:' + r + '"><span class="coord">' + q + ',' + r + '</span></div>';
    }
    bgEl.innerHTML = html;
  }
  // Show where the panel would land: blue when it fits, red when it does not.
  function showTarget(c, ok, text) {
    fxEl.innerHTML = '<div class="drop-target' + (ok ? '' : ' bad') + '"><span class="badge">' + esc(text) + '</span></div>';
    placeIn(fxEl.firstChild, c);
  }

  // Remember every panel's place (and the new size of one), then save.
  function commitCells(cells, sizedId) {
    var c = ensureCustom();
    var ids = Object.keys(cells).sort(function (a, b) { return cells[a].row - cells[b].row || cells[a].col - cells[b].col; });
    ids.forEach(function (id) { c.pos[id] = [cells[id].col, cells[id].row]; });
    if (sizedId) c.span[sizedId] = [cells[sizedId].w, cells[sizedId].h];
    commitOrder(ids);
  }
  function commitOrder(ids) {
    var c = ensureCustom();
    c.order = ids.concat((c.order || []).filter(function (x) { return ids.indexOf(x) < 0; }));
    saveCustom();
  }
  function panelIds() {
    return customPanels(snap.layout).map(function (p) { return p.id; });
  }
  function endGesture(commit) {
    var g = gesture;
    if (!g) return;
    gesture = null;
    document.body.classList.remove('gesturing');
    g.finish(commit);
    drawGrid();
  }

  // Pin every card to its cell for the length of a gesture, so taking one out of the
  // flow does not move the others. Gives back a function that undoes it.
  function pin(g, cells) {
    var undo = [];
    [].forEach.call($('grid').querySelectorAll('.card[data-id]'), function (el) {
      var c = cells[el.getAttribute('data-id')];
      undo.push([el, el.style.gridColumn, el.style.gridRow]);
      placeIn(el, c);
    });
    return function () { undo.forEach(function (u) { u[0].style.gridColumn = u[1]; u[0].style.gridRow = u[2]; }); };
  }

  // Move: the grabbed card floats with the pointer; the cells it would take light up.
  function startMove(grip, e) {
    var card = grip.closest('.card[data-id]');
    if (!card) return;
    var id = card.getAttribute('data-id'), g = gridGeom(), cells = readCells(g), mine = cells[id];
    var r = card.getBoundingClientRect(), grabX = e.clientX - r.left, grabY = e.clientY - r.top;
    var unpin = pin(g, cells), best = null;
    card.classList.add('lifted');
    card.style.width = r.width + 'px'; card.style.height = r.height + 'px';
    function track(x, y) {
      // Near the top or bottom edge of the grid, scroll it so rows below (or above) can be reached.
      var gb = $('grid').getBoundingClientRect(), grid = $('grid');
      if (y > gb.bottom - 60) grid.scrollTop += 14; else if (y < gb.top + 60) grid.scrollTop -= 14;
      card.style.left = (x - grabX) + 'px'; card.style.top = (y - grabY) + 'px';
      var c = cellOfRect(g, { left: x - grabX, top: y - grabY, width: r.width, height: r.height });
      c.w = mine.w; c.h = mine.h;
      c.col = Math.max(1, Math.min(c.col, g.cols - c.w + 1)); c.row = Math.max(1, Math.min(c.row, MAX_ROWS - c.h + 1));
      var ok = fits(g, cells, id, c);
      best = ok ? c : null;
      showTarget(c, ok, 'col ' + c.col + ', row ' + c.row);
    }
    track(e.clientX, e.clientY);
    gesture = { pointer: e.pointerId, move: function (ev) { track(ev.clientX, ev.clientY); }, finish: function (commit) {
      grip.releasePointerCapture && grip.releasePointerCapture(e.pointerId);
      card.classList.remove('lifted');
      card.style.width = card.style.height = card.style.left = card.style.top = '';
      fxEl.innerHTML = '';
      if (commit && best) { cells[id] = best; commitCells(cells); } else unpin();
    } };
    document.body.classList.add('gesturing');
    grip.setPointerCapture && grip.setPointerCapture(e.pointerId);
  }

  // Resize: the corner follows the pointer and snaps to the cells; red when it would
  // run into another panel. The panel itself changes when you let go.
  function startResize(handle, e) {
    var card = handle.closest('.card[data-id]');
    if (!card) return;
    var id = card.getAttribute('data-id'), g = gridGeom(), cells = readCells(g), mine = cells[id];
    var r = card.getBoundingClientRect(), best = null;
    handle.classList.add('on');
    function track(x, y) {
      var c = { col: mine.col, row: mine.row,
        w: Math.max(1, Math.min(g.cols - mine.col + 1, Math.round((x - r.left + g.gx) / (g.w + g.gx)))),
        h: Math.max(1, Math.min(MAX_ROWS - mine.row + 1, Math.round((y - r.top + g.gy) / (g.h + g.gy)))) };
      var ok = fits(g, cells, id, c);
      best = ok ? c : null;
      showTarget(c, ok, c.w + ' × ' + c.h);
    }
    track(e.clientX, e.clientY);
    gesture = { pointer: e.pointerId, move: function (ev) { track(ev.clientX, ev.clientY); }, finish: function (commit) {
      handle.releasePointerCapture && handle.releasePointerCapture(e.pointerId);
      handle.classList.remove('on');
      fxEl.innerHTML = '';
      if (commit && best && (best.w !== mine.w || best.h !== mine.h)) { cells[id] = best; commitCells(cells, id); }
    } };
    document.body.classList.add('gesturing');
    handle.setPointerCapture && handle.setPointerCapture(e.pointerId);
  }

  document.addEventListener('pointerdown', function (e) {
    if (!editing || gesture || (e.button && e.pointerType === 'mouse')) return;
    var t = e.target.closest ? e.target : null;
    var grip = t && t.closest('[data-grip]'), handle = t && t.closest('[data-resize]');
    if (grip) { e.preventDefault(); startMove(grip, e); }
    else if (handle) { e.preventDefault(); startResize(handle, e); }
  });
  document.addEventListener('pointermove', function (e) {
    if (gesture && e.pointerId === gesture.pointer) { e.preventDefault(); gesture.move(e); }
  });
  document.addEventListener('pointerup', function (e) { if (gesture && e.pointerId === gesture.pointer) endGesture(true); });
  document.addEventListener('pointercancel', function (e) { if (gesture && e.pointerId === gesture.pointer) endGesture(false); });

  // Keyboard: arrow keys on the grip move the panel one cell, on the handle they change its size.
  document.addEventListener('keydown', function (e) {
    if (gesture && e.key === 'Escape') { endGesture(false); return; }
    if (!editing || !e.target.closest) return;
    var grip = e.target.closest('[data-grip]'), handle = e.target.closest('[data-resize]');
    var d = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!d || !(grip || handle)) return;
    e.preventDefault();
    var id = (grip || handle).getAttribute(grip ? 'data-grip' : 'data-resize');
    var g = gridGeom(), cells = readCells(g), c = cells[id];
    if (!c) return;
    var next = grip ? { col: c.col + d[0], row: c.row + d[1], w: c.w, h: c.h } : { col: c.col, row: c.row, w: c.w + d[0], h: c.h + d[1] };
    if (next.w < 1 || next.h < 1 || !fits(g, cells, id, next)) return;
    cells[id] = next;
    commitCells(cells, handle ? id : null);
    drawGrid();
    var b = document.querySelector('[data-' + (grip ? 'grip' : 'resize') + '="' + id + '"]');
    if (b) b.focus();
  });

  // The cards. Also redrawn every few seconds without a new snapshot, so a
  // "Done" job and a tinted row go away on time.
  // The span and the place are in the key: a card is placed once, so a new size or place needs a new card.
  function cardKey(panel) {
    return panel.id + ':' + (panel.span || [1, 1]).join('x') + (panel.pos ? '@' + panel.pos.join(',') : '') + (editing ? ':e' : '');
  }
  function drawGrid() {
    if (gesture) return;
    var layout = snap.layout || { columns: 4, rows: 3, panels: [] };
    var grid = $('grid');
    grid.style.setProperty('--cols', layout.columns);
    grid.style.setProperty('--rows', layout.rows);
    var keep = {};
    var shown = customPanels(layout);
    grid.classList.toggle('editing', editing);
    shown.forEach(function (panel, i) {
      // The span is in the key: a card is placed once, so a new size needs a new card.
      var key = cardKey(panel);
      keep[key] = true;
      renderCard(key, panel, grid, i);
    });
    // Cards are placed in order, so the DOM order follows the layout.
    shown.forEach(function (panel) {
      var entry = cards[cardKey(panel)];
      if (entry) grid.appendChild(entry.el);
    });
    Object.keys(cards).forEach(function (key) {
      if (!keep[key]) { grid.removeChild(cards[key].el); delete cards[key]; }
    });
    drawBackground(layout);
    renderEditBar(layout);
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
