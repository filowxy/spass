// Implements the CLI entry point for building, running, testing, and packaging Spass projects.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

mod body_config;
mod config;
mod console;
mod deeppack;
mod progress;
mod saple;
mod win_spawn;

/// The configuration of the project in the current directory.
///
/// Every command runs inside a project, so `Spass.toml` is read once.
/// A malformed configuration is an error: a typo in `build.timeout`
/// must not silently fall back to the default.
fn cwd_config_raw() -> Result<&'static config::Config, String> {
    static CFG: std::sync::OnceLock<Result<config::Config, String>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let root = std::env::current_dir().unwrap_or_default();
        config::Config::load(&root).map_err(|e| format!("{:#}", e))
    })
        .as_ref()
        .map_err(|e| e.clone())
}

/// The configuration of the project, or an error the command reports.
fn cwd_config() -> Result<&'static config::Config> {
    cwd_config_raw().map_err(|e| anyhow::anyhow!(e))
}

/// The configuration of the project, with the defaults when it cannot
/// be read. For the paths that must not fail - the runner address, for
/// instance, where a broken file still has to produce an address.
fn cwd_config_or_default() -> &'static config::Config {
    static DEFAULT: std::sync::OnceLock<config::Config> = std::sync::OnceLock::new();
    match cwd_config_raw() {
        Ok(cfg) => cfg,
        Err(_) => DEFAULT.get_or_init(config::Config::default),
    }
}

/// The configuration of an arbitrary project.
///
/// The daemon serves many projects, so it cannot use the
/// configuration of the directory it happens to run in.
fn config_of(project_root: &Path) -> config::Config {
    config::Config::load(project_root).unwrap_or_default()
}

/// The file-mode channel directory of a project.
///
/// The runner owns the channel slots, so resolving the directory is the
/// runner's business: the program itself never touches the file
/// system. The answer is cached because it is asked for on every
/// channel access.
///
/// `SPASS_DATA_DIR` overrides the project's `spasio.data_dir`, the same
/// way the environment overrides the socket address: a relative path is
/// resolved against the project, an absolute one is used as it is.
fn project_data_dir(project: &Path) -> PathBuf {
    static DIRS: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, PathBuf>>> =
        std::sync::Mutex::new(None);
    let mut guard = DIRS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(Default::default);
    if let Some(dir) = map.get(project) {
        return dir.clone();
    }
    let dir = match std::env::var_os("SPASS_DATA_DIR") {
        Some(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                project.join(dir)
            }
        }
        _ => config_of(project).data_dir(project),
    };
    map.insert(project.to_path_buf(), dir.clone());
    dir
}

/// Add the project's runner settings to a child process.
///
/// The program connects to the runner over TCP, and the address comes
/// from the project's `spasio.socket_host` / `spasio.socket_port`.
/// Exporting them keeps the program and the runner on one address -
/// the runner the CLI starts inherits them the same way.
fn with_runner_env(mut cmd: Command) -> Command {
    let (host, port) = daemon_addr_parts();
    cmd.env("SPASS_SOCKET_HOST", &host);
    cmd.env("SPASS_SOCKET_PORT", port.to_string());
    cmd
}

/// The same runner settings as [`with_runner_env`], as plain variables.
///
/// A child that is started with a restricted handle set does not go
/// through `Command`, so its environment is handed over as a list of
/// overrides instead.
fn runner_env_overrides() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let (host, port) = daemon_addr_parts();
    vec![
        (
            std::ffi::OsString::from("SPASS_SOCKET_HOST"),
            std::ffi::OsString::from(host),
        ),
        (
            std::ffi::OsString::from("SPASS_SOCKET_PORT"),
            std::ffi::OsString::from(port.to_string()),
        ),
    ]
}

/// The `PATH` that lets a child find the toolchain's `rustc`.
///
/// `rustc` linked with `rust-lld` on Windows produces a binary that
/// depends on `libunwind.dll` / `std-*.dll` from the toolchain's
/// `bin/`. Without that directory in `PATH` the child fails to start
/// with `STATUS_DLL_NOT_FOUND` (0xC0000135). We detect the path via
/// `rustup which rustc` so the helper is independent of any specific
/// rustup install location. `ponytail`: a more aggressive form would
/// also set `RUSTUP_HOME` / `CARGO_HOME`; the MVP only needs `PATH`.
fn toolchain_path() -> Option<std::ffi::OsString> {
    let toolchain_bin = rustup_toolchain_bin()?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut new_path = std::ffi::OsString::from(&toolchain_bin);
    if !path.is_empty() {
        new_path.push(";");
        new_path.push(path);
    }
    Some(new_path)
}

/// Build a `Command` that inherits the current process's environment
/// plus the rustup toolchain's `bin` directory prepended to `PATH`.
fn with_rust_env(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    if let Some(path) = toolchain_path() {
        cmd.env("PATH", path);
    }
    cmd
}

/// Resolve the active rustup toolchain's `bin` directory by asking
/// `rustup which rustc` where the rustc binary lives. Returns `None`
/// when rustup is unavailable (e.g. on a system where rustc is on
/// `PATH` but rustup itself is not - in that case the toolchain's
/// DLLs are also on `PATH` already, so we don't need to inject).
fn rustup_toolchain_bin() -> Option<PathBuf> {    let output = Command::new("rustup")
        .args(["which", "rustc"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The output is "<path>/bin/rustc<exe>". Walk up to the parent
    // of `bin/`. We can't rely on `.parent` chained twice because
    // the rustc path may contain trailing newlines/whitespace.
    let trimmed = stdout.trim();
    let bin_dir = Path::new(trimmed).parent()?;
    if bin_dir.file_name()? != "bin" {
        return None;
    }
    Some(bin_dir.to_path_buf())
}

#[derive(Parser, Debug)]
#[command(name = "spass", version, about = "Spass language compiler", disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Compile the project in the current working directory.
    Build {
        /// Only check for errors, do not write outputs.
        #[arg(long)]
        check: bool,
        /// Emit verbose progress.
        #[arg(long, short)]
        verbose: bool,
        /// Also emit the deeppack artifact: a shared library the host
        /// loads into its own process, plus the ABI manifest that
        /// describes the exported symbols.
        #[arg(long)]
        deeppack: bool,
        /// Turn the optimiser on. The switches come from Spass's own
        /// configuration; this only says that this build uses them.
        #[arg(long)]
        optimize: bool,
        /// Turn the optimiser off for this build, whatever Spass's own
        /// configuration says.
        #[arg(long)]
        no_optimize: bool,
        /// The optimisation level: 0 = off, 1 = conservative, 2 = standard,
        /// 3 = aggressive.
        #[arg(long, value_name = "LEVEL")]
        optimize_level: Option<u8>,
        /// Print what the optimiser did, item by item.
        #[arg(long)]
        diagnostics: bool,
    },
    /// Optimise the project and write the result to `build/optimized/`.
    ///
    /// This is a channel of its own: the optimised artifacts land
    /// beside the normal ones and never replace them, so a build that
    /// went through here can always be compared with one that did not.
    CodeOptimize {
        /// Emit verbose progress.
        #[arg(long, short)]
        verbose: bool,
        /// The optimisation level: 0 = off, 1 = conservative, 2 = standard,
        /// 3 = aggressive. Defaults to Spass's own configuration.
        #[arg(long, value_name = "LEVEL")]
        optimize_level: Option<u8>,
        /// Print what the optimiser did, item by item.
        #[arg(long)]
        diagnostics: bool,
    },
    /// Run a `.saple` package: load its native module and call a
    /// function in it.
    ///
    /// This is what the `.saple` file association points at.
    Runner {
        /// The `.saple` file to run.
        saple: PathBuf,
        /// The function to call. Defaults to the package's entry point.
        func: Option<String>,
        /// Positional numeric arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a Spass function with the given arguments.
    Run {
        /// Function name (e.g. `height` or `_spass_height_2_f_f`).
        func: String,
        /// Positional arguments (numbers or `"strings"`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Compile and run in the current process (no temp.exe).
        #[arg(long)]
        direct: bool,
    },
    /// Delete the build directory.
    Clean,
    /// List @tag names in the project.
    Tag {
        /// Group tags with their functions.
        #[arg(long)]
        list: bool,
    },
    /// Discover and run tests in `test/`.
    Test {
        /// Emit verbose progress.
        #[arg(long, short)]
        verbose: bool,
    },
    /// Start the daemon: listens on 127.0.0.1:9100 and
    /// serves `spass run` requests with a compiled-module cache.
    Daemon {
        /// Print the daemon status instead of starting one.
        #[arg(long)]
        status: bool,
        /// Stop a running daemon.
        #[arg(long)]
        stop: bool,
    },
    /// Print the full help message.
    Help,
}

fn main() {
    // self-hosting: the numeric core of the inference engine runs
    // the Spass implementation (bootstrap.spass, compiled at build time
    // into this binary), with the Rust expression as a bit-exact
    // cross-check. See spass_infer::bootstrap for the bridge semantics.
    //
    // The cross-check is a valuable invariant, but it is paid twice on
    // every production compile and cannot be turned off. Setting
    // `SPASS_DISABLE_BOOTSTRAP=1` skips installing the Spass core, so
    // the inference engine uses its Rust results alone; the compiler
    // output is unchanged either way.
    if !bootstrap_disabled() {
        spass_bootstrap_core::install();
    }
    let cli = Cli::parse();
    // Deeply recursive compilation passes (recursive-descent parsing,
    // expression type checking) overflow the 1MB default main-thread
    // stack on large sources like examples/bootstrap. Run the whole
    // pipeline on a big-stack thread - same fix spass-bootstrap-core's
    // build.rs uses (and rustc itself).
    let result = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(cli))
        .expect("spawn compiler thread")
        .join();
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("error: {}", e);
            // Print the source chain for debugging.
            for cause in e.chain().skip(1) {
                eprintln!("  caused by: {}", cause);
            }
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("error: compiler thread panicked");
            std::process::exit(101);
        }
    }
}

