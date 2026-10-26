# Ply sh

Rust shell interpreter used by the model-visible `bash` tool. Scripts are parsed and executed here; they are not forwarded to `/bin/bash` or another host shell.

- Shell syntax: quoting, expansions, assignments, functions, arrays, conditionals, loops, finite pipelines, command substitutions and redirections.
- Builtins and bundled utilities include echo, printf, test, cat, ls, grep, sed, find, sort, head, tail, wc and du. Unregistered commands return 127; host executables such as git/cargo/python are not automatically executed.
- All file IO goes through Ply's mounted storage, including memory/SQLite backends and read-only mounts. No old trace/gate subsystem is required.
- A new shell is created for each bash-tool call; state persists within that script, not between tool calls. Direct Rust Shell users may reuse a session.
- Pipeline stages buffer their output and run in sequence. Infinite producers and OS job control are not supported; background-list items use an isolated state but run synchronously.
- Native bash-tool calls use a wall-clock deadline and cooperative cancellation, a 1 MiB script limit and 1 MiB per shell buffer. Buffer overflow returns a nonzero exit and an explicit stderr diagnostic. Parser nesting is limited to 64 and arithmetic expressions to 256 tokens.

`tests/spec.rs` checks shell features and utility formatting; `tests/vs_bash.rs` compares supported behavior against the reference system bash (tests only). `tests/io_control.rs` validates namespace, read-only mounts, storage swaps and isolated sessions. System bash is used only as a differential test reference, not as this tool's execution backend.
