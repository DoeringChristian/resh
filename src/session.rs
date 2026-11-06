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
    /// One-line rendering for `resh <host> list`.
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

/// Resolve a session named on the command line against what the host has.
///
/// The check is the point of the command. shpool's own `attach` creates any
/// session name it does not already know, so passing a name straight through
/// would answer a typo with a brand new shell rather than the session being
/// asked for. A name matches in full, or by the hex suffix `list` prints after
/// the client prefix -- the only part that differs between one client's names.
pub fn resolve_session(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    requested: &str,
    paths: &RemotePaths,
) -> Result<String> {
    let sessions = list_sessions(ssh, host, extra_args, paths)?;
    let entry = match_session(&sessions, host, requested)?;

    // The picker shows each session's status, so whoever uses it can see they
    // are about to take a session away from another window. A name on the
    // command line shows nothing, and resh attaches with --force.
    if entry.status == SessionStatus::Attached {
        eprintln!(
            "{}",
            format!("{} is attached elsewhere; taking it over.", entry.name).yellow()
        );
    }

    let name = entry.name.clone();
    vlog!("session: resolved {requested} = {name}");
    Ok(name)
}

fn match_session<'a>(
    sessions: &'a [SessionEntry],
    host: &str,
    requested: &str,
) -> Result<&'a SessionEntry> {
    if let Some(exact) = sessions.iter().find(|s| s.name == requested) {
        return Ok(exact);
    }

    let suffix = format!("-{requested}");
    let matches: Vec<&SessionEntry> = sessions
        .iter()
        .filter(|s| s.name.ends_with(&suffix))
        .collect();

    match matches.as_slice() {
        [only] => Ok(only),
        [] => bail!(
            "no session '{requested}' on {host}{}",
            miss_hint(sessions, host)
        ),
        many => bail!(
            "'{requested}' matches {} sessions on {host}: {}",
            many.len(),
            names(many.iter().copied())
        ),
    }
}

/// What a miss can usefully add: the names while they still fit on a line, a
/// pointer to `list` once a host has collected more than a handful.
fn miss_hint(sessions: &[SessionEntry], host: &str) -> String {
    match sessions.len() {
        0 => " (it has no sessions)".to_string(),
        n if n <= 6 => format!(" (sessions: {})", names(sessions)),
        n => format!(" ({n} sessions; try: resh {host} list)"),
    }
}

fn names<'a>(sessions: impl IntoIterator<Item = &'a SessionEntry>) -> String {
    sessions
        .into_iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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
    let failures: Vec<String> = kill_each(ssh, host, sessions, paths)
        .into_iter()
        .filter_map(|(name, err)| err.map(|e| format!("{name}: {e}")))
        .collect();

    if failures.is_empty() {
        return Ok(());
    }
    bail!(
        "failed to kill {} of {} session(s):\n  {}",
        failures.len(),
        sessions.len(),
        failures.join("\n  ")
    );
}

/// Kill each session with its own request and report per-session outcomes.
///
/// One request per session on purpose: shpool's daemon walks a multi-session
/// kill in order and aborts the whole batch on the first session it cannot
/// signal, so a single unkillable session would otherwise spare every session
/// listed behind it (shpool < 0.11.0 leaves such sessions behind whenever a
/// shell dies without its entry being reaped).
pub fn kill_each(
    ssh: &SshContext,
    host: &str,
    sessions: &[String],
    paths: &RemotePaths,
) -> Vec<(String, Option<String>)> {
    let mut outcomes = Vec::with_capacity(sessions.len());
    let mut iter = sessions.iter();

    for name in iter.by_ref() {
        // `--` terminates option parsing so a session name beginning with
        // `-` is never treated as a flag, and the name is one shell word.
        let cmd = format!(
            "{} --socket {} kill -- {}",
            paths.shpool(),
            paths.socket(),
            quote(name)
        );
        match ssh.run_remote(host, &[], &cmd) {
            // ssh itself failed (host down, auth, broken master). Every
            // remaining session would fail the same way, so report them all
            // rather than sitting through one connection timeout each.
            Err(e) => {
                let err = format!("{e:#}");
                vlog!("session: kill {name} failed: {err}");
                outcomes.push((name.clone(), Some(err.clone())));
                outcomes.extend(iter.map(|rest| (rest.clone(), Some(err.clone()))));
                break;
            }
            // shpool exits non-zero for a session it cannot find, but a
            // session that is already gone is exactly what kill wants.
            Ok(out) if !out.status.success() && !is_already_gone(&out.stderr) => {
                let err = out
                    .error_detail()
                    .unwrap_or_else(|| format!("shpool kill exited with {}", out.status));
                vlog!("session: kill {name} failed: {err}");
                outcomes.push((name.clone(), Some(err)));
            }
            Ok(_) => outcomes.push((name.clone(), None)),
        }
    }

    outcomes
}

