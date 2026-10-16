//! Terminal user variables (OSC 1337 SetUserVar).
//!
//! These belong to the terminal *window*, not to this process, so one that is
//! left set outlives resh and misdirects whatever runs in that window next: a
//! launcher reading `resh_host` will connect a local window to a remote host,
//! in the remote's name, with the local working directory.
//!
//! So the variables are not a pair of fire-and-forget writes but a claim with
//! an owner. `SessionVars` takes host and session together -- there is no way
//! to announce a host before a session exists -- and withdraws both when it is
//! dropped, which covers every `?` on the way out. The paths Drop cannot
//! reach are process exit (see `cmd_connect`, which drops before it exits) and
//! signals (see `signal::install_interrupt_handler`).

use base64::Engine;

/// Keys resh publishes. Kept here so the setting and the clearing cannot drift.
const HOST: &str = "resh_host";
const SESSION: &str = "resh_session";

/// The sequence that withdraws both variables, pre-rendered as a constant so a
/// signal handler can write it without allocating. An empty value is how OSC
/// 1337 unsets one: base64 of "" is "".
pub const WITHDRAW_BOTH: &str =
    "\x1b]1337;SetUserVar=resh_host=\x07\x1b]1337;SetUserVar=resh_session=\x07";

/// The escape sequence announcing `key=value`.
fn seq(key: &str, value: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(value);
    format!("\x1b]1337;SetUserVar={key}={encoded}\x07")
}

type Emit = fn(&str);

fn emit_to_terminal(s: &str) {
    eprint!("{s}");
}

/// The host and session of a live session, published to the terminal for as
/// long as it lasts and withdrawn when this value is dropped.
pub struct SessionVars {
    emit: Emit,
}

impl SessionVars {
    /// Announce a session. Taking both halves at once is the point: a host
    /// without a session is exactly the state that strands a window.
    pub fn announce(host: &str, session: &str) -> Self {
        Self::announce_with(emit_to_terminal, host, session)
    }

    fn announce_with(emit: Emit, host: &str, session: &str) -> Self {
        emit(&seq(HOST, host));
        emit(&seq(SESSION, session));
        Self { emit }
    }
}

impl Drop for SessionVars {
    fn drop(&mut self) {
        (self.emit)(WITHDRAW_BOTH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static EMITTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    // The recording sink is global because `Emit` is a plain fn pointer, so the
    // tests that use it have to run one at a time. Poisoning is stepped over
    // rather than unwrapped: a test that fails inside the lock should report its
    // own assertion, not reappear as a PoisonError in the next one.
    fn recording() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EMITTED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        guard
    }

    fn record(s: &str) {
        EMITTED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(s.to_string());
    }

    fn emitted() -> String {
        EMITTED.lock().unwrap_or_else(|e| e.into_inner()).concat()
    }

    #[test]
    fn a_variable_is_announced_as_base64_in_osc_1337() {
        assert_eq!(
            seq("resh_host", "fermat"),
            "\x1b]1337;SetUserVar=resh_host=ZmVybWF0\x07"
        );
    }

    // The failure this guards against is a window left claiming a host after
    // resh is gone: whatever runs there next gets launched against the remote.
    #[test]
    fn dropping_the_guard_withdraws_both_variables() {
        let _lock = recording();

        {
            let _vars = SessionVars::announce_with(record, "fermat", "rgl-c025787m-abc123");
            assert!(
                emitted().contains("ZmVybWF0"),
                "the host should be announced while the session lives: {:?}",
                emitted()
            );
        }

        let out = emitted();
        assert!(
            out.ends_with(WITHDRAW_BOTH),
            "the guard must withdraw both variables as it goes: {out:?}"
        );
    }

    // An error on the way to a session is the path that actually stranded a
    // window: `?` unwinds past the guard, which must still withdraw.
    #[test]
    fn an_early_return_still_withdraws() {
        let _lock = recording();

        fn connect_then_fail() -> Result<(), &'static str> {
            let _vars = SessionVars::announce_with(record, "kerr", "rgl-c025787m-408bda");
            Err("remote command failed")
        }

        assert!(connect_then_fail().is_err());
        assert!(
            emitted().ends_with(WITHDRAW_BOTH),
            "a failed connect must not leave the window claiming a host: {:?}",
            emitted()
        );
    }

    // The constant exists so a signal handler can write it without allocating;
    // if it drifted from `seq` it would silently stop clearing anything.
    #[test]
    fn the_prerendered_withdrawal_matches_the_generated_one() {
        assert_eq!(
            WITHDRAW_BOTH,
            format!("{}{}", seq(HOST, ""), seq(SESSION, ""))
        );
    }
}
