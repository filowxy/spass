# Spass — A Statically Typed Language for 3D Spatial Computation

Spass is a compiled, statically typed language for describing continuous
spatial properties — terrain height, density, temperature, noise fields.
A value domain like `float[0.0, 380.0, 0.1]` is proved at compile time and
crosses the ABI boundary as a `uint16_t` state index rather than a `double`.

> The developer defines the meaning of the world; the compiler owns every
> symbolic conversion. No runtime range checks, no warp votes, no per-sample
> branch divergence.

---

## What Spass does

### Compile-time analysis removes runtime work

Range inference and bit-width allocation happen in the compiler. The emitted
machine code carries no runtime clamping or overflow checks. A small state
index replaces a full-width floating-point value at every boundary.

```spass
func terrain_height(x: float[0.0, 380.0, 0.1], z: float[0.0, 380.0, 0.1]) -> float[0.0, 20.0, 0.01] {
    let h = fbm(x * 0.01, z * 0.01);
    return clamp(h * 20.0, 0.0, 20.0);
}
```

### Functions instead of arrays

3D data is modelled as functions, not grids. Storage cost is independent of
sample count, precision is unbounded, and there is no LOD switching or chunk
boundary management. External state is applied as parameterized differences
at call time — the base function never changes.

### Two artifact flavours

- **`.saple`** — a ZIP package holding the native module and its schema. The
  built-in runner loads it and owns the program's lifetime. External programs
  talk to the runner through the spasio protocol; they never load the library
  directly.
- **deeppack** — a shared library plus an ABI manifest, for hosts that need
  in-process calls with no runner.

### Task and state-machine runtime

`@checkpoint` / `@resume` lower into C or Rust state machines. `spawn` and
`parallel` map to worker threads. `sequence` blocks run in the runner-owned
resident world.

### Optional playground layer

The `s2d:` / `s3d:` / `spio:` namespaces add a software-rendered 2D window,
a z-buffered 3D rasterizer, and native file I/O. A program that does not call
them contains no pixel or filesystem code in its output.

---

## Build and test

```powershell
cd compiler
cargo build --release -p spass
cargo test --release
```

Build the CLI first: a bare `cargo build --release` fails because the
installer's build script embeds the finished compiler.

Verify the end-to-end examples and packaging:

```powershell
cd compiler
.\scripts\verify-examples.ps1
.\scripts\verify-packaging.ps1
.\scripts\verify-deeppack.ps1
```

Toolchain: `stable-x86_64-pc-windows-gnullvm` with `rust-lld`. The compiler
passes `-C linker=rust-lld` explicitly; the default MinGW linker is not
installed.

---

## Install

Prebuilt installers are described in [INSTALL.md](INSTALL.md):

- `spass-setup.exe` — one self-contained file, no admin rights.
- `spass-<version>-x64.msi` — per-user Windows Installer package, scriptable
  with `msiexec`.

---

## Repository layout

```
compiler/crates/    lexer, parser, semantic, infer, transform, optimize,
                    codegen, runner, cli, runtime, bootstrap-core,
                    installer, playground, bench
compiler/scripts/   measurement and verification scripts
examples/<feature>/ one project per language feature
installer/          WiX source and MSI build script
```

The Chinese language specification is the normative reference; two additive
documents define the playground namespaces and the deeppack artifact.
`AGENTS.md` is the repository map for coding agents.

---

## Status

305 workspace tests. 19 example projects: 16 run end to end, 3 are
build-only because their sequences wait on an external peer.
