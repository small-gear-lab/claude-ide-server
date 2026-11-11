**An early release.** This is an unofficial project and is not affiliated with Anthropic; the `/ide` protocol that it speaks is
not documented by Anthropic, so it can break when Claude Code changes.

- **What is here.** `claude-ide-server-<version>-<target>`: the server, for Linux (x86_64 and aarch64; `-musl` is static and runs
  on any distribution, `-gnu` is dynamic and needs glibc 2.34 or newer, as in RHEL 9, Ubuntu 22.04 and Debian 12), macOS (Apple silicon
  and Intel) and Windows (x86_64); the files for systemd, launchd and Task Scheduler are in `contrib/`.
  `claude-ide-nvim-<version>.zip`: the Neovim adapter (Neovim 0.11 or newer). `SHA256SUMS`: the checksums of all of them.
- **How it is verified.** Each file carries a build provenance attestation (`gh attestation verify <file> --repo small-gear-lab/claude-ide-server`).
- **What was tried.** A real Claude Code (2.1.322) with the Neovim adapter, on Linux. macOS and Windows are tried in CI with
  stand-ins for Claude Code, not with a real one. Claude Code in WSL, with an editor on Windows, is not supported yet.
- **Install.** Put the executable on the `PATH` (or name it in the adapter's `server.command` option), unpack the adapter, add its
  directory to Neovim's runtime path and call `require('claude_ide').setup()`. See the README of the adapter in the archive.

