# AGENTS.md

Repository map for agents working on the Spass compiler. Everything here was
read off this repository; if an entry contradicts the code, the code wins and
this file is the thing to fix.

## What this repository is

Spass is a statically typed, value-domain-annotated language for 3D spatial
computation. The thesis is that compile-time analysis removes runtime work:
a value domain like `float[0.0, 380.0, 0.1]` is proved at compile time and
crosses the ABI boundary as a `uint16_t` state index rather than a `double`.

- `项目简介.md` is the language specification and is **normative**. It is in
  Chinese; the code and its comments are in English.
- `项目简介-Playground.md` and `项目简介-Deeppack.md` are **additive**
  extensions: they define the playground namespaces and the deeppack
  artifact without changing a clause of the normative spec.
- `task_plan.md`, `progress.md`, `findings.md` are the project log.
  `findings.md` records the self-hosting design decisions.

## Layout

```
compiler/crates/spass-lexer       tokenizer
compiler/crates/spass-parser      AST + parser (include expansion happens here)
compiler/crates/spass-semantic    symbol table, type checking, TypedModule
compiler/crates/spass-infer       range/value-domain inference, rational model
compiler/crates/spass-transform   if_present/@dispatch rewrites, emission plan
compiler/crates/spass-optimize    optional compile-time reductions (off by default)
compiler/crates/spass-codegen     Rust + C emitters, runtime prelude
compiler/crates/spass-runner      `.saple` reader, native-module loader, module ABI
compiler/crates/spass-cli         the `spass` binary (package name is `spass`)
compiler/crates/spass-runtime     the C runtime source, inlined into every module
compiler/crates/spass-bootstrap-core  self-hosting numeric core
compiler/crates/spass-installer   MSI packaging (not `.saple`)
compiler/crates/spass-playground  playground runtime appended to generated Rust
compiler/crates/spass-bench       criterion benchmarks (dev-dependency only)
compiler/scripts/                 measurement and example-verification scripts
examples/<feature>/               one project per feature
installer/                        WiX source + MSI build script
```

The repository root also contains loose `*.rs.txt` and `head_*.rs` files.
They are scratch comparison artefacts, not part of any build.

## Build

```powershell
cd compiler
cargo build --release -p spass
```

**A bare `cargo build --release` fails on a fresh clone.** `spass-installer`'s
build script panics with `spass.exe not found ... Build the compiler in release
mode first: cargo build --release -p spass`. The CLI has to exist before the
packaging crate can build. This is not a bug to fix; build the CLI first.

`compiler/target/` is **not** tracked (the `.gitignore` rule only took effect
after the old artifacts were untracked with `git rm -r --cached`). A build
directory in the repository is a trap: cargo reuses its artifacts, and a
half-written `.rlib` from an earlier run fails the link with
`failed to get bitcode from object file for LTO`.

## Test

```powershell
cargo test --release
```

305 tests across the workspace: 94 codegen, 68 semantic, 30 CLI, 29 parser,
29 optimise, 15 installer, 13 infer, 8 transform, 8 runner, 6 playground, plus
the bootstrap and runtime crates. `spass test` runs the end-to-end example
projects on top of that.

```powershell
cd compiler
.\scripts\verify-examples.ps1     # build + run every example project
.\scripts\verify-playground.ps1   # render a frame and assert the pixels
.\scripts\verify-deeppack.ps1     # host loads the DLL and calls it
.\scripts\verify-packaging.ps1    # the three packaging modes agree, function by function
.\scripts\verify-runner.ps1       # the runner does not hold the caller's output
.\scripts\verify-msi.ps1          # install, run, uninstall; the machine is left as found
```

`verify-examples.ps1` builds each `examples/*` project and runs one exported
function. Three of them (`game`, `voxel`, `terrain_gen`) are **build-only**:
their sequences block on a socket slot or an ack channel that only an
external peer can satisfy, so running them would hang. The script enforces a
timeout, starts the daemon (the spasio channels are runner-owned) if it is
not already up, and prepends the rustup toolchain's `bin` directory to
`PATH` — `spass.exe` links the toolchain's `libunwind.dll`, which is not
on PATH otherwise.


## Lint

```powershell
cargo clippy
```

Do **not** run `cargo fmt`. The `rustfmt` component is not installed in this
toolchain and there is no `rustfmt.toml`, so the codebase is hand-formatted.
Running the formatter would produce a very large, meaningless diff.

## Toolchain

`stable-x86_64-pc-windows-gnullvm`. The default linker
`x86_64-w64-mingw32-clang` is **not** installed, so every standalone `rustc`
invocation needs `-C linker=rust-lld`. That is why the compiler passes it
explicitly rather than relying on the default.

