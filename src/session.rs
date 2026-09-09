use anyhow::{bail, Context, Result};
use dialoguer::{MultiSelect, Select};
use owo_colors::OwoColorize;
use serde::Deserialize;
use std::collections::HashSet;

use crate::posix::quote;
use crate::ssh::SshContext;
use crate::upload::RemotePaths;
use crate::vlog;

pub fn local_prefix() -> String {
    let full = stable_hostname();
    let short = full.split('.').next().unwrap_or(&full);
    short.to_lowercase().replace(' ', "-")
}

fn stable_hostname() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("scutil")
            .args(["--get", "LocalHostName"])
            .output()
        {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

/// Whether a session currently has a client attached. `#[serde(other)]` keeps
/// parsing forward-compatible: a status spelling shpool adds in a future release
/// deserializes to `Unknown` instead of failing the whole list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum SessionStatus {
    Attached,
    Disconnected,
    #[serde(other)]
    #[default]
    Unknown,
}

impl SessionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Disconnected => "disconnected",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionEntry {
    pub name: String,
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default)]
    pub started_at_unix_ms: Option<i64>,
}

impl SessionEntry {
    /// One-line rendering for `sshr <host> list`.
    pub fn display_line(&self) -> String {
        let started = self
            .started_at_unix_ms
            .map(|ms| ms.to_string())
            .unwrap_or_else(|| "-".into());
        format!("{}\t{}\t{}", self.name, started, self.status.as_str())
    }
}

#[derive(Debug, Deserialize)]
struct SessionList {
    #[serde(default)]
    sessions: Vec<SessionEntry>,
}

pub fn list_sessions(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    paths: &RemotePaths,
) -> Result<Vec<SessionEntry>> {
    let cmd = format!(
        "{} --socket {} list --json 2>/dev/null",
        paths.shpool(),
        paths.socket()
    );
    let output = ssh.run_capture(host, extra_args, &cmd)?;
    parse_session_list(&output)
}

fn parse_session_list(output: &str) -> Result<Vec<SessionEntry>> {
    // No daemon / no sessions can yield empty stdout; treat that as no sessions
    // rather than a parse error.
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }
    let reply: SessionList =
        serde_json::from_str(output).context("invalid shpool list --json response")?;
    Ok(reply.sessions)
}

pub fn new_session_name(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    paths: &RemotePaths,
) -> Result<String> {
    let prefix = local_prefix();
    let sessions = list_sessions(ssh, host, extra_args, paths)?;
    let existing: HashSet<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    loop {
        let id: u32 = rand::random();
        let name = format!("{prefix}-{:06x}", id & 0xFFFFFF);
        if !existing.contains(name.as_str()) {
            vlog!("session: new = {name}");
            return Ok(name);
        }
    }
}

pub fn pick_session_interactive(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    all: bool,
    paths: &RemotePaths,
) -> Result<String> {
    let prefix = local_prefix();
    let sessions: Vec<_> = list_sessions(ssh, host, extra_args, paths)?
        .into_iter()
        .filter(|s| all || s.name.starts_with(&format!("{prefix}-")))
        .collect();
    if sessions.is_empty() {
        bail!("no existing sessions on {}", host);
    }

    let items: Vec<String> = sessions.iter().map(|s| s.display_line()).collect();
    let idx = Select::new()
        .with_prompt("Attach to session")
        .items(&items)
        .default(0)
        .interact()?;

    let name = sessions[idx].name.clone();
    vlog!("session: selected = {name}");
    Ok(name)
}

pub fn pick_sessions_to_kill(
    ssh: &SshContext,
    host: &str,
    all: bool,
    paths: &RemotePaths,
) -> Result<Vec<String>> {
    let prefix = local_prefix();
    let sessions: Vec<_> = list_sessions(ssh, host, &[], paths)?
        .into_iter()
        .filter(|s| all || s.name.starts_with(&format!("{prefix}-")))
        .collect();
    if sessions.is_empty() {
        bail!("no sessions on {}", host);
    }

    let items: Vec<String> = sessions.iter().map(|s| s.display_line()).collect();
    let indices = MultiSelect::new()
        .with_prompt("Kill sessions (space to toggle, enter to confirm)")
        .items(&items)
        .interact()?;

    if indices.is_empty() {
        bail!("no sessions selected");
    }

    Ok(indices.iter().map(|&i| sessions[i].name.clone()).collect())
}

pub fn kill_sessions(
    ssh: &SshContext,
    host: &str,
    sessions: &[String],
    paths: &RemotePaths,
) -> Result<()> {
    let session_list = sessions
        .iter()
        .map(|s| quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    // `--` terminates option parsing so a session name beginning with `-` is
    // never treated as a flag, and each name is quoted as one shell word.
    let cmd = format!(
        "{} --socket {} kill -- {session_list}",
        paths.shpool(),
        paths.socket()
    );
    ssh.run_capture(host, &[], &cmd)?;
    Ok(())
}

pub fn clean_detached(ssh: &SshContext, host: &str, all: bool, paths: &RemotePaths) -> Result<()> {
    let sessions = list_sessions(ssh, host, &[], paths)?;
    let prefix = local_prefix();
    let detached: Vec<&str> = sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Disconnected)
        .filter(|s| all || s.name.starts_with(&format!("{prefix}-")))
        .map(|s| s.name.as_str())
        .collect();

    if detached.is_empty() {
        eprintln!("No disconnected sessions.");
        return Ok(());
    }

    eprintln!(
        "Killing disconnected sessions: {}",
        detached.join(", ").green()
    );
    let names: Vec<String> = detached.iter().map(|s| s.to_string()).collect();
    kill_sessions(ssh, host, &names, paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_list_with_status_and_timestamps() {
        let output = r#"{
          "sessions": [
            {"name": "s0", "started_at_unix_ms": 1779472949300, "status": "Attached"},
            {"name": "s1", "started_at_unix_ms": 1779472980000, "status": "Disconnected"}
          ]
        }"#;
        let sessions = parse_session_list(output).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "s0");
        assert_eq!(sessions[0].status, SessionStatus::Attached);
        assert_eq!(sessions[1].name, "s1");
        assert_eq!(sessions[1].status, SessionStatus::Disconnected);
        assert_eq!(
            sessions[1].display_line(),
            "s1\t1779472980000\tdisconnected"
        );
    }

    #[test]
    fn empty_output_and_empty_session_array_are_no_sessions() {
        assert!(parse_session_list("").unwrap().is_empty());
        assert!(parse_session_list(r#"{"sessions":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn unknown_status_and_extra_fields_are_tolerated() {
        let output =
            r#"{"future":1,"sessions":[{"name":"s","status":"PausedForUpgrade","extra":true}]}"#;
        let sessions = parse_session_list(output).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, SessionStatus::Unknown);
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_session_list("NAME STARTED STATUS").is_err());
    }
}
