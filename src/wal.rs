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

    if session::kill_sessions(ssh, host, &names, paths).is_ok() {
        let remaining: Vec<WalEntry> = entries.into_iter().filter(|e| e.host != host).collect();
        let _ = write_entries(&remaining);
        vlog!("wal: flushed entries for {host}");
    }
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
