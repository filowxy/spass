//! Defines the reporter abstraction shared by the console and GUI installers.

use std::sync::OnceLock;

pub trait Reporter: Send + Sync {
    /// A major phase begins, e.g. "Installing Spass".
    fn step(&self, msg: &str);
    /// A successful detail line.
    fn ok(&self, msg: &str);
    /// A neutral detail line.
    fn note(&self, msg: &str);
    /// A failure.
    fn error(&self, msg: &str);
    /// A concluding line, visually separated.
    fn finish(&self, msg: &str);
    /// A verbatim line (usage text, commands to copy).
    fn plain(&self, msg: &str);
    /// Ask a yes/no question. Defaults to "no" when nobody can be asked.
    fn confirm(&self, prompt: &str) -> bool;
    /// Report byte progress. `total == 0` means indeterminate.
    fn progress(&self, done: u64, total: u64);
}

/// The console reporter: exactly the output shape the CLI has always had.
pub struct Console;

impl Reporter for Console {
    fn step(&self, m: &str) {
        println!("==> {}", m);
    }
    fn ok(&self, m: &str) {
        println!("    {}", m);
    }
    fn note(&self, m: &str) {
        println!("    {}", m);
    }
    fn error(&self, m: &str) {
        eprintln!("error: {}", m);
    }
    fn finish(&self, m: &str) {
        println!();
        println!("{}", m);
    }
    fn plain(&self, m: &str) {
        println!("{}", m);
    }
    fn confirm(&self, prompt: &str) -> bool {
        use std::io::Write;
        print!("{}", prompt);
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        )
    }
    fn progress(&self, _done: u64, _total: u64) {}
}

static REPORTER: OnceLock<std::sync::Arc<dyn Reporter>> = OnceLock::new();

/// The active reporter. Defaults to the console.
pub fn reporter() -> std::sync::Arc<dyn Reporter> {
    REPORTER
        .get_or_init(|| std::sync::Arc::new(Console))
        .clone()
}

/// Install a reporter. The first call wins; later calls are ignored, which
/// keeps startup order from mattering.
pub fn set(r: std::sync::Arc<dyn Reporter>) {
    let _ = REPORTER.set(r);
}