The compiler shells out to `rustc` to build generated modules, so it does not
read the workspace `[profile.release]`. The flags it passes are chosen
deliberately; see "Generated-module build flags" below.

## The self-hosting constraint — read this before editing codegen

`spass-bootstrap-core/build.rs` runs the **entire** parse → semantic → codegen
pipeline inside a build script, on a thread with a 512 MB stack because the
default stack overflows.

A change to the codegen can therefore break the compiler's own build, and the
failure surfaces as a build-script panic in a place that has nothing to do with
your change. If `cargo build` dies inside `spass-bootstrap-core`, suspect the
codegen you just touched.

## The `ponytail` marker

Comments throughout the codebase use `ponytail` to mean **"deferred
improvement, deliberately not done"** — a known gap with a stated reason. A
`ponytail` comment is not an undiscovered bug. Do not "fix" one as part of an
unrelated change, and do not report it as a defect.

## Generated-module build flags

The compiler invokes `rustc` directly for generated modules. The flags are:

| Flag | Why |
|---|---|
| `-O` | optimisation level 3 |
| `-C codegen-units=1` | single code-generation unit: better code, slower compile |
| `-C embed-bitcode=no` | standalone `rustc` embeds bitcode by default; Cargo does not |
| `-C linker=rust-lld` | the default linker is not installed |

`-C lto` is deliberately **not** passed: the generated module is a single crate,
so there is no cross-crate graph, and LTO is auto-disabled at
`codegen-units=1` anyway. `-C target-cpu=native` and `-C panic=abort` are
open decisions, not defaults — `target-cpu=native` produces a binary that only
runs on the build machine.

## The runtime prelude is sectioned

`spass-codegen` emits only the runtime a program actually uses.
`runtime_sections.rs` decides which of `vec_mat`, `spasio`, `tasks`,
`sequences` and `playground` a module needs, and both emitters gate on that
decision. The `playground` section alone is ~52 KB of Rust (~39 KB of C).

Two consequences for anyone changing the emitters:

- A runtime helper that is only emitted when its section is on is **not
  available to a program outside that section**. Adding a call to a
  sequence or spasio helper from a function in a sectionless module
  produces a generated module that does not compile.
- The detection is exact for `vec_mat`: it reuses `vec_mat_builtin` rather
  than matching builtin names, so `length` on a list does not pull in the
  vector runtime.

## The call index

`call_index.rs` maps a function name to its mangled symbol and its
per-parameter value domains. `Ctx` borrows it for the whole module, so a
call site costs one hash probe instead of rebuilding the mangled name and a
`Vec<ValueDomain>` (six `String` allocations per domain) per call
expression. The index resolves a duplicated name to the **last** definition,
which is what `TypedModule::func_index` does — keep the two in step.

## Two artifact flavours, one pipeline

`spass_codegen::RustFlavor` selects how the generated module is packaged:

- `Program` — a standalone binary with a `fn main()` dispatcher on `argv`.
- `Cdylib` — a shared library with `#[no_mangle]` on the public exports
  and no `main`. `spass build --deeppack` produces this.

`extern "C"` alone does **not** stop Rust from mangling the symbol, so the
cdylib flavour has to add `#[no_mangle]` itself. The dispatcher lives in
`emit_rust_dispatcher`, which only the program flavour calls.

The deeppack artifact is `build/spass_module.dll` plus
`build/spass_module.abi.json` — the manifest that tells the host each
symbol's C type, which parameters are `out` parameters, and the value
domain it has to encode with. `abi.rs` builds that description;
`spass-cli/src/deeppack.rs` turns it into the manifest and drives `rustc`.

The library exports three uniform entries: `spass_module_start` launches
the sequences, `spass_module_wait` joins them, and `spass_module_call`
invokes a function by its mangled name. A host that owns the module's
lifetime must call `wait` before unloading it — the sequence threads
belong to the module. `spass runner` does this; skipping it crashes on
exit (an access violation while the DLL is being unloaded).

The library links the toolchain's `libunwind.dll`, so the deeppack build
copies it next to the DLL. Without it the host's `LoadLibrary` fails with
`STATUS_DLL_NOT_FOUND` even though the library itself is present.

## The `.saple` carries machine code

`spass build` always compiles the cdylib, and the `.saple` is a ZIP that
holds it: `native/<platform>.<ext>` is **byte for byte** the same file as
`build/spass_module.dll`. `--deeppack` only adds the ABI manifest; it never
recompiles. Two containers, one machine code — so the two packaging modes
cannot drift apart in what they compute.

