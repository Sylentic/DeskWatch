# Backup: export and import

Setup > **Backup** downloads your settings as one file and restores them on this or another install. It needs a
[data folder](data-dir.md), because that is where the settings the page saves live.

## What is in the file

| Part | From | Secret? |
| --- | --- | --- |
| Sources, broker, alerts, dashboard and update settings, switched-off sources | `sources.toml` | no (it only names token files) |
| GitHub and Microsoft sign-in rules | `oauth.toml` | no |
| Each person's own dashboard layout | `layouts.json` | no |
| Tokens, webhook secrets, OAuth client secrets | `credentials/` | **yes, only in a locked export** |

Settings kept in `bridge.toml` and credentials mounted by Docker or systemd are not in the file: they are yours to
back up with the rest of your deployment.

**Never exported, in any mode:** administrator, operator and viewer passwords, API keys and sessions (`auth.toml`).
Set those up again on the new install (Setup > Access). A password hash is a credential too, and a restore that
brought it back would put the old install's accounts on the new one without anyone choosing that.

## Export

- **Without secrets** (the default) is plain, readable JSON. It is safe to keep in a repository or send to a
  colleague. After a restore the sources show "token missing" until you enter their tokens again.
- **With secrets, locked by a password** also holds the stored tokens and client secrets, encrypted. The password
  needs at least 12 characters. The key comes from the password with Argon2id (64 MiB, 3 passes, random salt) and the
  secrets are sealed with XChaCha20-Poly1305; a changed file or a wrong password is refused. There is no way to
  recover a lost password, by design.

The file is named `deskwatch-<date>.deskwatch`.

## Import

Choose a file. The bridge opens it (asking for the password of a locked one), checks every part and shows what is
inside: sources by type, the broker, alerts, layouts, sign-in providers and the number of secrets. Nothing changes
until you press **Replace my settings**.

Replace means: `sources.toml` is replaced; `oauth.toml` and `layouts.json` are replaced when the file has them
(otherwise left alone); secrets in the file are written next to the existing ones (others are not deleted). A file
that does not parse, names a secret badly or comes from a newer DeskWatch is refused before anything is written.
Restart the bridge afterwards to use the new settings.

The same files can be copied by hand: they are the ones listed in [data-dir.md](data-dir.md).

## Security notes

Both actions are for administrators, like the rest of Setup, and need the same-origin and `X-DeskWatch` checks. A
plain export reads nothing from `credentials/`. Every download is logged (without contents).
