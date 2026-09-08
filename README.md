# sshr

Resilient SSH sessions with automatic reconnection and persistent shells.

sshr wraps SSH with:

- **Persistent sessions** via [shpool](https://github.com/shell-pool/shpool) — your shell survives connection drops
- **Automatic reconnection** — prompts to reconnect when the connection is lost
- **SSH multiplexing** — reuses a single TCP connection for fast new windows
- **Auto-upload** — ships a shpool binary to remotes that don't have it installed
- **Shell-agnostic** — works with any login shell (bash, zsh, fish, etc.) and injects OSC 7 CWD reporting
- **Kitty integration** — optional kittens for smart window launch/close

## Install

### Nix

```bash
nix profile install github:DoeringChristian/sshr
```

### Manual

Build from source and install the executable together with its runtime assets:

```bash
git clone https://github.com/DoeringChristian/sshr.git
cd sshr
cargo build --release

mkdir -p "$HOME/.local/bin" "$HOME/.local/share/sshr"
install -m 755 target/release/sshr "$HOME/.local/bin/sshr"
cp -R shpool kitty "$HOME/.local/share/sshr/"
```

Ensure `$HOME/.local/bin` is on your `PATH`. Installing the `shpool` directory under `$HOME/.local/share/sshr/` is required for automatic uploads to remote hosts.

## Usage

```bash
# Connect to a host (creates a new shpool session)
sshr myhost

# Attach to an existing session (interactive picker)
sshr myhost attach

# Start in a specific directory
sshr --remote-cwd ~/projects myhost

# Use a specific shell on the remote
sshr --shell /bin/zsh myhost

# List remote sessions
sshr myhost list        # or: sshr myhost ls

# Kill sessions (interactive picker, or by name)
sshr myhost kill
sshr myhost kill macbook-a3f21b macbook-c7e049

# Kill all detached sessions
sshr myhost clean

# Show sessions from all clients, not just this machine
sshr -a myhost list

# Force re-upload of shpool binary
sshr --force-upload myhost

# Verbose logging (SSH commands, paths)
sshr -v myhost
```

The general form is `sshr [flags] <host> [subcommand] [args...]`. Session names are randomly generated (e.g. `macbook-a3f21b`). When `kill` or `attach` is run without session names, an interactive picker is shown.

## Shell Support

By default, sshr deploys lightweight init files to the configured remote data directory (`~/.local/share/sshr/init/`) that add OSC 7 CWD reporting to your shell. This enables features like opening new windows in the same remote directory. Set `shell_integration = false` to disable these hooks. Supported shells:

- **bash** — init via `ENV` + POSIX mode
- **zsh** — init via `ZDOTDIR`
- **fish** — init via `XDG_DATA_DIRS`
- **other** — launched directly (no OSC 7 injection)

By default sshr uses the remote's login shell. Use `--shell` to override, or set a default in the config file.

## Connection Management

sshr uses SSH `ControlMaster=auto` with `ControlPersist=10m` to multiplex all sessions to a host through a single TCP connection. Multiple sshr windows share one master; when the last one exits, the master lingers for 10 minutes before shutting down.

**Reconnection**: when an SSH connection drops (exit code 255), sshr tears down the broken master, cleans up stale sockets, and immediately retries. If the retry also fails, it prompts you to press any key to try again. Non-SSH failures (e.g. a shpool crash) prompt without touching the master, since another session may be using it.

**Session cleanup**: when sshr exits (normally or via SIGHUP/SIGTERM), it records the session in a write-ahead log and kills it on the remote. If the connection is already down, pending kills are replayed on the next connect to that host.

## Configuration

sshr reads `~/.config/sshr/config.toml` (or `$XDG_CONFIG_HOME/sshr/config.toml`).

### Example

```toml
# Defaults for all hosts
shell = "fish"

[env]
PATH = "$PATH:$HOME/.nix-profile/bin"

# Per-host overrides
[hosts."myserver-*"]
shell = "/bin/zsh"
copy = [".vimrc"]

[hosts."myserver-*".env]
EDITOR = "vim"

[hosts."legacy-*"]
delegate = "ssh"
```

### Top-level options

**shell** — Login shell on the remote. Bare names (e.g. `"fish"`) are resolved via PATH on the remote. CLI `--shell` overrides this.

**cwd** — Working directory on the remote. CLI `--remote-cwd` overrides this.

**delegate** — Skip sshr for this host and run the specified command instead (e.g. `"ssh"` for plain SSH).

**remote_dir** — Directory under the remote home directory where sshr installs shpool and its shell-init files. Default: `".local/share/sshr"`. A leading `~/` or `/` is accepted and resolved relative to the remote home directory, matching Kitty's SSH kitten behavior.

**shell_integration** — Toggle OSC 7 CWD reporting injection (`true`/`false`). Default: `true`. Disabling it avoids sshr's shell-init hooks, but features that depend on knowing the remote working directory, such as Kitty smart launch, will not preserve that directory.

### `[env]`

Set environment variables on the remote:

```toml
[env]
PATH = "$PATH:$HOME/.nix-profile/bin"
EDITOR = "vim"
TERM_PROGRAM = "_kitty_copy_env_var_"  # copies value from local env
```

Values are exported in the remote shell, so shell variables like `$PATH` and `$HOME` are expanded on the remote side. The special value `_kitty_copy_env_var_` copies the variable's value from your local environment.

### `copy`

Copy files from local to remote via SCP. Paths are relative to HOME on both sides.

```toml
# Simple: list of files
copy = [".vimrc", ".zshrc"]

# Detailed: with destination, glob, or exclusions
[[copy]]
src = ".vimrc"
dest = "my-conf/vim/vimrc"

[[copy]]
src = "images/*"
glob = true
exclude = ["*.jpg", "*.bmp"]
```

### `[hosts."pattern"]`

Per-host sections with glob pattern matching. Supports `*`, `?`, and `user@host` form. Each section can override any top-level option:

```toml
[hosts."admin@prod-*"]
shell = "/bin/bash"
cwd = "~/deployments"

[hosts."admin@prod-*".env]
DEPLOY_ENV = "production"
```

## Kitty Integration

sshr works in any terminal, but ships optional kittens for kitty users. Copy `kitty/smart_launch.py` and `kitty/smart_close.py` to `~/.config/kitty/`, then add to `kitty.conf`:

```conf
map cmd+enter kitten smart_launch.py
map kitty_mod+enter kitten smart_launch.py
map cmd+x kitten smart_close.py
map kitty_mod+x kitten smart_close.py
```

**smart_launch** (`cmd+enter`) is context-aware: in an sshr window it opens a new sshr session to the same host in the same directory; in a local window it opens a local shell in the current directory.

**smart_close** (`cmd+x`) closes the window. For sshr sessions, the closing signal triggers sshr's cleanup handler, which records the session in a write-ahead log and kills it on the remote (or on next connect if the connection is already down).

## Pre-built shpool Binaries

sshr can auto-upload a shpool binary to remotes that don't have it installed. To build binaries for a platform, run `shpool/build.sh` on that platform:

```bash
# On each target machine:
bash shpool/build.sh
```

This builds a portable shpool binary and places it in `shpool/bin/`. On Linux, it produces a statically-linked musl binary.

You can also set `SSHR_SHPOOL_DIR` to point to a custom directory containing the binaries.

## License

MIT
