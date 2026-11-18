# Language Guide

## The idea

Spass describes continuous spatial properties: terrain height, density,
temperature, noise fields. It is a pure computation language. It does not
draw pixels, play audio, or manage network connections — external programs
consume the values it computes.

> The developer defines the meaning of the world; the compiler owns every
> symbolic conversion.

## Value domains

A numeric type may carry an explicit domain:

```spass
float[0.0, 380.0, 0.1]
```

The compiler proves the domain at compile time and encodes it on the ABI
boundary as an unsigned state index — `uint16_t` for the example above —
instead of a `double`. Range inference and bit-width allocation are exact and
use rational arithmetic, so the compiler is immune to floating-point drift.
The emitted machine code contains no runtime clamping or overflow checks.

ADD/SUB uses a tight step (`gcd` of the operands' steps). MUL uses a
conservative step: a few extra bits in exchange for O(1) inference and a
guarantee that no runtime value can overflow.

## Functions instead of arrays

A function is the unit of storage for 3D data. Cost is independent of sample
count, precision is unbounded, and there is no LOD switching or chunk
boundary management. External state is applied as parameterized differences
at call time; the base function stays unchanged.

```spass
func terrain_height(x: float[0.0, 380.0, 0.1], z: float[0.0, 380.0, 0.1]) -> float[0.0, 20.0, 0.01] {
    let h = fbm(x * 0.01, z * 0.01);
    return clamp(h * 20.0, 0.0, 20.0);
}
```

## Modules and visibility

`include` expands another source file into the current translation unit.
`fileclude` and `ownclude` functions get internal linkage so their symbols
never escape. `@tag` annotations and `@dispatch` let a call fan out over a
tag-matched, ordered set of functions at compile time.

## Tasks and sequences

- `@checkpoint name` / `@resume name` lower a function into a C or Rust state
  machine. The runner can suspend and resume it by stored state id.
- `spawn` and `parallel` map calls to worker threads.
- `sequence` blocks are launched by the runner and run in its resident world;
  `spass_module_start` starts them and `spass_module_wait` joins them.

## spasio channels

Programs read and write channels by name. Channels come in three modes:
file, socket, and memory. All data flows through the runner — the compiled
program never touches the filesystem or listens on a port for channel I/O.
External programs set and get channel values through the runner, so a
running program sees replaced values on its next read.

## Playground namespaces

The optional `s2d:`, `s3d:`, and `spio:` namespaces provide a software 2D
window and rasterizer, a z-buffered 3D pipeline with lighting, and native
file I/O. A module that does not call them contains no pixel or filesystem
code in its output.
