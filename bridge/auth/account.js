// "My account": every signed-in person can change their own password and
// sign out. After an administrator chose a password, this is the only page
// open until it is replaced.
(function () {
  'use strict';
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

  function post(url, body) {
    return fetch(url, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-DeskWatch': '1' },
      body: JSON.stringify(body || {}),
    }).then(function (r) {
      return r.json().catch(function () { return {}; }).then(function (j) {
        if (!r.ok) throw new Error(j.error || 'Something went wrong (HTTP ' + r.status + ')');
        return j;
      });
    });
  }

  function field(label, name, autocomplete) {
    var l = el('label', {}, label);
    l.appendChild(el('input', { name: name, type: 'password', autocomplete: autocomplete, required: '' }));
    return l;
  }

  var ROLES = {
    admin: 'Administrator: everything, including setup and users.',
    operator: 'Operator: the dashboard, and dismissing alerts and failed runs.',
    viewer: 'Viewer: the dashboard only.'
  };

  function show(s) {
    card.textContent = '';
    var who = s.name || s.user || 'you';
    card.appendChild(el('h2', {}, s.must_change ? 'Choose a new password' : 'My account'));
    card.appendChild(el('p', { class: 'muted' }, s.must_change
      ? 'An administrator chose your password. Replace it before you continue.'
      : 'Signed in as ' + who + '. ' + (ROLES[s.role] || '')));
    var message = el('div', { class: 'msg bad', hidden: '' });
    card.appendChild(message);
    if (s.name) {
      var form = el('form');
      form.appendChild(field('Current password', 'current', 'current-password'));
      form.appendChild(field('New password', 'new', 'new-password'));
      form.appendChild(el('span', { class: 'hint' }, 'At least ' + (s.min_password || 12) + ' characters. A few words are fine.'));
      form.appendChild(field('Repeat new password', 'repeat', 'new-password'));
      var button = el('button', { type: 'submit' }, 'Change password');
      form.appendChild(button);
      form.addEventListener('submit', function (ev) {
        ev.preventDefault();
        message.hidden = true;
        var data = new FormData(form);
        if (data.get('new') !== data.get('repeat')) {
          message.textContent = 'The two passwords differ.';
          message.hidden = false;
          return;
        }
        button.disabled = true;
        post('api/auth/password', { current: data.get('current'), new: data.get('new') })
          .then(function () { location.href = '/'; })
          .catch(function (err) {
            message.textContent = err.message;
            message.hidden = false;
            button.disabled = false;
          });
      });
      card.appendChild(form);
    } else {
      card.appendChild(el('p', { class: 'muted' }, 'This sign-in comes from a provider, so there is no password to change here.'));
    }
    var out = el('button', { type: 'button', class: 'link' }, 'Sign out');
    out.addEventListener('click', function () {
      post('api/auth/logout').then(function () { location.href = '/login'; });
    });
    card.appendChild(out);
    if (!s.must_change) card.appendChild(el('p', { class: 'hint links' }, '')).appendChild(el('a', { href: '/' }, 'Back to the dashboard'));
  }

  fetch('api/auth/status', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.json(); })
    .then(function (s) {
      if (s.state === 'login') { location.href = '/login?next=/account'; return; }
      if (s.state !== 'signed_in') { location.href = '/'; return; }
      show(s);
    })
    .catch(function () {
      card.textContent = '';
      card.appendChild(el('p', { class: 'muted' }, 'Cannot reach the bridge.'));
    });
})();
