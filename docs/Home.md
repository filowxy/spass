# Spass Wiki

Spass is a compiled, statically typed language for continuous 3D spatial
computation. A value domain such as `float[0.0, 380.0, 0.1]` is proved at
compile time and crosses the ABI boundary as a `uint16_t` state index rather
than a `double`.

This wiki holds all usage documentation. The repository README only states
what Spass is.

## Pages

- **Build-and-Test** — toolchain, release build, workspace tests, example verification.
- **Install** — the self-contained installer and the MSI package.
- **CLI** — subcommands, project config, progress readout, daemon lifecycle.
- **Packaging** — `.saple` and deeppack artifacts and how they agree.
- **Language-Guide** — value domains, functions, modules, tasks, playground namespaces.
- **Repository-Layout** — crate map and where to find each stage.
- **Status** — test counts and example coverage.

## Quick start

```powershell
cd compiler
cargo build --release -p spass
cargo test --release
```

See [Build-and-Test](Build-and-Test) for the full procedure.
