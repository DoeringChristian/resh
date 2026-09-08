use anyhow::{bail, Result};
use dialoguer::{MultiSelect, Select};
use owo_colors::OwoColorize;
use std::collections::HashSet;

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

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub name: String,
    pub raw_line: String,
}

pub fn list_sessions(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    paths: &RemotePaths,
) -> Result<Vec<SessionEntry>> {
    let cmd = format!(
        "{} --socket {} list 2>/dev/null",
        paths.shpool(),
        paths.socket()
    );
    let output = ssh.run_capture(host, extra_args, &cmd)?;
    Ok(parse_session_list(&output))
}

fn parse_session_list(output: &str) -> Vec<SessionEntry> {
    output
        .lines()
        .skip(1) // skip header
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let name = line.split_whitespace().next().unwrap_or("").to_string();
            SessionEntry {
                name,
                raw_line: line.to_string(),
            }
        })
        .collect()
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

    let items: Vec<&str> = sessions.iter().map(|s| s.raw_line.as_str()).collect();
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

    let items: Vec<&str> = sessions.iter().map(|s| s.raw_line.as_str()).collect();
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
    let session_list = sessions.join(" ");
    let cmd = format!(
        "{} --socket {} kill {session_list}",
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
        .filter(|s| s.raw_line.contains("detached"))
        .filter(|s| all || s.name.starts_with(&format!("{prefix}-")))
        .map(|s| s.name.as_str())
        .collect();

    if detached.is_empty() {
        eprintln!("No detached sessions.");
        return Ok(());
    }

    eprintln!("Killing detached sessions: {}", detached.join(", ").green());
    let names: Vec<String> = detached.iter().map(|s| s.to_string()).collect();
    kill_sessions(ssh, host, &names, paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_session_list() {
        let output = "NAME    STARTED_AT      STATUS\n\
                       s0    2026-05-22T18:02:29.300+00:00   attached\n\
                       s1    2026-05-22T18:03:00.000+00:00   detached\n";
        let sessions = parse_session_list(output);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "s0");
        assert_eq!(sessions[1].name, "s1");
    }

    #[test]
    fn test_parse_empty_session_list() {
        let output = "NAME    STARTED_AT      STATUS\n";
        let sessions = parse_session_list(output);
        assert!(sessions.is_empty());
    }
}
