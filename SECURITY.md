# Security policy

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use
[private vulnerability reporting](https://github.com/Sylentic/DeskWatch/security/advisories/new) on this repository
instead. Describe what you found, how to reproduce it and which version you used. You will get an answer as soon as
the maintainer can, and a fix is released as a normal version with a note in the [changelog](CHANGELOG.md).

DeskWatch is a one-person project, so there is no bug bounty and no fixed response time.

## Supported versions

Only the latest release gets fixes.

## Useful to know

- Without a password the dashboard has no login. Keep it on your own network until you have set one
  ([docs/auth.md](docs/auth.md)) and, across untrusted networks, HTTPS ([docs/https.md](docs/https.md)).
- Secrets are read from credential files, never from the config file ([docs/data-dir.md](docs/data-dir.md)).
- Never paste tokens, passwords, hostnames or IP addresses into an issue.
