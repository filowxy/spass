# Status

- 305 workspace tests across the compiler, runner, installer, and playground crates.
- 19 example projects: 16 run end to end, 3 are build-only because their
  sequences wait on an external peer (`game`, `voxel`, `terrain_gen`).
- Verification scripts cover examples, playground pixels, deeppack loading,
  packaging consistency, runner output-handle isolation, and MSI install/run/uninstall.

Run `cargo test --release` from `compiler/`, then the scripts under
`compiler/scripts/`. See [Build-and-Test](Build-and-Test) for details.
