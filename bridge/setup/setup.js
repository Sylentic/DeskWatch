// DeskWatch setup page. Shows how the bridge is configured, tests a connection
// (the bridge makes the requests, with the real credentials) and, when the
// bridge has a data folder, adds, changes and removes sources. Secrets are
// never sent to the browser: a credential is only reported as present or
// missing, and a token typed here goes to the bridge and is never shown again. Everything is built with textContent, so
// nothing from the config can inject markup.
(function () {
  'use strict';

  var app = document.getElementById('app');
  var REFRESH_MS = 5000;
  var timer = null;
  var last = null;       // latest status, so a finished test can redraw at once
  var results = {};      // source id -> latest test report (kept across refreshes)
  var running = {};      // source id -> true while its test runs
  var notice = null;     // message after the last save that could not be applied
  var form = null;       // the open add/edit form, or null (see openForm)

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
  var KIND = { gitea: 'Gitea', github: 'GitHub', azure_devops: 'Azure DevOps', prometheus: 'Prometheus' };

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

  function removeSource(s) {
    if (!window.confirm('Remove ' + s.name + ' and the tokens saved for it?')) return;
    post('api/setup/source/delete', { kind: s.kind, name: s.name }).then(function (j) {
      if (!j) return;
      if (j.error) window.alert(j.error);
      else notice = applyNotice(j);
      load();
    });
  }

  // Ask the bridge to restart itself (only offered when [setup] allow_restart
  // is on), then wait for it to answer again.
  function restartBridge() {
    if (!window.confirm('Restart the bridge now? The dashboard and panel are offline for a few seconds.')) return;
    post('api/setup/restart', {}).then(function (j) {
      if (!j || j.error) { window.alert(j && j.error || 'The restart failed.'); return; }
      clearTimeout(timer);
      app.textContent = '';
      app.appendChild(el('p', 'banner', 'Restarting the bridge…'));
      timer = setTimeout(load, 4000);
    });
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

  function addCard() {
    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Add a source'));
    var kind = el('select');
    Object.keys(FORMS).forEach(function (k) { var o = el('option', null, KIND[k]); o.value = k; kind.appendChild(o); });
    var go = el('button', null, 'Add');
    go.type = 'button'; go.addEventListener('click', function () { openForm(kind.value, null); });
    var line = el('div', 'buttons');
    line.appendChild(kind); line.appendChild(go);
    card.appendChild(line);
    return card;
  }

  function sourceCard(s) {
    var card = el('section', 'card');
    var h = el('h2', null, (KIND[s.kind] || s.kind) + ': ' + s.name + ' ');
    var hs = HEALTH[s.health] || [s.health, 'warn'];
    h.appendChild(pill(hs[0], hs[1]));
    card.appendChild(h);
    var dl = el('dl');
    row(dl, 'Address', s.address);
    if (s.repos && s.repos.length) row(dl, s.kind === 'prometheus' ? 'Hosts' : s.kind === 'azure_devops' ? 'Projects' : 'Repositories', s.repos.join(', '));
    row(dl, 'Defined in', s.origin === 'data_folder' ? 'data folder (sources.toml)' : 'bridge.toml');
    s.credentials.forEach(function (c) {
      var v = el('span');
      v.appendChild(credState(c));
      v.appendChild(document.createTextNode(' ' + c.name));
      row(dl, c.label, v);
    });
    card.appendChild(dl);
    if (!s.running) card.appendChild(el('p', 'muted', 'Saved, but not running. Fix the details and save again, or restart the bridge.'));
    card.appendChild(testBlock(s.id));
    if (s.editable) {
      var b = el('div', 'buttons');
      var edit = el('button', 'secondary', 'Edit');
      edit.type = 'button'; edit.addEventListener('click', function () { openForm(s.kind, s); });
      var rm = el('button', 'secondary danger', 'Remove');
      rm.type = 'button'; rm.addEventListener('click', function () { removeSource(s); });
      b.appendChild(edit); b.appendChild(rm);
      card.appendChild(b);
    }
    return card;
  }

  // How the dashboard's built-in layout shows the hosts. Applies after a restart.
  function hostViewCard(d) {
    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Dashboard hosts'));
    var choice = el('select');
    [['detailed', 'Detailed: one host in detail on top, every host as a list below (tap to switch)'],
     ['compact', 'Compact: only the list of hosts, with network rates']].forEach(function (o) {
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
      save.addEventListener('click', function () {
        post('api/setup/kiosk', { host_view: choice.value }).then(function (j) {
          if (j && j.error) window.alert(j.error);
          load();
        });
      });
      var line = el('div', 'buttons'); line.appendChild(save); card.appendChild(line);
    }
    card.appendChild(el('p', 'muted', 'Only for the built-in layout; panels set in bridge.toml stay as they are. '
      + (d.host_view_editable ? 'Needs a restart of the bridge.' : 'Give the bridge a data folder to change it here.')));
    return card;
  }

  function render(d) {
    last = d;
    document.getElementById('build').textContent = 'version ' + d.build;
    document.getElementById('dashboard-link').hidden = !d.kiosk;
    app.textContent = '';
    var info = el('section', 'card');
    info.appendChild(el('h2', null, 'Bridge'));
    var dl = el('dl');
    row(dl, 'Web listener', d.http_listen);
    row(dl, 'Data folder', d.data_folder || 'not set (nothing can be saved from here yet)');
    var m = d.mqtt;
    if (m.enabled) {
      var link = el('span');
      link.appendChild(pill(m.connected ? 'connected' : 'not connected', m.connected ? 'ok' : 'bad'));
      link.appendChild(document.createTextNode(' ' + m.host + ':' + m.port + (m.tls ? ' (TLS)' : '')));
      row(dl, 'MQTT broker', link);
      row(dl, 'MQTT login', (m.username || 'anonymous') + (m.username ? ', password ' + (CRED[m.password] || [m.password])[0] : ''));
    } else {
      row(dl, 'MQTT broker', 'off');
    }
    row(dl, 'MQTT settings from', m.origin === 'data_folder' ? 'data folder (changed here)' : 'bridge.toml');
    info.appendChild(dl);
    if (m.enabled) info.appendChild(testBlock('mqtt'));
    if (m.editable && !form) {
      var change = el('button', 'secondary', 'Change broker settings');
      change.type = 'button';
      change.addEventListener('click', function () { openForm('mqtt', { name: 'mqtt', fields: m }); });
      var line = el('div', 'buttons'); line.appendChild(change); info.appendChild(line);
    }
    app.appendChild(info);
    if (d.kiosk) app.appendChild(hostViewCard(d));
    if (form && form.kind === 'mqtt') app.appendChild(formCard());

    if (d.restart_needed || notice) {
      var banner = el('p', 'banner', notice || 'Changes are saved. Restart the bridge to apply them.');
      banner.setAttribute('role', 'status');
      if (d.restart_needed && d.can_restart) {
        var again = el('button', null, 'Restart now');
        again.type = 'button';
        again.addEventListener('click', restartBridge);
        banner.appendChild(document.createTextNode(' '));
        banner.appendChild(again);
      }
      app.insertBefore(banner, app.firstChild);
    }
    app.appendChild(el('h2', null, 'Sources'));
    if (!d.sources.length) app.appendChild(el('p', 'muted', 'No sources configured.'));
    d.sources.forEach(function (s) {
      var editing = form && form.mode === 'edit' && form.kind === s.kind && form.name === s.name;
      app.appendChild(editing ? formCard() : sourceCard(s));
    });
    if (d.can_edit) app.appendChild(form && form.mode === 'add' ? formCard() : addCard());
    app.appendChild(el('p', 'muted', d.can_edit
      ? 'Tokens are never sent to the browser. Sources from bridge.toml are changed in that file; the ones added here live in the data folder and start, change or stop right away.'
      : 'Tokens are never sent to the browser. To add and change sources here, give the bridge a data folder ([data] dir, see docs/data-dir.md).'));
  }

  function load() {
    clearTimeout(timer);
    // A refresh would redraw (and lose the focus of) a form being filled in.
    if (form && !form.busy) { timer = setTimeout(load, REFRESH_MS); return Promise.resolve(); }
    return fetch('api/setup/status', { cache: 'no-store', credentials: 'same-origin' }).then(function (r) {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json().then(function (d) { render(d); timer = setTimeout(load, REFRESH_MS); });
    }).catch(function (e) {
      app.textContent = '';
      app.appendChild(el('p', 'err', 'Cannot load the setup: ' + e.message));
      timer = setTimeout(load, REFRESH_MS);
    });
  }

  load();
})();
