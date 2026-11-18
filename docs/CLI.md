# CLI

## Subcommands

```
spass build           scan src/, emit C and Rust, package as .saple
spass code-optimize   write optimised artifacts to build/optimized/
spass run <func> ...  recompile src/ and call <func> with the given args
spass runner <pkg>    load a .saple and call a function in its module
spass test            run the tests under test/
spass daemon          start / status / stop the spasio runner
spass clean           delete the build/ directory
spass tag [--list]    list @tag names in the project
spass help            print usage
```

## Project configuration

`Spass.toml` is read once from the current working directory.

| Key | Purpose | Default |
| --- | --- | --- |
| `build.output` | artifact directory, relative to project root | `build` |
| `build.timeout` | compile timeout in seconds | `120` |
| `spasio.data_dir` | file-mode channel directory | `data` |
| `spasio.socket_host` | runner address | `127.0.0.1` |
| `spasio.socket_port` | runner port | `9100` |
| `debug.log_level` | `error` / `warn` / `info` / `debug` | `warn` |
| `runner.idle_secs` | runner self-exit after idle seconds | `300` |

Precedence: **environment variable > project config > default**.

| Variable | Overrides |
| --- | --- |
| `SPASS_SOCKET_HOST` / `SPASS_SOCKET_PORT` | `spasio.socket_host` / `spasio.socket_port` |
| `SPASS_DAEMON_IDLE_SECS` | `runner.idle_secs` |
| `SPASS_DATA_DIR` | `spasio.data_dir` |

Unknown keys and unsupported TOML constructs are skipped, so a project using
an unfamiliar TOML feature still builds. A known key with an unusable value is
an error — it never silently falls back. A missing file is not an error.

`debug.log_level = "info"` (or `"debug"`) behaves like `--verbose` on every
invocation.

## Runner lifecycle

- `spass run` and `spass test` start a local runner when none is listening.
- The runner outlives the command that started it, so it can serve later
  calls and act as the endpoint for external programs.
- It exits after `runner.idle_secs` seconds idle, but not while a client is
  connected.
- `spass daemon --status` reports "not running" with exit code 0 when no
  runner is up. `spass daemon --stop` terminates it immediately.

### Output-handle isolation

The runner is a child process that outlives the CLI. Windows copies every
inheritable handle into a child created with `bInheritHandles`, and a shell
hands its pipes to the CLI — so a runner that inherits them would hold
`spass run | tee log` open until it exited on its own.

The runner therefore inherits no handles except its own standard handles,
which are bound to the NUL device. Clearing the inherit flag on the three
standard handles alone is not enough: the shell's pipes are separate handles.
See `compiler/crates/spass-cli/src/win_spawn.rs`; regression test:
`compiler/scripts/verify-runner.ps1`.

## Optimisation channel

Optimisation changes what the compiler computes, not how fast it runs, and is
off by default. "Off" means the output is bit-for-bit unchanged.

```
spass build --optimize                  # enable, standard artifacts
spass build --no-optimize               # force off
spass build --optimize-level 3          # choose a level
spass build --diagnostics               # list what changed
spass code-optimize --optimize-level 2  # separate channel: build/optimized/
```

`spass code-optimize` never overwrites the regular build. It writes the
optimised `module.c`, `module.rs`, compiled `module_rs.exe`, and an
`optimize-report.json` into `build/optimized/` so the two artifacts can be
compared side by side.

| Switch | Effect |
| --- | --- |
| `readonly_locals` | a `let` never reassigned is no longer mutable |
| `inline_single_use` | a binding used once moves to its use site |
| `fold_constants` | a binding with a compile-time-known initialiser becomes a literal |
| `const_fold_arith` | compile-time-known arithmetic folds to a constant |
| `dead_code_elim` | unreferenced, unexported functions are not emitted |

| Level | Enabled switches |
| --- | --- |
| 0 | none (default) |
| 1 | `readonly_locals` |
| 2 | level 1 + `inline_single_use` + `fold_constants` + `const_fold_arith` |
| 3 | level 2 + `dead_code_elim` |

`dead_code_elim` is only at level 3 because it makes a symbol disappear:
a host that looks functions up by name will not find it.
