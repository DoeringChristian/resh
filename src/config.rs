use indexmap::IndexMap;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum EnvDirective {
    Set(String, String),
}

#[derive(Debug, Clone)]
pub struct CopyDirective {
    pub src: String,
    pub dest: Option<String>,
    pub glob: bool,
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    pub shell: Option<String>,
    pub env: Vec<EnvDirective>,
    pub cwd: Option<String>,
    pub copy: Vec<CopyDirective>,
    pub delegate: Option<String>,
    pub remote_dir: Option<String>,
    pub shell_integration: Option<bool>,
}

// --- TOML schema ---

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TomlConfig {
    shell: Option<String>,
    cwd: Option<String>,
    delegate: Option<String>,
    remote_dir: Option<String>,
    shell_integration: Option<bool>,
    env: Option<IndexMap<String, String>>,
    copy: Option<Vec<TomlCopy>>,
    hosts: Option<IndexMap<String, TomlHostSection>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlHostSection {
    shell: Option<String>,
    cwd: Option<String>,
    delegate: Option<String>,
    remote_dir: Option<String>,
    shell_integration: Option<bool>,
    env: Option<IndexMap<String, String>>,
    copy: Option<Vec<TomlCopy>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum TomlCopy {
    Simple(String),
    Detailed(TomlCopyDetailed),
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct TomlCopyDetailed {
    src: String,
    dest: Option<String>,
    glob: Option<bool>,
    exclude: Option<Vec<String>>,
}

// --- Public API ---

pub struct Config {
    defaults: HostConfig,
    hosts: Vec<(String, HostConfig)>,
}

impl Config {
    pub fn load() -> Config {
        let path = config_path();
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                return Config {
                    defaults: HostConfig::default(),
                    hosts: Vec::new(),
                }
            }
        };

        match toml::from_str::<TomlConfig>(&content) {
            Ok(toml) => convert_toml(toml),
            Err(e) => {
                eprintln!("warning: failed to parse {}: {e}", path.display());
                Config {
                    defaults: HostConfig::default(),
                    hosts: Vec::new(),
                }
            }
        }
    }

    pub fn for_host(&self, hostname: &str) -> HostConfig {
        let mut result = self.defaults.clone();
        for (pattern, host_cfg) in &self.hosts {
            if host_matches(pattern, hostname) {
                merge_config(&mut result, host_cfg);
            }
        }
        result
    }
}

// --- Conversion ---

fn convert_toml(toml: TomlConfig) -> Config {
    let defaults = HostConfig {
        shell: toml.shell,
        env: convert_env(&toml.env),
        cwd: toml.cwd,
        copy: convert_copy(&toml.copy),
        delegate: toml.delegate,
        remote_dir: toml.remote_dir,
        shell_integration: toml.shell_integration,
    };

    let hosts: Vec<(String, HostConfig)> = toml
        .hosts
        .unwrap_or_default()
        .into_iter()
        .map(|(pattern, section)| {
            let cfg = HostConfig {
                shell: section.shell,
                env: convert_env(&section.env),
                cwd: section.cwd,
                copy: convert_copy(&section.copy),
                delegate: section.delegate,
                remote_dir: section.remote_dir,
                shell_integration: section.shell_integration,
            };
            (pattern, cfg)
        })
        .collect();

    Config { defaults, hosts }
}

fn convert_env(env: &Option<IndexMap<String, String>>) -> Vec<EnvDirective> {
    let Some(map) = env else { return Vec::new() };
    map.iter()
        .map(|(name, value)| {
            let resolved = if value == "_kitty_copy_env_var_" {
                std::env::var(name).unwrap_or_default()
            } else {
                value.clone()
            };
            EnvDirective::Set(name.clone(), resolved)
        })
        .collect()
}

fn convert_copy(copy: &Option<Vec<TomlCopy>>) -> Vec<CopyDirective> {
    let Some(list) = copy else { return Vec::new() };
    list.iter()
        .map(|c| match c {
            TomlCopy::Simple(s) => CopyDirective {
                src: s.clone(),
                dest: None,
                glob: false,
                excludes: Vec::new(),
            },
            TomlCopy::Detailed(d) => CopyDirective {
                src: d.src.clone(),
                dest: d.dest.clone(),
                glob: d.glob.unwrap_or(false),
                excludes: d.exclude.clone().unwrap_or_default(),
            },
        })
        .collect()
}

// --- Host matching ---

fn host_matches(pattern: &str, hostname: &str) -> bool {
    let (pat_user, pat_host) = match pattern.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, pattern),
    };

    let (host_user, host_host) = match hostname.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, hostname),
    };

    if let Some(pu) = pat_user {
        match host_user {
            Some(hu) if !glob_match(pu, hu) => return false,
            None => return false,
            _ => {}
        }
    }

    glob_match(pat_host, host_host)
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let pb = pattern.as_bytes();
    let tb = text.as_bytes();
    let mut star_pi = usize::MAX;
    let mut star_ti = 0;

    while ti < tb.len() {
        if pi < pb.len() && (pb[pi] == b'?' || pb[pi] == tb[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pb.len() && pb[pi] == b'*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pb.len() && pb[pi] == b'*' {
        pi += 1;
    }

    pi == pb.len()
}

fn merge_config(base: &mut HostConfig, overlay: &HostConfig) {
    if overlay.shell.is_some() {
        base.shell = overlay.shell.clone();
    }
    // Later matching sections override an earlier value for the same key in
    // place, and add new keys in declaration order.
    for directive in &overlay.env {
        let EnvDirective::Set(name, _) = directive;
        if let Some(existing) = base.env.iter_mut().find(|existing| {
            let EnvDirective::Set(existing_name, _) = existing;
            existing_name == name
        }) {
            *existing = directive.clone();
        } else {
            base.env.push(directive.clone());
        }
    }
    if overlay.cwd.is_some() {
        base.cwd = overlay.cwd.clone();
    }
    base.copy.extend(overlay.copy.iter().cloned());
    if overlay.delegate.is_some() {
        base.delegate = overlay.delegate.clone();
    }
    if overlay.remote_dir.is_some() {
        base.remote_dir = overlay.remote_dir.clone();
    }
    if overlay.shell_integration.is_some() {
        base.shell_integration = overlay.shell_integration;
    }
}

fn config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join("sshr/config.toml")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/sshr/config.toml")
    } else {
        PathBuf::from("~/.config/sshr/config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        let toml: TomlConfig = toml::from_str(s).unwrap();
        convert_toml(toml)
    }

    #[test]
    fn test_global_shell() {
        let cfg = parse(r#"shell = "fish""#);
        assert_eq!(cfg.defaults.shell.as_deref(), Some("fish"));
    }

    #[test]
    fn test_env_set() {
        let cfg = parse(
            r#"
[env]
FOO = "bar"
BAZ = ""
"#,
        );
        assert_eq!(cfg.defaults.env.len(), 2);
        // IndexMap preserves declaration order: FOO was written before BAZ.
        let EnvDirective::Set(k, v) = &cfg.defaults.env[0];
        assert_eq!(k, "FOO");
        assert_eq!(v, "bar");
        let EnvDirective::Set(k, v) = &cfg.defaults.env[1];
        assert_eq!(k, "BAZ");
        assert_eq!(v, "");
    }

    #[test]
    fn later_host_section_env_overrides_in_place_and_appends_new_keys() {
        let cfg = parse(
            r#"
[env]
SHARED = "default"

[hosts."server-*"]
[hosts."server-*".env]
SHARED = "override"
EXTRA = "yes"
"#,
        );
        let host = cfg.for_host("server-1");
        let env: Vec<(&str, &str)> = host
            .env
            .iter()
            .map(|d| {
                let EnvDirective::Set(k, v) = d;
                (k.as_str(), v.as_str())
            })
            .collect();
        assert_eq!(env, [("SHARED", "override"), ("EXTRA", "yes")]);
    }

    #[test]
    fn test_hostname_section() {
        let cfg = parse(
            r#"
shell = "bash"

[hosts."myserver-*"]
shell = "/bin/zsh"

[hosts."myserver-*".env]
EDITOR = "vim"
"#,
        );
        assert_eq!(cfg.defaults.shell.as_deref(), Some("bash"));
        assert_eq!(cfg.hosts.len(), 1);
        assert_eq!(cfg.hosts[0].1.shell.as_deref(), Some("/bin/zsh"));
    }

    #[test]
    fn test_for_host_matching() {
        let cfg = parse(
            r#"
shell = "bash"

[hosts."myserver-*"]
shell = "/bin/zsh"
"#,
        );
        let host_cfg = cfg.for_host("myserver-prod");
        assert_eq!(host_cfg.shell.as_deref(), Some("/bin/zsh"));

        let other_cfg = cfg.for_host("otherhost");
        assert_eq!(other_cfg.shell.as_deref(), Some("bash"));
    }

    #[test]
    fn test_for_host_user_at_host() {
        let cfg = parse(
            r#"
[hosts."admin@server"]
shell = "/bin/zsh"
"#,
        );
        assert!(cfg.for_host("admin@server").shell.is_some());
        assert!(cfg.for_host("other@server").shell.is_none());
        assert!(cfg.for_host("server").shell.is_none());
    }

    #[test]
    fn test_delegate() {
        let cfg = parse(
            r#"
[hosts."legacy-*"]
delegate = "ssh"
"#,
        );
        let host_cfg = cfg.for_host("legacy-box");
        assert_eq!(host_cfg.delegate.as_deref(), Some("ssh"));
    }

    #[test]
    fn test_copy_simple() {
        let cfg = parse(
            r#"
copy = [".vimrc", ".zshrc"]
"#,
        );
        assert_eq!(cfg.defaults.copy.len(), 2);
        assert_eq!(cfg.defaults.copy[0].src, ".vimrc");
        assert_eq!(cfg.defaults.copy[1].src, ".zshrc");
    }

    #[test]
    fn test_copy_detailed() {
        let cfg = parse(
            r#"
[[copy]]
src = ".vimrc"
dest = "my-conf/vim/vimrc"

[[copy]]
src = "images/*"
glob = true
exclude = ["*.jpg", "*.bmp"]
"#,
        );
        assert_eq!(cfg.defaults.copy.len(), 2);
        assert_eq!(
            cfg.defaults.copy[0].dest.as_deref(),
            Some("my-conf/vim/vimrc")
        );
        assert!(cfg.defaults.copy[1].glob);
        assert_eq!(cfg.defaults.copy[1].excludes, vec!["*.jpg", "*.bmp"]);
    }

    #[test]
    fn test_merge_env_accumulates() {
        let cfg = parse(
            r#"
[env]
A = "1"

[hosts.server]
[hosts.server.env]
B = "2"
"#,
        );
        let host_cfg = cfg.for_host("server");
        assert_eq!(host_cfg.env.len(), 2);
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("server-*", "server-prod"));
        assert!(!glob_match("server-*", "other"));
        assert!(glob_match("s?rver", "server"));
        assert!(!glob_match("s?rver", "sserver"));
    }

    #[test]
    fn test_shell_integration() {
        let cfg = parse(
            r#"
[hosts.server]
shell_integration = false
"#,
        );
        let host_cfg = cfg.for_host("server");
        assert_eq!(host_cfg.shell_integration, Some(false));
    }

    #[test]
    fn test_remote_dir() {
        let cfg = parse(
            r#"
remote_dir = ".local/share/custom-sshr"

[hosts.server]
remote_dir = "opt/sshr"
"#,
        );
        assert_eq!(
            cfg.for_host("other").remote_dir.as_deref(),
            Some(".local/share/custom-sshr")
        );
        assert_eq!(
            cfg.for_host("server").remote_dir.as_deref(),
            Some("opt/sshr")
        );
    }

    #[test]
    fn test_empty_config() {
        let cfg = parse("");
        assert!(cfg.defaults.shell.is_none());
        assert!(cfg.hosts.is_empty());
    }
}