The package also carries the unwinder next to the module (the loader
resolves it there) and `native/source.c` as a reference for a host that has
to rebuild the module for another platform.

`spass-cli/src/saple.rs` writes the package; `spass-runner` reads it.

## The optimisation pass

`spass-optimize` runs between the semantic pass and the generator. It is
**off by default**, and "off" has to mean *the emitted module is the same
bytes* — so `optimise_module` returns `plan: None` when no switch is on,
and the generator falls back to the module it was given.

Two things about the pipeline are not obvious:

- **The `@dispatch` expansion happens before the optimiser.** `@dispatch`
  assigns to its target, so a reducer that ran first would not see the
  assignment and would fold the target away. `spass-transform::prepare`
  does the `if_present` parsing and the `@dispatch` expansion in one step,
  ahead of the optimiser; the generator only emits.
- **A folded float is an exact rational, not an IEEE-754 value.**
  `0.1 + 0.2` folds to `3/10`, which is not the double the runtime would
  have produced. That is why the pass is off by default and why
  `--diagnostics` has to list every reduction: the change has to be
  visible. Division is never folded, and integer arithmetic uses
  `checked_*` so an overflow is exposed rather than wrapped.

The switches live in **Spass's own per-user configuration**
(`%LOCALAPPDATA%\Spass\config.toml`, else `~/.config/spass/config.toml`),
not in the project's `Spass.toml`: a folded constant is not the same number
as the one the runtime would have produced, bit for bit, so the choice is
the user's, not the project's. A project `Spass.toml` that mentions an
`[optimize]` section is an **error**, not a silent no-op.

`spass code-optimize` is a separate channel that writes to
`build/optimized/` and never overwrites the ordinary artifacts.

## The saple runner

`spass-runner` loads a native module and calls into it. The module exports
three uniform entries, so the loader needs to know **no** Spass signature:

- `spass_module_start` — run the module's sequences;
- `spass_module_wait` — join them;
- `spass_module_call` — call one function by its mangled name.

The order is fixed: **start → call → join**. The join happens even when
the call failed — the error is reported after it. A module whose sequence
threads are still running when the library is unloaded is an access
violation, so `spass_module_wait` is not optional for a host that owns the
module's lifetime.

`spass runner <file.saple> [func] [args…]` is the one-shot form of the
runner, and it is what the `.saple` file association points at. The
resident form is `spass daemon`. The installers register the association
under `HKCU\Software\Classes`, so it never needs elevation, and both
installers remove it again on uninstall.

The library is loaded from the runner's cache directory
(`%LOCALAPPDATA%\Spass\run-cache`, `SPASS_RUNNER_CACHE` overrides it),
because a shared library has to be a file on disk before the loader can
load it.

## The progress readout

`spass-cli/src/progress.rs` draws the readout, `spass-cli/src/console.rs` is the
surface it draws on. Four things about it are not obvious:

- **A Windows console has a code page; a redirected stream has none.** The
  readout draws with typographic glyphs, so on Windows it writes through
  `WriteConsoleW`, which takes UTF-16 and cannot be mangled by the code
  page. Everywhere else the bytes are UTF-8.
- **A colour escape occupies characters but no cells.** `console::width`
  skips the escape sequences, or the line would be padded and erased with
  the wrong width and the previous frame would leave fragments behind.
- **The colour is opt-in.** `ENABLE_VIRTUAL_TERMINAL_PROCESSING` is off by
  default on a Windows console, so `console::supports_color` turns it on
  and reports whether the console took it. A console that refuses gets the
  plain glyphs, never the literal escape text.
- **The readout owns stderr while it runs.** `rustc`'s stderr is piped and
  relayed *after* the readout is erased — an inherited stderr would land
  on the animated line and the next frame would wipe the diagnostics out.
- **The animation thread waits on a channel, not on a sleep.** A sleep would
  make the CLI wait for the next frame before it could exit.

## The runner is a long-lived child of the CLI

The spasio channels are runner-owned (spec §9.2): the program talks to the
runner, the runner owns the slots. The daemon is the runner's CLI form, so
`spass run` / `spass test` start one when no runner is listening
(`ensure_runner` in `spass-cli/src/main.rs`).

Two consequences that are easy to get wrong:

- **The daemon must not inherit the caller's handles.** Windows copies every
  inheritable handle of the parent into a child created with
  `bInheritHandles`, which is what `Command` does. A daemon that inherits
  the caller's redirected stdout keeps that file locked and that pipe short
  of EOF for its whole life — `spass run | tee log` then looks like it hung
  until the daemon stopped on its own.

  Clearing `HANDLE_FLAG_INHERIT` on the three standard handles is **not**
  enough: a shell hands its own pipes down to the CLI, the CLI never names
  them, and they are inherited just the same. `win_spawn.rs` creates the
  daemon with `CreateProcessW` and `bInheritHandles = FALSE`, handing over
  only the NUL device as its standard handles. Every child that outlives
  the CLI goes through it — the daemon and the resident world.

