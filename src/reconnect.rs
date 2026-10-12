use anyhow::Result;
use owo_colors::OwoColorize;
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};

/// Escape sequences that put the terminal back to a sane state.
///
/// A full-screen program on the remote switches these modes on and switches
/// them off when it exits. When the connection dies it never exits -- it is
/// still running on the far side -- so nobody switches them off, and only the
/// local side can. `stty sane` fixes the tty's line discipline; none of this
/// is the line discipline.
///
/// The keyboard protocol is cleared twice because the main and alternate
/// screens keep independent keyboard-mode stacks, so one clear only fixes
/// whichever screen is current.
fn reset_sequence() -> &'static str {
    concat!(
        "\x1b[=0;1u",  // kitty keyboard protocol: back to legacy keys (alt screen)
        "\x1b[?1049l", // leave the alternate screen
        "\x1b[=0;1u",  // and again for the main screen
        "\x1b[!p",     // soft reset: cursor keys, keypad, origin, margins, charset
        "\x1b[?25h",   // show the cursor
        "\x1b[0m",     // drop colours and attributes
        "\x1b[r",      // scroll region back to the whole screen
        "\x1b(B",      // ASCII charset, not line drawing
        "\x1b>",       // numeric keypad
        "\x1b[?1l",    // normal cursor keys
        "\x1b[?2026l", // synchronized output off, or the screen can stop updating
        "\x1b[?2004l", // bracketed paste off
        "\x1b[?1004l", // focus reporting off
        "\x1b[?1006l", // SGR mouse off
        "\x1b[?1015l", // urxvt mouse off
        "\x1b[?1005l", // utf8 mouse off
        "\x1b[?1003l", // any-event mouse tracking off
        "\x1b[?1002l", // button-event mouse tracking off
        "\x1b[?1000l", // normal mouse tracking off
    )
}

fn reset_terminal() {
    let _ = Command::new("stty")
        .arg("sane")
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    eprint!("{}", reset_sequence());
}

/// Run a connection function in a loop, prompting to reconnect on failure.
/// `on_ssh_error` is called when SSH itself fails (exit 255), e.g. to tear down
/// a broken ControlMaster. It is NOT called when the remote command exits
/// non-zero, since the SSH connection may still be healthy (another session's master).
/// Returns the exit code resh should propagate: 0 on a clean session exit, or
/// the last remote/SSH failure code when the loop ends via shutdown or the user
/// declining to reconnect.
pub fn run_with_reconnect<F, B>(connect: F, on_ssh_error: B) -> Result<i32>
where
    F: Fn() -> Result<ExitStatus>,
    B: Fn(),
{
    run_reconnect_loop(
        connect,
        on_ssh_error,
        wait_for_keypress,
        crate::signal::is_closing,
    )
}

pub(crate) fn run_reconnect_loop<F, B, W, C>(
    connect: F,
    on_ssh_error: B,
    wait: W,
    is_closing: C,
) -> Result<i32>
where
    F: Fn() -> Result<ExitStatus>,
    B: Fn(),
    W: Fn() -> bool,
    C: Fn() -> bool,
{
    let mut last_failure = 1;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let mut status = connect()?;
        reset_terminal();

        if status.success() {
            return Ok(0);
        }
        last_failure = status.code().unwrap_or(last_failure);
        if is_closing() {
            return Ok(last_failure);
        }

        if status.code() == Some(255) {
            on_ssh_error();
            status = connect()?;
            reset_terminal();
            if status.success() {
                return Ok(0);
            }
            last_failure = status.code().unwrap_or(last_failure);
            if is_closing() {
                return Ok(last_failure);
            }
        }

        // 255 is SSH's own exit code; anything else came from the remote
        // command, meaning the connection itself is fine.
        let reason = match status.code() {
            Some(255) => "Connection lost".to_string(),
            Some(code) => format!("Remote command failed (exit {code})"),
            None => "Connection killed by signal".to_string(),
        };
        eprintln!();
        eprintln!(
            "{}",
            format!("{reason}, attempt {attempt} of reconnecting failed.")
                .yellow()
                .bold()
        );
        eprintln!(
            "{}",
            "Press any key to retry, Ctrl-C to quit.".yellow().bold()
        );

        if !wait() {
            return Ok(last_failure);
        }

        eprintln!("{}", "Reconnecting...".dimmed());
    }
}

