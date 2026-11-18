//! Reads the project's `Spass.toml` configuration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The compiler's own log level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    /// Parse a level name. An unknown name is an error, because a
    /// misspelled level would otherwise silently disable the output
    /// the user asked for.
    pub fn parse(raw: &str) -> Option<LogLevel> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "error" => Some(LogLevel::Error),
            "warn" | "warning" => Some(LogLevel::Warn),
            "info" => Some(LogLevel::Info),
            "debug" => Some(LogLevel::Debug),
            _ => None,
        }
    }
}

/// The configuration of one project.
#[derive(Debug, Clone)]
pub struct Config {
    /// `build.output` - where the artifacts go. Default `build`.
    pub build_output: PathBuf,
    /// `build.timeout` - the compile timeout in seconds. Default 120.
    pub build_timeout_secs: u64,
    /// `spasio.data_dir` - the file-mode channel directory. Default
    /// `data`.
    pub data_dir: PathBuf,
    /// `spasio.socket_host` - the runner address. Default 127.0.0.1.
    pub socket_host: String,
    /// `spasio.socket_port` - the runner port. Default 9100.
    pub socket_port: u16,
    /// `debug.log_level` - the compiler's own log level. Default warn.
    pub log_level: LogLevel,
    /// `runner.idle_secs` - how long the runner stays up without a
    /// client. `None` means "use the default".
    pub runner_idle_secs: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            build_output: PathBuf::from("build"),
            build_timeout_secs: 120,
            data_dir: PathBuf::from("data"),
            socket_host: "127.0.0.1".to_string(),
            socket_port: 9100,
            log_level: LogLevel::Warn,
            runner_idle_secs: None,
        }
    }
}

/// A parsed value.
#[derive(Debug, Clone)]
pub(crate) enum Value {
    Str(String),
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A value this reader does not understand (a float, an array, a
    /// date). It is kept so the key can be recognised as "present but
    /// unusable".
    Other,
}

/// The raw contents of a configuration file: `section.key -> value`.
#[derive(Debug, Default)]
pub(crate) struct Raw {
    pub(crate) values: HashMap<String, Value>,
}

impl Raw {
    fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.values.get(&format!("{}.{}", section, key))
    }

    /// Whether the file mentions anything under `section`.
    pub(crate) fn has_section(&self, section: &str) -> bool {
        let prefix = format!("{}.", section);
        self.values.keys().any(|k| k.starts_with(&prefix))
    }

    fn str(&self, section: &str, key: &str) -> Option<Option<String>> {
        match self.get(section, key) {
            None => None,
            Some(Value::Str(s)) => Some(Some(s.clone())),
            Some(_) => Some(None),
        }
    }

    pub(crate) fn int(&self, section: &str, key: &str) -> Option<Option<i64>> {
        match self.get(section, key) {
            None => None,
            Some(Value::Int(i)) => Some(Some(*i)),
            Some(_) => Some(None),
        }
    }

    pub(crate) fn bool(&self, section: &str, key: &str) -> Option<Option<bool>> {
        match self.get(section, key) {
            None => None,
            Some(Value::Bool(b)) => Some(Some(*b)),
            Some(_) => Some(None),
        }
    }
}

