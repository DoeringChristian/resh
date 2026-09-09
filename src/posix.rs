//! Correct POSIX shell quoting for values interpolated into remote commands.
//!
//! Session names, shells, and working directories can contain spaces, quotes,
//! and shell metacharacters. Single-quoting with `'\''` escaping turns any byte
//! sequence into exactly one shell word, so the remote shell never re-interprets
//! it. This replaces the older ad-hoc `shell_escape`, which left several
//! metacharacters unquoted.

/// Quote one arbitrary value as a single POSIX shell word.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Quote a remote path as one shell word, giving a leading `~/` explicit
/// remote-HOME semantics without relying on shell-specific tilde expansion.
/// A bare `~` becomes `"$HOME"`, `~/foo` becomes `"$HOME"/'foo'`, and any other
/// value is quoted literally.
pub fn quote_remote_path(value: &str) -> String {
    if value == "~" {
        return "\"$HOME\"".into();
    }
    if let Some(relative) = value.strip_prefix("~/") {
        if relative.is_empty() {
            "\"$HOME\"".into()
        } else {
            format!("\"$HOME\"/{}", quote(relative))
        }
    } else {
        quote(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_handles_metacharacters_spaces_quotes_unicode_and_leading_dash() {
        let value = "- name '雪';$(touch nope) & * ?";
        let quoted = quote(value);
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("printf %s {quoted}")])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, value.as_bytes());
    }

    #[test]
    fn leading_tilde_has_explicit_remote_home_semantics() {
        let quoted = quote_remote_path("~/dir with ' quote/雪");
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("printf %s {quoted}")])
            .env("HOME", "/remote/home")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, "/remote/home/dir with ' quote/雪".as_bytes());
        assert_eq!(quote_remote_path("~"), "\"$HOME\"");
        assert_eq!(quote_remote_path("/absolute/path"), "'/absolute/path'");
    }
}
