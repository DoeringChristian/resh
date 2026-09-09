use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::PathBuf;

use crate::config::{EnvDirective, HostConfig};
use crate::ssh::SshContext;
use crate::vlog;

const DEFAULT_REMOTE_DIR: &str = ".local/share/sshr";
const REMOTE_SOCKET_DIR: &str = ".local/run/sshr";

/// Paths used by sshr on the remote host. Installed files can be relocated
/// with `remote_dir`; runtime state deliberately remains under ~/.local/run.
#[derive(Debug, Clone)]
pub struct RemotePaths {
    remote_dir: String,
}

impl RemotePaths {
    pub fn new(remote_dir: Option<&str>) -> Result<Self> {
        let configured = remote_dir.unwrap_or(DEFAULT_REMOTE_DIR);
        let relative = configured
            .strip_prefix("~/")
            .unwrap_or(configured)
            .trim_start_matches('/')
            .trim_end_matches('/');

        anyhow::ensure!(!relative.is_empty(), "remote_dir must not be empty");
        anyhow::ensure!(
            !relative.split('/').any(|part| part == ".."),
            "remote_dir must stay within the remote home directory"
        );
        anyhow::ensure!(
            !relative.contains(['\n', '\r', '\0']),
            "remote_dir contains invalid characters"
        );

        Ok(Self {
            remote_dir: relative.to_string(),
        })
    }

    fn home_path(&self, suffix: &str) -> String {
        let relative = if suffix.is_empty() {
            self.remote_dir.clone()
        } else {
            format!("{}/{suffix}", self.remote_dir)
        };
        format!(r#""$HOME/{}""#, escape_double_quoted(&relative))
    }

    fn scp_path(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.remote_dir)
    }

    pub fn shpool_dir(&self) -> String {
        self.home_path("bin")
    }

    pub fn shpool(&self) -> String {
        self.home_path("bin/shpool")
    }

    pub fn launch(&self) -> String {
        self.home_path("init/launch.sh")
    }

    /// The launcher path relative to the remote HOME (no `$HOME` prefix). Used
    /// where the value is expanded by a shell we invoke ourselves rather than by
    /// shpool, which execs its `-c` command directly without a shell.
    pub fn launch_home_relative(&self) -> String {
        format!("{}/init/launch.sh", self.remote_dir)
    }

    pub fn init_dir(&self) -> String {
        self.home_path("init")
    }