impl Config {
    /// Read `Spass.toml` from a project directory. A missing file is
    /// not an error: the defaults are a valid configuration.
    pub fn load(project_root: &Path) -> Result<Config> {
        let path = project_root.join("Spass.toml");
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Config::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Read a configuration from its text.
    pub fn from_str(text: &str) -> Result<Config> {
        let raw = parse_toml(text)?;
        let mut cfg = Config::default();

        // The optimiser's switches are Spass's own, not the project's:
        // they change what the compiler computes for everyone, so they
        // live in Spass's own configuration file. A project that tries
        // to set them gets an error rather than a silent no-op - a
        // project that believes it asked for an optimisation it did not
        // get is worse than a build that refuses to start.
        if raw.has_section("optimize") {
            bail!(
                "`Spass.toml` cannot set the optimiser's switches: they belong to Spass's own \
                 configuration ({}). Remove the `[optimize]` section, or run `spass code-optimize`.",
                crate::body_config::config_path().display()
            );
        }

        if let Some(value) = raw.str("build", "output") {
            match value {
                Some(s) => cfg.build_output = PathBuf::from(s),
                None => bail!("build.output must be a string"),
            }
        }
        if let Some(value) = raw.int("build", "timeout") {
            match value {
                Some(i) if i > 0 => cfg.build_timeout_secs = i as u64,
                Some(_) => bail!("build.timeout must be a positive number of seconds"),
                None => bail!("build.timeout must be a number"),
            }
        }
        if let Some(value) = raw.str("spasio", "data_dir") {
            match value {
                Some(s) => cfg.data_dir = PathBuf::from(s),
                None => bail!("spasio.data_dir must be a string"),
            }
        }
        if let Some(value) = raw.str("spasio", "socket_host") {
            match value {
                Some(s) => cfg.socket_host = s,
                None => bail!("spasio.socket_host must be a string"),
            }
        }
        if let Some(value) = raw.int("spasio", "socket_port") {
            match value {
                Some(i) if (1..=65535).contains(&i) => cfg.socket_port = i as u16,
                Some(_) => bail!("spasio.socket_port must be a port number"),
                None => bail!("spasio.socket_port must be a number"),
            }
        }
        if let Some(value) = raw.str("debug", "log_level") {
            match value {
                Some(s) => match LogLevel::parse(&s) {
                    Some(level) => cfg.log_level = level,
                    None => bail!(
                        "debug.log_level must be one of error, warn, info, debug (got `{}`)",
                        s
                    ),
                },
                None => bail!("debug.log_level must be a string"),
            }
        }
        if let Some(value) = raw.int("runner", "idle_secs") {
            match value {
                Some(i) if i >= 0 => cfg.runner_idle_secs = Some(i as u64),
                Some(_) => bail!("runner.idle_secs must not be negative"),
                None => bail!("runner.idle_secs must be a number"),
            }
        }
        Ok(cfg)
    }

    /// The build directory of a project, resolved against its root.
    pub fn build_dir(&self, project_root: &Path) -> PathBuf {
        project_root.join(&self.build_output)
    }

    /// The file-mode channel directory of a project, resolved against
    /// its root.
    pub fn data_dir(&self, project_root: &Path) -> PathBuf {
        project_root.join(&self.data_dir)
    }

    /// Whether the compiler should print its progress.
    ///
    /// The `--verbose` flag asks for it, and so does a
    /// `debug.log_level` of `info` or `debug`: a project can ask for a
    /// build summary without every invocation carrying the flag.
    pub fn wants_verbose(&self, flag: bool) -> bool {
        flag || self.log_level >= LogLevel::Info
    }

    /// Whether the resolved configuration should be printed.
    pub fn wants_config_dump(&self) -> bool {
        self.log_level >= LogLevel::Debug
    }
}

/// Parse the subset of TOML the configuration table uses.
///
/// Unknown constructs are skipped: a line that is not a section header
/// and not a `key = value` pair is ignored, and so is a value this
/// reader cannot represent. The key is still recorded, so a key that
/// *is* known can report that its value is unusable.
pub(crate) fn parse_toml(text: &str) -> Result<Raw> {    let mut raw = Raw::default();
    let mut section = String::new();
    for (n, line) in text.lines().enumerate() {
        let line = strip_comment(line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            // A section header. An unterminated one is not a header.
            if let Some(end) = line.find(']') {
                section = line[1..end].trim().to_string();
            }
            continue;
        }
        let eq = match line.find('=') {
            Some(i) => i,
            None => continue,
        };
        let key = line[..eq].trim();
        if key.is_empty() {
            continue;
        }
        let value = parse_value(line[eq + 1..].trim());
        let full = if section.is_empty() {
            key.to_string()
        } else {
            format!("{}.{}", section, key)
        };
        raw.values.insert(full, value);
        let _ = n;
    }
    Ok(raw)
}

/// Remove a trailing comment, ignoring a `#` inside a string.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut quote = b'"';
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == quote {
                in_string = false;
            }
        } else if c == b'"' || c == b'\'' {
            in_string = true;
            quote = c;
        } else if c == b'#' {
            return &line[..i];
        }
        i += 1;
    }
    line
}

