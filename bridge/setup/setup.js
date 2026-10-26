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
    var tip = name === 'light' ? 'Dark mode' : 'Light mode';
    themeButton.setAttribute('data-tip', tip);
    themeButton.setAttribute('aria-label', tip);
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
  var SECTIONS = ['sources', 'mqtt', 'dashboard', 'alerts', 'access', 'signin', 'backup', 'bridge', 'secrets'];
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

  // Small line icons for buttons, as the shapes they are drawn from (the page
  // never parses markup, see the note at the top).
  var ICONS = {
    close: [['path', { d: 'M6 6l12 12M18 6L6 18' }]],
    plus: [['path', { d: 'M12 5v14M5 12h14' }]],
    edit: [['path', { d: 'M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16v4z' }], ['path', { d: 'M13.5 6.5l4 4' }]],
    trash: [['path', { d: 'M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3' }]],
    key: [['circle', { cx: '8', cy: '15', r: '4' }], ['path', { d: 'M11 12l9-9M17 6l3 3M15 8l2 2' }]],
    power: [['path', { d: 'M12 3v9' }], ['path', { d: 'M6.3 7.3a8 8 0 1 0 11.4 0' }]]
  };
  function svg(name) {
    var NS = 'http://www.w3.org/2000/svg';
    var s = document.createElementNS(NS, 'svg');
    s.setAttribute('viewBox', '0 0 24 24'); s.setAttribute('fill', 'none'); s.setAttribute('stroke', 'currentColor');
    s.setAttribute('stroke-width', '1.7'); s.setAttribute('stroke-linecap', 'round'); s.setAttribute('stroke-linejoin', 'round');
    s.setAttribute('aria-hidden', 'true');
    ICONS[name].forEach(function (shape) {
      var e = document.createElementNS(NS, shape[0]);
      Object.keys(shape[1]).forEach(function (k) { e.setAttribute(k, shape[1][k]); });
      s.appendChild(e);
    });
    return s;
  }
  // A square button with only an icon: its name is in the tooltip and in aria-label.
  function iconButton(name, tip, cls) {
    var b = el('button', 'icon-btn' + (cls ? ' ' + cls : ''));
    b.type = 'button';
    b.setAttribute('data-tip', tip); b.setAttribute('aria-label', tip);
    b.appendChild(svg(name));
    return b;
  }
  // A text button with an icon in front, such as "+ New user".
  function addButton(text) {
    var b = el('button', 'add'); b.type = 'button';
    b.appendChild(svg('plus')); b.appendChild(document.createTextNode(text));
    return b;
  }

  // ---- The pop-up ----
  // One <dialog> outside #app, so the redraw every few seconds leaves it alone.
  // Every pop-up is a form: Enter sends it, Esc, the X and Cancel close it.
  var modal = document.getElementById('modal');
  var onModalClose = null;
  function openModal(title, wide) {
    modal.textContent = '';
    modal.className = 'modal' + (wide ? ' wide' : '');
    var f = el('form', 'modal-card');
    f.noValidate = true;
    var head = el('div', 'modal-head');
    var h = el('h2', null, title); h.id = 'modal-title';
    var x = iconButton('close', 'Close');
    x.addEventListener('click', closeModal);
    head.appendChild(h); head.appendChild(x);
    var body = el('div', 'modal-body');
    var foot = el('div', 'modal-foot');
    f.appendChild(head); f.appendChild(body); f.appendChild(foot);
    modal.appendChild(f);
    if (!modal.open) modal.showModal();
    return { form: f, body: body, foot: foot };
  }
  function closeModal() { if (modal.open) modal.close(); }
  modal.addEventListener('close', function () {
    modal.textContent = '';
    var f = onModalClose; onModalClose = null;
    if (f) f();
  });
  // Cancel and the main button at the bottom of a pop-up.
  function modalButtons(m, text, cls) {
    var cancel = el('button', 'secondary', 'Cancel'); cancel.type = 'button';
    cancel.addEventListener('click', closeModal);
    var ok = el('button', cls || null, text); ok.type = 'submit';
    m.foot.appendChild(cancel); m.foot.appendChild(ok);
    return ok;
  }
  function modalError(m, text) {
    var old = m.body.querySelector('.err.modal-err'); if (old) old.remove();
    var e = el('p', 'err modal-err', text); e.setAttribute('role', 'alert');
    m.body.appendChild(e);
  }
  // A question with Cancel and a red button, for anything that removes or stops.
  function confirmModal(question, detail, yes, onYes) {
    var m = openModal(question);
    if (detail) m.body.appendChild(el('p', 'muted', detail));
    modalButtons(m, yes, 'danger solid');
    m.form.addEventListener('submit', function (ev) { ev.preventDefault(); closeModal(); onYes(); });
  }

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
      secrets: [],
      auth: [
        ['token', 'Access token', 'A fine-grained token. Acts as the person who made it.',
         { strings: [], secrets: [['token', 'Token (read-only, fine-grained)', true]] }],
        ['app', 'GitHub App', 'An app installed on your organisation. Not tied to a person.',
         { strings: [['app_id', 'App id', ''], ['installation_id', 'Installation id', '']],
           secrets: [['private_key', 'Private key (the .pem file, pasted)', true]], multiline: ['private_key'] }]
      ]
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
      secrets: [],
      auth: [
        ['pat', 'Personal access token', 'Acts as the person who made the token and stops working when they leave.',
         { strings: [], secrets: [['token', 'Personal access token (read-only)', true]] }],
        ['entra', 'Entra service principal', 'An app registration that is not tied to a person. Needs a Basic licence in the organisation.',
         { strings: [['tenant_id', 'Tenant (directory) id', ''], ['client_id', 'Application (client) id', '']],
           secrets: [['client_secret', 'Client secret', true]] }]
      ]
    },
    prometheus: {
      strings: [['url', 'Prometheus address', 'http://prometheus.example.lan:9090']],
      lists: [['hosts', 'Hosts, one per line (the node_exporter instance, such as node-exporter:9100)']],
      secrets: [['token', 'Bearer token (only behind a proxy)', false]]
    }
  };

  // Every Git host can name the Firefox container its links open in.
  Object.keys(FORMS).forEach(function (k) {
    if (k !== 'prometheus') FORMS[k].strings.push(['container', 'Firefox container for links (optional)', '']);
  });

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

  // Kinds with a choice of sign-in method (`auth`) have their extra fields per method.
  function modeOf(spec, values) {
    var m = (spec.auth || []).filter(function (a) { return a[0] === values.auth; })[0];
    return m ? m[3] : { strings: [], secrets: [] };
  }

  function openForm(kind, source) {
    var spec = FORMS[kind], values = {}, f = source && source.fields || {};
    if (spec.auth) {
      values.auth = f.auth || spec.auth[0][0];
      spec.auth.forEach(function (a) { a[3].strings.forEach(function (x) { values[x[0]] = f[x[0]] != null ? String(f[x[0]]) : x[2]; }); });
    }
    spec.strings.forEach(function (x) { values[x[0]] = f[x[0]] != null ? String(f[x[0]]) : x[2]; });
    spec.lists.forEach(function (x) { values[x[0]] = (f[x[0]] || []).join('\n'); });
    (spec.bools || []).forEach(function (x) { values[x[0]] = !!f[x[0]]; });
    if (kind === 'mqtt') values.username = f.username || '';
    form = { kind: kind, mode: source ? 'edit' : 'add', name: source ? source.name : '', values: values,
             secrets: {}, advanced: !!f.hosts_advanced, busy: false, result: null, error: null };
    drawForm();
  }

  function submitForm(force) {
    var spec = FORMS[form.kind], fields = {}, secrets = {};
    spec.strings.forEach(function (x) { fields[x[0]] = form.values[x[0]]; });
    var mode = modeOf(spec, form.values);
    if (spec.auth) fields.auth = form.values.auth;
    mode.strings.forEach(function (x) { fields[x[0]] = form.values[x[0]]; });
    mode.secrets.forEach(function (x) { if (form.secrets[x[0]]) secrets[x[0]] = form.secrets[x[0]]; });
    spec.lists.forEach(function (x) {
      if (x[0] === 'hosts' && form.advanced) return;     // advanced host settings stay as they are
      fields[x[0]] = form.values[x[0]].split('\n').map(function (l) { return l.trim(); }).filter(Boolean);
    });
    (spec.bools || []).forEach(function (x) { fields[x[0]] = !!form.values[x[0]]; });
    spec.secrets.forEach(function (x) { if (form.secrets[x[0]]) secrets[x[0]] = form.secrets[x[0]]; });
    notice = null;
    form.busy = true; form.error = null; if (!force) form.result = null;
    drawForm();
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
      else if (j.saved) { results[j.id] = j.report; notice = isMqtt ? null : applyNotice(j); closeModal(); load(); return; }
      else form.result = j.report;
      drawForm();
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

  // The add or change form of a source (or of the broker), in the pop-up. It is
  // drawn again when its state changes (testing, a failed test).
  function drawForm() {
    var spec = FORMS[form.kind];
    var isMqtt = form.kind === 'mqtt';
    var m = openModal(isMqtt ? 'Change the MQTT broker settings'
      : (form.mode === 'add' ? 'Add ' : 'Change ') + (KIND[form.kind] || form.kind) + ' source', true);
    onModalClose = function () { form = null; };
    var card = m.body;
    var box = el('div', 'form');
    if (!isMqtt) {
      var name = el('input'); name.value = form.name; name.readOnly = form.mode === 'edit';
      if (form.mode === 'add') name.autofocus = true;
      name.addEventListener('input', function () { form.name = name.value; });
      box.appendChild(field('Name (letters, digits, - and _)', name));
    }
    var mode = modeOf(spec, form.values);
    function textField(x) {
      var i = el('input'); i.value = form.values[x[0]];
      i.addEventListener('input', function () { form.values[x[0]] = i.value; });
      return field(x[1], i);
    }
    function secretField(x) {
      var multi = (mode.multiline || []).indexOf(x[0]) >= 0;
      var i = el(multi ? 'textarea' : 'input');
      if (multi) { i.rows = 4; i.className = 'secret-area'; } else i.type = 'password';
      i.autocomplete = 'new-password'; i.spellcheck = false;
      i.placeholder = form.mode === 'edit' ? 'leave empty to keep the current one' : (x[2] ? 'required' : 'optional');
      i.addEventListener('input', function () { form.secrets[x[0]] = i.value; });
      return field(x[1], i);
    }
    var authBox = null;
    if (spec.auth) {
      var pick = el('select');
      spec.auth.forEach(function (a) {
        var op = el('option', null, a[1]); op.value = a[0]; if (a[0] === form.values.auth) op.selected = true; pick.appendChild(op);
      });
      pick.addEventListener('change', function () { form.values.auth = pick.value; form.result = null; form.error = null; drawForm(); });
      authBox = el('div', 'authbox');
      authBox.appendChild(field('Sign in with', pick));
      var cur = spec.auth.filter(function (a) { return a[0] === form.values.auth; })[0];
      authBox.appendChild(el('p', 'muted authhelp', cur[2]));
      mode.strings.forEach(function (x) { authBox.appendChild(textField(x)); });
      mode.secrets.forEach(function (x) { authBox.appendChild(secretField(x)); });
    }
    spec.strings.forEach(function (x) {
      if (x[0] === 'container' && authBox) { box.appendChild(authBox); authBox = null; }
      var i = el('input'); i.value = form.values[x[0]];
      i.addEventListener('input', function () { form.values[x[0]] = i.value; });
      var lab = field(x[1], i);
      if (x[0] === 'container') {
        var help = el('span', 'muted');
        help.appendChild(document.createTextNode('Firefox only. The links of this source type open in that container. Needs the add-on '));
        var a = el('a', null, 'Open external links in a container');
        a.href = 'https://addons.mozilla.org/firefox/addon/open-url-in-container/';
        a.target = '_blank'; a.rel = 'noopener noreferrer';
        help.appendChild(a);
        help.appendChild(document.createTextNode('. Leave empty for normal links.'));
        lab.appendChild(help);
      }
      box.appendChild(lab);
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
    if (form.result && !form.result.ok) {
      var force = el('button', 'secondary', 'Save anyway');
      force.type = 'button'; force.addEventListener('click', function () { submitForm(true); });
      m.foot.appendChild(force);
    }
    var save = modalButtons(m, form.busy ? 'Testing…' : 'Test and save');
    save.disabled = form.busy;
    m.form.addEventListener('submit', function (ev) { ev.preventDefault(); if (!form.busy) submitForm(false); });
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
      list.scrollIntoView({ block: 'nearest' });
    }
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

  // The first step of adding a source: pick its kind.
  function addSource() {
    var m = openModal('Add a source', true);
    m.body.appendChild(el('p', 'muted', 'What should DeskWatch watch? A new source starts working right away, no restart needed.'));
    var grid = el('div', 'kinds');
    Object.keys(FORMS).forEach(function (k) {
      if (k === 'mqtt') return;
      var b = el('button', 'kind'); b.type = 'button';
      b.appendChild(el('span', 'mark ' + k, MARK[k]));
      var t = el('span'); t.appendChild(el('b', null, KIND[k])); t.appendChild(el('small', null, KIND_HINT[k]));
      b.appendChild(t);
      b.addEventListener('click', function () { openForm(k, null); });
      grid.appendChild(b);
    });
    m.body.appendChild(grid);
    var cancel = el('button', 'secondary', 'Cancel'); cancel.type = 'button';
    cancel.addEventListener('click', closeModal);
    m.foot.appendChild(cancel);
  }

  // The same two-letter mark as on the dashboard.
  var MARK = { gitea: 'Gt', github: 'GH', gitlab: 'GL', bitbucket: 'BB', azure_devops: 'AZ', prometheus: 'PM' };
  var KIND_HINT = { github: 'Pull requests and Actions runs', gitlab: 'Merge requests and pipelines', bitbucket: 'Pull requests and pipelines',
    gitea: 'Pull requests and Actions runs, by webhook', azure_devops: 'Pull requests, pipelines and agents', prometheus: 'Host stats from node_exporter' };

  function sourceCard(s) {
    var card = el('section', 'card' + (s.hidden ? ' off' : ''));
    var head = el('div', 'cardhead');
    var h = el('h2');
    if (MARK[s.kind]) h.appendChild(el('span', 'mark ' + s.kind, MARK[s.kind]));
    h.appendChild(document.createTextNode((KIND[s.kind] || s.kind) + ': ' + s.name + ' '));
    var hs = HEALTH[s.health] || (s.hidden ? ['switched off', 'muted'] : [s.health, 'warn']);
    h.appendChild(pill(hs[0], hs[1]));
    head.appendChild(h);
    var acts = el('div', 'rowacts');
    head.appendChild(acts);
    card.appendChild(head);
    var dl = el('dl');
    row(dl, 'Address', s.address);
    if (s.repos && s.repos.length) row(dl, s.kind === 'prometheus' ? 'Hosts' : (s.kind === 'azure_devops' || s.kind === 'gitlab') ? 'Projects' : 'Repositories', s.repos.join(', '));
    if (s.fields && s.fields.container) row(dl, 'Firefox container', s.fields.container);
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
    if (s.editable) {
      var edit = iconButton('edit', 'Change');
      edit.addEventListener('click', function () { openForm(s.kind, s); });
      var rm = iconButton('trash', 'Remove', 'danger');
      rm.addEventListener('click', function () {
        confirmModal('Remove ' + s.name + '?', 'The source stops and its tokens saved in the data folder are deleted. This cannot be undone.',
          'Yes, remove', function () { removeSource(s); });
      });
      acts.appendChild(edit); acts.appendChild(rm);
    } else if (s.hideable) {
      if (s.hidden) {
        var on = el('button', 'secondary', 'Switch on again'); on.type = 'button';
        on.addEventListener('click', function () { change('api/setup/source/hide', { kind: s.kind, name: s.name, hidden: false }); });
        acts.appendChild(on);
      } else {
        var off = iconButton('power', 'Switch off here', 'danger');
        off.addEventListener('click', function () {
          confirmModal('Switch off ' + s.name + '?', 'It stops and leaves the dashboard. Its token file belongs to bridge.toml and stays. You can switch it on again here.',
            'Yes, switch off', function () { change('api/setup/source/hide', { kind: s.kind, name: s.name, hidden: true }); });
        });
        acts.appendChild(off);
        card.appendChild(el('p', 'muted', 'Defined in bridge.toml, which this page never edits. Switching it off stops it here; to delete it for good, remove its block from bridge.toml.'));
      }
    } else if (!s.hidden) {
      card.appendChild(el('p', 'muted', 'This source is defined in bridge.toml. Give the bridge a data folder to switch it off from here; to delete it, remove its block from bridge.toml.'));
    }
    return card;
  }

  function sourcesSection(d) {
    var box = el('div');
    if (!d.can_edit) box.appendChild(el('p', 'muted', 'To add and change sources here, give the bridge a data folder ([data] dir, see docs/data-dir.md).'));
    if (!d.sources.length) box.appendChild(el('p', 'muted', 'No sources configured.'));
    var grid = el('div', 'srcgrid');
    d.sources.forEach(function (s) { grid.appendChild(sourceCard(s)); });
    box.appendChild(grid);
    box.appendChild(el('p', 'muted', 'Tokens are never sent to the browser.'));
    return box;
  }

  function mqttSection(d) {
    var m = d.mqtt, box = el('div');
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
    var choice = el('select'); choice.dataset.keep = 'host-view';
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

    // The bridge's own machine as a host next to the ones from Prometheus.
    var own = checkbox(d.server.enabled, !d.server.editable);
    card.appendChild(settingRow('Show this machine\'s own stats',
      'The bridge reads its own /proc and /sys. Turn off when a Prometheus source already covers this machine, for instance in Docker, where the local numbers are the container\'s', own));
    if (d.server.editable) {
      var ownSave = el('button', 'secondary', 'Save');
      ownSave.type = 'button';
      ownSave.addEventListener('click', function () {
        post('api/setup/server', { enabled: own.checked }).then(function (j) { notice = (j && j.error) || null; load(); });
      });
      var ownLine = el('div', 'buttons'); ownLine.appendChild(ownSave); card.appendChild(ownLine);
    }
    card.appendChild(el('p', 'muted', d.server.editable ? 'Needs a restart of the bridge.'
      : 'Give the bridge a data folder to change this here, or set enabled = false under [server] in bridge.toml.'));
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


  // ---- Backup: export and import of the settings ----
  var backup = { withSecrets: false };
  function backupSection(d) {
    var box = el('div', 'two');
    var out = el('section', 'card');
    out.appendChild(el('h2', null, 'Export'));
    out.appendChild(el('p', 'muted', 'Download your settings as one file: sources, broker, alerts, dashboard layouts and sign-in rules. '
      + 'Tokens, client secrets and passwords are left out, so the file is safe to keep and share.'));
    var plain = el('input'); plain.type = 'radio'; plain.name = 'bk'; plain.checked = !backup.withSecrets;
    var sealed = el('input'); sealed.type = 'radio'; sealed.name = 'bk'; sealed.checked = backup.withSecrets;
    plain.addEventListener('change', function () { backup.withSecrets = false; });
    sealed.addEventListener('change', function () { backup.withSecrets = true; });
    var r1 = settingRow('Without secrets', 'Recommended. After an import you enter the tokens again.', plain);
    var r2 = settingRow('With secrets, locked by a password', 'Tokens and client secrets are encrypted in the file. You need the password to import it.', sealed);
    [[r1, plain], [r2, sealed]].forEach(function (x) {
      x[0].classList.add('pick');
      x[0].addEventListener('click', function (ev) { if (ev.target !== x[1]) { x[1].checked = true; x[1].dispatchEvent(new Event('change')); } });
    });
    out.appendChild(r1); out.appendChild(r2);
    var go = el('button', null, 'Download backup'); go.type = 'button'; go.disabled = !d.can_edit;
    go.addEventListener('click', function () { backup.withSecrets ? exportSealed() : exportPlain(); });
    var line = el('div', 'buttons'); line.appendChild(go); out.appendChild(line);
    out.appendChild(el('p', 'muted', 'Never included: administrator and viewer passwords, API keys and sessions. Set those up again on the new install.'));
    box.appendChild(out);

    var inn = el('section', 'card');
    inn.appendChild(el('h2', null, 'Import'));
    inn.appendChild(el('p', 'muted', 'Restore settings from a backup file. You see what is inside before anything changes.'));
    var file = el('input'); file.type = 'file'; file.accept = '.deskwatch,application/json';
    file.addEventListener('change', function () { if (file.files[0]) readBackup(file.files[0]); });
    var pick = el('button', 'secondary', 'Choose backup file…'); pick.type = 'button'; pick.disabled = !d.can_edit;
    pick.addEventListener('click', function () { file.click(); });
    var l2 = el('div', 'buttons'); l2.appendChild(pick); inn.appendChild(l2);
    inn.appendChild(el('p', 'muted', 'Importing replaces your sources, alerts and layouts with the ones in the file. The bridge needs a restart afterwards.'));
    box.appendChild(inn);
    if (!d.can_edit) {
      var none = el('p', 'muted', 'Give the bridge a data folder to make or restore a backup here ([data] dir, see docs/data-dir.md).');
      var wrap = el('div'); wrap.appendChild(none); wrap.appendChild(box); return wrap;
    }
    return box;
  }


  // The file goes to the browser as a download; nothing is kept in the page.
  function saveFile(r) {
    var name = /filename="([^"]+)"/.exec(r.headers.get('Content-Disposition') || '');
    return r.blob().then(function (blob) {
      var a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = name ? name[1] : 'deskwatch.deskwatch';
      document.body.appendChild(a); a.click(); a.remove();
      setTimeout(function () { URL.revokeObjectURL(a.href); }, 1000);
    });
  }
  function exportPlain() {
    fetch('api/setup/export', { cache: 'no-store', credentials: 'same-origin' }).then(function (r) {
      if (!r.ok) return r.json().then(function (j) { notice = j.error || 'Cannot make the backup.'; load(); });
      return saveFile(r);
    });
  }
  function exportSealed() {
    var m = openModal('Lock the backup with a password');
    m.body.appendChild(el('p', 'muted', 'Tokens and client secrets go into the file, encrypted. Keep the password safe: without it the file cannot be opened, and it cannot be reset.'));
    var pw = input('password', 'pw', 'new-password'), pw2 = input('password', 'pw2', 'new-password');
    m.body.appendChild(labelled('Password (at least 12 characters)', pw));
    m.body.appendChild(labelled('Repeat password', pw2));
    var ok = modalButtons(m, 'Download locked backup');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      if (pw.value.length < 12) { modalError(m, 'The password needs at least 12 characters.'); return; }
      if (pw.value !== pw2.value) { modalError(m, 'The two passwords are not the same.'); return; }
      ok.disabled = true;
      fetch('api/setup/export', {
        method: 'POST', cache: 'no-store', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ password: pw.value })
      }).then(function (r) {
        if (r.ok) { closeModal(); return saveFile(r); }
        return r.json().then(function (j) { ok.disabled = false; modalError(m, j.error || 'Cannot make the backup.'); });
      });
    });
    pw.focus();
  }

  // The chosen file is read in the browser and sent to the bridge, which opens
  // it and says what is inside. Nothing changes until the person confirms.
  function readBackup(f) {
    if (f.size > 1024 * 1024) { notice = 'That file is too big to be a DeskWatch backup.'; load(); return; }
    var reader = new FileReader();
    reader.onload = function () { checkBackup(f.name, String(reader.result), ''); };
    reader.readAsText(f);
  }
  function checkBackup(name, text, password) {
    post('api/setup/import/check', { file: text, password: password }).then(function (j) {
      if (j.needs_password || (password && j.error && /password/.test(j.error))) { askBackupPassword(name, text, j.error); return; }
      if (j.error) { var e = openModal('This is not a usable backup'); e.body.appendChild(el('p', 'err', j.error)); modalButtons(e, 'Close'); e.form.addEventListener('submit', function (ev) { ev.preventDefault(); closeModal(); }); return; }
      showBackup(name, text, password, j.summary);
    });
  }
  function askBackupPassword(name, text, error) {
    var m = openModal('This backup is locked');
    m.body.appendChild(el('p', 'muted', name + ' holds secrets locked with a password. Enter it to see what is inside.'));
    var pw = input('password', 'pw', 'current-password');
    m.body.appendChild(labelled('Password', pw));
    if (error) modalError(m, error);
    modalButtons(m, 'Open backup');
    m.form.addEventListener('submit', function (ev) { ev.preventDefault(); closeModal(); checkBackup(name, text, pw.value); });
    pw.focus();
  }
  function showBackup(name, text, password, s) {
    var m = openModal('Import this backup?');
    var when = s.created ? new Date(s.created * 1000).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' }) : '';
    m.body.appendChild(el('p', 'muted', name + ' · made by DeskWatch ' + s.app + (when ? ' on ' + when : '')));
    var dl = el('dl');
    var total = s.sources.reduce(function (n, x) { return n + x[1]; }, 0);
    row(dl, 'Sources', total ? total + ' (' + s.sources.map(function (x) { return x[1] + ' ' + (KIND[x[0]] || x[0]); }).join(', ') + ')' : 'none');
    row(dl, 'MQTT broker', s.mqtt ? 'yes' : 'no');
    row(dl, 'Alerts and dashboard', (s.alerts || s.dashboard ? 'yes' : 'no') + (s.layouts != null ? ' · ' + s.layouts + ' personal layout' + (s.layouts === 1 ? '' : 's') : ''));
    row(dl, 'Sign-in providers', s.providers.length ? s.providers.length + ' (' + s.providers.join(', ') + ')' : 'none');
    row(dl, 'Secrets', s.secrets ? s.secrets + ' (tokens and client secrets)' : 'none in this file');
    m.body.appendChild(dl);
    m.body.appendChild(el('p', 'warn-note', 'This replaces your current sources, alerts and layouts. Passwords and API keys stay as they are.'
      + (s.secrets ? '' : ' Sources will show "token missing" until you enter their tokens again.')));
    var ok = modalButtons(m, 'Replace my settings', 'danger solid');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      ok.disabled = true;
      post('api/setup/import', { file: text, password: password }).then(function (j) {
        if (j.error) { ok.disabled = false; modalError(m, j.error); return; }
        closeModal(); notice = null; load();
      });
    });
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
        document.getElementById('signout').hidden = a.state !== 'signed_in';
        // A person (not a screen key) gets My account.
        document.getElementById('account-link').hidden = !(a.state === 'signed_in' && (a.name || a.user));
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
    var box = el('div');
    while (card.firstChild) box.appendChild(card.firstChild);   // the result line of the last change

    var stats = el('div', 'stats');
    [['Users', a.users.length], ['API keys', a.keys.length], ['Signed in', a.sessions]].forEach(function (x) {
      var t = el('div', 'stat'); t.appendChild(el('span', null, x[0])); t.appendChild(el('b', null, String(x[1])));
      stats.appendChild(t);
    });
    box.appendChild(stats);

    // Users: one row each, the role changes on the spot.
    var users = el('section', 'card');
    var uh = el('div', 'cardhead');
    var ut = el('div'); ut.appendChild(el('h2', null, 'Users'));
    ut.appendChild(el('p', 'muted', 'Who can sign in with a password.'));
    uh.appendChild(ut);
    var addUser = addButton('New user');
    addUser.addEventListener('click', function () { newUserModal(min); });
    uh.appendChild(addUser);
    users.appendChild(uh);
    var list = el('div', 'ulist');
    a.users.forEach(function (u) {
      var line = el('div', 'urow');
      line.appendChild(el('span', 'av ' + u.role, u.name.charAt(0).toUpperCase()));
      var who = el('div', 'who');
      who.appendChild(el('b', null, u.name));
      if (u.must_change) who.appendChild(pill('new password at next sign-in', 'warn'));
      line.appendChild(who);
      var role = el('select'); role.setAttribute('aria-label', 'Role of ' + u.name);
      ['viewer', 'operator', 'admin'].forEach(function (r) {
        var op = el('option', null, ROLE_NAMES[r]); op.value = r; if (r === u.role) op.selected = true; role.appendChild(op);
      });
      role.addEventListener('change', function () {
        accessCall('api/auth/user/role', { name: u.name, role: role.value }, 'The role of ' + u.name + ' is changed.');
      });
      line.appendChild(role);
      var acts = el('div', 'rowacts');
      var reset = iconButton('key', 'New password');
      reset.addEventListener('click', function () { passwordModal(u.name, min); });
      var del = iconButton('trash', 'Delete user', 'danger');
      del.addEventListener('click', function () {
        confirmModal('Remove ' + u.name + '?', 'They are signed out right away. This cannot be undone.', 'Yes, remove',
          function () { accessCall('api/auth/user/delete', { name: u.name }, 'The user ' + u.name + ' is removed.'); });
      });
      acts.appendChild(reset); acts.appendChild(del);
      line.appendChild(acts);
      list.appendChild(line);
    });
    users.appendChild(list);
    users.appendChild(el('p', 'muted small', 'A viewer can look at the dashboard. An operator can also dismiss alerts and failed runs. An administrator can do everything, including this page.'));
    box.appendChild(users);

    var two = el('div', 'two');
    // API keys, for scripts and screens.
    var keys = el('section', 'card');
    var kh = el('div', 'cardhead');
    var kt = el('div'); kt.appendChild(el('h2', null, 'API keys'));
    kt.appendChild(el('p', 'muted', 'For scripts and screens, so they never need a password.'));
    kh.appendChild(kt);
    var addKey = addButton('New key');
    addKey.addEventListener('click', newKeyModal);
    kh.appendChild(addKey);
    keys.appendChild(kh);
    if (!a.keys.length) keys.appendChild(el('p', 'muted', 'No keys yet.'));
    var klist = el('div', 'ulist');
    a.keys.forEach(function (k) {
      var line = el('div', 'urow');
      var ic = el('span', 'av key'); ic.appendChild(svg('key')); line.appendChild(ic);
      var who = el('div', 'who'); who.appendChild(el('b', null, k.name));
      who.appendChild(el('small', null, k.scope === 'alert' ? 'may send alerts' : k.scope === 'viewer' ? 'may open the dashboard' : k.scope + ' key'));
      line.appendChild(who);
      var acts = el('div', 'rowacts');
      var del = iconButton('trash', 'Delete key', 'danger');
      del.addEventListener('click', function () {
        confirmModal('Delete the key ' + k.name + '?', 'Whatever uses it stops working right away.', 'Yes, delete',
          function () { accessCall('api/auth/key/delete', { name: k.name }, 'The key ' + k.name + ' is deleted.'); });
      });
      acts.appendChild(del); line.appendChild(acts);
      klist.appendChild(line);
    });
    keys.appendChild(klist);
    keys.appendChild(el('p', 'muted small', 'An alert key may send POST /api/alert (Authorization: Bearer <key>). A viewer key opens the dashboard once: https://host/?key=<key>.'));
    two.appendChild(keys);

    // Sessions.
    var sess = el('section', 'card');
    sess.appendChild(el('h2', null, 'Sessions'));
    sess.appendChild(el('p', 'muted', a.sessions === 1 ? 'One browser is signed in.' : a.sessions + ' browsers are signed in.'));
    var out = el('button', 'secondary danger', 'Sign out everywhere'); out.type = 'button';
    out.addEventListener('click', function () {
      confirmModal('Sign out everywhere?', 'Every browser and screen is signed out, this one too. Viewer keys keep working.', 'Yes, sign out',
        function () { post('api/auth/logout-all', {}).then(function () { location.href = '/login'; }); });
    });
    var sb = el('div', 'buttons'); sb.appendChild(out); sess.appendChild(sb);
    var me = el('p', 'muted small'); var meLink = el('a', null, 'My account'); meLink.href = '/account';
    me.appendChild(document.createTextNode('To change your own password: ')); me.appendChild(meLink); me.appendChild(document.createTextNode('.'));
    sess.appendChild(me);
    two.appendChild(sess);
    box.appendChild(two);
    return box;
  }

  var ROLE_NAMES = { admin: 'Administrator', operator: 'Operator', viewer: 'Viewer' };

  // "My account" in the pop-up: who you are, your own password, sign out. The
  // /account page stays for a password an administrator chose, which must be
  // replaced before anything else.
  var ROLE_TEXT = { admin: 'Administrator: everything, including setup and users.',
    operator: 'Operator: the dashboard, and dismissing alerts and failed runs.', viewer: 'Viewer: the dashboard only.' };
  function accountModal() {
    if (!access) return;
    var m = openModal('My account');
    var name = access.name || access.user || 'you';
    var who = el('div', 'urow');
    who.appendChild(el('span', 'av ' + (access.role || ''), name.charAt(0).toUpperCase()));
    var w = el('div', 'who'); w.appendChild(el('b', null, name)); w.appendChild(el('small', null, ROLE_TEXT[access.role] || ''));
    who.appendChild(w);
    m.body.appendChild(who);
    var out = el('button', 'secondary', 'Sign out'); out.type = 'button';
    out.addEventListener('click', function () { post('api/auth/logout', {}).then(function () { location.href = '/login'; }); });
    m.foot.appendChild(out);
    m.foot.appendChild(el('span', 'grow'));
    if (!access.name) {
      m.body.appendChild(el('p', 'muted', 'This sign-in comes from a provider, so there is no password to change here.'));
      m.form.addEventListener('submit', function (ev) { ev.preventDefault(); });
      return;
    }
    var min = access.min_password || 12;
    var cur = input('password', 'current', 'current-password'); cur.autofocus = true;
    var neu = input('password', 'new', 'new-password'), rep = input('password', 'repeat', 'new-password');
    var f = el('div', 'form');
    f.appendChild(el('h3', null, 'Change password'));
    f.appendChild(labelled('Current password', cur));
    f.appendChild(labelled('New password (at least ' + min + ' characters)', neu));
    f.appendChild(labelled('Repeat new password', rep));
    m.body.appendChild(f);
    var save = el('button', null, 'Change password'); save.type = 'submit';
    m.foot.appendChild(save);
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      if (!neu.value) { modalError(m, 'Type a new password.'); return; }
      if (neu.value !== rep.value) { modalError(m, 'The two passwords differ.'); return; }
      save.disabled = true;
      post('api/auth/password', { current: cur.value, new: neu.value }).then(function (j) {
        save.disabled = false;
        if (j.error || j.status >= 400) { modalError(m, j.error || 'The password was not changed.'); return; }
        cur.value = neu.value = rep.value = '';
        var old = m.body.querySelector('.modal-err'); if (old) old.remove();
        var ok = el('p', 'ok modal-err', 'Your password is changed.'); ok.setAttribute('role', 'status');
        m.body.appendChild(ok);
      });
    });
  }
  document.getElementById('account-link').addEventListener('click', function (ev) {
    if (!access) return;            // not loaded yet: the link opens /account
    ev.preventDefault(); accountModal();
  });

  // Send a pop-up's form; on success close it and show the result on the page,
  // on an error keep it open and say what is wrong.
  function modalCall(m, url, body, done, then) {
    var b = m.form.querySelector('button[type=submit]'); b.disabled = true;
    return post(url, body).then(function (j) {
      b.disabled = false;
      if (j.error) { modalError(m, j.error); return; }
      closeModal();
      accessNote = { ok: done };
      if (then) then(j);
      loadAccess();
    });
  }

  function newUserModal(min) {
    var m = openModal('New user');
    var name = input('text', 'name', 'off'); name.autofocus = true;
    var role = el('select'); role.name = 'role';
    [['viewer', 'Viewer: the dashboard only'], ['operator', 'Operator: also dismiss alerts and failed runs'], ['admin', 'Administrator: everything']].forEach(function (o) {
      var op = el('option', null, o[1]); op.value = o[0]; if (o[0] === 'operator') op.selected = true; role.appendChild(op);
    });
    var pw = input('password', 'password', 'new-password');
    var f = el('div', 'form');
    f.appendChild(labelled('User name', name));
    f.appendChild(labelled('Role', role));
    f.appendChild(labelled('First password (at least ' + min + ' characters)', pw));
    f.appendChild(el('p', 'muted small', 'They choose their own password at the first sign-in.'));
    m.body.appendChild(f);
    modalButtons(m, 'Create user');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      modalCall(m, 'api/auth/user', { name: name.value, role: role.value, password: pw.value }, 'The user ' + name.value + ' is created.');
    });
  }

  function passwordModal(user, min) {
    var m = openModal('New password for ' + user);
    var pw = input('password', 'password', 'new-password'); pw.autofocus = true;
    var f = el('div', 'form');
    f.appendChild(el('p', 'muted', 'They are signed out and must replace it at the next sign-in.'));
    f.appendChild(labelled('Password (at least ' + min + ' characters)', pw));
    m.body.appendChild(f);
    modalButtons(m, 'Set password');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      modalCall(m, 'api/auth/user/password', { name: user, password: pw.value }, 'The password of ' + user + ' is changed.');
    });
  }

  function newKeyModal() {
    var m = openModal('New API key');
    var name = input('text', 'name'); name.autofocus = true;
    var scope = el('select'); scope.name = 'scope';
    [['alert', 'Send alerts (POST /api/alert)'], ['viewer', 'Open the dashboard']].forEach(function (o) {
      var op = el('option', null, o[1]); op.value = o[0]; scope.appendChild(op);
    });
    var f = el('div', 'form');
    f.appendChild(labelled('Name (what uses it, such as homeassistant)', name));
    f.appendChild(labelled('What it may do', scope));
    m.body.appendChild(f);
    modalButtons(m, 'Create key');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      var keyName = name.value;
      modalCall(m, 'api/auth/key', { name: keyName, scope: scope.value }, 'The key ' + keyName + ' is created.', function (j) {
        if (j.key) showKey(keyName, j.key);
      });
    });
  }

  // A new key is shown once, in its own pop-up with a copy button.
  function showKey(name, key) {
    var m = openModal('Key ' + name);
    m.body.appendChild(el('p', null, 'Copy this key now. It is shown once and cannot be read again.'));
    var code = el('code', 'onekey', key);
    m.body.appendChild(code);
    var copy = el('button', 'secondary', 'Copy'); copy.type = 'button';
    copy.addEventListener('click', function () {
      var range = document.createRange(); range.selectNodeContents(code);
      var sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(range);
      var done = function () { copy.textContent = 'Copied'; };
      if (navigator.clipboard && window.isSecureContext) navigator.clipboard.writeText(key).then(done, function () { document.execCommand('copy'); done(); });
      else { document.execCommand('copy'); done(); }
    });
    m.foot.appendChild(copy);
    var ok = el('button', null, 'Done'); ok.type = 'submit';
    m.foot.appendChild(ok);
    m.form.addEventListener('submit', function (ev) { ev.preventDefault(); closeModal(); });
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

  // What is saved for each provider, with an unchanged secret (null keeps it).
  function oauthGithub(g) {
    return g ? { client_id: g.client_id, secret: null, users: g.users || [], orgs: g.orgs || [], teams: g.teams || [],
      operator_users: g.operator_users || [], operator_teams: g.operator_teams || [], admin_users: g.admin_users || [], admin_teams: g.admin_teams || [] } : null;
  }
  function oauthMicrosoft(m) {
    return m ? { tenant: m.tenant, client_id: m.client_id, secret: null, groups: m.groups || [],
      operator_groups: m.operator_groups || [], admin_groups: m.admin_groups || [] } : null;
  }
  // Save the whole sign-in setting with one part changed. In a pop-up the
  // error stays in the pop-up.
  function saveOauth(part, done, m) {
    var body = { public_url: oauth.public_url || null, github: oauthGithub(oauth.github), microsoft: oauthMicrosoft(oauth.microsoft) };
    Object.keys(part).forEach(function (k) { body[k] = part[k]; });
    var b = m && m.form.querySelector('button[type=submit]'); if (b) b.disabled = true;
    return post('api/auth/oauth', body).then(function (j) {
      if (b) b.disabled = false;
      if (j.error) { if (m) { modalError(m, j.error); return; } oauthNote = { error: j.error }; }
      else { closeModal(); oauthNote = { ok: done }; }
      return loadOauth();
    });
  }

  function count(list, one, many) {
    var n = (list || []).length;
    return n ? n + ' ' + (n === 1 ? one : many) : null;
  }
  function who(parts) { var p = parts.filter(Boolean); return p.length ? p.join(', ') : 'nobody'; }

  function providerCard(id, name, mark, conf, summary, edit) {
    var card = el('section', 'card' + (conf ? '' : ' off'));
    var head = el('div', 'cardhead');
    var h = el('h2'); h.appendChild(el('span', 'mark ' + id, mark));
    h.appendChild(document.createTextNode(name + ' '));
    h.appendChild(pill(conf ? 'on' : 'off', conf ? 'ok' : 'muted'));
    head.appendChild(h);
    var acts = el('div', 'rowacts');
    if (conf) {
      var e = iconButton('edit', 'Change'); e.addEventListener('click', edit);
      var off = iconButton('power', 'Turn off', 'danger');
      off.addEventListener('click', function () {
        var part = {}; part[id] = null;
        confirmModal('Turn off sign-in with ' + name + '?', 'The button leaves the login page and its settings and secret are removed. People signed in with it stay signed in until they sign out.',
          'Yes, turn off', function () { saveOauth(part, 'Sign-in with ' + name + ' is off.'); });
      });
      acts.appendChild(e); acts.appendChild(off);
    }
    head.appendChild(acts);
    card.appendChild(head);
    if (conf) {
      var dl = el('dl');
      summary.forEach(function (r) { row(dl, r[0], r[1]); });
      card.appendChild(dl);
    } else {
      card.appendChild(el('p', 'muted', 'Not set up. People cannot sign in with ' + name + '.'));
      var b = el('div', 'buttons'); var set = el('button', 'secondary', 'Set up'); set.type = 'button';
      set.addEventListener('click', edit); b.appendChild(set); card.appendChild(b);
    }
    return card;
  }

  function signinSection() {
    var card = el('section', 'card');
    if (!oauth) { loadOauth(); card.appendChild(el('p', 'muted', 'Loading…')); return card; }
    if (oauth.unavailable) {
      card.appendChild(el('p', 'muted', 'Only the administrator can change the sign-in providers, and a login password must be set first (see Access).'));
      return card;
    }
    var box = el('div');
    if (oauthNote) {
      var n = el('p', oauthNote.error ? 'err' : 'banner', oauthNote.error || oauthNote.ok);
      n.setAttribute('role', 'status'); box.appendChild(n);
    }
    box.appendChild(el('p', 'muted lead', 'Let people sign in with GitHub or Microsoft. Nobody gets in by default: list who is allowed. Someone who matches an administrator rule is an administrator, an operator rule can also dismiss alerts and failed runs, and anyone else who is allowed can only look. The local password always keeps working.'));

    var addr = el('section', 'card');
    var ah = el('div', 'cardhead');
    var at = el('div'); at.appendChild(el('h2', null, 'Public address'));
    at.appendChild(el('p', 'muted', 'The address people open, over HTTPS. The providers send them back to it.'));
    ah.appendChild(at);
    var ae = el('div', 'rowacts'); var aeb = iconButton('edit', 'Change'); ae.appendChild(aeb); ah.appendChild(ae);
    addr.appendChild(ah);
    addr.appendChild(el('p', 'value' + (oauth.public_url ? '' : ' muted'), oauth.public_url || 'not set'));
    aeb.addEventListener('click', addressModal);
    box.appendChild(addr);

    var g = oauth.github, m = oauth.microsoft, two = el('div', 'two');
    two.appendChild(providerCard('github', 'GitHub', 'GH', g, g ? [
      ['Client ID', g.client_id], ['Client secret', g.secret_set ? 'set' : 'missing'],
      ['May sign in', who([count(g.users, 'user', 'users'), count(g.orgs, 'organisation', 'organisations'), count(g.teams, 'team', 'teams')])],
      ['Operators', who([count(g.operator_users, 'user', 'users'), count(g.operator_teams, 'team', 'teams')])],
      ['Administrators', who([count(g.admin_users, 'user', 'users'), count(g.admin_teams, 'team', 'teams')])]
    ] : [], githubModal));
    two.appendChild(providerCard('microsoft', 'Microsoft', 'MS', m, m ? [
      ['Tenant', m.tenant], ['Client ID', m.client_id], ['Client secret', m.secret_set ? 'set' : 'missing'],
      ['May sign in', who([count(m.groups, 'group', 'groups')])],
      ['Operators', who([count(m.operator_groups, 'group', 'groups')])],
      ['Administrators', who([count(m.admin_groups, 'group', 'groups')])]
    ] : [], microsoftModal));
    box.appendChild(two);
    return box;
  }

  function addressModal() {
    var m = openModal('Public address');
    var url = input('text', 'public_url'); url.value = oauth.public_url || ''; url.placeholder = 'https://deskwatch.example.org'; url.autofocus = true;
    var f = el('div', 'form'); f.appendChild(labelled('Public address (https://…)', url)); m.body.appendChild(f);
    modalButtons(m, 'Save');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      saveOauth({ public_url: url.value.trim() || null }, 'The public address is saved.', m);
    });
  }

  function group(title, hint) {
    var g = el('fieldset', 'group'); g.appendChild(el('legend', null, title));
    if (hint) g.appendChild(el('p', 'muted small', hint));
    return g;
  }

  function githubModal() {
    var g = oauth.github;
    var m = openModal(g ? 'GitHub sign-in' : 'Set up GitHub sign-in', true);
    var app = group('The OAuth app', 'On GitHub: Settings, Developer settings, OAuth Apps, New. Callback URL: ' + (oauth.redirect.github || '<public address>/auth/callback/github'));
    var gid = input('text', 'g_id'); gid.value = g ? g.client_id : ''; gid.autofocus = true;
    var gsec = input('password', 'g_secret', 'new-password'); gsec.placeholder = g && g.secret_set ? 'set, leave empty to keep' : '';
    app.appendChild(labelled('Client ID', gid));
    app.appendChild(labelled('Client secret (never shown again)', gsec));
    var allow = group('Who may sign in', 'One per line. Organisations and teams only work when the organisation approves this OAuth app (Organization settings, Third-party access).');
    var gUsers = textarea('g_users', 2, g && g.users), gOrgs = textarea('g_orgs', 2, g && g.orgs), gTeams = textarea('g_teams', 2, g && g.teams);
    allow.appendChild(labelled('GitHub users', gUsers));
    allow.appendChild(labelled('Organisations', gOrgs));
    allow.appendChild(labelled('Teams (organisation/team)', gTeams));
    var roles = group('Roles', 'Everyone else who may sign in is a viewer.');
    var gOU = textarea('g_ou', 2, g && g.operator_users), gOT = textarea('g_ot', 2, g && g.operator_teams);
    var gAU = textarea('g_au', 2, g && g.admin_users), gAT = textarea('g_at', 2, g && g.admin_teams);
    var grid = el('div', 'pair');
    grid.appendChild(labelled('Operators: users', gOU)); grid.appendChild(labelled('Operators: teams', gOT));
    grid.appendChild(labelled('Administrators: users', gAU)); grid.appendChild(labelled('Administrators: teams', gAT));
    roles.appendChild(grid);
    m.body.appendChild(app); m.body.appendChild(allow); m.body.appendChild(roles);
    modalButtons(m, g ? 'Save' : 'Turn on');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      saveOauth({ github: { client_id: gid.value, secret: gsec.value || null, users: lines(gUsers.value), orgs: lines(gOrgs.value),
        teams: lines(gTeams.value), operator_users: lines(gOU.value), operator_teams: lines(gOT.value), admin_users: lines(gAU.value), admin_teams: lines(gAT.value) } },
        'Saved. The GitHub button is on the login page.', m);
    });
  }

  function microsoftModal() {
    var x = oauth.microsoft;
    var m = openModal(x ? 'Microsoft sign-in' : 'Set up Microsoft sign-in', true);
    var app = group('The app registration', 'In Microsoft Entra: App registrations, New, Web, redirect URI: ' + (oauth.redirect.microsoft || '<public address>/auth/callback/microsoft') + '. Add a client secret. The permission User.Read (the default) is enough.');
    var mt = input('text', 'm_tenant'); mt.value = x ? x.tenant : ''; mt.autofocus = true;
    var mid = input('text', 'm_id'); mid.value = x ? x.client_id : '';
    var msec = input('password', 'm_secret', 'new-password'); msec.placeholder = x && x.secret_set ? 'set, leave empty to keep' : '';
    app.appendChild(labelled('Directory (tenant) ID', mt));
    app.appendChild(labelled('Application (client) ID', mid));
    app.appendChild(labelled('Client secret (never shown again)', msec));
    var allow = group('Who may sign in', 'The Object Id of each group, one per line. Everyone else who may sign in is a viewer.');
    var mg = textarea('m_groups', 2, x && x.groups), mog = textarea('m_op', 2, x && x.operator_groups), mag = textarea('m_admin', 2, x && x.admin_groups);
    allow.appendChild(labelled('Allowed groups', mg));
    allow.appendChild(labelled('Operator groups', mog));
    allow.appendChild(labelled('Administrator groups', mag));
    m.body.appendChild(app); m.body.appendChild(allow);
    modalButtons(m, x ? 'Save' : 'Turn on');
    m.form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      saveOauth({ microsoft: { tenant: mt.value, client_id: mid.value, secret: msec.value || null,
        groups: lines(mg.value), operator_groups: lines(mog.value), admin_groups: lines(mag.value) } },
        'Saved. The Microsoft button is on the login page.', m);
    });
  }

  var TITLES = { sources: 'Sources', mqtt: 'MQTT broker', dashboard: 'Dashboard', alerts: 'Alerts', access: 'Access', signin: 'Sign-in providers', backup: 'Backup', bridge: 'Bridge and restart', secrets: 'Stored secrets' };

  function currentSection() {
    var id = (location.hash || '').replace('#', '');
    return SECTIONS.indexOf(id) >= 0 ? id : 'sources';
  }

  // The page is redrawn every few seconds. A drop-down marked with
  // `data-keep` that the user changed but did not save yet keeps its choice
  // across a redraw, instead of jumping back to the first or saved value.
  // (Drop-downs that save on change, such as a user's role, are not marked.)
  function keptChoices() {
    var kept = {};
    app.querySelectorAll('select[data-keep]').forEach(function (s) {
      if (s.value !== s.dataset.initial) kept[s.dataset.keep] = s.value;
    });
    return kept;
  }
  function restoreChoices(kept) {
    app.querySelectorAll('select[data-keep]').forEach(function (s) {
      var want = kept[s.dataset.keep];
      var has = want != null && Array.prototype.some.call(s.options, function (o) { return o.value === want; });
      s.dataset.initial = s.value;
      if (has && !s.disabled) s.value = want;
    });
  }

  function render(d) {
    var kept = keptChoices();
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
    var head = el('div', 'sectionhead');
    head.appendChild(el('h2', 'sectiontitle', TITLES[section]));
    if (section === 'sources' && d.can_edit) {
      var add = addButton('Add source');
      add.addEventListener('click', addSource);
      head.appendChild(add);
    }
    content.appendChild(head);
    content.appendChild(({ sources: sourcesSection, mqtt: mqttSection, dashboard: dashboardSection, alerts: alertsSection,
                           access: accessSection, signin: signinSection, backup: backupSection, bridge: bridgeSection, secrets: secretsSection })[section](d));
    layout.appendChild(content);
    app.appendChild(layout);
    restoreChoices(kept);
  }

  window.addEventListener('hashchange', function () { closeModal(); form = null; confirming = null; if (last) render(last); });

  function load() {
    clearTimeout(timer);
    // A refresh would redraw (and lose the focus of) a form being filled in,
    // or close a drop-down that is open.
    var active = document.activeElement;
    var typing = active && app.contains(active) && ['INPUT', 'SELECT', 'TEXTAREA'].indexOf(active.tagName) >= 0;
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
