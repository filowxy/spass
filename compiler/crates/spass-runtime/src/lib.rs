//! Provides the C runtime source embedded into every compiled module.

/// The C source code of the runtime. Embedded at compile time so the
/// compiler binary is self-contained.
pub const RUNTIME_C: &str = include_str!("runtime.c");

/// Names of the runtime functions exposed to the linker.
pub const RUNTIME_SYMBOLS: &[&str] = &[
    // Task / state-machine runtime.
    "spass_task_new",
    "spass_task_retain",
    "spass_task_release",
    "spass_task_complete",
    "spass_task_resume",
    "spass_task_register",
    "spass_task_lookup",
    // Scalar math.
    "spass_sin",
    "spass_cos",
    "spass_tan",
    "spass_sqrt",
    "spass_abs",
    "spass_floor",
    "spass_ceil",
    "spass_round",
    "spass_exp",
    "spass_log",
    "spass_pow",
    "spass_lerp",
    "spass_clamp",
    "spass_smoothstep",
    "spass_max",
    "spass_min",
    "spass_noise_2d",
    "spass_fbm",
    "spass_hadamard_2",
    "spass_hadamard_3",
    "spass_hadamard_4",
    "spass_length_2",
    "spass_length_3",
    "spass_length_4",
    "spass_normalize_2",
    "spass_normalize_3",
    "spass_normalize_4",
    "spass_dot_2",
    "spass_dot_3",
    "spass_dot_4",
    "spass_cross_3",
    "spass_transpose_3",
    "spass_transpose_4",
    "spass_inverse_3",
    "spass_inverse_4",
    // spasio I/O.
    "spass_spasio_input",
    "spass_spasio_output",
    // Async task runtime.
    "spass_spawn",
    "spass_task_await",
    // Sequence message-passing runtime.
    "spass_seq_init",
    "spass_seq_send",
    "spass_seq_broadcast",
    "spass_seq_recv",
    "spass_seq_spawn",
    "spass_seq_wait_all",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_source_is_nonempty() {
        assert!(!RUNTIME_C.is_empty());
        assert!(RUNTIME_C.contains("spass_sin"));
    }

    /// socket mode is implemented in the C runtime
    /// as a client RPC to the runner (all modes unified; no file I/O
    /// and no bind listener left in the program process).
    #[test]
    fn runtime_implements_socket_mode() {
        assert!(RUNTIME_C.contains("spass_spasio_rpc"));
        assert!(RUNTIME_C.contains("spass_spasio_send_all"));
        assert!(!RUNTIME_C.contains("socket mode not yet implemented"));
        assert!(!RUNTIME_C.contains("fopen"), "direct file I/O still present");
        assert!(!RUNTIME_C.contains("listen("), "bind listener still present");
    }
}