/// Whether the self-hosting cross-check is switched off by the
/// environment. The Spass numeric core is installed by default; set
/// `SPASS_DISABLE_BOOTSTRAP=1` to skip it, so the inference engine
/// runs on its Rust results alone.
fn bootstrap_disabled() -> bool {
    matches!(
        std::env::var("SPASS_DISABLE_BOOTSTRAP").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Build { check, verbose, deeppack, optimize, no_optimize, optimize_level, diagnostics } => {
            cmd_build(check, verbose, deeppack, optimize, no_optimize, optimize_level, diagnostics)
        }
        Cmd::CodeOptimize { verbose, optimize_level, diagnostics } => {
            cmd_code_optimize(verbose, optimize_level, diagnostics)
        }
        Cmd::Runner { saple, func, args } => cmd_runner(&saple, func.as_deref(), &args),
        Cmd::Run { func, args, direct } => cmd_run(&func, &args, direct),
        Cmd::Clean => cmd_clean(),
        Cmd::Tag { list } => cmd_tag(list),
        Cmd::Test { verbose } => cmd_test(verbose),
        Cmd::Daemon { status, stop } => cmd_daemon(status, stop),
        Cmd::Help => {
            use clap::CommandFactory;
            let mut c = Cli::command();
            c.print_help().ok();
            println!();
            Ok(())
        }
    }
}

// ============================================================================
// Build
// ============================================================================

fn cmd_build(
    check: bool,
    verbose: bool,
    deeppack: bool,
    optimize: bool,
    no_optimize: bool,
    optimize_level: Option<u8>,
    diagnostics: bool,
) -> Result<()> {
    let project_root = std::env::current_dir().context("getting cwd")?;
    let spass_toml = project_root.join("Spass.toml");
    if !spass_toml.exists() {
        anyhow::bail!("no Spass.toml in current directory ({})", project_root.display());
    }
    let cfg = cwd_config()?;
    let verbose = cfg.wants_verbose(verbose);
    let build_dir = cfg.build_dir(&project_root);
    let src_dir = project_root.join("src");

    // The optimiser's switches are Spass's own, not the project's.
    let opt = body_config::resolve(body_config::OptimizeRequest {
        enable: optimize,
        disable: no_optimize,
        level: optimize_level,
        diagnostics,
    })?;

    if cfg.wants_config_dump() {
        println!(
            "config: build={} timeout={}s data={} runner={}:{} log={:?}",
            build_dir.display(),
            cfg.build_timeout_secs,
            cfg.data_dir(&project_root).display(),
            cfg.socket_host,
            cfg.socket_port,
            cfg.log_level
        );
        println!(
            "optimiser: {} switches={:?} depth={}",
            body_config::describe(),
            opt.switches,
            opt.max_fold_depth
        );
    }
    if verbose {
        println!("scanning {}", src_dir.display());
    }

    let spass_files = collect_spass_files(&src_dir)?;
    if spass_files.is_empty() {
        anyhow::bail!("no .spass files found in {}", src_dir.display());
    }

    if verbose {
        println!("found {} .spass file(s)", spass_files.len());
    }

    // Parse, semantic, codegen for all files.
    let project = compile_sources(&spass_files, verbose, &opt)?;

    // The reductions that change what the program computes have to be
    // visible: a folded constant is not always the number the runtime
    // would have produced.
    if project.plan.is_some() {
        print_optimiser_report(&project.report, opt.diagnostics);
    }

    // self-hosting observability: how many numeric-core
    // computations were produced by the compiled bootstrap.spass
    // (and how many fell back to the Rust cross-check because an
    // operand left the f64-exact bridge domain).
    if verbose {
        let (calls, fallbacks) = spass_bootstrap_core::core_stats();
        println!(
            "bootstrap core: {} Spass call(s) adopted, {} Rust fallback(s)",
            calls, fallbacks
        );
    }

    if check {
        println!("check passed ({} function(s))", project.typed.functions.len());
        return Ok(());
    }

    fs::create_dir_all(&build_dir).context("creating build dir")?;

    // Emit C (the language-mandated backend).
    let c_path = build_dir.join("module.c");
    fs::write(&c_path, &project.c_source).context("writing C output")?;
    if verbose {
        println!("emitted {}", c_path.display());
    }

    // Emit Rust (used by `spass run` and as a fallback executable).
    let rs_path = build_dir.join("module.rs");
    fs::write(&rs_path, &project.rust_source).context("writing Rust output")?;
    if verbose {
        println!("emitted {}", rs_path.display());
    }

    let module_name = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("spass_module");

    // Emit manifest.json. The same fields go into the package's
    // manifest, so the loose artifact a user inspects and the one the
    // runner reads cannot describe different modules.
    let manifest = saple::manifest_base(module_name, &project.typed);
    let manifest_path = build_dir.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .context("writing manifest")?;

    // Emit module.sps: the deterministic schema (functions, value
    // domains, bit widths, structs and channels).
    let sps_path = build_dir.join("module.sps");
    fs::write(&sps_path, saple::module_sps(&project.typed, module_name))
        .context("writing .sps")?;

    // The native module: the `.saple` carries machine code, so the
    // shared library is part of the build. The same library is the
    // deeppack artifact - one build, two containers.
    let lib_path = deeppack::build_library(
        &project.typed,
        project.plan.as_ref(),
        &build_dir,
        verbose,
    )?;

    // Package the.saple (ZIP): the machine code and the manifest that
    // describes it.
    let saple_path = build_dir.join("spassfile.saple");
    let mut natives: Vec<(String, Vec<u8>)> = Vec::new();
    natives.push((
        spass_runner::platform_tag(),
        fs::read(&lib_path).context("reading the native module")?,
    ));
    // The unwinder travels with the module: the loader resolves it next
    // to the library, and a module without it does not load at all.
    let unwind = build_dir.join("libunwind.dll");
    if unwind.exists() {
        natives.push((
            saple::UNWINDING_TAG.to_string(),
            fs::read(&unwind).context("reading the unwinder")?,
        ));
    }
    package_saple(&saple_path, module_name, &project.typed, &project.c_source, natives)?;

    // The deeppack artifact: the manifest that describes the exported
    // symbols for a host that loads the library into its own process.
    // The library itself is already there - the `.saple` and the
    // deeppack are the same machine code in two containers.
    let mut deeppack_lib = None;
    if deeppack {
        deeppack::write_manifest(&project.typed, &build_dir)?;
        deeppack_lib = Some(lib_path.clone());
    }

    println!(
        "{}",
        ansi::green_bold("Build successful")
    );
    println!("  artifacts: {}", build_dir.display());
    println!("  saple:     {}", saple_path.display());
    if let Some(lib) = deeppack_lib {
        println!("  deeppack:  {}", lib.display());
        println!("  manifest:  {}", build_dir.join(deeppack::MANIFEST_NAME).display());
    }
    println!("  functions: {}", project.typed.functions.len());

    Ok(())
}

#[derive(Default)]
struct CompiledProject {
    rust_source: String,
    c_source: String,
    typed: spass_semantic::TypedModule,
    /// The optimiser's plan, when it ran. `None` means the module is
    /// emitted exactly as it was before the optimiser existed.
    plan: Option<spass_transform::plan::ModulePlan>,
    /// What the optimiser did.
    report: spass_optimize::OptReport,
}

// ============================================================================
// Build inputs
//
// A build cache must key on the files the build actually reads. A
// directory listing of `src/` misses every file pulled in through
// `include` from outside `src/`, and a cache keyed on the listing
// alone serves stale code that touching the `src/` files cannot fix.
// The parser reports the transitive set of files it read, and both
// the daemon's cache and the local `--direct` cache consume it.
// ============================================================================

/// One build input: a file and the stamp that says whether it
/// changed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct InputFile {
    path: PathBuf,
    /// Modification time in nanoseconds since the Unix epoch.
    mtime_ns: u64,
    /// Size in bytes. A second guard for filesystems with a coarse
    /// modification-time resolution.
    size: u64,
}

/// The source files a build depends on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BuildInputs {
    /// Every file the build reads: the project's `src/` files plus
    /// every file reached through `include`.
    files: Vec<InputFile>,
    /// The project's `src/` files, kept separately so a file added to
    /// or removed from `src/` invalidates the build even when no
    /// existing file changed.
    src_files: Vec<PathBuf>,
    /// The configuration the build ran with.
    ///
    /// A cache that only keys on the source files serves a stale
    /// artifact when the configuration changed: turning the optimiser on
    /// produces a different module, and not one source file moved.
    flags: Vec<String>,
}

/// The stamp of a file: modification time and size.
fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// The canonical path of a file, falling back to the path itself.
fn canonical_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Collect the build inputs of a project: the `src/` files and every
/// file the parse reached through `include`.
fn build_inputs(
    src_files: &[PathBuf],
    included: &[PathBuf],
    flags: Vec<String>,
) -> Result<BuildInputs> {
    let mut files: Vec<InputFile> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut push = |path: &Path| -> Result<()> {
        let canonical = canonical_or_self(path);
        if !seen.insert(canonical.clone()) {
            return Ok(());
        }
        let (mtime_ns, size) = file_stamp(&canonical)
            .ok_or_else(|| anyhow::anyhow!("cannot stat {}", canonical.display()))?;
        files.push(InputFile {
            path: canonical,
            mtime_ns,
            size,
        });
        Ok(())
    };
    for p in src_files {
        push(p)?;
    }
    for p in included {
        push(p)?;
    }
    let mut src_canonical: Vec<PathBuf> = src_files.iter().map(|p| canonical_or_self(p)).collect();
    src_canonical.sort();
    Ok(BuildInputs {
        files,
        src_files: src_canonical,
        flags,
    })
}

/// Whether the recorded inputs still describe the sources on disk.
///
/// This is the daemon's fast path: it needs no parse, because any
/// change to the include structure changes a `src/` file's
/// modification time.
fn inputs_unchanged(project: &Path, recorded: &BuildInputs, flags: &[String]) -> bool {
    let src_now = match collect_spass_files(&project.join("src")) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut now: Vec<PathBuf> = src_now.iter().map(|p| canonical_or_self(p)).collect();
    now.sort();
    if now != recorded.src_files {
        return false;
    }
    if recorded.flags != flags {
        return false;
    }
    recorded.files.iter().all(|f| {
        match file_stamp(&f.path) {
            Some((mtime_ns, size)) => mtime_ns == f.mtime_ns && size == f.size,
            None => false,
        }
    })
}

