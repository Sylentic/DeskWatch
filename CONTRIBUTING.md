# Contributing to DeskWatch

Thanks for helping. The easiest ways, from small to large:

1. **Try it and tell us.** Several parts were only tested against mock answers (see "Help testing" in the
   [README](README.md)). Open a ["I tested a source or feature"](https://github.com/Sylentic/DeskWatch/issues/new/choose)
   issue, also when it simply works.
2. **Report a bug or ask for a feature** with an issue. Questions and "how do I" go to
   [Discussions](https://github.com/Sylentic/DeskWatch/discussions).
3. **Send a pull request.** For anything bigger than a small fix, open an issue first so we agree on the direction.

## Before you post

- Remove tokens, passwords, hostnames, IP addresses and company names from logs, configs and screenshots.
- Say which version you run and how (Docker, Linux, Windows).

## Pull requests

- Branch from `dev` and open the pull request against `dev`. Only `dev` goes to `main`, and a release happens when
  `dev` is merged with a new version ([docs/releasing.md](docs/releasing.md)). Do not bump the version in a normal PR.
- Running from source, the demo and the checks to run before a pull request are in
  [docs/development.md](docs/development.md).
- Add a line under `Unreleased` in [CHANGELOG.md](CHANGELOG.md) for a change users can see.
- Texts in the app and the docs are in English.
- Never commit secrets, hostnames, IP addresses or client names.

## License

Contributions are under the same terms as the project: MIT OR Apache-2.0.
