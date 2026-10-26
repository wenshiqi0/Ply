# Ply sh

Rust shell interpreter used by the model-visible `bash` tool. Scripts are parsed and executed here; they are not forwarded to `/bin/bash` or another host shell.

- Shell syntax: quoting, expansions, assignments, functions, arrays, conditionals, loops, finite pipelines, command substitutions and redirections.
- Builtins and bundled utilities include echo, printf, test, cat, ls, grep, sed, find, sort, head, tail, wc and du. Hosts may attach an ExternalCommands bridge; the model-visible bash tool uses it to resolve executable files through PATH, including git/cargo/lark-cli. Missing programs return 127 and non-executable files return 126. Standalone Shell::new has no host bridge.

The bridge preserves the launch process's PATH directory order and adds system, Homebrew and common user bin directories. It never loads bash/zsh startup files, aliases or functions. A child gets the internal shell's exported variables, not the host's full environment. Its default HOME points to the host account so installed CLIs can locate their own configuration.

Internal utilities, pipes and redirections keep using mounted IO. Native programs use a real disk cwd and the host filesystem: virtual paths in their arguments are not rewritten, and mount permissions do not sandbox a program's own filesystem access. Native processes remain inside the bash tool's approval/hook boundary, timeout and owning fiber; cancellation kills their process group. Pipeline stages remain buffered.
- Interpreter file IO goes through Ply's mounted storage, including memory/SQLite backends and read-only mounts. No old trace/gate subsystem is required.
- A new shell is created for each bash-tool call; state persists within that script, not between tool calls. Direct Rust Shell users may reuse a session.
- Pipeline stages buffer their output and run in sequence. Infinite producers and OS job control are not supported; background-list items use an isolated state but run synchronously.
- Native bash-tool calls use a wall-clock deadline and cooperative cancellation, a 1 MiB script limit and 1 MiB per shell buffer. Buffer overflow returns a nonzero exit and an explicit stderr diagnostic. Parser nesting is limited to 64 and arithmetic expressions to 256 tokens.

`tests/spec.rs` checks shell features and utility formatting; `tests/vs_bash.rs` compares supported behavior against the reference system bash (tests only). `tests/io_control.rs` validates namespace, read-only mounts, storage swaps and isolated sessions. System bash is used only as a differential test reference, not as this tool's execution backend.