/// The local build cache: the recorded inputs of the last successful
/// build, next to the artifacts it produced.
fn local_cache_path(project: &Path) -> PathBuf {
    config_of(project)
        .build_dir(project)
        .join("spass_inputs.json")
}

fn read_local_cache(project: &Path) -> Option<BuildInputs> {
    let bytes = fs::read(local_cache_path(project)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_local_cache(project: &Path, inputs: &BuildInputs) -> Result<()> {
    let path = local_cache_path(project);
    let bytes = serde_json::to_vec_pretty(inputs).context("serialising build inputs")?;
    fs::write(&path, bytes).context("writing build inputs")?;
    Ok(())
}

fn compile_sources(
    files: &[PathBuf],
    verbose: bool,
    opt: &body_config::BodyConfig,
) -> Result<CompiledProject> {
    let (typed, _deps) = analyze_sources(files, verbose)?;
    // The optimiser runs between the semantic pass and the generator:
    // it sees the body the compiler is going to emit, and the generator
    // emits what the optimiser produced.
    let outcome = spass_optimize::optimise_module(&typed, &opt_config(opt))
        .map_err(|e| anyhow::anyhow!("optimiser: {}", e))?;
    let plan = outcome.plan.as_ref();
    let c_source = spass_codegen::generate_c_with_plan(&typed, plan).context("C codegen")?;
    let rust_source = spass_codegen::generate_rust_with_plan(
        &typed,
        spass_codegen::RustFlavor::Program,
        plan,
    )
    .context("Rust codegen")?;
    Ok(CompiledProject {
        typed,
        c_source,
        rust_source,
        plan: outcome.plan,
        report: outcome.report,
    })
}

/// The optimiser's configuration, in the form the optimiser takes it.
fn opt_config(cfg: &body_config::BodyConfig) -> spass_optimize::OptConfig {
    spass_optimize::OptConfig {
        switches: cfg.switches,
        max_fold_depth: cfg.max_fold_depth,
    }
}

/// The configuration fingerprint of a build.
///
/// Everything that changes the artifact without changing a source file
/// belongs here: the build cache keys on it, so a build that ran with
/// a different configuration never reuses the other's artifact.
fn build_flags(cfg: &body_config::BodyConfig) -> Vec<String> {
    opt_config(cfg).fingerprint()
}

/// Print what the optimiser did.
///
/// The reductions that change what the program computes have to be
/// visible: a folded constant is not always the number the runtime
/// would have produced, and the user is the one who has to know.
fn print_optimiser_report(report: &spass_optimize::OptReport, itemise: bool) {
    println!(
        "optimiser: {} read-only, {} inlined, {} folded, {} expressions folded",
        report.report.readonly.len(),
        report.report.inlined.len(),
        report.report.folded.len(),
        report.report.const_folded
    );
    if !report.eliminated.is_empty() {
        println!("optimiser: {} function(s) eliminated", report.eliminated.len());
    }
    if !itemise {
        return;
    }
    for name in &report.report.folded {
        println!("  folded:   {}", name);
    }
    for name in &report.report.inlined {
        println!("  inlined:  {}", name);
    }
    for name in &report.report.readonly {
        println!("  readonly: {}", name);
    }
    for name in &report.eliminated {
        println!("  dropped:  {}", name);
    }
}

/// Parse and type-check the project sources. Returns the typed module
/// and the transitive set of files the parse read.
fn analyze_sources(
    files: &[PathBuf],
    verbose: bool,
) -> Result<(spass_semantic::TypedModule, Vec<PathBuf>)> {
    use spass_parser::Parser;
    use spass_semantic::analyze_full;

    if verbose {
        for path in files {
            println!("  parsing {}", path.display());
        }
    }

    // a header that two top-level files both `include`
    // must be expanded exactly once across the whole project. Use
    // the project-wide entry point so the per-file fresh-set
    // behaviour of `parse_file_with_includes` doesn't trigger E012
    // on the second copy of the header.
    let parsed = Parser::parse_project_with_dependencies(files, &FsFileLoader)
        .context("parsing project sources")?;
    let result = analyze_full(&parsed.module).context("semantic analysis")?;
    print_warnings(&result.warnings);
    Ok((result.module, parsed.dependencies))
}

fn collect_spass_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("spass") {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

/// The `rustc` invocation that builds a generated module.
///
/// The compiler shells out to `rustc` directly, so the workspace's
/// `[profile.release]` never applies - the flags have to be passed
/// explicitly:
///
/// * `-O` is `-C opt-level=3`.
/// * `-C codegen-units=1` puts the whole crate in one code-generation
/// unit, which produces better code at the cost of a slower build.
/// * `-C embed-bitcode=no` - standalone `rustc` embeds bitcode by
/// default (Cargo does not), which costs compile time and binary
/// size for a module that is never linked again.
/// * `-C linker=rust-lld` - the default linker is not installed in
/// this toolchain.
///
/// `-C lto` is deliberately absent: the module is a single crate, so
/// there is no cross-crate graph to optimise, and LTO is disabled
/// outright at `codegen-units=1`.
fn rustc_module_command(rs_path: &Path, exe_path: &Path) -> Command {
    let mut cmd = Command::new("rustc");
    cmd.arg(rs_path)
        .arg("-O")
        .arg("--crate-type")
        .arg("bin")
        .arg("-o")
        .arg(exe_path)
        .arg("-C")
        .arg("codegen-units=1")
        .arg("-C")
        .arg("embed-bitcode=no")
        .arg("-C")
        .arg("linker=rust-lld");
    cmd
}

/// Compile a generated Rust module into a runnable binary.
///
/// The `rustc` invocation is the slowest step of every command, so it
/// gets a readout: a silent wait of a second or more is
/// indistinguishable from a hang.
///
/// `timeout_secs` is the `build.timeout` setting. A compiler that hangs
/// forever is worse than one that fails, so the child is killed when
/// the budget runs out.
fn compile_module(rs_path: &Path, exe_path: &Path, timeout_secs: u64) -> Result<()> {
    let mut readout = progress::Readout::start("compiling the module");
    let mut child = match rustc_module_command(rs_path, exe_path)
        // rustc's own words must not land on the readout's line, where
        // the next frame would wipe them out.
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("invoking rustc")
    {
        Ok(c) => c,
        Err(e) => {
            readout.failed();
            return Err(e);
        }
    };
    // Drain the pipe in the background: rustc must not block on a full
    // pipe, and the diagnostics are shown once the readout is done.
    let mut diagnostics = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut pipe, &mut text);
            text
        })
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                show_diagnostics(diagnostics.take(), &mut readout);
                readout.failed();
                return Err(e).context("waiting for rustc");
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            show_diagnostics(diagnostics.take(), &mut readout);
            readout.failed();
            anyhow::bail!("rustc timed out after {}s", timeout_secs);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let status = status.expect("the loop only breaks with a status");
    show_diagnostics(diagnostics.take(), &mut readout);
    if !status.success() {
        readout.failed();
        anyhow::bail!("rustc failed");
    }
    readout.done();
    Ok(())
}

/// Show what `rustc` had to say.
///
/// The readout is cleared first: its line is in the way, and the next
/// frame would wipe the diagnostics out.
fn show_diagnostics(
    handle: Option<std::thread::JoinHandle<String>>,
    readout: &mut progress::Readout,
) {
    let text = handle
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return;
    }
    readout.clear();
    let _ = std::io::stderr().write_all(text.as_bytes());
}

struct FsFileLoader;
impl spass_parser::FileLoader for FsFileLoader {
    fn load(&self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }
}

// ============================================================================
// Run
// ============================================================================

/// Compile `project_root/src` into a runnable dispatcher binary. Shared
/// by the direct `spass run` path and the daemon's cache-miss path.
///
/// The build is cached: when the sources are unchanged since the last
/// successful build, the previous binary is reused and neither the
/// re-emit nor the rustc invocation happens. `--direct` keeps meaning
/// "no daemon" but no longer means "recompile every time".
fn compile_runnable(project_root: &Path) -> Result<(spass_semantic::TypedModule, PathBuf)> {
    if !project_root.join("Spass.toml").exists() {
        anyhow::bail!("no Spass.toml in {}", project_root.display());
    }
    // The daemon serves many projects, so the configuration comes from
    // the project itself and not from the directory the daemon runs in.
    let cfg = config_of(project_root);
    let build_dir = cfg.build_dir(project_root);
    fs::create_dir_all(&build_dir).context("creating build dir")?;
    let src_dir = project_root.join("src");
    let spass_files = collect_spass_files(&src_dir)?;
    if spass_files.is_empty() {
        anyhow::bail!("no .spass files in src/");
    }

    // Parse and type-check. The parse also reports the transitive set
    // of files it read, which is what the build cache keys on.
    //
    // The optimiser's switches come from Spass's own configuration: the
    // run path has no command-line override, so it always agrees with
    // the configuration the daemon compiled with.
    let opt = body_config::resolve(body_config::OptimizeRequest::default())?;
    let flags = build_flags(&opt);
    let (typed, dependencies) = analyze_sources(&spass_files, false)?;
    let inputs = build_inputs(&spass_files, &dependencies, flags)?;

    let rs_path = build_dir.join("module.rs");
    let exe_path = build_dir.join("module_rs.exe");
    if let Some(recorded) = read_local_cache(project_root) {
        if recorded == inputs && exe_path.exists() {
            return Ok((typed, exe_path));
        }
    }

    let outcome = spass_optimize::optimise_module(&typed, &opt_config(&opt))
        .map_err(|e| anyhow::anyhow!("optimiser: {}", e))?;
    let rust_source = spass_codegen::generate_rust_with_plan(
        &typed,
        spass_codegen::RustFlavor::Program,
        outcome.plan.as_ref(),
    )
    .context("Rust codegen")?;
    fs::write(&rs_path, &rust_source)?;

    // The Rust program exposes functions via `pub extern "C" fn _spass_*`
    // and dispatches via argv[1] in `main`. So we don't need to dlopen;
    // we just compile the Rust program and run it.
    compile_module(&rs_path, &exe_path, cfg.build_timeout_secs)?;
    write_local_cache(project_root, &inputs)?;
    Ok((typed, exe_path))
}

