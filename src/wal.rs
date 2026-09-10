use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::session;
use crate::ssh::SshContext;
use crate::upload::RemotePaths;
use crate::vlog;

pub fn wal_path() -> PathBuf {
    data_dir().join("sshr").join("close.wal")
}

#[derive(Debug, Clone)]
struct WalEntry {
    host: String,
    session: String,
}

fn read_entries() -> Vec<WalEntry> {
    let content = match fs::read_to_string(wal_path()) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter_map(|line| {
            let (host, session) = line.split_once(':')?;
            Some(WalEntry {
                host: host.to_string(),
                session: session.to_string(),
            })
        })
        .collect()
}

fn write_entries(entries: &[WalEntry]) -> Result<()> {
    let path = wal_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create WAL directory")?;
    }
    let content: String = entries
        .iter()
        .map(|e| format!("{}:{}", e.host, e.session))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        &path,
        if content.is_empty() {
            content
        } else {
            content + "\n"
        },
    )
    .context("failed to write WAL")?;
    Ok(())
}

/// Record that a session should be closed. Tries to kill immediately;
/// if that fails the entry stays for replay on next connect.
pub fn record_close(ssh: &SshContext, host: &str, session_name: &str, paths: &RemotePaths) {
    let mut entries = read_entries();
    entries.push(WalEntry {
        host: host.to_string(),
        session: session_name.to_string(),
    });
    if let Err(e) = write_entries(&entries) {
        vlog!("wal: failed to write: {e}");
        return;
    }
    vlog!("wal: recorded close for {host}:{session_name}");

    if session::kill_sessions(ssh, host, &[session_name.to_string()], paths).is_ok() {
        remove_entry(host, session_name);
    }
}

/// Replay pending close operations for a host. Called on connect.
pub fn replay(ssh: &SshContext, host: &str, paths: &RemotePaths) {
    let entries = read_entries();
    let pending: Vec<&WalEntry> = entries.iter().filter(|e| e.host == host).collect();
    if pending.is_empty() {
        return;
    }

    let names: Vec<String> = pending.iter().map(|e| e.session.clone()).collect();
    vlog!(
        "wal: replaying {} pending close(s) for {host}: {}",
        names.len(),
        names.join(", ")
    );

    let outcomes = session::kill_each(ssh, host, &names, paths);
    let remaining = remaining_after(entries, host, &outcomes);
    let _ = write_entries(&remaining);
    vlog!(
        "wal: {} of {} close(s) for {host} still pending",
        remaining.iter().filter(|e| e.host == host).count(),
        names.len()
    );
}

/// Drop the entries whose session was killed, keeping the ones that failed so
/// a later connect retries them. Entries for other hosts are left untouched.
fn remaining_after(
    entries: Vec<WalEntry>,
    host: &str,
    outcomes: &[(String, Option<String>)],
) -> Vec<WalEntry> {
    entries
        .into_iter()
        .filter(|e| {
            e.host != host
                || !outcomes
                    .iter()
                    .any(|(name, err)| *name == e.session && err.is_none())
        })
        .collect()
}

/// Drop pending close entries for sessions that are now gone. Called by
/// `sshr <host> kill`, which is both a user-facing command and the process the
/// close signal hands the kill to.
pub fn forget(host: &str, sessions: &[String]) {
    let remaining = without_sessions(read_entries(), host, sessions);
    let _ = write_entries(&remaining);
    vlog!("wal: forgot {} session(s) for {host}", sessions.len());
}

fn without_sessions(entries: Vec<WalEntry>, host: &str, sessions: &[String]) -> Vec<WalEntry> {
    entries
        .into_iter()
        .filter(|e| e.host != host || !sessions.contains(&e.session))
        .collect()
}

fn remove_entry(host: &str, session_name: &str) {
    let entries = read_entries();
    let remaining: Vec<WalEntry> = entries
        .into_iter()
        .filter(|e| !(e.host == host && e.session == session_name))
        .collect();
    let _ = write_entries(&remaining);
    vlog!("wal: removed {host}:{session_name}");
}

fn data_dir() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
                .join(".local/share")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, session: &str) -> WalEntry {
        WalEntry {
            host: host.into(),
            session: session.into(),
        }
    }

    /// `sshr <host> kill <session>` is what the close signal hands the kill to,
    /// so a successful kill has to clear the pending entry the signal handler
    /// wrote — including the duplicate a second close signal may have added.
    #[test]
    fn forgetting_a_session_drops_all_its_entries_for_that_host_only() {
        let entries = vec![
            entry("fermat", "alpha"),
            entry("fermat", "alpha"),
            entry("fermat", "beta"),
            entry("euler", "alpha"),
        ];

        let remaining = without_sessions(entries, "fermat", &["alpha".to_string()]);

        let left: Vec<(&str, &str)> = remaining
            .iter()
            .map(|e| (e.host.as_str(), e.session.as_str()))
            .collect();
        assert_eq!(left, vec![("fermat", "beta"), ("euler", "alpha")]);
    }

    /// A session that could not be killed must stay pending, but it must not
    /// hold back the entries that were killed successfully.
    #[test]
    fn only_the_sessions_that_failed_stay_pending() {
        let entries = vec![
            entry("fermat", "alpha"),
            entry("fermat", "beta"),
            entry("euler", "gamma"),
        ];
        let outcomes = vec![
            ("alpha".to_string(), None),
            ("beta".to_string(), Some("boom".to_string())),
        ];

        let remaining = remaining_after(entries, "fermat", &outcomes);

        let names: Vec<&str> = remaining.iter().map(|e| e.session.as_str()).collect();
        assert_eq!(names, vec!["beta", "gamma"]);
    }
}
