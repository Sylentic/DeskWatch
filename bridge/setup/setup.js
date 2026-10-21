// DeskWatch setup page. Shows how the bridge is configured, tests a connection
// (the bridge makes the requests, with the real credentials) and, when the
// bridge has a data folder, adds, changes and removes sources. Secrets are
// never sent to the browser: a credential is only reported as present or
// missing, and a token typed here goes to the bridge and is never shown again. Everything is built with textContent, so
// nothing from the config can inject markup.
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

  var app = document.getElementById('app');

  // Dark is the default. The button switches to light and remembers it in this
  // browser, for the dashboard too (the same key); the system's own light or
  // dark setting is not consulted. The old setup-only key still counts.
  var themeButton = document.getElementById('theme');
  function theme() {
    try { return (localStorage.getItem('deskwatch-theme') || localStorage.getItem('deskwatch-setup-theme')) === 'light' ? 'light' : 'dark'; }
    catch (e) { return 'dark'; }
  }
  function applyTheme(name) {
    if (name === 'light') document.documentElement.setAttribute('data-theme', 'light');
    else document.documentElement.removeAttribute('data-theme');
    themeButton.textContent = name === 'light' ? 'Dark mode' : 'Light mode';
  }
  applyTheme(theme());
  themeButton.addEventListener('click', function () {
    var next = theme() === 'light' ? 'dark' : 'light';
    try { localStorage.setItem('deskwatch-theme', next); } catch (e) { /* not kept */ }
    applyTheme(next);
  });
  var REFRESH_MS = 5000;
  var timer = null;
  var last = null;       // latest status, so a finished test can redraw at once
  var results = {};      // source id -> latest test report (kept across refreshes)
  var running = {};      // source id -> true while its test runs
  var notice = null;     // message after the last save that could not be applied
  var form = null;       // the open add/edit form, or null (see openForm)
  var confirming = null; // key of the action waiting for its confirm click, or null
  var restarting = null; // { boot, since } while the page waits for a restart to finish
  var SECTIONS = ['sources', 'mqtt', 'dashboard', 'alerts', 'access', 'signin', 'bridge', 'secrets'];
  var oauth = null;      // last answer of api/auth/oauth (the Sign-in section)
  var oauthNote = null;
  var access = null;     // last answer of api/auth/status (the Access section)
  var accessNote = null; // result line and one-time key of the Access section
  var alertNote = null;  // result of the last test alert button press

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function pill(text, cls) { return el('span', 'pill ' + cls, text); }

  function row(dl, term, value) {
    dl.appendChild(el('dt', null, term));
    var dd = el('dd');
    if (value instanceof Node) dd.appendChild(value); else dd.textContent = value;
    dl.appendChild(dd);
  }

  var HEALTH = {
    ok: ['working', 'ok'], auth_failed: ['token refused', 'bad'],
    unreachable: ['not reachable', 'bad'], unknown: ['no report yet', 'warn']
  };
  var CRED = {
    mounted: ['found (mounted file)', 'ok'], data_folder: ['found (data folder)', 'ok'],
    missing: ['missing', 'bad'], environment: ['from environment', 'ok'], none: ['none', 'muted']
  };
  var KIND = { gitea: 'Gitea', github: 'GitHub', gitlab: 'GitLab', bitbucket: 'Bitbucket', azure_devops: 'Azure DevOps', prometheus: 'Prometheus' };

  function credState(c) {
    var s = CRED[c.status] || [c.status, 'warn'];
    // An optional credential that is not there is fine.
    if (c.status === 'missing' && !c.required) s = ['not set', 'muted'];
    return pill(s[0], s[1]);
  }

  // A "Test connection" button and the result of the latest test.
  function testBlock(id) {
    var box = el('div', 'test');
    var button = el('button', 'secondary', running[id] ? 'Testing…' : 'Test connection');
    button.type = 'button';
    button.disabled = !!running[id];
    button.addEventListener('click', function () { runTest(id); });
    box.appendChild(button);
    var r = results[id];
    if (r) {
      var list = el('ul', 'checks');
      list.setAttribute('aria-label', 'test result');
      r.checks.forEach(function (c) {
        var li = el('li');
        li.appendChild(pill(c.ok ? 'ok' : 'failed', c.ok ? 'ok' : 'bad'));
        li.appendChild(document.createTextNode(' ' + c.name + (c.ok && c.message === 'ok' ? '' : ': ' + c.message)));
        list.appendChild(li);
      });
      if (r.error) list.appendChild(el('li', 'err', r.error));
      box.appendChild(list);
    }
    return box;
  }

  function runTest(id) {
    running[id] = true;
    delete results[id];
    if (last) render(last);
    fetch('api/setup/test', {
      method: 'POST', cache: 'no-store', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: id })
    }).then(function (r) {
      if (!r.ok) return { checks: [], error: r.status === 429 ? 'Another test is still running.' : r.status === 504 ? 'The test took too long.' : 'The test failed (HTTP ' + r.status + ').' };
      return r.json();
    }, function () { return { checks: [], error: 'Cannot reach the bridge.' }; })
      .then(function (report) {
        running[id] = false;
        if (report) results[id] = report;
        if (last) render(last);
      });
  }

  // What each kind of source asks for. `key` names are the bridge's own.
  var FORMS = {
    github: {
      strings: [['base_url', 'API address', 'https://api.github.com']],
      lists: [['repos', 'Repositories, one per line (owner/name)']],
      secrets: [['token', 'Token (read-only, fine-grained)', true]]
    },
    gitlab: {
      strings: [['base_url', 'GitLab address (without /api/v4)', 'https://gitlab.com']],
      lists: [['projects', 'Projects, one per line (group/project)']],
      secrets: [['token', 'Access token (read_api, read-only)', true]]
    },
    bitbucket: {
      strings: [],
      lists: [['repos', 'Repositories, one per line (workspace/repo)']],
      secrets: [['token', 'Access token (read-only: Pull requests, Pipelines)', true]]
    },
    gitea: {
      strings: [['base_url', 'Gitea address', 'https://gitea.example.com']],
      lists: [['repos', 'Repositories, one per line (owner/name)']],
      secrets: [['token', 'Token (read:repository, only for private repositories)', false],
                ['webhook_secret', 'Webhook secret (the same text as in Gitea)', true]]
    },
    azure_devops: {
      strings: [['organization', 'Organisation (the name after dev.azure.com/)', ''], ['base_url', 'Address (without the organisation)', 'https://dev.azure.com']],
      lists: [['projects', 'Projects, one per line']],
      bools: [['pull_requests', 'Show open pull requests']],
      secrets: [['token', 'Personal access token (read-only)', true]]
    },
    prometheus: {
      strings: [['url', 'Prometheus address', 'http://prometheus.example.lan:9090']],
      lists: [['hosts', 'Hosts, one per line (the node_exporter instance, such as node-exporter:9100)']],
      secrets: [['token', 'Bearer token (only behind a proxy)', false]]
    }
  };

  // The broker settings use the same form. The password is stored by the bridge
  // as a credential and never comes back.
  FORMS.mqtt = {
    strings: [['host', 'Broker address', 'localhost'], ['port', 'Port', '1883'],
              ['username', 'Login name (empty = no login)', '']],
    lists: [],
    bools: [['enabled', 'Connect to the broker'], ['tls', 'Use TLS (usually port 8883)']],
    secrets: [['password', 'Password', false]]
  };

  function post(url, body) {
    return fetch(url, {
      method: 'POST', cache: 'no-store', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body)
    }).then(function (r) {
      return r.json().catch(function () { return {}; }).then(function (j) { j.status = r.status; return j; });
    }, function () { return { error: 'Cannot reach the bridge.' }; });
  }

  function openForm(kind, source) {
    var spec = FORMS[kind], values = {}, f = source && source.fields || {};
    spec.strings.forEach(function (x) { values[x[0]] = f[x[0]] != null ? String(f[x[0]]) : x[2]; });
    spec.lists.forEach(function (x) { values[x[0]] = (f[x[0]] || []).join('\n'); });
    (spec.bools || []).forEach(function (x) { values[x[0]] = !!f[x[0]]; });
    if (kind === 'mqtt') values.username = f.username || '';
    form = { kind: kind, mode: source ? 'edit' : 'add', name: source ? source.name : '', values: values,
             secrets: {}, advanced: !!f.hosts_advanced, busy: false, result: null, error: null };
    if (last) render(last);
  }

  function closeForm() { form = null; if (last) render(last); }

  function submitForm(force) {
    var spec = FORMS[form.kind], fields = {}, secrets = {};
    spec.strings.forEach(function (x) { fields[x[0]] = form.values[x[0]]; });
    spec.lists.forEach(function (x) {
      if (x[0] === 'hosts' && form.advanced) return;     // advanced host settings stay as they are
      fields[x[0]] = form.values[x[0]].split('\n').map(function (l) { return l.trim(); }).filter(Boolean);
    });
    (spec.bools || []).forEach(function (x) { fields[x[0]] = !!form.values[x[0]]; });
    spec.secrets.forEach(function (x) { if (form.secrets[x[0]]) secrets[x[0]] = form.secrets[x[0]]; });
    notice = null;
    form.busy = true; form.error = null; if (!force) form.result = null;
    render(last);
    var isMqtt = form.kind === 'mqtt';
    if (isMqtt) fields.port = /^\d+$/.test(String(fields.port).trim()) ? parseInt(fields.port, 10) : fields.port;
    var request = isMqtt
      ? post('api/setup/mqtt', { fields: fields, secrets: secrets, force: !!force })
      : post('api/setup/source', { kind: form.kind, name: form.name.trim(), create: form.mode === 'add',
                                   fields: fields, secrets: secrets, force: !!force });
    request.then(function (j) {
      if (!j || !form) return;
      form.busy = false;
      if (j.error) form.error = j.error;
      else if (j.saved) { results[j.id] = j.report; notice = isMqtt ? null : applyNotice(j); form = null; load(); return; }
      else form.result = j.report;
      render(last);
    });
  }

  // What to tell after a save: it runs now, or it is saved but could not start.
  function applyNotice(j) {
    if (j.applied) return null;
    return 'Saved, but ' + j.id + ' could not be started: ' + (j.apply_error || 'unknown reason') + '. Restart the bridge to try again.';
  }


  function field(labelText, control) {
    var label = el('label', 'field');
    label.appendChild(el('span', null, labelText));
    label.appendChild(control);
    return label;
  }

  function formCard() {
    var spec = FORMS[form.kind], card = el('section', 'card');
    var isMqtt = form.kind === 'mqtt';
    card.appendChild(el('h2', null, isMqtt ? 'Change the MQTT broker settings'
      : (form.mode === 'add' ? 'Add ' : 'Change ') + (KIND[form.kind] || form.kind) + ' source'));
    var box = el('div', 'form');
    if (!isMqtt) {
      var name = el('input'); name.value = form.name; name.readOnly = form.mode === 'edit';
      name.addEventListener('input', function () { form.name = name.value; });
      box.appendChild(field('Name (letters, digits, - and _)', name));
    }
    spec.strings.forEach(function (x) {
      var i = el('input'); i.value = form.values[x[0]];
      i.addEventListener('input', function () { form.values[x[0]] = i.value; });
      box.appendChild(field(x[1], i));
    });
    spec.lists.forEach(function (x) {
      var t = el('textarea'); t.rows = 4; t.value = form.values[x[0]];
      t.disabled = x[0] === 'hosts' && form.advanced;
      t.addEventListener('input', function () { form.values[x[0]] = t.value; });
      var lab = field(x[1] + (t.disabled ? ' (has advanced settings: edit sources.toml by hand)' : ''), t);
      box.appendChild(lab);
    });
    (spec.bools || []).forEach(function (x) {
      var c = el('input'); c.type = 'checkbox'; c.checked = !!form.values[x[0]];
      c.addEventListener('change', function () { form.values[x[0]] = c.checked; });
      var lab = el('label', 'check'); lab.appendChild(c); lab.appendChild(document.createTextNode(' ' + x[1]));
      box.appendChild(lab);
    });
    spec.secrets.forEach(function (x) {
      var i = el('input'); i.type = 'password'; i.autocomplete = 'new-password'; i.spellcheck = false;
      i.placeholder = form.mode === 'edit' ? 'leave empty to keep the current one' : (x[2] ? 'required' : 'optional');
      i.addEventListener('input', function () { form.secrets[x[0]] = i.value; });
      box.appendChild(field(x[1], i));
    });
    card.appendChild(box);
    var buttons = el('div', 'buttons');
    var save = el('button', null, form.busy ? 'Testing…' : 'Test and save');
    save.type = 'button'; save.disabled = form.busy;
    save.addEventListener('click', function () { submitForm(false); });
    var cancel = el('button', 'secondary', 'Cancel');
    cancel.type = 'button'; cancel.addEventListener('click', closeForm);
    buttons.appendChild(save); buttons.appendChild(cancel);
    if (form.result && !form.result.ok) {
      var force = el('button', 'secondary', 'Save anyway');
      force.type = 'button'; force.addEventListener('click', function () { submitForm(true); });
      buttons.appendChild(force);
    }
    card.appendChild(buttons);
    if (form.error) { var e = el('p', 'err', form.error); e.setAttribute('role', 'alert'); card.appendChild(e); }
    if (form.result) {
      card.appendChild(el('p', form.result.ok ? 'muted' : 'err', form.result.ok ? 'The test passed.' : 'Not saved: the test failed. Fix the details, or save anyway.'));
      var list = el('ul', 'checks');
      form.result.checks.forEach(function (c) {
        var li = el('li');
        li.appendChild(pill(c.ok ? 'ok' : 'failed', c.ok ? 'ok' : 'bad'));
        li.appendChild(document.createTextNode(' ' + c.name + (c.ok && c.message === 'ok' ? '' : ': ' + c.message)));
        list.appendChild(li);
      });
      card.appendChild(list);
    }
    return card;
  }

  // ---- Confirm step: an inline box instead of a browser dialog ----

  function ask(key) { confirming = key; if (last) render(last); }
  function cancelAsk() { confirming = null; if (last) render(last); }

  // A box with a question and Yes/Cancel, drawn in place of the action's button.
  function confirmBox(key, question, detail, yes, onYes) {
    var box = el('div', 'confirm');
    box.setAttribute('role', 'alertdialog');
    box.appendChild(el('b', null, question));
    if (detail) box.appendChild(el('p', 'muted', detail));
    var b = el('div', 'buttons');
    var y = el('button', 'secondary danger', yes);
    y.type = 'button';
    y.addEventListener('click', function () { confirming = null; onYes(); });
    var n = el('button', 'secondary', 'Cancel');
    n.type = 'button'; n.addEventListener('click', cancelAsk);
    b.appendChild(y); b.appendChild(n);
    box.appendChild(b);
    return box;
  }

  // Do a change on the bridge, show what went wrong, then reload the status.
  function change(url, body) {
    post(url, body).then(function (j) {
      notice = null;
      if (j && j.error) notice = j.error;
      else if (j) notice = applyNotice(j);
      load();
    });
  }

  function removeSource(s) {
    change('api/setup/source/delete', { kind: s.kind, name: s.name });
  }

  // ---- Restart ----

  // Ask the bridge to restart itself, then wait until a new process answers
  // (its `boot` differs). It only comes back when Docker or systemd starts it
  // again; after a minute the page says so.
  function restartBridge() {
    post('api/setup/restart', {}).then(function (j) {
      if (!j || j.error) { notice = (j && j.error) || 'The restart failed.'; if (last) render(last); return; }
      restarting = { boot: last && last.boot, since: Date.now() };
      clearTimeout(timer);
      render(last);
      timer = setTimeout(load, 2000);
    });
  }

  function restartButton(label) {
    var b = el('button', 'restart', label || 'Restart bridge');
    b.type = 'button';
    b.addEventListener('click', function () { ask('restart'); });
    return b;
  }

  // ---- Pieces of the page ----

  function addBar(d) {
    var bar = el('section', 'card addbar');
    bar.appendChild(el('h2', null, 'Add a source'));
    var kind = el('select');
    Object.keys(FORMS).forEach(function (k) { if (k === 'mqtt') return; var o = el('option', null, KIND[k]); o.value = k; kind.appendChild(o); });
    var go = el('button', null, 'Add');
    go.type = 'button'; go.addEventListener('click', function () { openForm(kind.value, null); });
    bar.appendChild(kind); bar.appendChild(go);
    bar.appendChild(el('span', 'muted', 'A new source starts working right away, no restart needed.'));
    return bar;
  }

  function sourceCard(s) {
    var card = el('section', 'card' + (s.hidden ? ' off' : ''));
    var h = el('h2');
    // The same two-letter mark as on the dashboard.
    var MARK = { gitea: 'Gt', github: 'GH', gitlab: 'GL', bitbucket: 'BB', azure_devops: 'AZ', prometheus: 'PM' };
    if (MARK[s.kind]) h.appendChild(el('span', 'mark ' + s.kind, MARK[s.kind]));
    h.appendChild(document.createTextNode((KIND[s.kind] || s.kind) + ': ' + s.name + ' '));
    var hs = HEALTH[s.health] || (s.hidden ? ['switched off', 'muted'] : [s.health, 'warn']);
    h.appendChild(pill(hs[0], hs[1]));
    card.appendChild(h);
    var dl = el('dl');
    row(dl, 'Address', s.address);
    if (s.repos && s.repos.length) row(dl, s.kind === 'prometheus' ? 'Hosts' : (s.kind === 'azure_devops' || s.kind === 'gitlab') ? 'Projects' : 'Repositories', s.repos.join(', '));
    row(dl, 'Defined in', s.origin === 'data_folder' ? 'data folder (sources.toml)' : 'bridge.toml');
    s.credentials.forEach(function (c) {
      var v = el('span');
      v.appendChild(credState(c));
      v.appendChild(document.createTextNode(' ' + c.name));
      row(dl, c.label, v);
    });
    card.appendChild(dl);
    if (s.hidden) {
      card.appendChild(el('p', 'muted', 'Switched off from this page. It is not running and not on the dashboard. bridge.toml is untouched.'));
    } else {
      if (!s.running) card.appendChild(el('p', 'muted', 'Saved, but not running. Fix the details and save again, or restart the bridge.'));
      card.appendChild(testBlock(s.id));
    }
    var key = 'source:' + s.id;
    var b = el('div', 'buttons');
    if (s.editable) {
      var edit = el('button', 'secondary', 'Edit');
      edit.type = 'button'; edit.addEventListener('click', function () { openForm(s.kind, s); });
      var rm = el('button', 'secondary danger', 'Remove');
      rm.type = 'button'; rm.addEventListener('click', function () { ask(key); });
      b.appendChild(edit); b.appendChild(rm);
      card.appendChild(b);
      if (confirming === key) card.appendChild(confirmBox(key, 'Remove ' + s.name + '?',
        'The source stops and its tokens saved in the data folder are deleted. This cannot be undone.', 'Yes, remove',
        function () { removeSource(s); }));
    } else if (s.hideable) {
      var off = el('button', s.hidden ? 'secondary' : 'secondary danger', s.hidden ? 'Switch on again' : 'Switch off here');
      off.type = 'button';
      off.addEventListener('click', function () {
        if (s.hidden) change('api/setup/source/hide', { kind: s.kind, name: s.name, hidden: false });
        else ask(key);
      });
      b.appendChild(off);
      card.appendChild(b);
      if (!s.hidden) card.appendChild(el('p', 'muted', 'This source is defined in bridge.toml, which this page never edits. Switching it off stops it here and keeps bridge.toml as it is; to delete it for good, remove its block from bridge.toml.'));
      if (confirming === key) card.appendChild(confirmBox(key, 'Switch off ' + s.name + '?',
        'It stops and leaves the dashboard. Its token file belongs to bridge.toml and stays. You can switch it on again here.', 'Yes, switch off',
        function () { change('api/setup/source/hide', { kind: s.kind, name: s.name, hidden: true }); }));
    } else if (!s.hidden) {
      card.appendChild(el('p', 'muted', 'This source is defined in bridge.toml. Give the bridge a data folder to switch it off from here; to delete it, remove its block from bridge.toml.'));
    }
    return card;
  }

  function sourcesSection(d) {
    var box = el('div');
    if (form && form.mode === 'add') box.appendChild(formCard());
    else if (d.can_edit) box.appendChild(addBar(d));
    else box.appendChild(el('p', 'muted', 'To add and change sources here, give the bridge a data folder ([data] dir, see docs/data-dir.md).'));
    if (!d.sources.length) box.appendChild(el('p', 'muted', 'No sources configured.'));
    var grid = el('div', 'srcgrid');
    d.sources.forEach(function (s) {
      var editing = form && form.mode === 'edit' && form.kind === s.kind && form.name === s.name;
      grid.appendChild(editing ? formCard() : sourceCard(s));
    });
    box.appendChild(grid);
    box.appendChild(el('p', 'muted', 'Tokens are never sent to the browser.'));
    return box;
  }

  function mqttSection(d) {
    var m = d.mqtt, box = el('div');
    if (form && form.kind === 'mqtt') { box.appendChild(formCard()); return box; }
    var info = el('section', 'card');
    var h = el('h2', null, 'MQTT broker ');
    if (m.enabled) h.appendChild(pill(m.connected ? 'connected' : 'not connected', m.connected ? 'ok' : 'bad'));
    else h.appendChild(pill('off', 'muted'));
    info.appendChild(h);
    var dl = el('dl');
    if (m.enabled) {
      row(dl, 'Broker', m.host + ':' + m.port + (m.tls ? ' (TLS)' : ''));
      row(dl, 'Login', (m.username || 'anonymous') + (m.username ? ', password ' + (CRED[m.password] || [m.password])[0] : ''));
    } else {
      row(dl, 'Broker', 'off');
    }
    row(dl, 'Settings from', m.origin === 'data_folder' ? 'data folder (changed here)' : 'bridge.toml');
    info.appendChild(dl);
    if (m.enabled) info.appendChild(testBlock('mqtt'));
    if (m.editable) {
      var b = el('div', 'buttons');
      var chg = el('button', 'secondary', 'Change broker settings');
      chg.type = 'button';
      chg.addEventListener('click', function () { openForm('mqtt', { name: 'mqtt', fields: m }); });
      b.appendChild(chg);
      if (d.mqtt.has_login) {
        var rl = el('button', 'secondary danger', 'Remove login');
        rl.type = 'button'; rl.addEventListener('click', function () { ask('mqtt:login'); });
        b.appendChild(rl);
      }
      if (m.removable) {
        var rs = el('button', 'secondary danger', 'Reset to bridge.toml');
        rs.type = 'button'; rs.addEventListener('click', function () { ask('mqtt:settings'); });
        b.appendChild(rs);
      }
      info.appendChild(b);
      if (confirming === 'mqtt:login') info.appendChild(confirmBox('mqtt:login', 'Remove the broker login?',
        'The login name is cleared and the password saved in the data folder is deleted. Applies after a restart.', 'Yes, remove login',
        function () { change('api/setup/mqtt/delete', { what: 'login' }); }));
      if (confirming === 'mqtt:settings') info.appendChild(confirmBox('mqtt:settings', 'Remove everything saved here for the broker?',
        'Host, port, TLS, login and the saved password go; bridge.toml decides again. Applies after a restart.', 'Yes, reset',
        function () { change('api/setup/mqtt/delete', { what: 'settings' }); }));
    } else {
      info.appendChild(el('p', 'muted', 'Give the bridge a data folder to change the broker settings here.'));
    }
    box.appendChild(info);
    return box;
  }

  // How the dashboard's built-in layout shows the hosts. Applies after a restart.
  function dashboardSection(d) {
    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Dashboard hosts'));
    if (!d.kiosk) {
      card.appendChild(el('p', 'muted', 'The dashboard is off ([kiosk] enabled in bridge.toml).'));
      return card;
    }
    var choice = el('select');
    [['detailed', 'Detailed: a list of hosts with the chosen host in detail next to it (tap a host to switch)'],
     ['compact', 'Compact: one table of all hosts, with uptime and network rates']].forEach(function (o) {
      var opt = el('option', null, o[1]);
      opt.value = o[0];
      opt.selected = d.host_view === o[0];
      choice.appendChild(opt);
    });
    choice.disabled = !d.host_view_editable;
    card.appendChild(field('Host panel', choice));
    if (d.host_view_editable) {
      var save = el('button', 'secondary', 'Save');
      save.type = 'button';
      save.addEventListener('click', function () { change('api/setup/kiosk', { host_view: choice.value }); });
      var line = el('div', 'buttons'); line.appendChild(save); card.appendChild(line);
    }
    card.appendChild(el('p', 'muted', 'Only for the built-in layout; panels set in bridge.toml stay as they are. '
      + (d.host_view_editable ? 'Needs a restart of the bridge.' : 'Give the bridge a data folder to change it here.')));
    return card;
  }

  // ---- Alerts ----

  // One row of the ticker settings: a label with a hint and its control.
  function settingRow(title, hint, control) {
    var r = el('div', 'setting');
    var text = el('div');
    text.appendChild(el('span', null, title));
    text.appendChild(el('small', null, hint));
    r.appendChild(text);
    r.appendChild(control);
    return r;
  }

  function checkbox(on, disabled) {
    var c = el('input'); c.type = 'checkbox'; c.checked = !!on; c.disabled = !!disabled;
    return c;
  }

  // Ticker settings (they apply after a restart), a fixed test alert, and where
  // alerts can come from. The test button can only raise or clear its own alert.
  function alertsSection(d) {
    var a = d.alerts, box = el('div');
    var two = el('div', 'two');
    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Alerts in the header'));
    var ticker = checkbox(a.ticker, !a.editable), source = checkbox(a.show_source, !a.editable);
    var dismiss = checkbox(a.dismiss, !a.editable), httpInput = checkbox(a.http_input, !a.editable);
    var max = el('input'); max.type = 'number'; max.min = '1'; max.max = '50'; max.value = String(a.max_active); max.disabled = !a.editable;
    max.className = 'narrow';
    card.appendChild(settingRow('Show the latest alert', 'The most urgent alert is a coloured pill in the header bar of the dashboard', ticker));
    card.appendChild(settingRow('Show the source', 'Adds "via homeassistant" behind each alert', source));
    card.appendChild(settingRow('Most alerts kept', 'When there are more, the least urgent and oldest drops first (1 to 50)', max));
    card.appendChild(settingRow('Dismiss on the dashboard', 'A Dismiss button per alert in the Alerts menu. It clears the alert on every screen', dismiss));
    card.appendChild(settingRow('Accept alerts on the web address', 'POST /api/alert with the same JSON as MQTT. No login: for a trusted network only', httpInput));
    if (a.editable) {
      var save = el('button', null, 'Save');
      save.type = 'button';
      save.addEventListener('click', function () {
        post('api/setup/alerts', {
          ticker: ticker.checked, show_source: source.checked,
          dismiss: dismiss.checked, http_input: httpInput.checked, max_active: parseInt(max.value, 10) || 0
        }).then(function (j) { notice = (j && j.error) || null; load(); });
      });
      var line = el('div', 'buttons'); line.appendChild(save); card.appendChild(line);
    }
    card.appendChild(el('p', 'muted', a.editable ? 'Needs a restart of the bridge.'
      : 'Give the bridge a data folder to change these here.'));
    two.appendChild(card);

    var tryIt = el('section', 'card');
    tryIt.appendChild(el('h2', null, 'Try it'));
    tryIt.appendChild(el('p', 'muted', 'Raises one harmless test alert (title "Test alert", source "setup") that goes away by itself after a minute. '
      + 'Nothing else can be created from this page.'));
    var send = el('button', null, 'Send test alert'), clear = el('button', 'secondary', 'Clear it now');
    send.type = clear.type = 'button';
    send.disabled = clear.disabled = !a.test_available;
    function test(action) {
      post('api/setup/alerts/test', { action: action }).then(function (j) {
        alertNote = j && j.error ? j.error : (action === 'send'
          ? 'Test alert sent. Look at the header of the dashboard (' + (a.ticker ? 'the pill next to the badges' : 'the header pill is switched off') + ').' : 'Test alert cleared.');
        if (last) render(last);
      });
    }
    send.addEventListener('click', function () { test('send'); });
    clear.addEventListener('click', function () { test('clear'); });
    var buttons = el('div', 'buttons'); buttons.appendChild(send); buttons.appendChild(clear); tryIt.appendChild(buttons);
    if (alertNote) { var n = el('p', null, alertNote); n.setAttribute('role', 'status'); tryIt.appendChild(n); }
    if (!d.kiosk) tryIt.appendChild(el('p', 'muted', 'The dashboard is off ([kiosk] enabled in bridge.toml), so there is no header to look at.'));
    two.appendChild(tryIt);
    box.appendChild(two);

    var src = el('section', 'card');
    src.appendChild(el('h2', null, 'Where alerts come from'));
    var dl = el('dl');
    var mqtt;
    if (!a.enabled) mqtt = pill('alerts off ([alerts] enabled in bridge.toml)', 'muted');
    else if (!a.mqtt_enabled) mqtt = pill('MQTT is off', 'muted');
    else mqtt = pill(a.mqtt_connected ? 'broker connected' : 'no broker connection', a.mqtt_connected ? 'ok' : 'warn');
    row(dl, 'MQTT', mqtt);
    row(dl, 'Topic', 'deskpanel/alert (Home Assistant blueprints or any script, see docs/home-assistant.md)');
    row(dl, 'Web address', a.enabled ? pill(a.http_input ? 'on: POST /api/alert' : 'off', a.http_input ? 'ok' : 'muted') : pill('alerts off ([alerts] enabled in bridge.toml)', 'muted'));
    row(dl, 'Example', 'curl -X POST http://<bridge>:8787/api/alert -d \'{"v":2,"id":"x","severity":"warning","title":"Hello"}\' (see docs/home-assistant.md)');
    src.appendChild(dl);
    src.appendChild(el('p', 'muted', 'Failed pipeline runs are not alerts: they count in the Failed badge.'));
    box.appendChild(src);
    return box;
  }

  function bridgeSection(d) {
    var box = el('div', 'two');
    var info = el('section', 'card');
    info.appendChild(el('h2', null, 'Bridge'));
    var dl = el('dl');
    var ver = el('span', null, d.build);
    if (d.update) {
      ver.appendChild(document.createTextNode(' '));
      var ua = el('a', 'pill warn', 'update available: v' + d.update.version);
      ua.href = d.update.url; ua.target = '_blank'; ua.rel = 'noopener noreferrer';
      ver.appendChild(ua);
    }
    row(dl, 'Version', ver);
    row(dl, 'Web listener', d.http_listen);
    row(dl, 'Data folder', d.data_folder || 'not set (nothing can be saved from here yet)');
    info.appendChild(dl);
    box.appendChild(info);

    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Restart'));
    if (d.can_restart) {
      var line = el('div', 'buttons'); line.appendChild(restartButton()); card.appendChild(line);
      if (confirming === 'restart') card.appendChild(confirmBox('restart', 'Restart the bridge now?',
        'The dashboard and the panel are offline for a few seconds.', 'Yes, restart', restartBridge));
      card.appendChild(el('p', 'muted', 'The bridge says goodbye to the broker and exits; the container or service manager has to start it again '
        + '(Docker with restart: unless-stopped or on-failure, as in the compose files in deploy/; systemd with Restart=on-failure). '
        + 'Without a restart policy it stays stopped and has to be started by hand. This page cannot restart the Docker host or pull a new image.'));
    } else {
      var off = el('button', null, 'Restart bridge'); off.disabled = true; off.type = 'button';
      var line2 = el('div', 'buttons'); line2.appendChild(off); card.appendChild(line2);
      var p = el('p', 'muted');
      p.appendChild(document.createTextNode('Restarting from this page is off. To turn it on, put '));
      p.appendChild(el('code', null, 'allow_restart = true'));
      p.appendChild(document.createTextNode(' under '));
      p.appendChild(el('code', null, '[setup]'));
      p.appendChild(document.createTextNode(' in bridge.toml and restart once by hand (for Docker: docker compose restart deskwatch-bridge). It is off by default because it stops the bridge.'));
      card.appendChild(p);
    }
    box.appendChild(card);

    var chk = el('section', 'card');
    chk.appendChild(el('h2', null, 'Update check'));
    var check = checkbox(d.updates.check, !d.updates.editable);
    chk.appendChild(settingRow('Tell me about new versions', 'Once a day the bridge asks GitHub for the latest release number. Nothing about this install is sent. Turn off for a server without internet', check));
    if (d.updates.editable) {
      var usave = el('button', null, 'Save');
      usave.type = 'button';
      usave.addEventListener('click', function () {
        post('api/setup/updates', { check: check.checked }).then(function (j) { notice = (j && j.error) || null; load(); });
      });
      var uline = el('div', 'buttons'); uline.appendChild(usave); chk.appendChild(uline);
    }
    chk.appendChild(el('p', 'muted', d.updates.editable ? 'Needs a restart of the bridge.'
      : 'Give the bridge a data folder to change this here, or set check = false under [updates] in bridge.toml.'));
    box.appendChild(chk);
    return box;
  }

  function secretsSection(d) {
    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Stored secrets'));
    if (!d.can_edit) {
      card.appendChild(el('p', 'muted', 'Without a data folder nothing is stored by the bridge.'));
      return card;
    }
    card.appendChild(el('p', 'muted', 'Files in the data folder\'s credentials folder. Values are never shown. A secret that is in use goes away when you remove its source or login; one that nothing uses can be deleted here.'));
    if (!d.secrets.length) card.appendChild(el('p', 'muted', 'Nothing is stored.'));
    d.secrets.forEach(function (s) {
      var line = el('div', 'secret');
      var left = el('span');
      left.appendChild(document.createTextNode(s.name));
      left.appendChild(el('br'));
      left.appendChild(el('small', null, s.used_by ? 'used by ' + s.used_by : 'not used by anything'));
      line.appendChild(left);
      if (s.used_by) {
        line.appendChild(el('small', null, 'in use'));
      } else {
        var del = el('button', 'secondary danger', 'Delete');
        del.type = 'button'; del.addEventListener('click', function () { ask('secret:' + s.name); });
        line.appendChild(del);
      }
      card.appendChild(line);
      if (confirming === 'secret:' + s.name) card.appendChild(confirmBox('secret:' + s.name, 'Delete ' + s.name + '?',
        'The file is deleted from the data folder. This cannot be undone.', 'Yes, delete',
        function () { change('api/setup/secret/delete', { name: s.name }); }));
    });
    return card;
  }

  function loadAccess() {
    return fetch('api/auth/status', { cache: 'no-store', credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (a) {
        access = a;
        var out = document.getElementById('signout');
        if (out) out.hidden = a.state !== 'signed_in';
        if (last) render(last);
      }, function () { /* the section says it cannot load */ });
  }

  function accessCall(url, body, done) {
    return post(url, body).then(function (j) {
      if (j.error) { accessNote = { error: j.error }; }
      else { accessNote = { ok: done || 'Saved.', key: j.key || null }; }
      return loadAccess();
    });
  }

  function labelled(text, input) {
    var l = el('label', 'field');
    l.appendChild(el('span', 'muted', text));
    l.appendChild(input);
    return l;
  }

  function input(type, name, auto) {
    var i = el('input'); i.type = type; i.name = name; i.autocomplete = auto || 'off';
    return i;
  }

  function accessForm(title, fields, button, onSubmit) {
    var f = el('form', 'accessform');
    if (title) f.appendChild(el('h3', null, title));
    fields.forEach(function (x) { f.appendChild(x); });
    var b = el('button', 'secondary', button); b.type = 'submit';
    f.appendChild(b);
    f.addEventListener('submit', function (ev) { ev.preventDefault(); onSubmit(f); });
    return f;
  }

  function accessSection() {
    var card = el('section', 'card');
    if (!access) { loadAccess(); card.appendChild(el('p', 'muted', 'Loading…')); return card; }
    if (accessNote) {
      var n = el('p', accessNote.error ? 'err' : 'banner', accessNote.error || accessNote.ok);
      n.setAttribute('role', 'status'); card.appendChild(n);
      if (accessNote.key) {
        card.appendChild(el('p', 'muted', 'Copy this key now. It is shown once and cannot be read again:'));
        var code = el('code', null, accessNote.key); code.style.wordBreak = 'break-all';
        card.appendChild(code);
      }
    }
    var min = access.min_password || 12;
    if (access.state === 'open') {
      card.appendChild(el('p', 'muted', 'The login needs a data folder to save the password (see docs/data-dir.md). Until then the page only answers the local network.'));
      return card;
    }
    if (access.state === 'first_run') {
      card.appendChild(el('p', 'muted', 'No password is set. The page answers the local network only. Set an administrator password to require a sign-in for the dashboard and this page.'));
      card.appendChild(accessForm(null, [
        labelled('Username', Object.assign(input('text', 'username', 'username'), { value: 'admin' })),
        labelled('New password (at least ' + min + ' characters)', input('password', 'password', 'new-password'))
      ], 'Set password and sign in', function (f) {
        accessNote = null;
        post('api/auth/setup', { username: f.username.value, password: f.password.value }).then(function (j) {
          if (j.error) { accessNote = { error: j.error }; } else { accessNote = { ok: 'The password is set. You are signed in.' }; }
          loadAccess();
        });
      }));
      return card;
    }
    var a = access.account;
    if (!a) { card.appendChild(el('p', 'muted', 'Only the administrator can change the access settings.')); return card; }
    var dl = el('dl');
    row(dl, 'Users', String(a.users.length));
    row(dl, 'Signed-in browsers', String(a.sessions));
    card.appendChild(dl);

    var ROLE_NAMES = { admin: 'Administrator', operator: 'Operator', viewer: 'Viewer' };
    card.appendChild(el('h3', null, 'Users'));
    card.appendChild(el('p', 'muted', 'A viewer can look at the dashboard. An operator can also dismiss alerts and failed runs. An administrator can do everything, including this page. A new user gets the password you choose and must replace it at the first sign-in.'));
    a.users.forEach(function (u) {
      var line = el('div', 'secret');
      var left = el('span'); left.appendChild(document.createTextNode(u.name));
      left.appendChild(el('br'));
      left.appendChild(el('small', null, (ROLE_NAMES[u.role] || u.role) + (u.must_change ? ', has to choose a new password' : '')));
      line.appendChild(left);
      var role = el('select'); role.setAttribute('aria-label', 'Role of ' + u.name);
      ['viewer', 'operator', 'admin'].forEach(function (r) {
        var op = el('option', null, ROLE_NAMES[r]); op.value = r; if (r === u.role) op.selected = true; role.appendChild(op);
      });
      role.addEventListener('change', function () {
        accessCall('api/auth/user/role', { name: u.name, role: role.value }, 'The role is changed.');
      });
      line.appendChild(role);
      var reset = el('button', 'secondary', 'New password'); reset.type = 'button';
      reset.addEventListener('click', function () { ask('userpw:' + u.name); });
      line.appendChild(reset);
      var del = el('button', 'secondary danger', 'Delete'); del.type = 'button';
      del.addEventListener('click', function () { ask('userdel:' + u.name); });
      line.appendChild(del);
      card.appendChild(line);
      if (confirming === 'userdel:' + u.name) card.appendChild(confirmBox('userdel:' + u.name, 'Remove ' + u.name + '?',
        'They are signed out right away.', 'Yes, remove',
        function () { accessCall('api/auth/user/delete', { name: u.name }, 'The user is removed.'); }));
      if (confirming === 'userpw:' + u.name) {
        card.appendChild(accessForm('New password for ' + u.name, [
          el('p', 'muted', 'They are signed out and must replace it at the next sign-in.'),
          labelled('Password (at least ' + min + ' characters)', input('password', 'password', 'new-password'))
        ], 'Set password', function (f) {
          confirming = null;
          accessCall('api/auth/user/password', { name: u.name, password: f.password.value }, 'The password is changed.');
        }));
      }
    });
    var newRole = el('select'); newRole.name = 'role';
    [['viewer', 'Viewer (the dashboard only)'], ['operator', 'Operator (also dismiss alerts and failed runs)'], ['admin', 'Administrator (everything)']].forEach(function (o) {
      var op = el('option', null, o[1]); op.value = o[0]; if (o[0] === 'operator') op.selected = true; newRole.appendChild(op);
    });
    card.appendChild(accessForm('New user', [
      labelled('User name', input('text', 'name', 'off')),
      labelled('Role', newRole),
      labelled('Password (at least ' + min + ' characters, they replace it at the first sign-in)', input('password', 'password', 'new-password'))
    ], 'Create user', function (f) {
      accessCall('api/auth/user', { name: f.name.value, role: f.role.value, password: f.password.value }, 'The user is created.');
    }));
    var me = el('p', 'muted'); var meLink = el('a', null, 'My account'); meLink.href = '/account';
    me.appendChild(document.createTextNode('To change your own password: ')); me.appendChild(meLink); me.appendChild(document.createTextNode('.'));
    card.appendChild(me);

    var out = el('button', 'secondary', 'Sign out everywhere'); out.type = 'button';
    out.addEventListener('click', function () {
      post('api/auth/logout-all', {}).then(function () { location.href = '/login'; });
    });
    card.appendChild(el('h3', null, 'Sessions'));
    card.appendChild(out);

    card.appendChild(el('h3', null, 'API keys'));
    card.appendChild(el('p', 'muted', 'For scripts and screens, so they never need a password. An "alert" key may send POST /api/alert (Authorization: Bearer <key>). A "viewer" key opens the dashboard once: https://host/?key=<key>.'));
    if (!a.keys.length) card.appendChild(el('p', 'muted', 'No keys yet.'));
    a.keys.forEach(function (k) {
      var line = el('div', 'secret');
      var left = el('span'); left.appendChild(document.createTextNode(k.name));
      left.appendChild(el('br')); left.appendChild(el('small', null, k.scope + ' key'));
      line.appendChild(left);
      var del = el('button', 'secondary danger', 'Delete'); del.type = 'button';
      del.addEventListener('click', function () { accessCall('api/auth/key/delete', { name: k.name }, 'The key is deleted.'); });
      line.appendChild(del);
      card.appendChild(line);
    });
    var scope = el('select'); scope.name = 'scope';
    [['alert', 'alert (send alerts)'], ['viewer', 'viewer (open the dashboard)']].forEach(function (o) {
      var op = el('option', null, o[1]); op.value = o[0]; scope.appendChild(op);
    });
    card.appendChild(accessForm('New key', [
      labelled('Name', input('text', 'name')), labelled('What it may do', scope)
    ], 'Create key', function (f) {
      accessCall('api/auth/key', { name: f.name.value, scope: f.scope.value }, 'The key is created.');
    }));
    return card;
  }

  function loadOauth() {
    return fetch('api/auth/oauth', { cache: 'no-store', credentials: 'same-origin' })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (o) { oauth = o || { unavailable: true }; if (last) render(last); },
            function () { /* shown as unavailable */ });
  }

  function lines(text) {
    return text.split('\n').map(function (x) { return x.trim(); }).filter(Boolean);
  }

  function textarea(name, rows, value) {
    var t = el('textarea'); t.name = name; t.rows = rows; t.value = (value || []).join('\n');
    t.spellcheck = false;
    return t;
  }

  function signinSection() {
    var card = el('section', 'card');
    if (!oauth) { loadOauth(); card.appendChild(el('p', 'muted', 'Loading…')); return card; }
    if (oauth.unavailable) {
      card.appendChild(el('p', 'muted', 'Only the administrator can change the sign-in providers, and a login password must be set first (see Access).'));
      return card;
    }
    if (oauthNote) {
      var n = el('p', oauthNote.error ? 'err' : 'banner', oauthNote.error || oauthNote.ok);
      n.setAttribute('role', 'status'); card.appendChild(n);
    }
    card.appendChild(el('p', 'muted', 'Let people sign in with GitHub or Microsoft. Nobody gets in by default: list who is allowed. A person who matches an administrator rule is an administrator, one who matches an operator rule can also dismiss alerts and failed runs, and anyone else who is allowed can only look. With several matches the highest role wins. The local administrator password always keeps working. The sign-in needs the public address below, over HTTPS.'));
    var g = oauth.github, m = oauth.microsoft;
    var url = input('text', 'public_url'); url.value = oauth.public_url || ''; url.placeholder = 'https://deskwatch.example.org';
    var gOn = el('input'); gOn.type = 'checkbox'; gOn.checked = !!g;
    var mOn = el('input'); mOn.type = 'checkbox'; mOn.checked = !!m;
    var f = el('form', 'accessform');
    f.style.maxWidth = '40rem';
    f.appendChild(labelled('Public address', url));

    function toggle(box, text) { var l = el('label'); l.appendChild(box); l.appendChild(document.createTextNode(' ' + text)); return l; }

    f.appendChild(el('h3', null, 'GitHub'));
    f.appendChild(toggle(gOn, 'Allow sign-in with GitHub'));
    f.appendChild(el('p', 'muted', 'On GitHub: Settings, Developer settings, OAuth Apps, New. Callback URL: ' + (oauth.redirect.github || '<public address>/auth/callback/github')));
    var gid = input('text', 'g_id'); gid.value = g ? g.client_id : '';
    var gsec = input('password', 'g_secret', 'new-password'); gsec.placeholder = g && g.secret_set ? '(set, leave empty to keep)' : '';
    f.appendChild(labelled('Client ID', gid));
    f.appendChild(labelled('Client secret (never shown again)', gsec));
    var gUsers = textarea('g_users', 3, g && g.users), gOrgs = textarea('g_orgs', 2, g && g.orgs), gTeams = textarea('g_teams', 3, g && g.teams);
    var gOU = textarea('g_ou', 2, g && g.operator_users), gOT = textarea('g_ot', 2, g && g.operator_teams);
    var gAU = textarea('g_au', 2, g && g.admin_users), gAT = textarea('g_at', 2, g && g.admin_teams);
    f.appendChild(labelled('Allowed users (GitHub user names, one per line)', gUsers));
    f.appendChild(labelled('Allowed organisations (one per line)', gOrgs));
    f.appendChild(labelled('Allowed teams (organisation/team, one per line)', gTeams));
    f.appendChild(labelled('Operators: users', gOU));
    f.appendChild(labelled('Operators: teams (organisation/team)', gOT));
    f.appendChild(labelled('Administrators: users', gAU));
    f.appendChild(labelled('Administrators: teams (organisation/team)', gAT));
    f.appendChild(el('p', 'muted', 'Organisations and teams are only visible if the organisation approves this OAuth app (GitHub, Organization settings, Third-party access). Personal users always work.'));

    f.appendChild(el('h3', null, 'Microsoft'));
    f.appendChild(toggle(mOn, 'Allow sign-in with Microsoft'));
    f.appendChild(el('p', 'muted', 'In Microsoft Entra: App registrations, New, Web, redirect URI: ' + (oauth.redirect.microsoft || '<public address>/auth/callback/microsoft') + '. Add a client secret. The permission User.Read (the default) is enough.'));
    var mt = input('text', 'm_tenant'); mt.value = m ? m.tenant : '';
    var mid = input('text', 'm_id'); mid.value = m ? m.client_id : '';
    var msec = input('password', 'm_secret', 'new-password'); msec.placeholder = m && m.secret_set ? '(set, leave empty to keep)' : '';
    var mg = textarea('m_groups', 3, m && m.groups), mag = textarea('m_admin', 2, m && m.admin_groups);
    var mog = textarea('m_op', 2, m && m.operator_groups);
    f.appendChild(labelled('Directory (tenant) ID', mt));
    f.appendChild(labelled('Application (client) ID', mid));
    f.appendChild(labelled('Client secret (never shown again)', msec));
    f.appendChild(labelled('Allowed groups (Object Id of each group, one per line)', mg));
    f.appendChild(labelled('Operator groups (Object Id)', mog));
    f.appendChild(labelled('Administrator groups (Object Id)', mag));

    var b = el('button', null, 'Save'); b.type = 'submit'; f.appendChild(b);
    f.addEventListener('submit', function (ev) {
      ev.preventDefault();
      var body = { public_url: url.value.trim() || null, github: null, microsoft: null };
      if (gOn.checked) body.github = { client_id: gid.value, secret: gsec.value || null, users: lines(gUsers.value),
        orgs: lines(gOrgs.value), teams: lines(gTeams.value), operator_users: lines(gOU.value), operator_teams: lines(gOT.value), admin_users: lines(gAU.value), admin_teams: lines(gAT.value) };
      if (mOn.checked) body.microsoft = { tenant: mt.value, client_id: mid.value, secret: msec.value || null,
        groups: lines(mg.value), operator_groups: lines(mog.value), admin_groups: lines(mag.value) };
      b.disabled = true;
      post('api/auth/oauth', body).then(function (j) {
        oauthNote = j.error ? { error: j.error } : { ok: 'Saved. The sign-in buttons are on the login page now.' };
        b.disabled = false;
        return loadOauth();
      });
    });
    card.appendChild(f);
    return card;
  }

  var TITLES = { sources: 'Sources', mqtt: 'MQTT broker', dashboard: 'Dashboard', alerts: 'Alerts', access: 'Access', signin: 'Sign-in providers', bridge: 'Bridge and restart', secrets: 'Stored secrets' };

  function currentSection() {
    var id = (location.hash || '').replace('#', '');
    return SECTIONS.indexOf(id) >= 0 ? id : 'sources';
  }

  function render(d) {
    last = d;
    document.getElementById('build').textContent = 'version ' + d.build;
    document.getElementById('dashboard-link').hidden = !d.kiosk;
    app.textContent = '';
    var section = currentSection();

    var banner = null;
    if (restarting) {
      var waited = Date.now() - restarting.since;
      banner = el('p', 'banner', waited < 60000 ? 'Restarting the bridge… this page continues by itself when it answers again.'
        : 'The bridge has not come back after a minute. Nothing started it again: the container probably has no restart policy. Start it by hand (docker compose up -d).');
    } else if (d.restart_needed || notice) {
      banner = el('p', 'banner', notice || 'Changes are saved. Restart the bridge to apply them.');
      if (d.restart_needed && d.can_restart && !notice) {
        banner.appendChild(document.createTextNode(' '));
        banner.appendChild(restartButton('Restart now'));
      } else if (d.restart_needed && !d.can_restart && !notice) {
        banner.appendChild(document.createTextNode(' (Restart it yourself, or see Bridge and restart.)'));
      }
    }
    if (banner) { banner.setAttribute('role', 'status'); app.appendChild(banner); }
    if (confirming === 'restart' && section !== 'bridge') {
      app.appendChild(confirmBox('restart', 'Restart the bridge now?', 'The dashboard and the panel are offline for a few seconds.', 'Yes, restart', restartBridge));
    }

    var layout = el('div', 'sidebar');
    var nav = el('nav', 'side');
    nav.setAttribute('aria-label', 'sections');
    SECTIONS.forEach(function (id) {
      var a = el('a', id === section ? 'on' : null, TITLES[id]);
      a.href = '#' + id;
      var count = id === 'sources' ? d.sources.length : id === 'secrets' && d.can_edit ? d.secrets.length : null;
      if (count != null) a.appendChild(el('span', 'n', String(count)));
      if (id === section) a.setAttribute('aria-current', 'page');
      nav.appendChild(a);
    });
    layout.appendChild(nav);
    var content = el('div', 'content');
    content.appendChild(el('h2', 'sectiontitle', TITLES[section]));
    content.appendChild(({ sources: sourcesSection, mqtt: mqttSection, dashboard: dashboardSection, alerts: alertsSection,
                           access: accessSection, signin: signinSection, bridge: bridgeSection, secrets: secretsSection })[section](d));
    layout.appendChild(content);
    app.appendChild(layout);
  }

  window.addEventListener('hashchange', function () { form = null; confirming = null; if (last) render(last); });

  function load() {
    clearTimeout(timer);
    // A refresh would redraw (and lose the focus of) a form being filled in.
    var typing = document.activeElement && app.contains(document.activeElement) && document.activeElement.tagName === 'INPUT';
    if ((typing || form) && !(form && form.busy) && !restarting) { timer = setTimeout(load, REFRESH_MS); return Promise.resolve(); }
    return fetch('api/setup/status', { cache: 'no-store', credentials: 'same-origin' }).then(function (r) {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json().then(function (d) {
        if (restarting && d.boot === restarting.boot) {
          // Still the old process: it has not exited yet.
          last = d; render(d); timer = setTimeout(load, 2000); return;
        }
        if (restarting) { restarting = null; notice = null; }
        render(d); timer = setTimeout(load, REFRESH_MS);
      });
    }).catch(function (e) {
      if (restarting) {
        // Expected while the bridge is down; keep asking.
        if (last) render(last);
        timer = setTimeout(load, 2000);
        return;
      }
      app.textContent = '';
      app.appendChild(el('p', 'err', 'Cannot load the setup: ' + e.message));
      timer = setTimeout(load, REFRESH_MS);
    });
  }

  var signout = document.getElementById('signout');
  signout.addEventListener('click', function () {
    post('api/auth/logout', {}).then(function () { location.href = '/login'; });
  });
  loadAccess();
  load();
})();
