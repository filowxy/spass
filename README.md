# Spass — A Statically Typed Language for 3D Spatial Computation

Spass is a compiled, statically typed language for describing continuous
spatial properties — terrain height, density, temperature, noise fields.
A value domain such as `float[0.0, 380.0, 0.1]` is proved at compile time and
crosses the ABI boundary as a `uint16_t` state index rather than a `double`.

> The developer defines the meaning of the world; the compiler owns every
> symbolic conversion. No runtime range checks, no warp votes, no per-sample
> branch divergence.

3D data is modelled as functions, not grids: storage cost is independent of
sample count, precision is unbounded, and there is no LOD switching or chunk
boundary management. The compiler emits native modules that external programs
call through a runner, or load in-process as a shared library.

## Install on Windows

Download from the [latest release](https://github.com/filowxy/spass/releases/latest):

- `spass-setup.exe` — self-contained installer; choose just-me or all-users in the window.
- `spass-0.1.0-x64.msi` — per-machine Windows Installer package, scriptable with `msiexec`.

## Documentation

All usage documentation — building, installing, the CLI, packaging, the
language guide, and the repository layout — lives in [docs/](docs/Home.md).