    pub fn socket_dir(&self) -> String {
        format!(r#""$HOME/{REMOTE_SOCKET_DIR}""#)
    }

    pub fn socket(&self) -> String {
        format!(r#""$HOME/{REMOTE_SOCKET_DIR}/shpool.socket""#)
    }
}

fn escape_double_quoted(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

#[derive(Debug)]
struct RemotePlatform {
    os: String,
    arch: String,
}

impl RemotePlatform {
    fn binary_name(&self) -> String {
        format!("shpool-{}-{}", self.os, self.arch)
    }
}

/// Check if sshr's own shpool already exists on the remote.
pub fn has_sshr_shpool(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    paths: &RemotePaths,
) -> Result<bool> {
    let output = ssh.run_capture(
        host,
        extra_args,
        &format!("test -x {} && echo yes || echo no", paths.shpool()),
    )?;
    Ok(output.trim() == "yes")
}

/// Upload shpool binary to the remote. Returns true if successful.
fn upload_shpool(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    paths: &RemotePaths,
) -> Result<bool> {
    let platform = detect_remote_platform(ssh, host, extra_args)?;
    let binary_name = platform.binary_name();
    vlog!("remote platform: {}-{}", platform.os, platform.arch);

    let shpool_dir = match find_shpool_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!(
                "{}: no local shpool binaries found ({})",
                "warning".yellow().bold(),
                e
            );
            return Ok(false);
        }
    };

    let local_binary = shpool_dir.join(&binary_name);
    if !local_binary.exists() {
        eprintln!(
            "{}: no shpool binary for {} (expected {})",
            "warning".yellow().bold(),
            binary_name.dimmed(),
            local_binary.display().to_string().dimmed(),
        );
        return Ok(false);
    }

    vlog!("upload: local binary = {}", local_binary.display());
    vlog!("upload: remote path = {}", paths.shpool());
    eprintln!("Uploading shpool to {}...", host.cyan().bold());

    ssh.run_capture(
        host,
        extra_args,
        &format!("mkdir -p {}", paths.shpool_dir()),
    )?;

    ssh.scp_upload(
        host,
        extra_args,
        &local_binary,
        &paths.scp_path("bin/shpool"),
    )?;

    ssh.run_capture(host, extra_args, &format!("chmod +x {}", paths.shpool()))?;

    eprintln!("{}", "Done.".dimmed());
    Ok(true)
}

/// Ensure sshr's own shpool is on the remote. Upload if missing.
pub fn ensure_shpool(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    force: bool,
    host_cfg: &HostConfig,
    paths: &RemotePaths,
) -> Result<()> {
    if !force && has_sshr_shpool(ssh, host, extra_args, paths)? {
        vlog!("shpool: present at {}", paths.shpool());
    } else {
        if force {
            vlog!("shpool: forcing upload (--force-upload)");
        } else {
            vlog!("shpool: missing, uploading");
        }
        if !upload_shpool(ssh, host, extra_args, paths)? {
            anyhow::bail!("failed to install shpool on remote");
        }
    }

    ssh.run_capture(
        host,
        extra_args,
        &format!("mkdir -p {}", paths.socket_dir()),
    )?;

    ensure_init_files(ssh, host, extra_args, host_cfg, paths)?;
    Ok(())
}

fn build_env_exports(env: &[EnvDirective]) -> String {
    let mut lines = Vec::new();
    for directive in env {
        let EnvDirective::Set(name, value) = directive;
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        lines.push(format!("export {name}=\"{escaped}\""));
    }
    lines.join("\n")
}

fn ensure_init_files(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    host_cfg: &HostConfig,
    paths: &RemotePaths,
) -> Result<()> {
    let script = build_init_script(host_cfg, paths);
    ssh.run_capture(host, extra_args, &script)?;
    vlog!("init: created remote launcher at {}", paths.launch());
    Ok(())
}

fn build_init_script(host_cfg: &HostConfig, paths: &RemotePaths) -> String {
    let env_block = build_env_exports(&host_cfg.env);
    let env_section = if env_block.is_empty() {
        String::new()
    } else {
        format!("{env_block}\n")
    };
    let integration_enabled = host_cfg.shell_integration.unwrap_or(true);

    let launch_integration = if integration_enabled {
        r#"shell_name=$(basename "$login_shell")
case "$shell_name" in
    bash) exec env ENV="$init_dir/bash_init.sh" "$login_shell" --posix ;;
    zsh)  exec env ZDOTDIR="$init_dir/zsh" "$login_shell" ;;
    fish) exec env XDG_DATA_DIRS="$init_dir:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}" "$login_shell" ;;
    *)    exec "$login_shell" ;;
esac"#
    } else {
        r#"exec "$login_shell""#
    };

    let integration_files = if integration_enabled {
        format!(
            r#"cat > {} << 'SSHR_EOF'
set +o posix
unset ENV
[ -f ~/.bashrc ] && . ~/.bashrc
__sshr_uri_path() {{ printf %s "$1" | LC_ALL=C od -An -v -t x1 | awk '{{ for (i = 1; i <= NF; i++) {{ b = toupper($i); if (b == "2F") printf "/"; else printf "%%%s", b }} }}'; }}
__sshr_osc7() {{ printf '\033]7;file://%s' "$(hostname)"; __sshr_uri_path "$PWD"; printf '\a'; }}
PROMPT_COMMAND="${{PROMPT_COMMAND:+$PROMPT_COMMAND; }}__sshr_osc7"
SSHR_EOF
cat > {} << 'SSHR_EOF'
ZDOTDIR="$HOME"
[ -f "$ZDOTDIR/.zshenv" ] && . "$ZDOTDIR/.zshenv"
__sshr_uri_path() {{ printf %s "$1" | LC_ALL=C od -An -v -t x1 | awk '{{ for (i = 1; i <= NF; i++) {{ b = toupper($i); if (b == "2F") printf "/"; else printf "%%%s", b }} }}'; }}
__sshr_osc7() {{ printf '\033]7;file://%s' "$(hostname)"; __sshr_uri_path "$PWD"; printf '\a' }}
precmd_functions+=(__sshr_osc7)
SSHR_EOF
cat > {} << 'SSHR_EOF'
function __sshr_uri_path
    printf %s "$argv[1]" | env LC_ALL=C od -An -v -t x1 | awk '{{ for (i = 1; i <= NF; i++) {{ b = toupper($i); if (b == "2F") printf "/"; else printf "%%%s", b }} }}'
end
function __sshr_osc7 --on-event fish_prompt
    printf '\e]7;file://%s' (hostname)
    __sshr_uri_path "$PWD"
    printf '\a'
end
SSHR_EOF
"#,
            paths.home_path("init/bash_init.sh"),
            paths.home_path("init/zsh/.zshenv"),
            paths.home_path("init/fish/vendor_conf.d/sshr.fish"),
        )
    } else {
        String::new()
    };

    format!(
        r#"mkdir -p {} {} {}
cat > {} << 'SSHR_EOF'
#!/bin/sh
# Re-exec through a login shell to inherit the full environment
# (PATH from nix, homebrew, mise, etc.) — same as a normal SSH session.
if [ -z "$_SSHR_LOGIN" ]; then
    export _SSHR_LOGIN=1
    exec /bin/sh -l "$0" "$@"
fi
export SSH_CONNECTION="${{SSH_CONNECTION:-sshr}}"
{env_section}login_shell="${{1:-$SHELL}}"
if [ "${{login_shell#/}}" = "$login_shell" ]; then
    login_shell=$(command -v "$login_shell" 2>/dev/null || echo "$login_shell")
fi
if ! command -v "$login_shell" >/dev/null 2>&1; then
    echo "sshr: shell '$login_shell' not found, falling back to $SHELL" >&2
    login_shell="$SHELL"
fi
init_dir={}
{launch_integration}
SSHR_EOF
chmod +x {}
{integration_files}"#,
        paths.init_dir(),
        paths.home_path("init/zsh"),
        paths.home_path("init/fish/vendor_conf.d"),
        paths.launch(),
        paths.init_dir(),
        paths.launch(),
    )
}

