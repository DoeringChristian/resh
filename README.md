# resh

Resilient SSH sessions with automatic reconnection and persistent shells.

resh wraps SSH with:

- **Persistent sessions** via [shpool](https://github.com/shell-pool/shpool) — your shell survives connection drops
- **Automatic reconnection** — prompts to reconnect when the connection is lost
- **SSH multiplexing** — reuses a single TCP connection for fast new windows
- **Auto-upload** — ships a shpool binary to remotes that don't have it installed
- **Shell-agnostic** — works with any login shell (bash, zsh, fish, etc.) and injects OSC 7 CWD reporting
- **Kitty integration** — optional kittens for smart window launch/close

## Install

### Homebrew

The repo doubles as a tap, so the formula can be installed straight from it:

```bash
brew install https://raw.githubusercontent.com/DoeringChristian/resh/main/Formula/resh.rb
```

or, to track it as a tap and get `brew upgrade`:

```bash
brew tap doeringchristian/resh https://github.com/DoeringChristian/resh
brew install doeringchristian/resh/resh
```

Add `--HEAD` to either command to track `main` instead of the latest release. The formula installs the prebuilt shpool binaries and the kittens under `share/resh/`, where resh looks for them when uploading shpool to a remote.

### Nix

```bash
nix profile install github:DoeringChristian/resh
```

### Manual

Build from source and install the executable together with its runtime assets:

```bash
git clone https://github.com/DoeringChristian/resh.git
cd resh
cargo build --release

mkdir -p "$HOME/.local/bin" "$HOME/.local/share/resh"
install -m 755 target/release/resh "$HOME/.local/bin/resh"
cp -R shpool kitty "$HOME/.local/share/resh/"
```

Ensure `$HOME/.local/bin` is on your `PATH`. Installing the `shpool` directory under `$HOME/.local/share/resh/` is required for automatic uploads to remote hosts.

## Usage

```bash
# Connect to a host (creates a new shpool session)
resh myhost

# Attach to an existing session (interactive picker)
resh myhost attach

# Start in a specific directory
resh --remote-cwd ~/projects myhost

# Use a specific shell on the remote
resh --shell /bin/zsh myhost

# List remote sessions
resh myhost list        # or: resh myhost ls

# Kill sessions (interactive picker, or by name)
resh myhost kill
resh myhost kill macbook-a3f21b macbook-c7e049

# Kill all detached sessions
resh myhost clean

# Show sessions from all clients, not just this machine
resh -a myhost list

# Replace the remote shpool binary (kills sessions, restarts the daemon; asks first)
resh --force-upgrade myhost

# Verbose logging (SSH commands, paths)
resh -v myhost
```

The general form is `resh [flags] <host> [subcommand] [args...]`. Session names are randomly generated (e.g. `macbook-a3f21b`). When `kill` or `attach` is run without session names, an interactive picker is shown.

## Shell Support

By default, resh deploys lightweight init files to the configured remote data directory (`~/.local/share/resh/init/`) that add OSC 7 CWD reporting to your shell. This enables features like opening new windows in the same remote directory. Set `shell_integration = false` to disable these hooks. Supported shells:

- **bash** — init via `ENV` + POSIX mode
- **zsh** — init via `ZDOTDIR`
- **fish** — init via `XDG_DATA_DIRS`
- **other** — launched directly (no OSC 7 injection)

By default resh uses the remote's login shell. Use `--shell` to override, or set a default in the config file.

## Connection Management

resh uses SSH `ControlMaster=auto` with `ControlPersist=10m` to multiplex all sessions to a host through a single TCP connection. Multiple resh windows share one master; when the last one exits, the master lingers for 10 minutes before shutting down.

**Reconnection**: when an SSH connection drops (exit code 255), resh tears down the broken master, cleans up stale sockets, and immediately retries. If the retry also fails, it prompts you to press any key to try again. Non-SSH failures (e.g. a shpool crash) prompt without touching the master, since another session may be using it.

**Session cleanup**: on a clean exit resh kills its remote session directly. On SIGHUP/SIGTERM it cannot — closing a terminal window kills resh along with the window, usually before it reaches any cleanup code — so the signal handler writes a write-ahead log entry and hands the kill to `resh <host> kill <session>` in a session of its own (`fork` + `setsid`), which outlives the window and can build a fresh connection if the multiplexed one died with it. Whatever that process cannot finish stays in the WAL and is replayed on the next connect to that host.

Sessions are killed one request at a time. shpool's daemon walks a multi-session kill in order and aborts the whole batch on the first session it cannot signal, so batching lets one unkillable session spare every session behind it.

## Configuration

resh reads `~/.config/resh/config.toml` (or `$XDG_CONFIG_HOME/resh/config.toml`).

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

**delegate** — Skip resh for this host and run the specified command instead (e.g. `"ssh"` for plain SSH).

**remote_dir** — Directory under the remote home directory where resh installs shpool and its shell-init files. Default: `".local/share/resh"`. A leading `~/` or `/` is accepted and resolved relative to the remote home directory, matching Kitty's SSH kitten behavior.

**shell_integration** — Toggle OSC 7 CWD reporting injection (`true`/`false`). Default: `true`. Disabling it avoids resh's shell-init hooks, but features that depend on knowing the remote working directory, such as Kitty smart launch, will not preserve that directory.

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

resh works in any terminal, but ships an optional kitten for kitty users. Copy `kitty/smart_launch.py` to `~/.config/kitty/`, then add to `kitty.conf`:

```conf
map cmd+enter kitten smart_launch.py
map kitty_mod+enter kitten smart_launch.py
```

**smart_launch** (`cmd+enter`) is context-aware: in an resh window it opens a new resh session to the same host in the same directory; in a local window it opens a local shell in the current directory.

Closing a window needs no kitten — kitty's built-in `close_window` (or `close_window_with_confirmation`) is enough, and so is any other way the window goes away. Cleanup hangs off the signal the terminal sends, not off the key you pressed; see **Session cleanup** above.

## Pre-built shpool Binaries

resh can auto-upload a shpool binary to remotes that don't have it installed. To build binaries for a platform, run `shpool/build.sh` on that platform:

```bash
# On each target machine:
bash shpool/build.sh
```

This builds a portable shpool binary and places it in `shpool/bin/`. On Linux, it produces a statically-linked musl binary.

You can also set `RESH_SHPOOL_DIR` to point to a custom directory containing the binaries. resh otherwise looks for `shpool/bin/` (or `share/resh/shpool/bin/`) by walking up from its own executable, so a binary installed outside such a tree needs this variable.

**Upgrading an installed remote**: resh only uploads when the remote has no shpool at all, so replacing an existing one takes `resh --force-upgrade <host>`. It asks first, then kills every session on that host and stops the daemon, since the running daemon would otherwise hold the old binary open. The upload itself lands beside the target and is renamed over it, so anything still executing the old binary cannot block it.

## Debugging

Set `RESH_LOG_FILE` to mirror resh's verbose log to a file. Useful for the close path, which runs while the terminal is being torn down and has nowhere to print:

```bash
RESH_LOG_FILE=~/resh.log resh myhost
```

The detached process that performs the remote kill logs there too, under its own pid.

## License

MIT
