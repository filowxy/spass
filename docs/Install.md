# Install

Download both installers from the
[latest release](https://github.com/filowxy/spass/releases/latest).

Spass ships in two shapes:

- **`spass-setup.exe`** — a single self-contained installer with a window.
  It embeds the compiler and the self-hosted bootstrap source, so you can
  copy that one file anywhere and run it. No installer framework, no
  PowerShell. Choose **Just me** (no admin rights) or **All users** (prompts
  for elevation) in the window.
- **`spass-<version>-x64.msi`** — a Windows Installer package for the same
  payload. It installs for all users under Program Files, requires
  elevation, and is scriptable with `msiexec`.

## Install

### With `spass-setup.exe`

**Double-click `spass-setup.exe`.** A window opens where you first choose
**Just me** or **All users**, then review the install location, tick the
options you want, and press **Install**.

- **Just me** (default) installs to `%LOCALAPPDATA%\Programs\Spass`, adds that
  folder to your user `PATH`, and needs no administrator rights.
- **All users** installs to `%ProgramFiles%\Spass`, adds that folder to the
  system `PATH`, and registers the `.saple` association machine-wide. Windows
  asks for administrator rights; the installer then continues in an elevated
  window with your choices already filled in.

```
spass-setup.exe
```

**Open a new terminal afterwards** and check:

```powershell
spass --version
spass --help
```

To remove it, open the installer again and press **Uninstall** (or run
`spass-setup.exe --uninstall`).

### With the MSI

```powershell
msiexec /i spass-0.1.0-x64.msi      # install
msiexec /x spass-0.1.0-x64.msi      # uninstall
msiexec /i spass-0.1.0-x64.msi /qn  # silent install
msiexec /x {6B2D8F41-3C57-4A92-B0E6-71D4C8E5F309} /qn  # uninstall any build
```

The last form uses the product code instead of the file. It keeps working
after the MSI file has been rebuilt, because a rebuild keeps the product
code but gets a new package code — `msiexec /x <msi path>` then no longer
matches the install that the previous build of that file produced.

The MSI installs the same files to
`%ProgramFiles%\Spass`, adds that folder to the machine `PATH`,
and creates a Start Menu shortcut. It has no interface of its own:
`msiexec` shows its own progress bar, which is what you want for a
scriptable package. `spass-setup.exe` remains the installer with a full
interface.

## The window

| Control | What it does |
|---|---|
| **Just me / All users** | Who the install is for. Switching moves the default location between `%LOCALAPPDATA%\Programs\Spass` and `%ProgramFiles%\Spass`, and between the user and system `PATH`. |
| **Install location** | Where Spass is written. Edit it directly; **Reset to default** restores the default for the chosen scope. |
| **Add the install location to my PATH** | On by default, so `spass` works from any terminal. |
| **Also install the Rust toolchain** | On by default. `spass test` and `spass run` compile your Spass with `rustc`, so they need Rust. `spass build` does not. |
| **Progress** | Fills during the Rust download. |
| **Log** | Everything the installer did, including the checksum result. |
| **Install / Uninstall / Close** | Run an action, or exit. |

Confirmation prompts (replacing an existing install, removing files) appear
as normal dialog boxes. While an operation runs, the inputs are disabled and
the window refuses to close, so nothing is left half-installed.

## Command line

The same binary is also a full command-line tool. Any option means "run the
CLI" — the window only opens on a bare double-click or with `--gui`.

| Option | Effect |
|---|---|
| *(none, double-clicked)* | Open the window |
| `--gui` | Force the window even from a terminal |
| `--scope user\|machine` | Install for just you (default) or all users; `machine` elevates automatically |
| `--install-dir <DIR>` | Install elsewhere, e.g. `C:\Tools\Spass` |
| `--no-path` | Install without touching `PATH` |
| `--uninstall` | Remove the install and its `PATH` entry |
| `--install-rust` | Download the official Rust installer (rustup) |
| `--force`, `-y` | Overwrite without prompting |
| `--silent` | Never prompt (for scripted installs) |
| `--help`, `--version` | Usage / version |

Both front ends run the same install code, so they cannot drift apart.

## If you also need Rust

`spass build` works on its own, but `spass test` and `spass run` compile your
Spass code with `rustc`, so they need a Rust toolchain. The **Also install the
Rust toolchain** checkbox (or `--install-rust`) handles it: it downloads the
official `rustup-init.exe` from `static.rust-lang.org`, verifies it against
the published SHA-256, and then shows the commands to run. It never executes
the downloaded file without asking first.

Why the extra steps: a stock `rustup-init` defaults to the MSVC host, which
needs Visual Studio's `link.exe`, and the plain GNU host needs MinGW gcc.
Spass builds with the **`gnullvm`** target, which uses the `rust-lld` linker
bundled with Rust and therefore needs neither. So the full sequence is:

```powershell
rustup-init.exe -y --default-toolchain stable --profile minimal
rustup toolchain install stable-x86_64-pc-windows-gnullvm --component llvm-tools
rustup default stable-x86_64-pc-windows-gnullvm
```

The installer prints exactly these three lines with the right paths for you.

If you'd rather not use the download, the manual route is
[rustup.rs](https://rustup.rs) followed by the same two `rustup` commands.

## What gets installed

```
%LOCALAPPDATA%\Programs\Spass\
  spass.exe                    the compiler / runner CLI
  libunwind.dll                unwinder required by the Rust toolchain
  bootstrap\
    bootstrap.spass            the self-hosted numeric core (spec 12.4)
```

The C runtime is not a separate file: it travels as source inside every
module the compiler emits, so there is no runtime library to install.

## Building the installer yourself

From the repository root:

```powershell
.\package.ps1
```

This runs the test suite, builds the compiler in release, builds
`spass-setup.exe` with the payload embedded, and copies the result to
`dist\spass-setup.exe`.

Order matters: `crates/spass-installer/build.rs` embeds
`target/release/spass.exe` and fails with a clear message if it is missing.
`package.ps1` handles the ordering.

Skip the test run with `.\package.ps1 -SkipTests`.

### The MSI

The MSI is built separately, because it needs the WiX toolset:

```powershell
dotnet tool install --global wix --version 5.0.2
.\installer\build-msi.ps1
```

That stages the payload into `dist\msi-payload\` and writes
`dist\spass-<version>-x64.msi`. Pass `-SkipBuild` to reuse the existing
release build, and `-Version` to override the product version.

The WiX source lives in `installer\spass.wxs`. Two things about it are
deliberate:

- The install is **per machine** (`Scope="perMachine"`, under
  `%ProgramFiles%\Spass`), so it writes the machine `PATH` and HKLM and
  needs elevation. `spass-setup.exe` can do the same via its **All users**
  option, but defaults to a per-user install that needs no elevation.
- The package carries **no interface of its own**. `msiexec` shows its own
  progress bar. A hand-written UI sequence would need the `InstallExecute`
  action to block on the install script, and that action is not in WiX v5's
  standard library for the UI sequence; without it a modeless progress
  dialog closes while the files are still being copied.

## Using Spass

A project is any directory containing a `Spass.toml`. The compiler does **not**
search parent directories — with no `Spass.toml` in the current directory it
refuses to do anything.

The keys of `Spass.toml`, the progress output and the runner's lifecycle are
specified in `项目简介-CLI.md`.

```
my_project\
  Spass.toml        (may be empty)
  src\
    terrain.spass
  test\
    terrain_test.spass
```

```powershell
cd my_project
spass build              # compile to build/spassfile.saple
spass test               # run the tests in test/
spass run height 0.5 1.5
```

## Embedding Spass in your own program

`spass build --deeppack` additionally produces a shared library that your
program loads in-process:

```
build\spass_module.dll        the compiled module, exported symbols
build\spass_module.abi.json   the ABI manifest (types, value domains, structs)
build\libunwind.dll          the unwinder the library links against
```

Load it, resolve a symbol, and call it:

```c
HMODULE lib = LoadLibraryA("spass_module.dll");
double (*height)(double, double) =
    (double (*)(double, double))GetProcAddress(lib, "_spass_height_2_f_f");
double h = height(0.5, 1.5);
```

The manifest tells the host how to encode the arguments: a parameter with a
value domain crosses the boundary as an encoded state index, not as a
`double`. `libunwind.dll` has to be resolvable — keep it next to the library
or on `PATH`.

This is an addition, not a replacement: the `.saple` package and the
runner-mediated flow work exactly as before. A module loaded this way still
talks to the runner over the spasio protocol when it uses channels.

See `项目简介-Deeppack.md` for the full contract.

## Requirements after installing

- **The Rust toolchain is still needed.** `spass run` and `spass test` compile
  the project to Rust and invoke `rustc` on the result, so
  [rustup.rs](https://rustup.rs) must be present even though the installed
  binary is standalone.
- **A resident daemon locks `spass.exe`.** `spass run` uses a background daemon
  (spec 11.6) that holds the binary open. Run `spass daemon --stop` before
  reinstalling; the installer does this for you.
- **Uninstall is non-destructive.** It removes only the install directory and
  its own `PATH` entry, leaving any `%VAR%` references in your `PATH` intact.