/// Parse one value.
fn parse_value(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        return Value::Str(unescape(&raw[1..raw.len() - 1]));
    }
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return Value::Str(raw[1..raw.len() - 1].to_string());
    }
    match raw {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Int(i);
    }
    Value::Other
}

/// Expand the escapes a TOML basic string may contain.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_the_file_is_empty() {
        let cfg = Config::from_str("").expect("parse");
        assert_eq!(cfg.build_output, PathBuf::from("build"));
        assert_eq!(cfg.build_timeout_secs, 120);
        assert_eq!(cfg.data_dir, PathBuf::from("data"));
        assert_eq!(cfg.socket_port, 9100);
        assert_eq!(cfg.log_level, LogLevel::Warn);
    }

    /// The whole key table of the language has to be readable.
    #[test]
    fn reads_every_key_of_the_specification() {
        let text = r#"
[build]
output = "./out"
platform = "current"
timeout = 300

[spasio]
data_dir = "./channels"
mode = "file"
socket_host = "0.0.0.0"
socket_port = 9200
error_policy = "default"

[gpu]
shared_memory_limit = "48KB"
default_workgroup = 256

[debug]
max_checkpoint_size = "16MB"
log_level = "info"
"#;
        let cfg = Config::from_str(text).expect("parse");
        assert_eq!(cfg.build_output, PathBuf::from("./out"));
        assert_eq!(cfg.build_timeout_secs, 300);
        assert_eq!(cfg.data_dir, PathBuf::from("./channels"));
        assert_eq!(cfg.socket_host, "0.0.0.0");
        assert_eq!(cfg.socket_port, 9200);
        assert_eq!(cfg.log_level, LogLevel::Info);
    }

    /// A comment must not eat the value, and a `#` inside a string is
    /// not a comment.
    #[test]
    fn comments_and_strings_are_handled() {
        let text = r#"
# leading comment
[build]   # trailing comment
output = "di#r"   # the value keeps its hash
"#;
        let cfg = Config::from_str(text).expect("parse");
        assert_eq!(cfg.build_output, PathBuf::from("di#r"));
    }

    /// A key the reader knows but cannot use is an error - a typo must
    /// not silently fall back to the default.
    #[test]
    fn a_bad_value_for_a_known_key_is_an_error() {
        assert!(Config::from_str("[build]\ntimeout = \"soon\"").is_err());
        assert!(Config::from_str("[build]\ntimeout = 0").is_err());
        assert!(Config::from_str("[spasio]\nsocket_port = 99999").is_err());
        assert!(Config::from_str("[debug]\nlog_level = \"loud\"").is_err());
    }

    /// A key the reader does not know must not stop the project from
    /// building.
    #[test]
    fn unknown_keys_are_ignored() {
        let text = r#"
[future]
whatever = [1, 2, 3]
[build]
output = "b"
"#;
        let cfg = Config::from_str(text).expect("parse");
        assert_eq!(cfg.build_output, PathBuf::from("b"));
    }

    /// The runner's idle window is optional: unset means "the default".
    #[test]
    fn the_runner_idle_window_is_optional() {
        assert_eq!(Config::from_str("").expect("parse").runner_idle_secs, None);
        assert_eq!(
            Config::from_str("[runner]\nidle_secs = 30")
                .expect("parse")
                .runner_idle_secs,
            Some(30)
        );
    }
}
