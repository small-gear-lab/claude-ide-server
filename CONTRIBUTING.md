# Contributing

Thank you for looking at this. A few things are different about this project, and they are written here so that
nobody has to guess.

## This is an unofficial project, and what it says about Claude Code comes from observation

`claude-ide-server` is not made by Anthropic and is not endorsed by them. The `/ide` protocol that it speaks is not
documented by Anthropic. Everything that this project says about what Claude Code does (what it sends, what it
expects, what it drops, how it finds an editor) is one of these, and should say which:

- **Seen.** Observed with a real Claude Code, in a terminal, against the server. Say the version (`2.1.322`, for example).
- **Read.** Taken from the code of a released Claude Code or of an existing integration (the VS Code extension, the
  JetBrains plugin), or from a public write-up. Say that it was read and not tried.
- **Not tried.** Said so, in the place where it is said.

A guess is not written as a fact. If you are not sure, say what you saw, or leave it out. The documentation keeps
these apart on purpose (`docs/DESIGN.md`, "What a real CLI sends" and the sections after it; the provenance of each
fixture is in `crates/test-support/fixtures/README.md`), and a shape in a fixture changes when a new observation says it
should, not to make a test pass. Claude Code changes: a version number is part of the fact.

## Commit messages

- They are in **English**. This is a public project, and the history is read by people who do not read Japanese.
- The first line says **what the change does**, in a full sentence, and it may be long: it is what `git log --oneline`
  shows, and it should be enough to tell commits apart. Not the name of a branch, and not "merge" or "fix".
- The body says **why** (what was wrong, or what was found), and what a reader would not see from the diff.
- **One topic in a commit**, and every commit builds and passes the checks below on its own, so that `git bisect`
  works. If a fix belongs to an earlier commit of the same series, it goes into that commit (`git commit --fixup`
  and `git rebase -i --autosquash`).
- The history is **linear**: `main` is fast-forwarded to a series that has been checked, and no merge commits are made.

## Documentation, and the Japanese version

The documents (`README.md`, `docs/DESIGN.md`, `docs/NVIM.md`, `docs/index.md`) are written in **English**. The Japanese
version (`README.ja.md`, `docs/ja/`) is not written by hand: it is **built** from the gettext catalogs in
`docs/locale/ja/LC_MESSAGES/` by `tools/build-docs.sh`, and the Japanese Markdown that is built is **checked in**, and has
to be current: the `docs` workflow runs `git diff --exit-code -- docs/ja README.ja.md` after it builds.