fn run_symbol(exe: &Path, cwd: &Path, symbol: &str, args: &[String]) -> Result<()> {
    let mut cmd = with_runner_env(with_rust_env(exe));
    // Run with the project as cwd so spasio file-mode relative paths
    // resolve the same way they do in a direct invocation.
    cmd.current_dir(cwd);
    cmd.arg(symbol);
    for a in args {
        cmd.arg(a);
    }
    let output = cmd.output().context("running compiled module")?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }
    Ok(())
}

fn cmd_run(func: &str, args: &[String], direct: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("getting cwd")?;
    // `spass run` goes through the daemon by default;
    // `--direct` compiles and runs in the current process.
    if !direct {
        return cmd_run_via_daemon(&project_root, func, args);
    }
    run_direct(&project_root, func, args)
}

fn run_direct(project_root: &Path, func: &str, args: &[String]) -> Result<()> {
    let (typed, exe_path) = compile_runnable(project_root)?;

    // A program that talks to a channel needs the runner (the daemon is
    // its CLI form). `--direct` only changes how the function call is
    // dispatched, so the runner is started here if it is not up yet.
    if !typed.channels.is_empty() && !ensure_runner() {
        anyhow::bail!(
            "this program uses spasio channels, but no runner could be reached at {}\n  \
             help: start it with `spass daemon`",
            daemon_addr()
        );
    }

    // Resolve the requested function name. The user can pass either the
    // plain Spass name (e.g. `height`), the fully-mangled symbol, or a
    // bare tag that the function was declared with.
    let symbol = match resolve_symbol(&typed, func) {
        Ok(s) => s,
        Err(ResolveError::NotFound) => {
            anyhow::bail!("function or tag `{}` not found", func);
        }
        Err(ResolveError::AmbiguousTag(funcs)) => {
            anyhow::bail!(
                "tag `{}` is declared on multiple functions: {}\n  help: rename the tag or call the function by its explicit name",
                func,
                funcs.join(", "),
            );
        }
    };

    run_symbol(&exe_path, project_root, &symbol, args)
}

// ============================================================================
// Daemon
// ============================================================================
//
// `spass daemon` listens on 127.0.0.1:9100 and serves `spass run`
// requests. Frames reuse the header (4-byte big-endian
// length); the body is JSON. The daemon caches one compiled dispatcher
// per project: if no source mtime changed since the last request, the
// compile step is skipped entirely.
//
// `ponytail`: requests are served sequentially (compilation shells out
// to rustc anyway) and the cache is a single HashMap. Upgrade path: a
// worker pool + LRU eviction when many projects share one daemon.

/// The address of the runner.
///
/// The environment wins over the project's configuration, because the
/// daemon inherits the environment the CLI exports to it - that is how
/// a project with a non-default `spasio.socket_port` reaches a runner
/// that is listening there.
///
/// The CLI exports the address it *resolved*, not the raw configuration
/// value: a runner that bound a different port than the CLI connects to
/// is a runner nobody can reach.
fn daemon_addr_parts() -> (String, u16) {
    let cfg = cwd_config_or_default();
    let host = std::env::var("SPASS_SOCKET_HOST").unwrap_or_else(|_| cfg.socket_host.clone());
    let port = std::env::var("SPASS_SOCKET_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(cfg.socket_port);
    (host, port)
}

fn daemon_addr() -> String {
    let (host, port) = daemon_addr_parts();
    format!("{}:{}", host, port)
}

fn read_frame(stream: &mut std::net::TcpStream) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("frame too large");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(body)
}

fn write_frame(stream: &mut std::net::TcpStream, body: &[u8]) -> Result<()> {
    use std::io::Write;
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn daemon_exchange(req: &serde_json::Value) -> Result<serde_json::Value> {
    let mut stream =
        std::net::TcpStream::connect(daemon_addr()).context("cannot connect to daemon")?;
    write_frame(&mut stream, &serde_json::to_vec(req)?)?;
    let resp = read_frame(&mut stream)?;
    Ok(serde_json::from_slice(&resp)?)
}

#[derive(Clone)]
struct DaemonCacheEntry {
    typed: std::sync::Arc<spass_semantic::TypedModule>,
    exe: PathBuf,
    /// The build inputs this entry was compiled from: the `src/` files
    /// plus every file the parse reached through `include`. The
    /// daemon's cache and the local `--direct` cache share this one
    /// dependency set.
    inputs: BuildInputs,
}

/// The daemon's per-project state: the compiled-module cache and the
/// set of projects whose compile is currently in flight.
#[derive(Default)]
struct DaemonState {
    cache: std::collections::HashMap<PathBuf, DaemonCacheEntry>,
    /// Projects with a compile in flight. A second request for the
    /// same cold project waits for the first compile instead of
    /// starting a second one.
    compiling: std::collections::HashSet<PathBuf>,
}

fn daemon_state() -> &'static std::sync::Mutex<DaemonState> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<DaemonState>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(DaemonState::default()))
}

/// Signalled when a compile finishes, so that a request waiting on
/// the same cold project can re-check the cache.
fn daemon_state_cv() -> &'static std::sync::Condvar {
    static CV: std::sync::OnceLock<std::sync::Condvar> = std::sync::OnceLock::new();
    CV.get_or_init(std::sync::Condvar::new)
}

/// The compiled module for `project`, compiling it when the cache
/// missed or the sources changed.
///
/// The state lock is released across the compile: a compile takes
/// seconds, and holding the lock would serialise every other project
/// behind it.
fn daemon_module(project: &Path) -> Result<DaemonCacheEntry> {
    // The daemon serves many projects, so the optimiser's switches come
    // from Spass's own configuration and not from the directory the
    // daemon runs in.
    let opt = body_config::resolve(body_config::OptimizeRequest::default())?;
    let flags = build_flags(&opt);
    loop {
        {
            let state = daemon_state().lock().unwrap();
            if let Some(entry) = state.cache.get(project) {
                if inputs_unchanged(project, &entry.inputs, &flags) {
                    return Ok(entry.clone());
                }
            }
        }
        {
            let mut state = daemon_state().lock().unwrap();
            if !state.compiling.insert(project.to_path_buf()) {
                // Another request is already compiling this project.
                // Wait for it rather than compiling the same sources
                // a second time, then re-check the cache.
                let state = daemon_state_cv().wait(state).unwrap();
                drop(state);
                continue;
            }
        }
        let compiled = compile_runnable(project);
        let entry = {
            let mut state = daemon_state().lock().unwrap();
            state.compiling.remove(project);
            match &compiled {
                Ok((typed, exe)) => {
                    // `compile_runnable` records the build inputs it
                    // compiled from; the daemon's cache and the local
                    // `--direct` cache key on the same set.
                    let inputs = read_local_cache(project).unwrap_or(BuildInputs {
                        files: Vec::new(),
                        src_files: Vec::new(),
                        flags: Vec::new(),
                    });
                    let entry = DaemonCacheEntry {
                        typed: std::sync::Arc::new(typed.clone()),
                        exe: exe.clone(),
                        inputs,
                    };
                    state.cache.insert(project.to_path_buf(), entry.clone());
                    Some(entry)
                }
                Err(_) => None,
            }
        };
        daemon_state_cv().notify_all();
        return match compiled {
            Ok(_) => Ok(entry.expect("cache entry built")),
            Err(e) => Err(e),
        };
    }
}

#[derive(serde::Deserialize)]
struct RunRequest {
    project: PathBuf,
    func: String,
    #[serde(default)]
    args: Vec<String>,
}

fn daemon_run_request(r: RunRequest) -> serde_json::Value {
    let resp = daemon_run_inner(&r);
    match resp {
        Ok((code, stdout, stderr)) => {
            serde_json::json!({"exit": code, "stdout": stdout, "stderr": stderr})
        }
        Err(e) => serde_json::json!({"exit": 1, "stdout": "", "stderr": format!("{}", e)}),
    }
}

