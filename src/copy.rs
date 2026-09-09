use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::CopyDirective;
use crate::ssh::SshContext;
use crate::vlog;

pub fn run_copy_directives(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    copies: &[CopyDirective],
) -> Result<()> {
    let home = home_dir();

    for copy in copies {
        let sources = resolve_sources(&home, copy)?;

        for local_path in &sources {
            if !local_path.exists() {
                vlog!("copy: skipping {} (not found)", local_path.display());
                continue;
            }

            if copy.excludes.iter().any(|ex| {
                let name = local_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                glob_match_simple(ex, &name)
            }) {
                vlog!("copy: excluding {}", local_path.display());
                continue;
            }

            let remote_path = match &copy.dest {
                Some(dest) => dest.clone(),
                None => {
                    let rel = local_path
                        .strip_prefix(&home)
                        .unwrap_or(local_path.as_path());
                    rel.to_string_lossy().to_string()
                }
            };

            if local_path.is_dir() {
                copy_dir_recursive(ssh, host, extra_args, local_path, &remote_path)?;
            } else {
                let parent = std::path::Path::new(&remote_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string());
                if let Some(dir) = parent {
                    if !dir.is_empty() {
                        ssh.run_capture(host, extra_args, &format!("mkdir -p ~/{dir}"))?;
                    }
                }
                vlog!("copy: {} -> ~/{}", local_path.display(), remote_path);
                ssh.scp_upload(host, extra_args, local_path, &format!("~/{remote_path}"))?;
            }
        }
    }

    Ok(())
}

fn resolve_sources(home: &Path, copy: &CopyDirective) -> Result<Vec<PathBuf>> {
    let parts: Vec<&str> = copy.src.split_whitespace().collect();

    if copy.glob {
        let mut results = Vec::new();
        for pattern in &parts {
            let full_pattern = if pattern.starts_with('/') {
                pattern.to_string()
            } else {
                format!("{}/{pattern}", home.display())
            };
            for path in glob::glob(&full_pattern)
                .with_context(|| format!("invalid glob pattern: {full_pattern}"))?
                .flatten()
            {
                results.push(path);
            }
        }
        Ok(results)
    } else {
        Ok(parts
            .iter()
            .map(|p| {
                if p.starts_with('/') {
                    PathBuf::from(p)
                } else {
                    home.join(p)
                }
            })
            .collect())
    }
}

fn copy_dir_recursive(
    ssh: &SshContext,
    host: &str,
    extra_args: &[String],
    local_dir: &std::path::Path,
    remote_dir: &str,
) -> Result<()> {
    ssh.run_capture(host, extra_args, &format!("mkdir -p ~/{remote_dir}"))?;

    for entry in std::fs::read_dir(local_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let remote_path = format!("{remote_dir}/{name}");

        if path.is_dir() {
            copy_dir_recursive(ssh, host, extra_args, &path, &remote_path)?;
        } else {
            vlog!("copy: {} -> ~/{}", path.display(), remote_path);
            ssh.scp_upload(host, extra_args, &path, &format!("~/{remote_path}"))?;
        }
    }

    Ok(())
}

fn glob_match_simple(pattern: &str, text: &str) -> bool {
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

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}
