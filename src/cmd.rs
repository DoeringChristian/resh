use crate::upload::RemotePaths;

pub fn build_shpool_cmd(
    paths: &RemotePaths,
    session: &str,
    shell: Option<&str>,
    remote_cwd: Option<&str>,
) -> String {
    let mut cmd = format!(
        "{} --socket {} attach -f {session}",
        paths.shpool(),
        paths.socket()
    );
    let launch_cmd = match shell {
        Some(shell) => format!("{} {}", paths.launch(), shell_escape(shell)),
        None => paths.launch(),
    };
    cmd.push_str(&format!(" -c {}", shell_escape(&launch_cmd)));
    if let Some(cwd) = remote_cwd {
        cmd.push_str(&format!(" -d {}", shell_escape(cwd)));
    }
    cmd
}

fn shell_escape(s: &str) -> String {
    if s.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '\\') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_shell() {
        let paths = RemotePaths::new(None).unwrap();
        let cmd = build_shpool_cmd(&paths, "s0", None, None);
        assert!(cmd.contains("attach -f s0"));
        assert!(cmd.contains(r#"-c '"$HOME/.local/share/sshr/init/launch.sh"'"#));
        assert!(!cmd.contains("-d "));
    }

    #[test]
    fn test_shell_override_with_cwd() {
        let paths = RemotePaths::new(Some("custom/sshr")).unwrap();
        let cmd = build_shpool_cmd(&paths, "s0", Some("/bin/zsh"), Some("~/projects"));
        assert!(cmd.contains("attach -f s0"));
        assert!(cmd.contains(r#""$HOME/custom/sshr/init/launch.sh" /bin/zsh"#));
        assert!(cmd.contains("-d ~/projects"));
    }
}
