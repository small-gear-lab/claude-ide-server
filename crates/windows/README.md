# claude-ide-windows

The calls to Windows that [`claude-ide-server`](https://crates.io/crates/claude-ide-server) needs (unofficial, and not affiliated
with Anthropic): whether a process is alive, who the user is (a name for a named pipe has the user's SID in it), and a named pipe
that only that user can reach. They need `unsafe`, which the rest of the server forbids, so they are here, each with its reason.
On every other system the crate is empty.

It is part of [the repository](https://github.com/small-gear-lab/claude-ide-server), and has no use on its own.

Licensed under either of the Apache License 2.0 or the MIT license, at your option.
