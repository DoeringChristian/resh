use std::ffi::CString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

static CLOSING: AtomicBool = AtomicBool::new(false);
// Set by the first close signal. Closing a terminal window delivers more than
// one (the terminal signals the process group and the kernel signals it again
// when the pty goes away), and the close must only be acted on once.
static CLOSE_HANDLED: AtomicBool = AtomicBool::new(false);
static WAL_CONTEXT: Mutex<Option<WalContext>> = Mutex::new(None);
static KILL_COMMAND: Mutex<Option<KillCommand>> = Mutex::new(None);
// PID of the current interactive ssh child. A close signal terminates it so
// `run_interactive` returns at once and the main thread can kill the remote
// session directly, rather than blocking in wait() until the terminal SIGKILLs
// us (which would defer cleanup to the next connect). 0 = no child running.
static SSH_CHILD_PID: AtomicI32 = AtomicI32::new(0);

struct WalContext {
    wal_path: Vec<u8>,
    entry_line: Vec<u8>,
}

/// `sshr <host> kill <session>`, marshalled ahead of time so the handler can
/// exec it without allocating.
struct KillCommand {
    exe: CString,
    host: CString,
    kill: CString,
    session: CString,
}

extern "C" fn handle_signal(signo: libc::c_int) {
    CLOSING.store(true, Ordering::SeqCst);

    if CLOSE_HANDLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        write_wal_entry();
        spawn_detached_killer();
    }

    // Pass the signal on to the interactive ssh child so its wait() returns
    // promptly and the caller runs the direct remote-session kill now. A signal
    // handler cannot do the SSH kill itself (not async-signal-safe); kill(2) is.
    //
    // On a window close the kernel already SIGHUPs the whole foreground process
    // group, ssh included, so this is a no-op there. It matters for a bare
    // `kill <pid>`, which reaches sshr alone and would otherwise leave ssh
    // running and wait() blocked until something SIGKILLs us. Forwarding the
    // signal we received rather than a substitute keeps the two paths honest.
    // Guarded by > 0 so it is a no-op when no child is running.
    let pid = SSH_CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signo);
        }
    }
}

/// Append this session's close entry to the WAL with async-signal-safe syscalls.
fn write_wal_entry() {
    if let Ok(guard) = WAL_CONTEXT.try_lock() {
        if let Some(ctx) = guard.as_ref() {
            unsafe {
                let fd = libc::open(
                    ctx.wal_path.as_ptr() as *const libc::c_char,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    0o644,
                );
                if fd >= 0 {
                    libc::write(
                        fd,
                        ctx.entry_line.as_ptr() as *const _,
                        ctx.entry_line.len(),
                    );
                    libc::close(fd);
                }
            }
        }
    }
}

/// Hand the remote kill to `sshr <host> kill <session>` in a session of its own.
///
/// Closing a terminal window kills this process along with the window, usually
/// before it can reach its own cleanup code — the WAL exists precisely because
/// that cleanup is unreliable. A child in a new session survives the teardown of
/// the window's process group and can take as long as it needs, including
/// building a fresh SSH connection once the multiplexed one is gone.
///
/// `fork`, `setsid` and `execv` are all async-signal-safe; the arguments are
/// marshalled in `install_handlers` so nothing here allocates.
fn spawn_detached_killer() {
    let Ok(guard) = KILL_COMMAND.try_lock() else {
        return;
    };
    let Some(cmd) = guard.as_ref() else {
        return;
    };

    // Stack array: building a Vec of pointers here would allocate.
    let argv: [*const libc::c_char; 5] = [
        cmd.exe.as_ptr(),
        cmd.host.as_ptr(),
        cmd.kill.as_ptr(),
        cmd.session.as_ptr(),
        std::ptr::null(),
    ];

    unsafe {
        if libc::fork() == 0 {
            libc::setsid();
            libc::execv(cmd.exe.as_ptr(), argv.as_ptr());
            // Only reachable if exec failed; _exit avoids running atexit
            // handlers inherited from the parent.
            libc::_exit(127);
        }
    }
}

/// Record the current interactive ssh child PID (0 to clear once it has exited),
/// so the close signal handler can end the session promptly.
pub fn set_ssh_child(pid: i32) {
    SSH_CHILD_PID.store(pid, Ordering::SeqCst);
}

pub fn install_handlers(host: &str, session_name: &str) {
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("sshr"));
    install_handlers_with_exe(&exe, host, session_name);
}