fn daemon_run_inner(r: &RunRequest) -> Result<(i32, String, String)> {
    let project = r.project.canonicalize().context("resolving project path")?;
    let entry = daemon_module(&project)?;

    let symbol = match resolve_symbol(&entry.typed, &r.func) {
        Ok(s) => s,
        Err(ResolveError::NotFound) => {
            anyhow::bail!("function or tag `{}` not found", r.func);
        }
        Err(ResolveError::AmbiguousTag(funcs)) => {
            anyhow::bail!(
                "tag `{}` is declared on multiple functions: {}",
                r.func,
                funcs.join(", ")
            );
        }
    };

    let mut cmd = with_runner_env(with_rust_env(&entry.exe));
    cmd.current_dir(&project).arg(&symbol);
    for a in &r.args {
        cmd.arg(a);
    }
    let output = cmd.output().context("running compiled module")?;
    Ok((
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

// ---------------------------------------------------------------------------
// Set/get gateway. External programs set variables the running
// program reads via `spasio:input[file]`, and get values the program writes
// via `spasio:output[file]`. The runner translates between the frame protocol
// and the on-disk channel files - the file slot semantic ("read = freshest
// value") is exactly what a settable variable needs.
// ---------------------------------------------------------------------------

/// The channel-slot directories the daemon has already created. A SET
/// creates its slot directory on the first write; repeating the
/// `mkdir -p` on every call is a syscall the daemon pays per channel
/// write, and a program writing one value per frame pays it per frame.
fn daemon_created_dirs() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static DIRS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    DIRS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Create `dir` once. Later calls for the same directory are no-ops.
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    {
        let dirs = daemon_created_dirs().lock().unwrap();
        if dirs.contains(dir) {
            return Ok(());
        }
    }
    fs::create_dir_all(dir)?;
    daemon_created_dirs().lock().unwrap().insert(dir.to_path_buf());
    Ok(())
}

fn daemon_channel_path(project: &Path, dir: &str, channel: &str) -> PathBuf {
    project_data_dir(project).join(dir).join(format!("{}.spb", channel))
}

// ---------------------------------------------------------------------------
// runner-owned channel state (spasio data plane).
// ---------------------------------------------------------------------------

// In-memory message queues for socket/memory channels. file channels
// live on disk (the runner proxies the reads/writes). The program's
// `input[socket]` CONSUMES one message (blocking when the queue is
// empty - "input waits for a message"); the external JSON
// `get` only peeks at the latest one.
fn spass_queues(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, std::collections::VecDeque<f64>>>
{
    static Q: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::collections::VecDeque<f64>>>,
    > = std::sync::OnceLock::new();
    Q.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn spass_slot_cv() -> &'static std::sync::Condvar {
    static CV: std::sync::OnceLock<std::sync::Condvar> = std::sync::OnceLock::new();
    CV.get_or_init(std::sync::Condvar::new)
}

fn queue_push(channel: &str, v: f64) {
    spass_queues()
        .lock()
        .unwrap()
        .entry(channel.to_string())
        .or_default()
        .push_back(v);
    spass_slot_cv().notify_all();
}

fn queue_pop_wait(channel: &str) -> f64 {
    let mut g = spass_queues().lock().unwrap();
    loop {
        if let Some(v) = g.get_mut(channel).and_then(|q| q.pop_front()) {
            return v;
        }
        g = spass_slot_cv().wait(g).unwrap();
    }
}

fn queue_peek(channel: &str) -> Option<f64> {
    spass_queues().lock().unwrap().get(channel).and_then(|q| q.back().copied())
}

/// Parse a data-plane frame body (after the 4-byte length prefix):
/// `[u8 mode][u32 BE proj_len][project][u8 ch_len][channel][8B f64]`.
/// Returns (mode, project, channel, value). mode: 0=file, 1=socket.
fn daemon_parse_data_frame(
    body: &[u8],
) -> Option<(u8, PathBuf, String, f64)> {
    if body.len() < 1 + 4 + 1 {
        return None;
    }
    let mode = body[0];
    let plen = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    let mut i = 5;
    if body.len() < i + plen + 1 {
        return None;
    }
    let project = PathBuf::from(String::from_utf8_lossy(&body[i..i + plen]).into_owned());
    i += plen;
    let clen = body[i] as usize;
    i += 1;
    if body.len() < i + clen + 8 {
        return None;
    }
    let channel = String::from_utf8_lossy(&body[i..i + clen]).into_owned();
    i += clen;
    let mut vb = [0u8; 8];
    vb.copy_from_slice(&body[i..i + 8]);
    Some((mode, project, channel, f64::from_le_bytes(vb)))
}

/// Data-plane GET: file -> read the disk slot (`data/input/<ch>.spb`,
/// ERR when absent - the program side keeps its existing panic-on-missing
/// semantics); socket -> wait on the in-memory slot until a value arrives.
fn daemon_data_get(body: &[u8]) -> Vec<u8> {
    let (found, value) = match daemon_parse_data_frame(body) {
        Some((0, project, channel, _)) => {
            let path = daemon_channel_path(&project, "input", &channel);
            match fs::read(&path) {
                Ok(bytes) if bytes.len() >= 8 => {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(&bytes[..8]);
                    (true, f64::from_le_bytes(buf))
                }
                _ => (false, 0.0),
            }
        }
        Some((_, _project, channel, _)) => (true, queue_pop_wait(&channel)),
        None => (false, 0.0),
    };
    let status = if found { 0u8 } else { 1u8 };
    let mut resp = Vec::with_capacity(9);
    resp.push(status);
    resp.extend_from_slice(&value.to_le_bytes());
    resp
}

/// Data-plane SET: file -> write the disk slot; socket -> update the
/// in-memory slot and wake waiters. A program SET is an `spasio:output`
/// - its slot lives under `data/output/` (the external `get` side).
fn daemon_data_set(body: &[u8]) -> Vec<u8> {
    let ok = match daemon_parse_data_frame(body) {
        Some((0, project, channel, value)) => {
            let path = daemon_channel_path(&project, "output", &channel);
            if let Some(dir) = path.parent() {
                let _ = ensure_dir(dir);
            }
            fs::write(&path, value.to_le_bytes()).is_ok()
        }
        Some((_, _project, channel, value)) => {
            queue_push(&channel, value);
            true
        }
        None => false,
    };
    let status = if ok { 0u8 } else { 1u8 };
    let mut resp = Vec::with_capacity(9);
    resp.push(status);
    resp.extend_from_slice(&0f64.to_le_bytes());
    resp
}

// ---------------------------------------------------------------------------
// Resident world (minimal). `start` compiles the project and
// launches its dispatcher in FOREVER mode (sequences run until they all
// terminate). The world is a detached child of the daemon; `stop` reaps it.
// ---------------------------------------------------------------------------

fn daemon_world_pid() -> &'static std::sync::Mutex<Option<u32>> {
    static PID: std::sync::OnceLock<std::sync::Mutex<Option<u32>>> = std::sync::OnceLock::new();
    PID.get_or_init(|| std::sync::Mutex::new(None))
}

fn daemon_kill_world() {
    let pid = daemon_world_pid().lock().unwrap().take();
    if let Some(pid) = pid {
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
        }
        #[cfg(not(windows))]
        {
            let _ = Command::new("kill").arg(pid.to_string()).output();
        }
    }
}

fn daemon_handle_start(req: &serde_json::Value) -> serde_json::Value {
    let project = match req["project"].as_str() {
        Some(p) => match PathBuf::from(p).canonicalize() {
            Ok(p) => p,
            Err(e) => return serde_json::json!({"ok": false, "error": format!("bad project: {}", e)}),
        },
        None => return serde_json::json!({"ok": false, "error": "missing `project`"}),
    };
    // Compile (or reuse the cached dispatcher) - same path as `run`.
    let exe = match daemon_module(&project) {
        Ok(entry) => entry.exe,
        Err(e) => return serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    // Restart the world: kill any previous resident module first so two
    // worlds never compete for the same channel slots.
    daemon_kill_world();
    // The world outlives the request that started it, so it is started
    // with a restricted handle set: it must not keep the daemon's handles
    // - and through them the caller's output - alive.
    let mut env = runner_env_overrides();
    if let Some(path) = toolchain_path() {
        env.push((std::ffi::OsString::from("PATH"), path));
    }
    match win_spawn::spawn_detached(
        &exe,
        &[std::ffi::OsString::from("__sp_forever")],
        &env,
        Some(&project),
    ) {
        Ok(pid) => {
        *daemon_world_pid().lock().unwrap() = Some(pid);
            serde_json::json!({"ok": true, "world": pid})
        }
        Err(e) => serde_json::json!({"ok": false, "error": format!("spawn world: {}", e)}),
    }
}

fn daemon_handle_set(req: &serde_json::Value) -> serde_json::Value {
    let channel = match req["channel"].as_str() {
        Some(c) if !c.is_empty() => c,
        _ => return serde_json::json!({"ok": false, "error": "missing `channel`"}),
    };
    let value = match req["value"].as_f64() {
        Some(v) => v,
        None => return serde_json::json!({"ok": false, "error": "missing `value` (f64)"}),
    };
    let project = match req["project"].as_str() {
        Some(p) => PathBuf::from(p),
        None => return serde_json::json!({"ok": false, "error": "missing `project`"}),
    };
    // mode: "file" (default) writes the disk slot the program's
    // input[file] reads through the runner; "socket" updates the
    // in-memory slot (same state the program's input[socket] waits on).
    if req["mode"].as_str() == Some("socket") {
        queue_push(channel, value);
        return serde_json::json!({"ok": true, "channel": channel, "value": value});
    }
    let path = daemon_channel_path(&project, "input", channel);
    if let Some(dir) = path.parent() {
        let _ = ensure_dir(dir);
    }
    match fs::write(&path, value.to_le_bytes()) {
        Ok(()) => serde_json::json!({"ok": true, "channel": channel, "value": value}),
        Err(e) => serde_json::json!({"ok": false, "error": format!("write {}: {}", path.display(), e)}),
    }
}

fn daemon_handle_get(req: &serde_json::Value) -> serde_json::Value {
    let channel = match req["channel"].as_str() {
        Some(c) if !c.is_empty() => c,
        _ => return serde_json::json!({"ok": false, "error": "missing `channel`"}),
    };
    let project = match req["project"].as_str() {
        Some(p) => PathBuf::from(p),
        None => return serde_json::json!({"ok": false, "error": "missing `project`"}),
    };
    // mode: "socket" reads the in-memory slot the program's
    // output[socket] wrote; "file" (default) reads the disk slot.
    if req["mode"].as_str() == Some("socket") {
        return match queue_peek(channel) {
            Some(v) => serde_json::json!({"ok": true, "channel": channel, "found": true, "value": v}),
            None => serde_json::json!({"ok": true, "channel": channel, "found": false}),
        };
    }
    let path = daemon_channel_path(&project, "output", channel);
    match fs::read(&path) {
        Ok(bytes) if bytes.len() >= 8 => {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&bytes[..8]);
            serde_json::json!({"ok": true, "channel": channel, "found": true, "value": f64::from_le_bytes(buf)})
        }
        Ok(_) => serde_json::json!({"ok": true, "channel": channel, "found": false}),
        Err(_) => serde_json::json!({"ok": true, "channel": channel, "found": false}),
    }
}

/// How long the daemon stays up without a client.
///
/// A runner that nobody talks to should not outlive the session that
/// started it: an auto-started runner would otherwise sit in the
/// background forever. The window is generous because a long-running
/// program may hold a channel for a long time between reads - a
/// connected client keeps the daemon alive regardless of this window,
/// so the timer only fires when no program is holding a channel.
///
/// `SPASS_DAEMON_IDLE_SECS` overrides it; 0 keeps the daemon up until
/// `spass daemon --stop`. The project's `runner.idle_secs` is the
/// setting a project carries; the environment variable is the escape
/// hatch for a one-off run.
fn daemon_idle_secs() -> u64 {
    if let Ok(raw) = std::env::var("SPASS_DAEMON_IDLE_SECS") {
        return daemon_idle_from(Some(&raw));
    }
    let cfg = cwd_config_or_default();
    match cfg.runner_idle_secs {
        Some(secs) => secs,
        None => daemon_idle_from(None),
    }
}

/// The idle window a raw setting maps to. An unset or unreadable value
/// falls back to the default rather than disabling the expiry - a typo
/// must not turn a bounded runner into a permanent one.
fn daemon_idle_from(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(300)
}

/// The daemon's liveness bookkeeping: when it was last used and how many
/// clients are connected right now.
struct DaemonLiveness {
    last_activity: std::sync::Mutex<std::time::Instant>,
    clients: std::sync::atomic::AtomicUsize,
}

impl DaemonLiveness {
    fn new() -> Self {
        DaemonLiveness {
            last_activity: std::sync::Mutex::new(std::time::Instant::now()),
            clients: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn touch(&self) {
        if let Ok(mut t) = self.last_activity.lock() {
    *t = std::time::Instant::now();
        }
    }

    fn idle_for(&self) -> std::time::Duration {
        self.last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    fn client_opened(&self) {
        self.clients.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.touch();
    }

    fn client_closed(&self) {
        self.clients.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.touch();
    }
}

/// The liveness record of the running daemon. It is a process-wide
/// singleton because the status handler runs on a connection thread and
/// still has to report the daemon's own uptime.
fn daemon_liveness() -> &'static std::sync::Arc<DaemonLiveness> {
    static LIVENESS: std::sync::OnceLock<std::sync::Arc<DaemonLiveness>> =
        std::sync::OnceLock::new();
    LIVENESS.get_or_init(|| std::sync::Arc::new(DaemonLiveness::new()))
}

fn daemon_serve() -> Result<()> {
    let addr = daemon_addr();
    let listener = std::net::TcpListener::bind(&addr)
        .with_context(|| format!("daemon cannot bind {}", addr))?;
    eprintln!("spass daemon listening on {}", addr);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let liveness = daemon_liveness().clone();
    let idle_secs = daemon_idle_secs();
    // The accept loop has to wake up periodically to notice that the
    // daemon went idle; a non-blocking accept is what makes that
    // possible (`TcpListener` has no accept timeout in std).
    listener
        .set_nonblocking(true)
        .context("making the daemon listener non-blocking")?;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        let (stream, _peer) = match listener.accept() {
            Ok(s) => s,
            Err(e) => {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                // Nothing to accept right now: this is how the idle
                // window is measured, not a failure.
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    let clients = liveness.clients.load(std::sync::atomic::Ordering::SeqCst);
                    if idle_secs > 0
                        && clients == 0
                        && liveness.idle_for() >= std::time::Duration::from_secs(idle_secs)
                    {
                        eprintln!("daemon: idle for {}s, exiting", idle_secs);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
                eprintln!("daemon: accept failed: {}", e);
                continue;
            }
        };
        let _ = stream.set_nonblocking(false);
        // The frame protocol is a write-write-read: a length prefix,
        // then the body, then the response. Nagle's algorithm delays
        // the second write until the first is acknowledged, which
        // stalls exactly this pattern.
        let _ = stream.set_nodelay(true);
        // Per-connection thread: a blocking spasio GET (waiting for an
        // external `set`) must not stall the accept loop.
        let stop_flag = stop.clone();
        let live = liveness.clone();
        live.client_opened();
        std::thread::spawn(move || {
            daemon_handle_conn(stream, stop_flag);
            live.client_closed();
        });
    }
    eprintln!("spass daemon stopping");
    Ok(())
}

/// Serve one connection until EOF. The first byte of each frame body
/// dispatches between the control plane (JSON, starts with `{`) and
/// the spasio data plane (opcodes 0=GET / 1=SET, ).
fn daemon_handle_conn(
    mut stream: std::net::TcpStream,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        let body = match read_frame(&mut stream) {
            Ok(b) if !b.is_empty() => b,
            _ => return,
        };
        match body[0] {
            b'{' => {
                let req: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
                let resp = match req["cmd"].as_str().unwrap_or("run") {
                    "status" => serde_json::json!({
                        "ok": true,
                        "modules": daemon_state().lock().unwrap().cache.len(),
                        "clients": daemon_liveness().clients.load(std::sync::atomic::Ordering::SeqCst),
                        "idle_secs": daemon_idle_secs(),
                        "idle_for": daemon_liveness().idle_for().as_secs(),
                    }),
                    "stop" => {
                        stop.store(true, std::sync::atomic::Ordering::SeqCst);
                        daemon_kill_world();
                        // Self-connect to wake the accept loop so it can
                        // observe the stop flag.
                        let _ = std::net::TcpStream::connect(daemon_addr());
                        serde_json::json!({"ok": true})
                    }
                    // Start the resident world (sequences).
                    "start" => daemon_handle_start(&req),
                    // Set/get - the runner owns the channel
                    // state and proxies it to external programs.
                    "set" => daemon_handle_set(&req),
                    "get" => daemon_handle_get(&req),
                    _ => match serde_json::from_value::<RunRequest>(req) {
                        Ok(r) => daemon_run_request(r),
                        Err(e) => serde_json::json!({
                            "exit": 1, "stdout": "",
                            "stderr": format!("bad request: {}", e),
                        }),
                    },
                };
                if write_frame(&mut stream, &serde_json::to_vec(&resp).unwrap_or_default())
                    .is_err()
                {
                    return;
                }
            }
            0x00 => {
                let resp = daemon_data_get(&body[1..]);
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            0x01 => {
                let resp = daemon_data_set(&body[1..]);
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            _ => return, // unknown opcode: drop the connection
        }
    }
}

fn cmd_daemon(status: bool, stop: bool) -> Result<()> {
    if status {
        // A status query is not an operation: a daemon that is not up is
        // a valid answer, not a connection error.
        let resp = match daemon_exchange(&serde_json::json!({"cmd": "status"})) {
            Ok(r) => r,
            Err(_) => {
                println!("daemon: not running");
                return Ok(());
            }
        };
        println!(
            "daemon: {} module(s) cached, {} client(s) connected",
            resp["modules"].as_u64().unwrap_or(0),
            resp["clients"].as_u64().unwrap_or(0)
        );
        let idle_secs = resp["idle_secs"].as_u64().unwrap_or(0);
        if idle_secs > 0 {
            println!(
                "daemon: idle for {}s, exits after {}s without a client",
                resp["idle_for"].as_u64().unwrap_or(0),
                idle_secs
            );
        } else {
            println!("daemon: stays up until `spass daemon --stop`");
        }
        return Ok(());
    }
    if stop {
        // Stopping a daemon that is not up is already the wanted state.
        match daemon_exchange(&serde_json::json!({"cmd": "stop"})) {
            Ok(_) => println!("daemon stopped"),
            Err(_) => println!("daemon: not running"),
        }
        return Ok(());
    }
    daemon_serve()
}

/// `spass run` (no `--direct`): send the request to the daemon; if none
/// is running, start one detached, wait for the port, and retry. If the
/// daemon still cannot be reached, fall back to the direct path so a
/// broken daemon never blocks plain usage (`ponytail`).
/// Make sure a runner is listening for the spasio channels.
///
/// The runner owns the channel slots; the daemon is its CLI
/// form. A program that uses a channel needs one, no
/// matter how the function call itself was dispatched - so `--direct`
/// ("do not route the call through the daemon") must not take the
/// runner away from a program that needs it.
///
/// Returns whether a runner is reachable afterwards.
fn ensure_runner() -> bool {
    if std::net::TcpStream::connect(daemon_addr()).is_ok() {
        return true;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return false,
    };
    // The runner outlives this call, so it must not keep the caller's
    // output open: `spass test > out.txt` would leave `out.txt` locked and
    // `spass test | tee log` would never reach EOF. It is started with a
    // restricted handle set for that reason - see `win_spawn`.
    let mut env = runner_env_overrides();
    if let Some(path) = toolchain_path() {
        env.push((std::ffi::OsString::from("PATH"), path));
    }
    if let Err(e) = win_spawn::spawn_detached(
        &exe,
        &[std::ffi::OsString::from("daemon")],
        &env,
        None,
    ) {
        // A runner that cannot be started is not something the user should
        // have to guess at: say why.
        eprintln!(
            "runner: could not start a local runner on {}: {}",
            daemon_addr(),
            e
        );
        return false;
    }
    for _ in 0..50 {
        if std::net::TcpStream::connect(daemon_addr()).is_ok() {
            eprintln!(
                "runner: started a local runner on {} (it stops itself when idle; `spass daemon --stop` ends it now)",
                daemon_addr()
            );
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}


fn cmd_run_via_daemon(project_root: &Path, func: &str, args: &[String]) -> Result<()> {
    let req = serde_json::json!({
        "cmd": "run",
        "project": project_root,
        "func": func,
        "args": args,
    });
    // The daemon compiles on a cache miss, so this call can take a
    // second. Show what is going on instead of sitting there.
    let mut readout = progress::Readout::start(&format!("running {}", func));
    let resp = match daemon_exchange(&req) {
        Ok(r) => r,
        Err(_) => {
            readout.failed();
            if !ensure_runner() {
                // Never block plain usage on a broken daemon.
                eprintln!("warning: daemon failed to start; falling back to direct run");
                return run_direct(project_root, func, args);
            }
            match daemon_exchange(&req) {
                Ok(r) => r,
                Err(e) => {
                    readout.failed();
                    return Err(e);
                }
            }
        }
    };
    readout.done();
    let code = resp["exit"].as_i64().unwrap_or(1);
    print!("{}", resp["stdout"].as_str().unwrap_or(""));
    eprint!("{}", resp["stderr"].as_str().unwrap_or(""));
    if code != 0 {
        std::process::exit(code as i32);
    }
    Ok(())
}

/// Outcome of resolving a user-supplied name (function or tag) to a
/// mangled symbol. Tags are aliases and a single tag can
/// be shared by multiple functions - that case is reported separately so
/// `cmd_run` can give a useful error.
enum ResolveError {
    NotFound,
    AmbiguousTag(Vec<String>),
}

fn resolve_symbol(typed: &spass_semantic::TypedModule, name: &str) -> Result<String, ResolveError> {
    if name.starts_with("_spass_") {
        return Ok(name.to_string());
    }
    // 1. Direct function-name match (the common path).
    // `fileclude` / `ownclude` functions are outside the
    // public namespace, so `spass run` cannot dispatch to them - the
    // name lookup skips them (they would also fail at link time: a
    // `fileclude` helper is `static` in C and an `ownclude` closure
    // needs its captured values passed in).
    for f in &typed.functions {
        if f.name == name {
            if !spass_codegen::is_public_export(f) {
                return Err(ResolveError::NotFound);
            }
            return Ok(mangle_for_call(f));
        }
    }
    // 2. Bare-tag alias. A function may have any number of
    // bare tags; matching a tag is equivalent to matching the
    // function that owns it. Collect ALL candidates so we can
    // distinguish "ambiguous" from "not found".
    let matches: Vec<&spass_semantic::TypedFunc> = typed
        .functions
        .iter()
        .filter(|f| spass_codegen::is_public_export(f) && f.tags.iter().any(|t| t == name))
        .collect();
    match matches.as_slice() {
        [] => Err(ResolveError::NotFound),
        [f] => Ok(mangle_for_call(f)),
        _ => Err(ResolveError::AmbiguousTag(
            matches.iter().map(|f| f.name.clone()).collect(),
        )),
    }
}

/// Pick the symbol to call for a function. Functions with `if_present`
/// compile into two variants; the CLI always picks the
/// `present` variant by default because every Spass call site passes
/// every parameter. The `absent` variant is reachable only by passing
/// the full mangled name to `spass run`.
///
/// Always feeds the *real* `TypedFunc` to the official mangler so
/// module-system fields (`visibility`, `captures`, `enclosing_func`)
/// are honoured - a synthetic stand-in would drop the segments
/// and produce a symbol that does not exist in the emitted code.
fn mangle_for_call(f: &spass_semantic::TypedFunc) -> String {
    let base = spass_codegen::mangle_function_name(f);
    if f.has_if_present {
        format!("{}_present", base)
    } else {
        base
    }
}

// ============================================================================
// Clean
// ============================================================================

fn cmd_clean() -> Result<()> {
    let root = std::env::current_dir()?;
    let build = cwd_config()?.build_dir(&root);
    if build.exists() {
        fs::remove_dir_all(&build).context("removing the build directory")?;
        println!("removed {}", build.display());
    } else {
        println!("no build directory at {}", build.display());
    }
    Ok(())
}

// ============================================================================
// Test
// ============================================================================
//
// The test driver discovers `.spass` files under `test/`, parses them,
// merges their items with the items from `src/`, and analyses the
// combined module so test bodies can reference the same functions the
// rest of the project exposes. Each `test "..." { body }` is then
// lowered to a synthetic Spass function that returns `0` on pass and
// `1` on any failed assert, compiled together with the rest of the
// project, and executed via the existing `spass run` machinery.
//
// Reserved interfaces (future features, no refactor needed):
// * Test matrix: the `with` block already supports per-test
// parameters and defaults. A future revision can sweep these
// over a grid without changing the on-disk format.
// * Multiple assert kinds: `assert_to_stmts` in `spass_codegen` is
// the only place to extend when adding new assert forms.
// * Filtering: a future `--filter <name>` flag can be added
// without touching the discovery loop below.

fn cmd_test(verbose: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("getting cwd")?;
    if !project_root.join("Spass.toml").exists() {
        anyhow::bail!("no Spass.toml in current directory");
    }
    let cfg = cwd_config()?;
    let verbose = cfg.wants_verbose(verbose);
    let build_dir = cfg.build_dir(&project_root);
    let src_dir = project_root.join("src");
    let test_dir = project_root.join("test");
    fs::create_dir_all(&build_dir).context("creating build dir")?;

    // Discover files: src/ is optional for a test-only project, but
    // test/ is required (otherwise there's nothing to do).
    let src_files = collect_spass_files(&src_dir).unwrap_or_default();
    let test_files = collect_spass_files(&test_dir).unwrap_or_default();
    if test_files.is_empty() {
        anyhow::bail!("no .spass files found in {}", test_dir.display());
    }
    if verbose {
        println!("scanning {} src file(s), {} test file(s)", src_files.len(), test_files.len());
    }

    // Parse src/ and test/ items into a single Module so the
    // semantic pass can resolve cross-file references (e.g. a test
    // body calling a function defined in src/).
    let (test_names, project) = compile_test_project(&src_files, &test_files, verbose)?;
    let rs_path = build_dir.join("module.rs");
    fs::write(&rs_path, &project.rust_source).context("writing Rust output")?;

    // Compile the Rust module once; per-test invocation re-uses the
    // binary by passing the test function's mangled symbol as
    // argv[1]. This matches the existing `spass run` protocol.
    let exe_path = build_dir.join("module_rs.exe");
    compile_module(&rs_path, &exe_path, cfg.build_timeout_secs)?;

    // A test body that touches a channel needs the runner (the daemon is
    // its CLI form). The test binary is executed directly here rather than
    // through the daemon, so the runner has to be started separately -
    // `spass run` does the same via `ensure_runner`.
    if !project.typed.channels.is_empty() && !ensure_runner() {
        anyhow::bail!(
            "this project uses spasio channels, but no runner could be reached at {}\n  \
             help: start it with `spass daemon`",
            daemon_addr()
        );
    }

    // Run each test, collect pass/fail, and report.
    //
    // The test functions are synthetic Spass wrappers that return
    // `0` on pass and `1` on a failed `assert`. The regular Rust
    // dispatcher prints the return value and exits 0, so we read
    // the printed value back from stdout and treat 0 as a pass,
    // 1 (or anything else) as a fail. This keeps the dispatcher
    // single-purpose while still letting the test driver gate on
    // the synthetic function's return value.
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();
    for (display_name, mangled) in &test_names {
        let output = with_runner_env(with_rust_env(&exe_path))
            .arg(mangled)
            .output()
            .with_context(|| format!("running test `{}`", display_name))?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // The dispatcher prints the return value of the test
        // function. 0 is pass, anything else is fail. We also fall
        // back to the process exit code in case the dispatcher is
        // ever changed to print nothing on success.
        let test_passed = stdout == "0" || (stdout.is_empty() && output.status.code() == Some(0));
        if test_passed && !stdout.is_empty() {
            passed += 1;
            println!("  {} {} (rc=0)", ansi::green_bold("ok"), display_name);
        } else if test_passed {
            passed += 1;
            println!("  {} {}", ansi::green_bold("ok"), display_name);
        } else {
            failed += 1;
            let detail = if !stdout.is_empty() {
                format!("assert returned {}", stdout)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if stderr.is_empty() { format!("exit code {:?}", output.status.code()) } else { stderr }
            };
            failures.push((display_name.clone(), detail));
            println!("  {} {} (rc={})", ansi::red_bold("FAIL"), display_name, stdout);
        }
    }

    println!();
    println!("test result: {} passed, {} failed", passed, failed);
    if !failures.is_empty() {
        for (name, detail) in &failures {
            eprintln!("\n---- {} ----\n{}", name, detail);
        }
        std::process::exit(1);
    }
    Ok(())
}

/// Parse + analyse the merged src/ + test/ project and add the
/// synthetic test functions to the typed module. Returns the list
/// of `(display_name, mangled_symbol)` pairs the driver needs to
/// run each test.
fn compile_test_project(
    src_files: &[PathBuf],
    test_files: &[PathBuf],
    _verbose: bool,
) -> Result<(Vec<(String, String)>, CompiledProject)> {
    use spass_codegen::test_codegen::build_test_func;
    use spass_parser::Parser;
    use spass_semantic::{analyze, analyze_full};

    // Collect all items into one Module so the semantic pass sees
    // every function and test at once. The `parse_project_with_includes`
    // entry point shares the include-guard set across the whole
    // src + test tree, so a header `include`d by both is expanded
    // exactly once.
    let all_files: Vec<PathBuf> = src_files.iter().chain(test_files.iter()).cloned().collect();
    if all_files.is_empty() {
        anyhow::bail!("no .spass files in src/ or test/");
    }
    let merged = Parser::parse_project_with_includes(&all_files, &FsFileLoader)
        .context("parsing project sources (src/ + test/)")?;

    // Analyse the merged module. The test bodies now resolve
    // identifiers against the same symbol table that holds the
    // functions defined in src/; E005 ("undefined ident") is
    // raised here if a test references something missing.
    let mut typed = analyze(&merged).context("semantic analysis")?;

    // Collect the test items in source order (the merged module
    // preserves order), build a synthetic function for each, and
    // push it onto the typed module's function list so the codegen
    // layer emits a wrapper per test.
    let mut test_specs: Vec<(String, String)> = Vec::new(); // (display_name, func_name)
    let mut test_funcs: Vec<spass_parser::ast::FuncDef> = Vec::new();
    for item in &merged.items {
        if let spass_parser::ast::Item::Test(t) = item {
            let display = t.name.clone();
            let func = build_test_func(t);
            test_specs.push((display, func.name.clone()));
            test_funcs.push(func);
        }
    }

    // Re-analyse the test functions now that they are in scope as
    // regular functions. We do this by appending the synthetic
    // FuncDef items to the merged module and re-running analyze.
    if !test_funcs.is_empty() {
        let mut augmented_items = merged.items.clone();
        for f in &test_funcs {
            augmented_items.push(spass_parser::ast::Item::Func(f.clone()));
        }
        let augmented = spass_parser::ast::Module {
            items: augmented_items,
            source_file: merged.source_file.clone(),
        };
        let result = analyze_full(&augmented).context("semantic analysis (with test funcs)")?;
        print_warnings(&result.warnings);
        typed = result.module;
    }

    // Resolve the mangled name of each test function using the
    // official mangle_function_name via the typed module. This
    // keeps the symbol we pass to the dispatcher in lock-step
    // with what the codegen actually emits.
    let mut test_names: Vec<(String, String)> = Vec::new();
    for (display, func_name) in &test_specs {
        let mangled = match typed.func(func_name) {
            Some(f) => mangle_for_call(f),
            None => anyhow::bail!("test function `{}` not found in typed module", func_name),
        };
        test_names.push((display.clone(), mangled));
    }

    // Sanity: the test funcs must be in the typed module now.
    if !test_funcs.is_empty() {
        let names: Vec<&str> = test_funcs.iter().map(|f| f.name.as_str()).collect();
        let missing: Vec<&&str> = names
            .iter()
            .filter(|n| !typed.functions.iter().any(|f| &f.name == *n))
            .collect();
        if !missing.is_empty() {
            anyhow::bail!("test functions missing from typed module: {:?}", missing);
        }
    }

    let c_source = spass_codegen::generate_c(&typed).context("C codegen")?;
    let rust_source = spass_codegen::generate_rust(&typed).context("Rust codegen")?;

    Ok((
        test_names,
        CompiledProject {
            typed,
            c_source,
            rust_source,
            // The test harness compiles the module as it stands: the
            // optimiser is a build-time choice, and `spass test` measures
            // the module the user wrote.
            plan: None,
            report: spass_optimize::OptReport::default(),
        },
    ))
}



// ============================================================================
// Tag
// ============================================================================

fn cmd_tag(list_grouped: bool) -> Result<()> {
    let src_dir = std::env::current_dir()?.join("src");
    let files = collect_spass_files(&src_dir)?;
    if files.is_empty() {
        println!("(no .spass files)");
        return Ok(());
    }
    use spass_parser::Parser;
    let mut tags: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for f in &files {
        let module = Parser::parse_file_with_includes(f, &FsFileLoader)?;
        for item in &module.items {
            if let spass_parser::ast::Item::Func(fd) = item {
                for ann in &fd.annotations {
                    if let spass_parser::ast::Annotation::Tag { items, .. } = ann {
                        for ti in items {
                            let key = match ti {
                                spass_parser::ast::TagItem::Bare(n) => n.clone(),
                                spass_parser::ast::TagItem::Valued(k, _, _) => k.clone(),
                            };
                            tags.entry(key).or_default().push(fd.name.clone());
                        }
                    }
                }
            }
        }
    }
    if list_grouped {
        for (tag, funcs) in &tags {
            let mut fs = funcs.clone();
            fs.sort();
            fs.dedup();
            println!("{}:", tag);
            for f in fs {
                println!("  {}", f);
            }
        }
    } else {
        for tag in tags.keys() {
            println!("{}", tag);
        }
    }
    Ok(())
}

// ============================================================================
// saple packaging
// ============================================================================

fn package_saple(
    out: &Path,
    module_name: &str,
    typed: &spass_semantic::TypedModule,
    c_source: &str,
    natives: Vec<(String, Vec<u8>)>,
) -> Result<()> {
    saple::package(out, module_name, typed, c_source, natives)
}

// ============================================================================
// The optimiser channel
// ============================================================================

/// Optimise the project and write the result beside the normal build.
///
/// The optimised artifacts land in `build/optimized/` and never replace
/// the normal ones: a build that went through this channel can always
/// be compared with one that did not.
fn cmd_code_optimize(
    verbose: bool,
    optimize_level: Option<u8>,
    diagnostics: bool,
) -> Result<()> {
    let project_root = std::env::current_dir().context("getting cwd")?;
    if !project_root.join("Spass.toml").exists() {
        anyhow::bail!("no Spass.toml in current directory ({})", project_root.display());
    }
    let cfg = cwd_config()?;
    let verbose = cfg.wants_verbose(verbose);
    let build_dir = cfg.build_dir(&project_root);
    let out_dir = build_dir.join("optimized");

    // The channel is explicit: it always runs, even when Spass's own
    // configuration leaves the optimiser off.
    let opt = body_config::resolve(body_config::OptimizeRequest {
        enable: true,
        disable: false,
        level: optimize_level,
        diagnostics,
    })?;
    if !opt.is_enabled() {
        anyhow::bail!(
            "the optimiser is off (level 0), so there is nothing to optimise. Pick a level with \
             --optimize-level, or turn it on in Spass's own configuration ({}).",
            body_config::describe()
        );
    }

    let src_dir = project_root.join("src");
    let spass_files = collect_spass_files(&src_dir)?;
    if spass_files.is_empty() {
        anyhow::bail!("no .spass files found in {}", src_dir.display());
    }

    let project = compile_sources(&spass_files, verbose, &opt)?;
    fs::create_dir_all(&out_dir).context("creating the optimised build dir")?;

    let c_path = out_dir.join("module.c");
    fs::write(&c_path, &project.c_source).context("writing the optimised C output")?;
    let rs_path = out_dir.join("module.rs");
    fs::write(&rs_path, &project.rust_source).context("writing the optimised Rust output")?;

    // The optimised module is compiled too: the point of the channel is
    // an artifact that runs, not a source that looks optimised.
    let exe_path = out_dir.join("module_rs.exe");
    compile_module(&rs_path, &exe_path, cfg.build_timeout_secs)?;

    // The report travels with the artifacts: what was changed is part
    // of the artifact.
    let report = serde_json::json!({
        "switches": {
            "readonly_locals": opt.switches.readonly_locals,
            "inline_single_use": opt.switches.inline_single_use,
            "fold_constants": opt.switches.fold_constants,
            "const_fold_arith": opt.switches.const_fold_arith,
            "dead_code_elim": opt.switches.dead_code_elim,
        },
        "readonly": project.report.report.readonly,
        "inlined": project.report.report.inlined,
        "folded": project.report.report.folded,
        "const_folded": project.report.report.const_folded,
        "eliminated": project.report.eliminated,
    });
    let report_path = out_dir.join("optimize-report.json");
    fs::write(
        &report_path,
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .context("writing the optimiser report")?;

    print_optimiser_report(&project.report, opt.diagnostics);
    println!("  optimised: {}", out_dir.display());
    println!("  report:    {}", report_path.display());
    Ok(())
}

// ============================================================================
// The.saple runner
// ============================================================================

/// Run a `.saple`: load its native module and call a function in it.
///
/// This is what the `.saple` file association points at, and what the
/// resident runner calls when it is handed a package.
fn cmd_runner(saple_path: &Path, func: Option<&str>, args: &[String]) -> Result<()> {
    let package = spass_runner::Saple::read(saple_path)?;
    if package.native_for_current_platform().is_none() {
        anyhow::bail!(
            "the package carries no native module for this platform ({})",
            spass_runner::platform_tag()
        );
    }

    let runner = package.manifest.get("runner");
    let has_sequences = runner
        .and_then(|r| r.get("has_sequences"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sequences: Vec<&str> = runner
        .and_then(|r| r.get("sequences"))
        .and_then(|v| v.as_array())
        .map(|list| list.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let uses_channels = package
        .manifest
        .get("channels")
        .and_then(|c| c.as_array())
        .map(|list| !list.is_empty())
        .unwrap_or(false);

    // A package that talks to a channel needs the runner (the daemon is
    // its CLI form) before its sequences start: a sequence that blocks
    // on an input would otherwise have nowhere to connect.
    if uses_channels && !ensure_runner() {
        anyhow::bail!(
            "this package uses spasio channels, but no runner could be reached at {}\n  \
             help: start it with `spass daemon`",
            daemon_addr()
        );
    }

    // Resolve the call before starting anything: a bad function name
    // should leave the module untouched.
    if let Some(name) = func {
        if sequences.contains(&name) {
            anyhow::bail!(
                "`{}` is a sequence, not a callable function: it starts with the module",
                name
            );
        }
    }
    let symbol = if func.is_some() || !has_sequences {
        Some(saple::resolve_symbol(&package.manifest, func)?)
    } else {
        // The package is a resident world: its sequences are the entry,
        // and there is no function call to make.
        None
    };
    let values = runner_args(args)?;

    // A library has to be a file on disk before the loader can load it,
    // so the package is unpacked into the runner's cache.
    let dir = spass_runner::cache_dir().join(cache_key(saple_path));
    fs::create_dir_all(&dir).context("creating the runner cache directory")?;
    let module_path = package
        .extract_current(&dir)
        .context("extracting the native module")?;

    // The unwinder travels next to the module: the loader resolves it
    // there, and a module without it does not load at all.
    if let Some(unwind) = package.native_for("libunwind") {
        if let Some(name) = Path::new(&unwind.entry).file_name() {
            let _ = fs::write(dir.join(name), &unwind.bytes);
        }
    }

    let module = spass_runner::Module::load(&module_path).context("loading the native module")?;
    module.start().context("starting the module")?;

    let call = match &symbol {
        Some(symbol) => {
            let slots = saple::result_slots(&package.manifest, symbol);
            module.call(symbol, &values, slots).map(|results| {
                for value in results {
                    println!("{}", value);
                }
            })
        }
        None => Ok(()),
    };

    // Join the sequences before the process exits. Skipping this
    // unloads the library while its sequence threads are still running,
    // which crashes the runner and loses the sequences' output. The
    // call's error is reported after the join, so it is not swallowed.
    let waited = module.wait();
    call?;
    waited?;
    Ok(())
}

/// The numeric arguments of a runner call.
///
/// The module's uniform call ABI takes numbers only, so a non-number is
/// an error rather than an argument that silently disappears.
fn runner_args(args: &[String]) -> Result<Vec<f64>> {
    args.iter()
        .map(|a| {
            a.parse::<f64>().map_err(|_| {
                anyhow::anyhow!(
                    "argument `{}` is not a number; the .saple call ABI takes numbers only",
                    a
                )
            })
        })
        .collect()
}

/// The cache directory a package is unpacked into.
///
/// The key is the package's path: two packages with the same name in
/// different directories are different packages.
fn cache_key(path: &Path) -> String {
    let mut hash: u64 = 1469598103934665603;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{:016x}", hash)
}

// ============================================================================
// ANSI helpers
// ============================================================================

mod ansi {
    pub fn green_bold(s: &str) -> String {
        format!("\x1b[1;32m{}\x1b[0m", s)
    }
    pub fn red_bold(s: &str) -> String {
        format!("\x1b[1;31m{}\x1b[0m", s)
    }
    pub fn yellow_bold(s: &str) -> String {
        format!("\x1b[1;33m{}\x1b[0m", s)
    }
}

/// print semantic warnings (yellow bold) after analysis.
fn print_warnings(warnings: &[spass_semantic::SemanticWarning]) {
    for w in warnings {
        println!("{} {}", ansi::yellow_bold("warning:"), w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The idle window a raw setting maps to. An unreadable value falls
    /// back to the default instead of disabling the expiry: a typo must
    /// not turn a bounded runner into a permanent one.
    #[test]
    fn idle_window_falls_back_to_the_default_on_garbage() {
        assert_eq!(daemon_idle_from(Some("30")), 30);
        assert_eq!(daemon_idle_from(Some(" 30 ")), 30);
        assert_eq!(daemon_idle_from(Some("0")), 0);
        assert_eq!(daemon_idle_from(None), 300);
        assert_eq!(daemon_idle_from(Some("abc")), 300);
        assert_eq!(daemon_idle_from(Some("-5")), 300);
    }
}