/// `shpool kill` reports a missing session as `not found: <name>` on stderr.
fn is_already_gone(stderr: &str) -> bool {
    stderr.lines().any(|l| l.trim().starts_with("not found:"))
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
    use crate::ssh::mock::{make_ctx, MockSsh};

    fn paths() -> RemotePaths {
        RemotePaths::new(None).unwrap()
    }

    /// One dead session must not stop the others from being killed. shpool's
    /// daemon aborts a whole multi-session kill on the first failure, so resh
    /// sends one session per request.
    #[test]
    fn kill_sends_one_request_per_session() {
        let mock = MockSsh::new("exit 0");
        let ctx = make_ctx(&mock);

        kill_sessions(
            &ctx,
            "fermat",
            &["alpha".into(), "beta".into(), "gamma".into()],
            &paths(),
        )
        .unwrap();

        let calls = mock.calls();
        assert_eq!(
            calls.len(),
            3,
            "expected one ssh call per session: {calls:?}"
        );
        assert!(calls[0].contains("alpha") && !calls[0].contains("beta"));
        assert!(calls[1].contains("beta"));
        assert!(calls[2].contains("gamma"));
    }

    #[test]
    fn kill_continues_after_a_session_fails() {
        let mock = MockSsh::new("case \"$*\" in *beta*) echo 'boom' >&2; exit 1;; esac\nexit 0");
        let ctx = make_ctx(&mock);

        let err = kill_sessions(
            &ctx,
            "fermat",
            &["alpha".into(), "beta".into(), "gamma".into()],
            &paths(),
        )
        .unwrap_err();

        let calls = mock.calls();
        assert_eq!(
            calls.len(),
            3,
            "a failure must not abort the rest: {calls:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("beta"), "error must name the session: {msg}");
        assert!(
            !msg.contains("alpha"),
            "must not blame healthy sessions: {msg}"
        );
    }

    /// shpool exits non-zero for a session it cannot find, but "already gone"
    /// is the state kill is asking for. Treating it as a failure would keep
    /// WAL entries for vanished sessions pending forever.
    #[test]
    fn kill_treats_an_already_gone_session_as_success() {
        let mock = MockSsh::new("echo 'not found: alpha' >&2\nexit 1");
        let ctx = make_ctx(&mock);

        kill_sessions(&ctx, "fermat", &["alpha".into()], &paths()).unwrap();
    }

    /// A dead host fails identically for every session, so resh must not sit
    /// through one connection timeout per selected session.
    #[test]
    fn kill_stops_dialling_after_the_connection_fails() {
        let mock = MockSsh::new(
            "echo 'ssh: connect to host fermat port 22: No route to host' >&2\nexit 255",
        );
        let ctx = make_ctx(&mock);

        let err = kill_sessions(
            &ctx,
            "fermat",
            &["alpha".into(), "beta".into(), "gamma".into()],
            &paths(),
        )
        .unwrap_err();

        assert_eq!(
            mock.calls().len(),
            1,
            "must stop after the first connection failure"
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("alpha") && msg.contains("beta") && msg.contains("gamma"));
        assert!(msg.contains("No route to host"), "got: {msg}");
    }

    #[test]
    fn kill_reports_remote_stderr() {
        let mock = MockSsh::new("echo 'killing shell proc: ESRCH' >&2\nexit 1");
        let ctx = make_ctx(&mock);

        let err = kill_sessions(&ctx, "fermat", &["alpha".into()], &paths()).unwrap_err();

        let msg = format!("{err:#}");
        assert!(
            msg.contains("ESRCH"),
            "remote stderr must reach the user: {msg}"
        );
    }

    fn entries(names: &[&str]) -> Vec<SessionEntry> {
        names
            .iter()
            .map(|n| SessionEntry {
                name: (*n).to_string(),
                status: SessionStatus::Disconnected,
                started_at_unix_ms: None,
            })
            .collect()
    }

    #[test]
    fn attach_resolves_a_full_session_name() {
        let sessions = entries(&["mac-a3f21b", "mac-c7e049"]);
        assert_eq!(
            match_session(&sessions, "fermat", "mac-c7e049")
                .unwrap()
                .name,
            "mac-c7e049"
        );
    }

    /// Every session one client makes carries the same hostname prefix, so the
    /// hex suffix is the only part worth typing -- and the only part that
    /// differs between the names `list` prints.
    #[test]
    fn attach_resolves_a_unique_suffix() {
        let sessions = entries(&["mac-a3f21b", "mac-c7e049"]);
        assert_eq!(
            match_session(&sessions, "fermat", "c7e049").unwrap().name,
            "mac-c7e049"
        );
    }

    /// With `-a` the list spans clients, and two of them can land on the same
    /// suffix. Picking one would be a coin toss, so both are named instead.
    #[test]
    fn an_ambiguous_suffix_names_every_candidate() {
        let sessions = entries(&["mac-a3f21b", "legion-a3f21b"]);
        let err = match_session(&sessions, "fermat", "a3f21b")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("mac-a3f21b") && err.contains("legion-a3f21b"),
            "both candidates must be named: {err}"
        );
    }

    /// shpool's own `attach` creates whatever name it cannot find, so handing
    /// it an unchecked name answers a typo with a brand new shell instead of
    /// the session asked for. A name resh cannot resolve is refused.
    #[test]
    fn an_unknown_session_name_is_refused_rather_than_created() {
        let sessions = entries(&["mac-a3f21b"]);
        let err = match_session(&sessions, "fermat", "a3f2")
            .unwrap_err()
            .to_string();
        assert!(err.contains("a3f2"), "must quote what was asked: {err}");
        assert!(
            err.contains("mac-a3f21b"),
            "and show what the host has: {err}"
        );
    }

    /// A host collects sessions for weeks, so listing them all turns a typo
    /// into a wall of hex. Past a handful the miss points at `list` instead.
    #[test]
    fn a_miss_on_a_crowded_host_points_at_list() {
        let many: Vec<String> = (0..24).map(|i| format!("mac-{i:06x}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let sessions = entries(&refs);

        let err = match_session(&sessions, "fermat", "nope")
            .unwrap_err()
            .to_string();

        assert!(err.contains("24 sessions"), "must say how many: {err}");
        assert!(err.contains("resh fermat list"), "and where to look: {err}");
        assert!(
            !err.contains("mac-000000"),
            "but not dump every name: {err}"
        );
    }

    #[test]
    fn resolving_a_name_asks_the_host_for_its_sessions() {
        let mock = MockSsh::new(
            "echo '{\"sessions\":[{\"name\":\"mac-a3f21b\",\"status\":\"Disconnected\"}]}'",
        );
        let ctx = make_ctx(&mock);

        let name = resolve_session(&ctx, "fermat", &[], "a3f21b", &paths()).unwrap();

        assert_eq!(name, "mac-a3f21b");
        let calls = mock.calls();
        assert!(
            calls.iter().any(|c| c.contains("list --json")),
            "must resolve against the host's list: {calls:?}"
        );
    }

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