/// `install_handlers` with an explicit path to the binary the close signal
/// should exec, so tests can point it at a stand-in.
fn install_handlers_with_exe(exe: &Path, host: &str, session_name: &str) {
    let wal_path = crate::wal::wal_path();

    // Ensure the WAL directory exists before we need it in a signal handler
    if let Some(parent) = wal_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Null-terminated path for libc::open
    let mut path_bytes = wal_path.into_os_string().into_vec();
    path_bytes.push(0);

    let entry_line = format!("{host}:{session_name}\n").into_bytes();

    *WAL_CONTEXT.lock().unwrap() = Some(WalContext {
        wal_path: path_bytes,
        entry_line,
    });

    // Marshalled now so the handler only has to fork and exec. A NUL in any of
    // these is impossible from a path or a session name, but bail rather than
    // panic in a process that is about to be signalled.
    if let (Ok(exe), Ok(host), Ok(kill), Ok(session)) = (
        CString::new(exe.as_os_str().as_bytes()),
        CString::new(host),
        CString::new("kill"),
        CString::new(session_name),
    ) {
        *KILL_COMMAND.lock().unwrap() = Some(KillCommand {
            exe,
            host,
            kill,
            session,
        });
    }

    unsafe {
        libc::signal(
            libc::SIGHUP,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

pub fn is_closing() -> bool {
    CLOSING.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;

    // The signal is forwarded unchanged: ssh should die of the same signal that
    // reached sshr, not of a substitute. Runs in an isolated subprocess for the
    // same reason as the test below.
    #[test]
    fn close_signal_is_forwarded_unchanged_to_the_ssh_child() {
        use std::os::unix::process::ExitStatusExt;

        if std::env::var_os("SSHR_SIGNAL_FORWARD_TEST").is_none() {
            let temp = std::env::temp_dir().join(format!("sshr-fwd-test-{}", std::process::id()));
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "signal::tests::close_signal_is_forwarded_unchanged_to_the_ssh_child",
                    "--nocapture",
                ])
                .env("SSHR_SIGNAL_FORWARD_TEST", "1")
                .env("XDG_DATA_HOME", &temp)
                .status()
                .unwrap();
            let _ = std::fs::remove_dir_all(&temp);
            assert!(status.success());
            return;
        }

        let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
        // An inert exec target: the real one is this test binary, and re-execing
        // it with `host kill session` would run a subset of the suite.
        install_handlers_with_exe(Path::new("/usr/bin/true"), "host", "session");
        set_ssh_child(sleeper.id() as i32);

        unsafe {
            libc::raise(libc::SIGHUP);
        }

        let status = sleeper.wait().unwrap();
        assert_eq!(
            status.signal(),
            Some(libc::SIGHUP),
            "ssh child should receive the signal sshr received"
        );
        set_ssh_child(0);
    }

    // The reason this exists: on a window close sshr is killed along with the
    // window and never reaches its own cleanup code, so the kill has to be
    // handed to a process outside the doomed process group while sshr is still
    // alive — which is only true inside the handler.
    #[test]
    fn close_signal_spawns_a_detached_killer_once() {
        if std::env::var_os("SSHR_SIGNAL_SPAWN_TEST").is_none() {
            let temp = std::env::temp_dir().join(format!("sshr-spawn-test-{}", std::process::id()));
            std::fs::create_dir_all(&temp).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "signal::tests::close_signal_spawns_a_detached_killer_once",
                    "--nocapture",
                ])
                .env("SSHR_SIGNAL_SPAWN_TEST", &temp)
                .env("XDG_DATA_HOME", &temp)
                .status()
                .unwrap();
            let _ = std::fs::remove_dir_all(&temp);
            assert!(status.success());
            return;
        }

        let temp = PathBuf::from(std::env::var_os("SSHR_SIGNAL_SPAWN_TEST").unwrap());
        let marker = temp.join("invocations");
        let fake_sshr = temp.join("fake-sshr");
        std::fs::write(
            &fake_sshr,
            format!("#!/bin/sh\necho \"$@\" >> {}\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake_sshr, std::fs::Permissions::from_mode(0o755)).unwrap();

        install_handlers_with_exe(&fake_sshr, "myhost", "mysession");

        // Two signals: kitty sends one itself and the kernel sends another when
        // the pty closes. That must not produce two killers.
        unsafe {
            libc::raise(libc::SIGHUP);
            libc::raise(libc::SIGHUP);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if marker.exists() {
                std::thread::sleep(std::time::Duration::from_millis(200));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let content = std::fs::read_to_string(&marker).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines, vec!["myhost kill mysession"], "got: {content:?}");
    }

    // Verifies the missing link that used to defer cleanup: on a close signal
    // the handler terminates the registered interactive ssh child, so its wait()
    // returns and the caller can kill the remote session immediately. Runs in an
    // isolated subprocess so raising SIGHUP and writing the WAL cannot affect
    // other tests.
    #[test]
    fn close_signal_terminates_the_registered_ssh_child() {
        if std::env::var_os("SSHR_SIGNAL_CHILD_TEST").is_none() {
            let temp = std::env::temp_dir().join(format!("sshr-sig-test-{}", std::process::id()));
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "signal::tests::close_signal_terminates_the_registered_ssh_child",
                    "--nocapture",
                ])
                .env("SSHR_SIGNAL_CHILD_TEST", "1")
                .env("XDG_DATA_HOME", &temp)
                .status()
                .unwrap();
            let _ = std::fs::remove_dir_all(&temp);
            assert!(status.success());
            return;
        }

        // A long sleeper stands in for the interactive ssh child.
        let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
        // An inert exec target: the real one is this test binary, and re-execing
        // it with `host kill session` would run a subset of the suite.
        install_handlers_with_exe(Path::new("/usr/bin/true"), "host", "session");
        set_ssh_child(sleeper.id() as i32);
        assert!(!is_closing());

        unsafe {
            libc::raise(libc::SIGHUP);
        }

        // The handler must have SIGTERM'd the sleeper and flagged closing.
        let status = sleeper.wait().unwrap();
        assert!(
            !status.success(),
            "close handler should terminate the registered ssh child"
        );
        assert!(is_closing());
        set_ssh_child(0);
    }
}
