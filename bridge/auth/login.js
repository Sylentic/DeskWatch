// The sign-in page. It asks /api/auth/status what to show: the first-run form
// (no password yet), the sign-in form, or, when already signed in, it goes on.
(function () {
  'use strict';
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

  function show(state, min) {
    card.textContent = '';
    var first = state === 'first_run';
    card.appendChild(el('h2', {}, first ? 'Set an administrator password' : 'Sign in'));
    card.appendChild(el('p', { class: 'muted' }, first
      ? 'DeskWatch has no password yet. Choose one before anyone else can.'
      : 'to open the dashboard and setup'));
    var message = el('div', { class: 'msg bad', hidden: '' });
    card.appendChild(message);
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
      show(s.state, s.min_password || 12);
    })
    .catch(function () {
      card.textContent = '';
      card.appendChild(el('p', { class: 'muted' }, 'Cannot reach the bridge.'));
    });
})();