- **The daemon must not outlive the session.** It exits after
  `SPASS_DAEMON_IDLE_SECS` (default 300) without a connected client. A
  client connection keeps it alive regardless, so a long-running program
  that holds a channel is never cut off. `spass daemon --stop` ends it
  now; `spass daemon --status` reports the window and answers "not
  running" instead of a connection error.

`compiler/scripts/verify-runner.ps1` checks both: it runs the CLI with its
output on a pipe and reads that pipe to the end, which is exactly the
situation the leak used to break.

## The playground is an opt-in layer

`s2d:` / `s3d:` / `spio:` are the playground namespaces: a software 2D
rasterizer, a z-buffered 3D rasterizer, and native file I/O. The runtime
is ~52 KB of Rust and is linked into a module **only** when the program
calls one of those namespaces.

Two things to know before touching it:

- The normative spec (`项目简介.md` §1.5) says Spass is not a renderer and
  has no unrestricted file access. The playground is an *opt-in* layer:
  a program that does not use those namespaces has neither, so §1.5 still
  holds for the default language. `项目简介-Playground.md` states this
  explicitly and adds the namespace reference.
- "It compiles" is not evidence that it draws. `scripts/verify-playground.ps1`
  renders a frame headless and asserts the pixels that landed in the
  framebuffer.

## The C playground backend cannot be compiled here

`playground.c` is the C backend of the playground runtime, and it mirrors
`playground.rs` function for function. **There is no C compiler on this
machine** — no `gcc`, no `clang`, no `cl`, and the `gcc` that rustup ships
in the gnu toolchain has no `cc1`, so it can link but not compile. The C
backend is therefore only ever *structurally* verified.

`spass-playground/src/surface.rs` is the guard. It holds the language
surface (every `s2d:` / `s3d:` / `spio:` name and its parameter count) in
one place, and the tests check:

- both backends export exactly that surface, with the same arity;
- the C backend never calls an `spg_*` helper it has not defined yet
  (C has no forward declarations there, so that is a compile error);
- the C backend calls nothing outside itself that is not on an explicit
  list, and includes the headers that declare what it calls (a missing
  header is a compile error too, and the list is where that gets
  noticed);
- both sources balance their brackets;
- the semantic layer registers exactly that surface, with the same arity
  (`spass-semantic` checks this against the same table).

The checks have caught real defects — a helper called before its
definition, a parameter count that disagreed with the runtime, and two
missing headers (`<direct.h>` for `_mkdir`, `<limits.h>` for `LONG_MAX`)
that would have failed the C build on every platform. Read
`surface.rs` before adding a builtin: the table, both runtimes and the
semantic layer all have to move together.

## The unit tests do not compile the generated module

The codegen tests assert on the *emitted text*. Nothing in `cargo test`
runs `rustc` over a generated module, so a codegen change can pass every
unit test and still produce a module that does not compile — a wrong
type in the runtime prelude, for instance.

`verify-examples.ps1` is what catches that: it builds every example and
runs `rustc` over the generated Rust. Run it after any codegen or
runtime-prelude change, not just `cargo test`.

## Benchmarks

```powershell
cd compiler
cargo bench -p spass-bench
```

`spass-bench` is a dev-dependency crate with criterion benchmarks for the
parse, analyze, codegen and full-pipeline stages, over the
`examples/bootstrap` and `examples/terrain` sources.

Criterion's `change:` line compares against the previous run on the same
machine, which is **not** a stable baseline: the machine drifts by more than
most of the effects being measured. Compare ratios within a single session
(`codegen/c ÷ parse`) rather than absolute numbers across sessions.

`scripts/measure.ps1` measures wall-clock times for cold direct, warm direct
and daemon-backed runs against a throw-away copy of a project.


## Conventions

- Conventional commits: `feat:`, `fix:`, `docs:`, `chore:`.
- Code comments are English; the specification documents are Chinese.
- Comments state the invariant or the non-obvious constraint. They do not
  narrate what bug was fixed or how the code used to be wrong — that history
  belongs in the commit message.
- When something is awkward to use, **change the code and the design, not the
  test**. A test that has to be weakened to pass is a design defect that is
  still there. Say what was awkward and what changed instead.
- The generated Rust prelude is inlined into every generated module, so
  `spass-codegen` is a hot path for compile time as well as for the compiler
  itself.
