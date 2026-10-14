// DeskWatch setup page. Shows how the bridge is configured and can test a
// connection (the bridge makes the requests, with the real credentials).
// Secrets are never sent to the browser: a credential is only reported as
// present or missing. Everything is built with textContent, so
// nothing from the config can inject markup.
(function () {
  'use strict';

  var app = document.getElementById('app');
  var REFRESH_MS = 5000;
  var timer = null;
  var last = null;       // latest status, so a finished test can redraw at once
  var results = {};      // source id -> latest test report (kept across refreshes)
  var running = {};      // source id -> true while its test runs

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
      if (r.status === 401) { loginForm(); return null; }
      if (!r.ok) return { checks: [], error: r.status === 429 ? 'Another test is still running.' : r.status === 504 ? 'The test took too long.' : 'The test failed (HTTP ' + r.status + ').' };
      return r.json();
    }, function () { return { checks: [], error: 'Cannot reach the bridge.' }; })
      .then(function (report) {
        running[id] = false;
        if (report) results[id] = report;
        if (last) render(last);
      });
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
    card.appendChild(testBlock(s.id));
    return card;
  }

  function render(d) {
    last = d;
    document.getElementById('build').textContent = 'version ' + d.build;
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
    info.appendChild(dl);
    if (m.enabled) info.appendChild(testBlock('mqtt'));
    app.appendChild(info);

    app.appendChild(el('h2', null, 'Sources'));
    if (!d.sources.length) app.appendChild(el('p', 'muted', 'No sources configured.'));
    d.sources.forEach(function (s) { app.appendChild(sourceCard(s)); });
    app.appendChild(el('p', 'muted', 'Tokens are never sent to the browser. Editing comes later; for now change bridge.toml or the data folder.'));
  }

  function loginForm(message) {
    clearTimeout(timer);
    app.textContent = '';
    var card = el('section', 'card');
    card.appendChild(el('h2', null, 'Setup token'));
    card.appendChild(el('p', 'muted', 'The text in the file named by [setup] token_file in the bridge config. It is not a CI token.'));
    var form = el('form');
    var input = el('input');
    input.type = 'password'; input.autocomplete = 'off'; input.spellcheck = false;
    input.setAttribute('aria-label', 'setup token');
    var button = el('button', null, 'Log in');
    button.type = 'submit';
    form.appendChild(input); form.appendChild(button);
    form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      var given = input.value.trim();
      if (!given) return;
      fetch('api/setup/login', {
        method: 'POST', cache: 'no-store', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token: given })
      }).then(function (r) {
        if (r.ok) return load();
        loginForm(r.status === 429 ? 'Too many wrong tokens. Wait a minute and try again.'
          : r.status === 401 ? 'That token was not accepted.' : 'The bridge answered with an error (HTTP ' + r.status + ').');
      }, function () { loginForm('Cannot reach the bridge.'); });
    });
    card.appendChild(form);
    if (message) { var p = el('p', 'err', message); p.setAttribute('role', 'alert'); card.appendChild(p); }
    app.appendChild(card);
    input.focus();
  }

  function load() {
    clearTimeout(timer);
    return fetch('api/setup/status', { cache: 'no-store', credentials: 'same-origin' }).then(function (r) {
      if (r.status === 401) { loginForm(); return; }
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