/// Wait for a single keypress. Returns false on EOF or error (e.g. Ctrl-C).
///
/// Reads in raw mode for two reasons. `reset_terminal` has just run `stty
/// sane`, which restores canonical mode and echo -- so a cooked read would not
/// return until Enter, making "press any key" a lie, and every key pressed
/// before that would be echoed to the screen. With a remote TUI's keyboard
/// mode still in effect those echoes are escape sequences like `^[[106;9:3u`.
///
/// The input queue is flushed afterwards: one keypress can produce several
/// bytes, and the remainder would otherwise be delivered to the reconnected
/// session as if typed.
fn wait_for_keypress() -> bool {
    let fd = libc::STDIN_FILENO;
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let is_tty = unsafe { libc::tcgetattr(fd, &mut saved) } == 0;

    if is_tty {
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
    }

    let mut buf = [0u8; 1];
    let got = std::io::stdin().read(&mut buf);

    if is_tty {
        unsafe {
            libc::tcflush(fd, libc::TCIFLUSH);
            libc::tcsetattr(fd, libc::TCSANOW, &saved);
        }
    }

    matches!(got, Ok(1..))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TUI that dies with the connection never gets to switch these back off
    /// -- it is still running on the far side of a dead wire. Until the local
    /// side clears them, keys arrive as `^[[106;9:3u`, the cursor can be
    /// invisible, colours stick, and output is confined to a stale scroll
    /// region.
    #[test]
    fn terminal_reset_clears_what_a_dead_tui_leaves_behind() {
        let seq = reset_sequence();

        for (what, code) in [
            ("kitty keyboard protocol", "\x1b[=0;1u"),
            ("alternate screen", "\x1b[?1049l"),
            ("cursor visibility", "\x1b[?25h"),
            ("text attributes", "\x1b[0m"),
            ("scroll region", "\x1b[r"),
            ("ASCII charset", "\x1b(B"),
            ("numeric keypad", "\x1b>"),
            ("application cursor keys", "\x1b[?1l"),
            ("synchronized output", "\x1b[?2026l"),
            ("bracketed paste", "\x1b[?2004l"),
            ("focus reporting", "\x1b[?1004l"),
        ] {
            assert!(seq.contains(code), "reset must clear {what} ({code:?})");
        }
    }

    /// The two screens keep independent keyboard-mode stacks, so clearing it
    /// once only fixes whichever screen happens to be current.
    #[test]
    fn keyboard_protocol_is_cleared_on_both_screens() {
        let seq = reset_sequence();
        let alt_exit = seq
            .find("\x1b[?1049l")
            .expect("leaves the alternate screen");
        let first = seq
            .find("\x1b[=0;1u")
            .expect("clears the keyboard protocol");
        let last = seq.rfind("\x1b[=0;1u").unwrap();

        assert!(
            first < alt_exit,
            "must clear it before leaving the alt screen"
        );
        assert!(last > alt_exit, "and again after, for the main screen");
    }

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn exit_status(code: i32) -> ExitStatus {
        Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .status()
            .unwrap()
    }

    struct ConnectSequence {
        codes: Vec<i32>,
        call_count: AtomicUsize,
    }

    impl ConnectSequence {
        fn new(codes: Vec<i32>) -> Self {
            Self {
                codes,
                call_count: AtomicUsize::new(0),
            }
        }

        fn connect(&self) -> Result<ExitStatus> {
            let i = self.call_count.fetch_add(1, Ordering::SeqCst);
            let code = self.codes.get(i).copied().unwrap_or(0);
            Ok(exit_status(code))
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn success_on_first_connect() {
        let seq = ConnectSequence::new(vec![0]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("should not wait for keypress"),
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 1);
        assert_eq!(errors.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn signal_breaks_loop_immediately() {
        let seq = ConnectSequence::new(vec![1]);

        let result = run_reconnect_loop(
            || seq.connect(),
            || panic!("should not call on_ssh_error"),
            || panic!("should not wait"),
            || true, // closing
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 1);
    }

    #[test]
    fn remote_cmd_failure_does_not_kill_master() {
        let seq = ConnectSequence::new(vec![1]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || false, // user quits
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 1);
        assert_eq!(
            errors.load(Ordering::SeqCst),
            0,
            "on_ssh_error must not fire for non-255 exits"
        );
    }

    #[test]
    fn ssh_error_kills_master_and_retries() {
        // Call 1: exit 255 (broken master) → kill master → Call 2: exit 0 (fresh)
        let seq = ConnectSequence::new(vec![255, 0]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("immediate retry should succeed, no prompt needed"),
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 2);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ssh_error_retry_also_fails_user_quits() {
        // Call 1: 255, kill master, Call 2: 255 (network down), user quits
        let seq = ConnectSequence::new(vec![255, 255]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || false, // user quits
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 2);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ssh_error_network_recovers_after_prompt() {
        // Call 1: 255 (broken), Call 2: 255 (still down), user retries,
        // Call 3: 0 (network back)
        let seq = ConnectSequence::new(vec![255, 255, 0]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || true, // user keeps retrying
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 3);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn multiple_ssh_errors_before_recovery() {
        // 255 → retry 255 → prompt → 255 → retry 255 → prompt → 0
        let seq = ConnectSequence::new(vec![255, 255, 255, 255, 0]);
        let errors = AtomicUsize::new(0);
        let waits = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || {
                waits.fetch_add(1, Ordering::SeqCst);
                true
            },
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 5);
        assert_eq!(errors.load(Ordering::SeqCst), 2);
        assert_eq!(waits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn signal_during_retry_after_ssh_error() {
        // Call 1: 255, kill master, Call 2: non-zero but signal fires
        let seq = ConnectSequence::new(vec![255, 1]);
        let errors = AtomicUsize::new(0);
        let closing = AtomicBool::new(false);

        let call_count = &seq.call_count;
        let result = run_reconnect_loop(
            || {
                let status = seq.connect();
                if call_count.load(Ordering::SeqCst) == 2 {
                    closing.store(true, Ordering::SeqCst);
                }
                status
            },
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("signal should prevent prompt"),
            || closing.load(Ordering::SeqCst),
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 2);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn connect_error_propagates() {
        let result = run_reconnect_loop(
            || Err(anyhow::anyhow!("ssh not found")),
            || panic!("should not call on_ssh_error"),
            || panic!("should not wait"),
            || false,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ssh not found"));
    }

    #[test]
    fn remote_failure_then_user_reconnects_successfully() {
        // Exit 1 (shpool crash), user retries, exit 0
        let seq = ConnectSequence::new(vec![1, 0]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || true,
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(seq.calls(), 2);
        assert_eq!(
            errors.load(Ordering::SeqCst),
            0,
            "non-255 should never trigger on_ssh_error"
        );
    }

    // When B already established a healthy master, A's connect through it succeeds
    // on the first attempt. on_ssh_error is never called → B's master is untouched.
    #[test]
    fn healthy_master_from_other_session_not_killed() {
        let seq = ConnectSequence::new(vec![0]);
        let errors = AtomicUsize::new(0);

        let result = run_reconnect_loop(
            || seq.connect(),
            || {
                errors.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("should not wait"),
            || false,
        );

        assert!(result.is_ok());
        assert_eq!(
            errors.load(Ordering::SeqCst),
            0,
            "must not kill another session's master"
        );
    }

    // Simulates: A connects, connection drops, master is stale. Reconnect kills
    // master (exit 255 → on_ssh_error), immediate retry creates fresh master.
    #[test]
    fn stale_master_killed_then_fresh_connection() {
        let seq = ConnectSequence::new(vec![255, 0]);
        let master_alive = AtomicBool::new(true);

        let result = run_reconnect_loop(
            || {
                if master_alive.load(Ordering::SeqCst) {
                    seq.connect()
                } else {
                    let _ = seq.connect();
                    Ok(exit_status(0))
                }
            },
            || {
                master_alive.store(false, Ordering::SeqCst);
            },
            || panic!("immediate retry should succeed"),
            || false,
        );

        assert!(result.is_ok());
        assert!(
            !master_alive.load(Ordering::SeqCst),
            "master should have been killed"
        );
    }
}