You need [`uv`](https://docs.astral.sh/uv/) (the script runs Sphinx with the packages in `docs/requirements.txt`).

When you change an English document:

1. `tools/build-docs.sh update` refreshes the catalogs (`docs/locale/ja/LC_MESSAGES/*.po`) from the English text. A
   changed paragraph comes back as a **fuzzy** entry (with the old translation, marked `#, fuzzy`) and a new one with an
   empty translation.
2. Translate each of them, and take the `fuzzy` mark off. Keep to the words that the existing entries use for the same
   thing, so that the Japanese reads as one text.
3. `tools/build-docs.sh` builds the HTML (`docs/_build/html/en` and `.../ja`) and writes the Japanese Markdown again.
   Commit the changes of `docs/ja/`, `README.ja.md` and the catalogs together with the English change.
4. The code blocks of the README are not translated by gettext: their Japanese is in `docs/locale/ja/readme-code.json`
   (the English block is the key). A block that is not there stays in English, and the script says so.

If you do not read Japanese, change the English and say so when you send it: the Japanese can be filled in before it is merged.

## Building, checking and testing

Rust 1.88 or newer (`rust-version` in `Cargo.toml`); CI runs the least version, and the latest stable.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --workspace     # a broken link in the documentation is a bug
CLAUDE_IDE_REQUIRE_NVIM=1 cargo test --workspace --locked
cargo +1.88 test --workspace --locked                           # the least version
```

- The tests of the **Neovim adapter** run a headless Neovim (0.11 or newer; CI runs 0.11.7 and 0.12.6). Without `nvim`
  on the path they say so and pass, unless `CLAUDE_IDE_REQUIRE_NVIM` is set (CI sets it, so that a run cannot pass by
  skipping them). The parts of the adapter that need no server are also run by
  `nvim --headless -u NONE -l editors/nvim/tests/unit.lua` from the root of the repository.
- The tests use temporary directories for everything they write: the lock files, the sockets, the home of a Neovim.
  They never touch your own `~/.claude`.
- **Other systems.** The code for macOS and Windows is type-checked from Linux with a target, which catches most of what
  would break there (`rustup target add aarch64-apple-darwin x86_64-pc-windows-msvc`):

  ```sh
  cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings
  cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
  ```

  Running them is for CI (Ubuntu, macOS and Windows runners; see `.github/workflows/ci.yml`). Two tests need the real
  supervisor of the system and are skipped unless they are asked for: the test of the server under launchd
  (`CLAUDE_IDE_REQUIRE_LAUNCHD`, macOS) and the one under Task Scheduler (`CLAUDE_IDE_TRY_TASK_SCHEDULER=1`, Windows;
  a task cannot be given an environment, so it uses the paths of the user, and must not be run on a machine that has a
  server of its own).

## The code

- `docs/DESIGN.md` is the design: who talks to whom, the protocol between the server and an editor adapter, and the
  seams (the transport, the SDK, the file system and the environment). Read it before a change that crosses one.
- The workspace **forbids `unsafe`** (`unsafe_code = "forbid"`). The calls into the system that need it are in two small
  crates, `claude-ide-launchd` and `claude-ide-windows`, each use with its reason written next to it. If something needs
  `unsafe`, it goes there, and what they give back is safe to use.
- What a system does differently is chosen by `cfg` at the edges (the sources of the adapter socket, the listener, the
  signals), not through the code. Paths and `file://` URIs of both Unix and Windows are in `crates/core/src/paths.rs`,
  which works on strings so that both are tested on every system.
- What the tests of the core and of the executable share is in `crates/test-support`.

## Releasing

A release is made by a tag, and a person publishes it.

1. The version is the one of `[workspace.package]` in `Cargo.toml`, the same for every crate. Change it there, run
   `cargo update --workspace` (so that `Cargo.lock` follows), and commit.
2. Read `.github/release-notes.md`, which is the head of the notes of every release: what was tried, with which Claude Code,
   and what was not. Change it if that has changed. Write each paragraph and each item on one line: the page of a release shows a line break of this file as a line break.
3. Tag the commit `v<version>` and push the tag. The workflow refuses a tag that is not the version of `Cargo.toml`.
4. The `release` workflow (`.github/workflows/release.yml`) builds the executable for each target, packs it and the Neovim
   adapter (`tools/package-release.sh`, which can be run by hand to see what a release holds), writes `SHA256SUMS`, makes a
   build provenance attestation of the files, and leaves a **draft** release (a prerelease only if the version has a suffix, as in `0.2.0-rc.1`).
5. Look at the draft: download one file, check it against `SHA256SUMS`, run it. Then publish it.

*Actions, release, Run workflow* does everything except the release, and the attestation, to see what a tag would make; the
files are kept for a week as the artifact `dist`.

## Reporting a problem

Say the version of Claude Code, of Neovim (or of the editor), of `claude-ide-server`, and the system. The server's log
(`claude-ide-server -v`, to standard error) is the first thing that helps. If it is about what Claude Code does, say
whether you saw it or read it.

## License

The project is licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT),
at your option. Unless you explicitly state otherwise, any contribution that you intentionally submit for inclusion in
the project, as defined in the Apache-2.0 license, is dual licensed as above, without any additional terms or conditions.
