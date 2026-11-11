# claude-ide-launchd

launchd's socket activation on macOS, for [`claude-ide-server`](https://crates.io/crates/claude-ide-server) (unofficial, and not
affiliated with Anthropic). launchd hands a socket over by name, through `launch_activate_socket`, which is a call of C; this
crate wraps it, so that the rest of the server can forbid `unsafe`. It gives back owned file descriptors. On every other system
the crate is empty.

It is part of [the repository](https://github.com/small-gear-lab/claude-ide-server), and has no use on its own.

Licensed under either of the Apache License 2.0 or the MIT license, at your option.
