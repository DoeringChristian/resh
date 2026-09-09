use crate::posix::{quote, quote_remote_path};
use crate::upload::RemotePaths;

// shpool execs its `-c` command directly via shell-word splitting, without a
// shell, so `$HOME` in the value is never expanded. Run the launcher through an
// explicit `/bin/sh -c` so the shell expands `$HOME/<relative launcher>`.
const LAUNCH_COMMAND_SCRIPT: &str = r#"launcher=$1; shift; exec "$HOME/$launcher" "$@""#;

pub fn build_shpool_cmd(
    paths: &RemotePaths,
    session: &str,
    shell: Option<&str>,
    remote_cwd: Option<&str>,
) -> String {
    let launch_command = build_launch_command(paths, shell);
    let mut cmd = format!(
        "{} --socket {} attach -f -c {}",
        paths.shpool(),
        paths.socket(),
        quote(&launch_command),
    );
    if let Some(cwd) = remote_cwd {
        cmd.push_str(&format!(" -d {}", quote_remote_path(cwd)));
    }
    cmd.push_str(&format!(" -- {}", quote(session)));
    cmd
}

fn build_launch_command(paths: &RemotePaths, shell: Option<&str>) -> String {
    let mut command = format!(
        "/bin/sh -c {} sshr-launch {}",
        quote(LAUNCH_COMMAND_SCRIPT),
        quote(&paths.launch_home_relative()),
    );
    if let Some(shell) = shell {
        command.push(' ');
        command.push_str(&quote(shell));
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn test_default_shell() {
        let paths = RemotePaths::new(None).unwrap();
        let cmd = build_shpool_cmd(&paths, "s0", None, None);
        assert!(cmd.contains("attach -f -c "));
        assert!(cmd.contains("/bin/sh -c "));
        assert!(cmd.contains("sshr-launch"));
        assert!(cmd.contains(".local/share/sshr/init/launch.sh"));
        assert!(cmd.trim_end().ends_with("-- 's0'"));
        assert!(!cmd.contains(" -d "));
    }

    #[test]
    fn test_shell_override_with_cwd() {
        let paths = RemotePaths::new(Some("custom/sshr")).unwrap();
        let cmd = build_shpool_cmd(&paths, "s0", Some("/bin/zsh"), Some("~/projects"));
        assert!(cmd.contains("custom/sshr/init/launch.sh"));
        assert!(cmd.contains("'/bin/zsh'"));
        assert!(cmd.contains(r#"-d "$HOME"/'projects'"#));
        assert!(cmd.trim_end().ends_with("-- 's0'"));
    }

    /// The core regression: shpool execs the `-c` value directly (no shell), so
    /// the launcher path must be resolved by the `/bin/sh -c` wrapper, not left
    /// as a literal `$HOME`. Emulate shpool's shell-word split and exec.
    #[test]
    fn launch_command_resolves_home_and_execs_the_launcher() {
        let home = std::env::temp_dir().join(format!(
            "sshr-cmd-test-{}-{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let init = home.join(".local/share/sshr/init");
        fs::create_dir_all(&init).unwrap();
        let launcher = init.join("launch.sh");
        fs::write(
            &launcher,
            "#!/bin/sh\nprintf '%s\\n' \"launched:$1\" > \"$HOME/marker\"\n",
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();

        let paths = RemotePaths::new(None).unwrap();
        let launch_command = build_launch_command(&paths, Some("theshell"));

        // `set -- <launch_command>` performs the same word split shpool applies,
        // then `exec "$@"` runs the resulting argv.
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("set -- {launch_command}; exec \"$@\"")])
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(home.join("marker")).unwrap(),
            "launched:theshell\n"
        );
        fs::remove_dir_all(home).unwrap();
    }
}
