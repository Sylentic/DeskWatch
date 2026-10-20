// The sign-in page. It asks /api/auth/status what to show: the first-run form
// (no password yet), the sign-in form, or, when already signed in, it goes on.
(function () {
  'use strict';
  // Light or dark: the choice made on the setup page, kept in this browser.
  try {
    if ((localStorage.getItem('deskwatch-theme') || localStorage.getItem('deskwatch-setup-theme')) === 'light') {
      document.documentElement.setAttribute('data-theme', 'light');
    }
  } catch (e) { /* dark */ }
  var card = document.getElementById('card');

  function el(tag, attrs, text) {
    var node = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) { node.setAttribute(k, attrs[k]); });
    if (text) node.textContent = text;
    return node;
  }

  // Only a path on this site: never an address on another one.
  function nextUrl() {
    var next = new URLSearchParams(location.search).get('next') || '/';
    return /^\/(?![\/\\])/.test(next) ? next : '/';
  }

  function post(url, body) {
    return fetch(url, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-DeskWatch': '1' },
      body: JSON.stringify(body),
    }).then(function (r) {
      return r.json().catch(function () { return {}; }).then(function (j) {
        if (!r.ok) throw new Error(j.error || 'Something went wrong (HTTP ' + r.status + ')');
        return j;
      });
    });
  }

  function field(label, name, type, value, autocomplete) {
    var l = el('label', {}, label);
    var i = el('input', { name: name, type: type, autocomplete: autocomplete, required: '' });
    if (value) i.value = value;
    l.appendChild(i);
    return l;
  }

  var ERRORS = {
    denied: 'The sign-in was cancelled or refused by the provider.',
    not_allowed: 'You signed in, but you are not allowed in. Ask an administrator to add you or your team.',
    expired: 'That sign-in link expired or was not started here. Try again.',
    failed: 'The sign-in did not work. The bridge log has the reason.'
  };
  var NAMES = { github: 'GitHub', microsoft: 'Microsoft' };

  function show(state, min, providers) {
    card.textContent = '';
    var first = state === 'first_run';
    card.appendChild(el('h2', {}, first ? 'Set an administrator password' : 'Sign in'));
    card.appendChild(el('p', { class: 'muted' }, first
      ? 'DeskWatch has no password yet. Choose one before anyone else can.'
      : 'to open the dashboard and setup'));
    var message = el('div', { class: 'msg bad', hidden: '' });
    var code = new URLSearchParams(location.search).get('error');
    if (code && ERRORS[code]) { message.textContent = ERRORS[code]; message.hidden = false; }
    card.appendChild(message);
    if (!first && providers.length) {
      var box = el('div', { class: 'oauth' });
      providers.forEach(function (id) {
        var a = el('a', { class: 'obtn', href: 'auth/login/' + id + '?next=' + encodeURIComponent(nextUrl()) },
          'Continue with ' + (NAMES[id] || id));
        box.appendChild(a);
      });
      card.appendChild(box);
      card.appendChild(el('div', { class: 'sep' }, 'or use a local account'));
    }
    var form = el('form');
    form.appendChild(field('Username', 'username', 'text', first ? 'admin' : '', 'username'));
    form.appendChild(field(first ? 'New password' : 'Password', 'password', 'password', '',
      first ? 'new-password' : 'current-password'));
    if (first) {
      form.appendChild(el('span', { class: 'hint' },
        'At least ' + min + ' characters. A passphrase of a few words is fine.'));
      form.appendChild(field('Repeat password', 'repeat', 'password', '', 'new-password'));
    }
    var button = el('button', { type: 'submit' }, first ? 'Save and sign in' : 'Sign in');
    form.appendChild(button);
    form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      message.hidden = true;
      var data = new FormData(form);
      if (first && data.get('password') !== data.get('repeat')) {
        message.textContent = 'The two passwords differ.';
        message.hidden = false;
        return;
      }
      button.disabled = true;
      post(first ? 'api/auth/setup' : 'api/auth/login',
        { username: data.get('username'), password: data.get('password') })
        .then(function () { location.href = nextUrl(); })
        .catch(function (err) {
          message.textContent = err.message;
          message.hidden = false;
          button.disabled = false;
        });
    });
    card.appendChild(form);
    if (first) {
      card.appendChild(el('div', { class: 'msg warn' },
        'Until a password is set, the page only answers the local network.'));
    } else {
      card.appendChild(el('p', { class: 'hint links' },
        'Forgot it? On the host run: deskwatch-bridge auth reset'));
    }
  }

  fetch('api/auth/status', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.json(); })
    .then(function (s) {
      if (s.state === 'signed_in' || s.state === 'open') { location.href = nextUrl(); return; }
      show(s.state, s.min_password || 12, s.providers || []);
    })
    .catch(function () {
      card.textContent = '';
      card.appendChild(el('p', { class: 'muted' }, 'Cannot reach the bridge.'));
    });
})();
