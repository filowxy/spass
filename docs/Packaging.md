# Packaging

Spass emits two artifact flavours from the same compiled module.

## `.saple`

A `.saple` is a ZIP container that a file manager can open. It holds the
native machine code for each supported platform, a manifest describing the
functions and their ABI encoding, the C source for hosts that need to
rebuild, and a `module.sps` schema of functions, domains, and channels.

The built-in runner loads a `.saple`, picks the native module for the current
platform, and calls into it. The module exports three uniform entry points
rather than one symbol per function:

- `spass_module_start` — run the declared sequences,
- `spass_module_wait` — wait for them to finish,
- `spass_module_call` — call one function by its mangled name.

External programs talk to the runner through the spasio protocol. They never
load the library inside the package directly. This keeps the runner as the
single trust boundary: it clamps out-of-range inputs before they enter the
compiled computation.

## deeppack

`spass build --deeppack` emits three files next to the normal build output:

- `spass_module.dll` — the compiled module as a Rust `cdylib`, with public
  functions exported under their exact symbol names,
- `spass_module.abi.json` — the manifest: C type of each parameter, which
  parameters are `out`, and the value-domain encoding a host must apply,
- `libunwind.dll` — the Rust toolchain's unwinder.

A host loads the library in-process and calls the symbols through the C ABI.
The host is the trust boundary here; the runner's out-of-range clamping does
not apply.

## Consistency

Both containers carry the same machine code. `compiler/scripts/verify-packaging.ps1`
calls every exported function three ways — through the `.saple` dispatcher,
through the runner, and through deeppack — and asserts the numbers agree.
`compiler/scripts/verify-deeppack.ps1` proves the host can load the library
and get correct results back.