fn detect_remote_platform(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
) -> Result<RemotePlatform> {
    let output = ssh
        .run_capture(host, extra_args, "uname -sm")
        .context("failed to detect remote platform")?;
    let parts: Vec<&str> = output.split_whitespace().collect();
    let os = parts.first().unwrap_or(&"unknown").to_lowercase();
    let mut arch = parts.get(1).unwrap_or(&"unknown").to_string();

    match arch.as_str() {
        "amd64" => arch = "x86_64".into(),
        "arm64" => arch = "aarch64".into(),
        _ => {}
    }

    Ok(RemotePlatform { os, arch })
}

fn find_shpool_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("SSHR_SHPOOL_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Ok(path);
        }
    }

    let exe = std::env::current_exe()?.canonicalize()?;

    let mut dir = exe.parent();
    while let Some(d) = dir {
        let repo_path = d.join("shpool/bin");
        if repo_path.is_dir() {
            return Ok(repo_path);
        }
        let nix_path = d.join("share/sshr/shpool/bin");
        if nix_path.is_dir() {
            return Ok(nix_path);
        }
        dir = d.parent();
    }

    anyhow::bail!("no shpool binary directory found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_default_to_sshr_data_dir() {
        let paths = RemotePaths::new(None).unwrap();
        assert_eq!(paths.shpool(), r#""$HOME/.local/share/sshr/bin/shpool""#);
        assert_eq!(paths.socket(), r#""$HOME/.local/run/sshr/shpool.socket""#);
    }

    #[test]
    fn remote_paths_support_custom_home_relative_dir() {
        let paths = RemotePaths::new(Some("~/opt/sshr")).unwrap();
        assert_eq!(paths.launch(), r#""$HOME/opt/sshr/init/launch.sh""#);
        assert!(RemotePaths::new(Some("../outside-home")).is_err());
    }

    #[test]
    fn shell_integration_is_enabled_by_default() {
        let paths = RemotePaths::new(None).unwrap();
        let script = build_init_script(&HostConfig::default(), &paths);
        assert!(script.contains("__sshr_osc7"));
        assert!(script.contains("PROMPT_COMMAND="));
        assert!(script.contains("XDG_DATA_DIRS="));
    }

    #[test]
    fn shell_integration_can_be_disabled() {
        let paths = RemotePaths::new(Some("custom/sshr")).unwrap();
        let config = HostConfig {
            shell_integration: Some(false),
            ..HostConfig::default()
        };
        let script = build_init_script(&config, &paths);

        assert!(!script.contains("__sshr_osc7"));
        assert!(!script.contains("PROMPT_COMMAND="));
        assert!(!script.contains("XDG_DATA_DIRS="));
        assert!(script.contains(r#"init_dir="$HOME/custom/sshr/init""#));
        assert!(script.contains(r#"exec "$login_shell""#));
    }
}
