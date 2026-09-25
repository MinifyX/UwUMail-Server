# Contributing

Thanks for wanting to help! (◕‿◕✿)

## Please read this first

UwUMail Server is a hobby project I build for myself, just for fun (see
[Why this exists](README.md#why-this-exists)). That means:

- Issues and pull requests are welcome, but I might answer late or not at all,
  and I may say no to things I don't need or don't want in the server. No hard
  feelings either way.
- Want it to go somewhere else? Fork it, that's what the license is for.
- Almost all of the code here is written with AI (Claude), so using AI for
  your contribution is fine too. Just make sure it builds and the tests pass.

## Ground rules

- Read [docs/vision.md](docs/vision.md) and [docs/architecture.md](docs/architecture.md) first.
- Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` before opening a pull request.
- Protocol changes need an end-to-end test (see `crates/uwumail-smtp/tests/integration/flow.rs`).
- Code, comments and docs are English. Texts users read exist in German and
  English, in a playful and a neutral tone.
- Examples, tests, sample data and placeholders only use names nobody can own:
  `example.com`, `example.net`, `example.org` and names under `.example`,
  `.test`, `.invalid` or `.localhost` (RFC 2606, RFC 6761), and addresses from
  the documentation networks `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`
  and `2001:db8::/32`. Never an invented name under a real ending like
  `example.de`: someone owns it, and mail sent there by mistake reaches them.
  Real names are only for things that really are them: providers, blocklists,
  public suffixes.
- Security issues: please don't open a public issue, see [SECURITY.md](SECURITY.md).
- By contributing you agree that your work is licensed under the AGPL-3.0.
