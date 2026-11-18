# Build and Test

## Toolchain

`stable-x86_64-pc-windows-gnullvm` with the bundled `rust-lld` linker. The
default `x86_64-w64-mingw32-clang` linker is not installed, so the compiler
passes `-C linker=rust-lld` explicitly when it shells out to `rustc`.

Add the rustup toolchain `bin` directory to `PATH` before running a built
module: `spass.exe` links the toolchain's `libunwind.dll`, which is not on
`PATH` otherwise.

## Build

```powershell
cd compiler
cargo build --release -p spass
```

Build the CLI first. A bare `cargo build --release` fails on a fresh clone:
`spass-installer`'s build script panics because it embeds the finished
`spass.exe`. This is expected, not a defect.

Do not run `cargo fmt`. The `rustfmt` component is not installed in this
toolchain and the codebase is hand-formatted; running the formatter would
produce a large, meaningless diff.

## Test

```powershell
cd compiler
cargo test --release
```

Lint with `cargo clippy`.

## End-to-end verification

```powershell
cd compiler
.\scripts\verify-examples.ps1
.\scripts\verify-playground.ps1
.\scripts\verify-deeppack.ps1
.\scripts\verify-packaging.ps1
.\scripts\verify-runner.ps1
.\scripts\verify-msi.ps1
```

`verify-examples.ps1` builds every project under `examples/` and runs one
exported function. Three projects (`game`, `voxel`, `terrain_gen`) are
build-only: their sequences block on a socket slot or an ack channel that
only an external peer can satisfy. The script enforces a timeout and starts
the daemon if it is not already up.

`verify-msi.ps1` installs, runs, and uninstalls the MSI, leaving the machine
as it was found.

## Generated-module build flags

The compiler invokes `rustc` directly for generated modules, so it does not
read the workspace `[profile.release]`. Flags passed deliberately:

| Flag | Reason |
| --- | --- |
| `-O` | optimisation level 3 |
| `-C codegen-units=1` | single code-generation unit |
| `-C embed-bitcode=no` | standalone `rustc` embeds bitcode by default; Cargo does not |
| `-C linker=rust-lld` | the default linker is not installed |

`-C lto` is deliberately not passed: the generated module is a single crate,
so there is no cross-crate graph.
