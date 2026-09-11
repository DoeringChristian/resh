use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Optional debug log, enabled by setting `RESH_LOG_FILE`.
///
/// The close path (window close, signal handling, the remote kill that follows)
/// runs while the terminal is being torn down, so anything written to stderr is
/// lost. A file survives it.
fn log_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os("RESH_LOG_FILE").map(PathBuf::from))
        .as_ref()
}

pub fn log_line(line: &str) {
    if let Some(path) = log_path() {
        log_line_to(path, line);
    }
}

fn log_line_to(path: &Path, line: &str) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
        .unwrap_or_else(|_| "?".into());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{stamp} [{}] {line}", std::process::id());
    }
}

pub fn set(v: bool) {
    VERBOSE.store(v, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        $crate::verbose::log_line(&line);
        if $crate::verbose::enabled() {
            use ::owo_colors::OwoColorize;
            eprintln!("{} {}", "resh:".dimmed(), line);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_are_appended_with_the_pid() {
        let path = std::env::temp_dir().join(format!("resh-log-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);

        log_line_to(&path, "first");
        log_line_to(&path, "second");

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "each call appends one line: {content:?}");
        assert!(lines[0].ends_with(" first"), "got: {}", lines[0]);
        assert!(lines[1].ends_with(" second"), "got: {}", lines[1]);
        assert!(
            lines[0].contains(&format!("[{}]", std::process::id())),
            "line must identify the process: {}",
            lines[0]
        );
        let _ = std::fs::remove_file(&path);
    }
}
