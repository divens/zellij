# Zellij Windows Porting Plan

> Comprehensive architectural audit and porting blueprint for making Zellij cross-platform.
> Generated from analysis of Zellij v0.44.0 codebase.

---

## Table of Contents

1. [Crate Structure Map](#1-crate-structure-map)
2. [OS-Dependent Code Audit](#2-os-dependent-code-audit)
3. [Abstraction Layer Design](#3-abstraction-layer-design)
4. [Dependency Migration Plan](#4-dependency-migration-plan)
5. [Test Coverage Analysis](#5-test-coverage-analysis)
6. [Ordered Work Plan](#6-ordered-work-plan)
7. [Risk Register](#7-risk-register)

---

## 1. Crate Structure Map

### 1.1 Workspace Members

| Crate | Path | Description |
|-------|------|-------------|
| `zellij` | `.` | Main binary — CLI entry point, session management |
| `zellij-server` | `zellij-server/` | Server process — PTY management, pane/tab/screen logic, plugins (WASM) |
| `zellij-client` | `zellij-client/` | Client process — terminal I/O, input handling, signal routing, web client |
| `zellij-utils` | `zellij-utils/` | Shared library — IPC, config parsing, data types, sessions, constants |
| `zellij-tile` | `zellij-tile/` | Plugin SDK — WASM plugin API |
| `zellij-tile-utils` | `zellij-tile-utils/` | Plugin utility library |
| `xtask` | `xtask/` | Build system — test runner, CI, proto generation |
| 13 default plugins | `default-plugins/*/` | Built-in WASM plugins (status-bar, tab-bar, strider, etc.) |

### 1.2 Internal Dependency Graph

```
zellij (binary)
├── zellij-client
│   └── zellij-utils
├── zellij-server
│   └── zellij-utils
└── zellij-utils

zellij-tile
└── zellij-utils

zellij-tile-utils (standalone, only depends on ansi_term)

default-plugins/*
├── zellij-tile
└── zellij-tile-utils (some plugins)
```

### 1.3 Platform-Specific External Dependencies

#### Unix-Only Dependencies (MUST be replaced or abstracted)

| Dependency | Version | Crates Using It | What It Does | Replacement |
|-----------|---------|----------------|--------------|-------------|
| `nix` | 0.23.1 | server, client, utils, binary | Safe POSIX syscall wrappers (PTY, signals, termios, umask, UID) | Per-function replacement (see §4) |
| `libc` | 0.2 | server, client | Raw `ioctl(TIOCSWINSZ/TIOCGWINSZ)`, `login_tty()` | `portable-pty` for PTY, `crossterm` for terminal size |
| `daemonize` | 0.5 | server, client | Fork+setsid to background the server process | Windows service or background process spawn |
| `signal-hook` | 0.3 | server, client | Register Unix signal handlers (SIGINT, SIGTERM, SIGWINCH, etc.) | `crossterm` events for resize; `ctrlc` for Ctrl+C; custom for others |
| `close_fds` | 0.3.2 | server | Close inherited file descriptors after fork | Not needed on Windows (no fork) |
| `interprocess` | 1.2.1 | server, client, utils, binary | Unix domain sockets (LocalSocketStream/Listener) | `interprocess` v2+ (supports Windows named pipes) or custom abstraction |
| `sysinfo` | 0.22.5 | server | Process CWD/command detection via `/proc` | `sysinfo` is cross-platform; verify Windows support |
| `mio` (os-ext) | 0.8.11 | client | Unix `SourceFd` for polling raw FDs | Not needed with crossterm's event polling |

#### Conditionally Platform-Specific

| Dependency | Notes |
|-----------|-------|
| `termwiz` | 0.23.2 — Used for input parsing (key events). WezTerm's crate. **Already cross-platform.** |
| `notify` | 6.1.1 — File watcher with `macos_kqueue` feature. **Cross-platform**, but feature flag is macOS-specific. |
| `async-std` | 1.3.0 — Used with `os::unix::io::FromRawFd`. Needs platform branching. |
| `tokio` | 1.38.1 — `tokio::signal::unix` used in client. Has `tokio::signal::windows` equivalent. |
| `isahc`/`curl-sys` | HTTP client with vendored curl. May need OpenSSL adjustments on Windows. |

#### Platform-Agnostic (No changes needed)

`anyhow`, `serde`, `serde_json`, `prost`, `kdl`, `regex`, `uuid`, `url`, `unicode-width`, `vte`, `clap`, `log`, `lazy_static`, `crossbeam`, `chrono`, `tempfile`, `directories`, `wasmi`/`wasmi_wasi`, `sha2`, `base64`, `bytes`, `sixel-*`

### 1.4 `cfg` Conditional Compilation Already Present

| Location | Condition | Purpose |
|----------|-----------|---------|
| `zellij-utils/src/consts.rs:148-181` | `#[cfg(unix)]` | UID-based temp dirs, socket paths, sock max length |
| `zellij-utils/src/shared.rs:14-33` | `#[cfg(unix)]` / `#[cfg(not(unix))]` | `set_permissions()` — already has a no-op stub for non-Unix |
| `zellij-utils/src/consts.rs:88-89` | `#[cfg(not(target_family = "wasm"))]` | Non-WASM asset loading, logging |
| `zellij-utils/Cargo.toml:43` | `[target.'cfg(not(target_family = "wasm"))'.dependencies]` | Conditional deps for non-WASM builds |
| `default-plugins/compact-bar/src/clipboard_utils.rs` | `#[cfg(target_os = "macos")]` | macOS clipboard commands |
| `default-plugins/status-bar/src/second_line.rs` | `#[cfg(target_os = "macos")]` | macOS system info |

---

## 2. OS-Dependent Code Audit

### 2.1 PTY Operations

| File | Lines | Function | What It Does | Difficulty |
|------|-------|----------|-------------|-----------|
| `zellij-server/src/os_input_output.rs` | 5-6 | `openpty()` import & usage (line 234) | Creates PTY master/slave pair | **High** |
| `zellij-server/src/os_input_output.rs` | 155-217 | `handle_openpty()` | Sets up child process with PTY: `login_tty()`, `close_fds`, `pre_exec` | **High** |
| `zellij-server/src/os_input_output.rs` | 189-194 | `pre_exec` closure | `libc::login_tty(pid_secondary)` + `close_fds::close_open_fds(3, &[])` | **High** |
| `zellij-server/src/os_input_output.rs` | 222-247 | `handle_terminal()` | Calls `openpty(None, &orig_termios)`, creates PTY with termios | **High** |
| `zellij-server/src/os_input_output.rs` | 447-465 | `RawFdAsyncReader` | Wraps `RawFd` as async reader via `AsyncFile::from_raw_fd()` | **Medium** |
| `zellij-server/src/os_input_output.rs` | 49-75 | `set_terminal_size_using_fd()` | `libc::ioctl(fd, TIOCSWINSZ, &winsize)` | **Medium** |
| `zellij-server/src/os_input_output.rs` | 657-658 | `read_from_tty_stdout()` | `nix::unistd::read(fd, buf)` | **Medium** |
| `zellij-server/src/os_input_output.rs` | 663-675 | `write_to_tty_stdin()` | `nix::unistd::write(*fd, buf)` | **Medium** |
| `zellij-server/src/os_input_output.rs` | 677-689 | `tcdrain()` | `nix::sys::termios::tcdrain(*fd)` | **Medium** |
| `zellij-server/src/os_input_output.rs` | 206 | PTY cleanup | `nix::unistd::close(pid_secondary)` | **Low** |
| `zellij-server/src/os_input_output.rs` | 919-931 | `get_server_os_input()` | `termios::tcgetattr(0)` to capture original terminal settings | **Medium** |

### 2.2 Terminal I/O (Client-side)

| File | Lines | Function | What It Does | Difficulty |
|------|-------|----------|-------------|-----------|
| `zellij-client/src/os_input_output.rs` | 121-128 | `into_raw_mode()` | `termios::tcgetattr()` + `cfmakeraw()` + `tcsetattr()` | **Medium** |
| `zellij-client/src/os_input_output.rs` | 130-132 | `unset_raw_mode()` | `termios::tcsetattr(TCSANOW, &orig_termios)` | **Medium** |
| `zellij-client/src/os_input_output.rs` | 134-168 | `get_terminal_size_using_fd()` | `libc::ioctl(fd, TIOCGWINSZ, ...)` | **Medium** |
| `zellij-client/src/os_input_output.rs` | 420-431 | `get_client_os_input()` | `termios::tcgetattr(0)` to capture orig termios | **Medium** |
| `zellij-client/src/os_input_output.rs` | 10 | `mio::unix::SourceFd` import | Unix FD polling (imported but usage may be limited) | **Low** |

### 2.3 Signal Handling

| File | Lines | Function | What It Does | Difficulty |
|------|-------|----------|-------------|-----------|
| `zellij-client/src/os_input_output.rs` | 74-118 | `AsyncSignalListener` | `tokio::signal::unix::Signal` for SIGWINCH, SIGTERM, SIGINT, SIGQUIT, SIGHUP | **High** |
| `zellij-client/src/os_input_output.rs` | 336-356 | `handle_signals()` | `signal_hook::iterator::Signals::new(&[SIGWINCH, SIGTERM, SIGINT, SIGQUIT, SIGHUP])` — blocking signal loop | **High** |
| `zellij-server/src/os_input_output.rs` | 79-126 | `handle_command_exit()` | `signal_hook::iterator::Signals::new(&[SIGINT, SIGTERM])` — forwards signals to child | **Medium** |
| `zellij-server/src/os_input_output.rs` | 694-704 | `kill()`, `force_kill()`, `send_sigint()` | `nix::sys::signal::kill(pid, Signal::SIG*)` | **Medium** |

### 2.4 IPC (Inter-Process Communication)

| File | Lines | Function | What It Does | Difficulty |
|------|-------|----------|-------------|-----------|
| `zellij-server/src/lib.rs` | 663-673 | Server socket listener | `LocalSocketListener::bind(&socket_path)` + incoming loop | **High** |
| `zellij-client/src/os_input_output.rs` | 357-374 | `connect_to_server()` | `LocalSocketStream::connect(path)` | **High** |
| `zellij-utils/src/ipc.rs` | 238-328 | `IpcSenderWithContext` / `IpcReceiverWithContext` | Wraps `LocalSocketStream` with protobuf serialization; uses `nix::unistd::dup()` + `FromRawFd` for bidirectional comms | **High** |
| `zellij-utils/src/sessions.rs` | 14, 35, 129, 145-162, 247, 262 | Session discovery | `FileTypeExt::is_socket()`, `LocalSocketStream::connect()` to probe sessions | **High** |
| `zellij-utils/src/web_server_commands.rs` | 5, 10, 62-68 | Web server IPC | `LocalSocketStream::connect()`, `FileTypeExt::is_socket()` | **Medium** |
| `zellij-server/src/plugins/zellij_exports.rs` | 9, 2477 | Plugin-to-server IPC | `LocalSocketStream::connect(path)` | **Medium** |

### 2.5 Process Management

| File | Lines | Function | What It Does | Difficulty |
|------|-------|----------|-------------|-----------|
| `zellij-server/src/lib.rs` | 637-643 | `start_server()` | `daemonize::Daemonize::new().umask().start()` — forks server to background | **High** |
| `zellij-client/src/web_client/mod.rs` | 250-262 | `daemonize_web_server()` | `daemonize::Daemonize::new()` — forks web server | **High** |
| `zellij-server/src/os_input_output.rs` | 173-198 | Child process spawn | `Command::new().pre_exec().spawn()` — Unix-only `pre_exec` | **High** |
| `zellij-server/src/os_input_output.rs` | 800 | `get_all_cmds_by_ppid()` | `Command::new("ps").args(vec!["-ao", "ppid,args"])` — Unix `ps` command | **Medium** |
| `zellij-server/src/os_input_output.rs` | 753-767, 769-796 | `get_cwd()`, `get_cwds()` | `sysinfo` crate for process CWD/command | **Low** (sysinfo is cross-platform) |
| `zellij-server/src/os_input_output.rs` | 971-985 | `run_command_hook()` | `Command::new("sh").arg("-c")` — shell invocation | **Medium** |

### 2.6 Filesystem / Permissions

| File | Lines | Function | What It Does | Difficulty |
|------|-------|----------|-------------|-----------|
| `zellij-utils/src/consts.rs` | 148-181 | `unix_only` module | `nix::unistd::Uid::current()` for user-specific temp/socket dirs | **Medium** |
| `zellij-utils/src/shared.rs` | 14-33 | `set_permissions()` | `PermissionsExt::set_mode()` — already has `#[cfg(not(unix))]` no-op | **Low** (done) |
| `zellij-server/src/lib.rs` | 637-638 | Umask | `nix::sys::stat::umask(Mode::all())` | **Low** |
| `zellij-server/src/lib.rs` | 678 | Socket permissions | `set_permissions(&socket_path, 0o1700)` — sticky bit | **Low** |
| `zellij-utils/src/sessions.rs` | 14, 35 | Session detection | `FileTypeExt::is_socket()` to identify socket files | **Medium** |
| `zellij-server/src/background_jobs.rs` | 24 | `FileTypeExt` | `is_socket()` check | **Low** |
| `zellij-utils/src/consts.rs` | 18 | `SYSTEM_DEFAULT_CONFIG_DIR` | Hardcoded `/etc/zellij` | **Low** |
| `zellij-utils/src/consts.rs` | 19 | `SYSTEM_DEFAULT_DATA_DIR_PREFIX` | Defaults to `/usr` | **Low** |

### 2.7 Unsafe Blocks Summary

| File | Lines | What | Risk |
|------|-------|------|------|
| `zellij-server/src/os_input_output.rs` | 72-74 | `ioctl(fd, TIOCSWINSZ, &winsize)` | Medium — replaced by portable-pty |
| `zellij-server/src/os_input_output.rs` | 173-198 | `Command::pre_exec()` + `login_tty()` | High — entire block is Unix-only |
| `zellij-server/src/os_input_output.rs` | 455-456 | `AsyncFile::from_raw_fd(fd)` | Medium — FD ownership transfer |
| `zellij-client/src/os_input_output.rs` | 150-152 | `ioctl(fd, TIOCGWINSZ, ...)` | Medium — replaced by crossterm |
| `zellij-utils/src/ipc.rs` | 273, 326 | `LocalSocketStream::from_raw_fd(dup_sock)` | High — FD duplication for bidirectional IPC |

---

## 3. Abstraction Layer Design

### 3.1 Overview

Create a new crate `zellij-os` (or a module within `zellij-utils`) that provides platform-abstracted traits. Each trait has a Unix implementation (wrapping current code) and a Windows implementation.

```
zellij-os/
├── src/
│   ├── lib.rs          # Re-exports
│   ├── pty.rs          # PtyApi trait
│   ├── ipc.rs          # IpcApi trait
│   ├── signals.rs      # SignalApi trait
│   ├── process.rs      # ProcessApi trait
│   ├── filesystem.rs   # FsApi helpers
│   ├── terminal.rs     # TerminalApi trait
│   ├── unix/           # #[cfg(unix)] implementations
│   └── windows/        # #[cfg(windows)] implementations
```

### 3.2 PTY Trait

```rust
/// Maps to `portable-pty` from WezTerm
pub trait PtySystem: Send + Sync {
    type Master: PtyMaster;
    type Child: PtyChild;

    fn openpty(&self, size: PtySize, termios: Option<&TerminalConfig>) -> Result<PtyPair>;
    fn set_size(&self, master: &Self::Master, size: PtySize) -> Result<()>;
}

pub trait PtyMaster: Send + Sync {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    fn write(&mut self, buf: &[u8]) -> Result<usize>;
    fn drain(&self) -> Result<()>;
    fn as_async_reader(&self) -> Box<dyn AsyncReader>;
}

pub trait PtyChild: Send + Sync {
    fn id(&self) -> u32;
    fn wait(&mut self) -> Result<ExitStatus>;
    fn try_wait(&mut self) -> Result<Option<ExitStatus>>;
    fn kill(&self) -> Result<()>;
}
```

**Current consumers:**
- `ServerOsInputOutput::spawn_terminal()` → uses `openpty()`, `handle_openpty()`
- `ServerOsInputOutput::set_terminal_size_using_terminal_id()` → uses `set_terminal_size_using_fd()`
- `ServerOsInputOutput::read_from_tty_stdout()` → uses `nix::unistd::read()`
- `ServerOsInputOutput::write_to_tty_stdin()` → uses `nix::unistd::write()`
- `ServerOsInputOutput::tcdrain()` → uses `nix::sys::termios::tcdrain()`

**Semantic gaps (Unix vs Windows):**
- **login_tty**: Unix establishes a controlling terminal via `login_tty()`. ConPTY on Windows handles this internally—the child process automatically gets a console.
- **pre_exec**: Unix uses `pre_exec` to run code in the child after `fork()` but before `exec()`. Windows has no fork; `portable-pty` handles process setup differently (CreateProcess with ConPTY).
- **close_fds**: Unix inherits all parent FDs across fork; `close_fds::close_open_fds(3, &[])` closes them. Windows `CreateProcess` does not inherit handles by default (unless explicitly configured), so this is unnecessary.
- **tcdrain**: No direct Windows equivalent. ConPTY input is buffered differently. The drain operation can be a no-op on Windows.
- **termios**: No Windows equivalent. ConPTY mode configuration is done through `SetConsoleMode()`. The `portable-pty` crate abstracts this.

### 3.3 IPC Trait

```rust
pub trait IpcTransport: Send + Sync {
    type Stream: Read + Write + Send + AsRawHandle;

    fn bind(path: &Path) -> Result<Self::Listener>;
    fn connect(path: &Path) -> Result<Self::Stream>;
    fn duplicate_stream(stream: &Self::Stream) -> Result<Self::Stream>;
}

pub trait IpcListener: Send + Sync {
    type Stream;
    fn incoming(&self) -> impl Iterator<Item = Result<Self::Stream>>;
}
```

**Current consumers:**
- `zellij-server/src/lib.rs:663-673` — `LocalSocketListener::bind()` + incoming loop
- `zellij-client/src/os_input_output.rs:357-374` — `LocalSocketStream::connect()`
- `zellij-utils/src/ipc.rs:267-275, 322-328` — `dup()` + `from_raw_fd()` for bidirectional
- `zellij-utils/src/sessions.rs:145, 247, 262` — Session probing via connect
- `zellij-utils/src/web_server_commands.rs:62-63` — Web server bus socket

**Semantic gaps:**
- **Socket files vs Named pipes**: Unix uses socket files in the filesystem; `is_socket()` detects them. Windows named pipes live in `\\.\pipe\` namespace, not the filesystem. Session discovery logic must be completely different on Windows.
- **FD duplication**: Unix `dup()` + `from_raw_fd()` creates a second handle to the same socket. Windows equivalent is `DuplicateHandle()`. The `interprocess` crate v2 may handle this, or we need a custom abstraction.
- **Permissions**: Unix sockets have file permissions (0o1700). Windows named pipes use security descriptors.
- **Socket path length**: Unix has a 108-byte limit on socket paths (`ZELLIJ_SOCK_MAX_LENGTH`). Windows named pipes have a 256-character limit.

### 3.4 Signal/Event Trait

```rust
pub enum OsEvent {
    Resize,
    Terminate,
    Interrupt,
    Hangup,
    ChildExited(u32),  // pid
}

pub trait OsEventListener: Send {
    fn recv(&mut self) -> Option<OsEvent>;
}

pub trait ProcessSignaler: Send + Sync {
    fn terminate(&self, pid: u32) -> Result<()>;       // SIGHUP on Unix, TerminateProcess on Windows
    fn force_kill(&self, pid: u32) -> Result<()>;      // SIGKILL on Unix, TerminateProcess on Windows
    fn interrupt(&self, pid: u32) -> Result<()>;       // SIGINT on Unix, GenerateConsoleCtrlEvent on Windows
}
```

**Current consumers:**
- `AsyncSignalListener` in `zellij-client/src/os_input_output.rs:74-118` — tokio-based
- `handle_signals()` in `zellij-client/src/os_input_output.rs:336-356` — blocking signal-hook loop
- `handle_command_exit()` in `zellij-server/src/os_input_output.rs:79-126`
- `kill()`, `force_kill()`, `send_sigint()` in `zellij-server/src/os_input_output.rs:694-704`

**Semantic gaps:**
- **SIGWINCH**: Unix sends this signal on terminal resize. Windows uses console events (`WINDOW_BUFFER_SIZE_EVENT`). `crossterm` provides cross-platform resize events.
- **SIGHUP**: Sent when terminal disconnects. No direct Windows equivalent; closest is console close event.
- **SIGKILL vs SIGTERM**: Both map to `TerminateProcess()` on Windows, but Unix distinguishes "catchable" (SIGTERM) from "uncatchable" (SIGKILL). On Windows, `TerminateProcess()` is always uncatchable.
- **SIGINT**: On Unix, sent to process group. On Windows, `GenerateConsoleCtrlEvent(CTRL_C_EVENT)` targets a console group.
- **Signal to child processes**: Unix `kill(pid, signal)` targets a specific process. Windows `GenerateConsoleCtrlEvent()` targets a process group (console group). Individual process signaling requires `TerminateProcess()`.

### 3.5 Process Management Trait

```rust
pub trait ProcessInfo: Send + Sync {
    fn get_cwd(&self, pid: u32) -> Option<PathBuf>;
    fn get_cwds(&self, pids: Vec<u32>) -> HashMap<u32, PathBuf>;
    fn get_all_cmds_by_ppid(&self) -> HashMap<String, Vec<String>>;
}

pub trait Daemonizer {
    fn daemonize(working_dir: PathBuf) -> Result<()>;
}
```

**Current consumers:**
- `get_cwd()` / `get_cwds()` in server — uses `sysinfo` crate (already cross-platform)
- `get_all_cmds_by_ppid()` — uses `ps -ao ppid,args` (Unix-only command)
- `start_server()` — uses `daemonize` crate
- `daemonize_web_server()` — uses `daemonize` crate

**Semantic gaps:**
- **Daemonization**: Unix `fork()`+`setsid()`. Windows has no fork. Alternative: spawn the server as a detached child process via `Command::new(current_exe).creation_flags(DETACHED_PROCESS)`.
- **ps command**: Not available on Windows. Use `sysinfo` or Windows API (`CreateToolhelp32Snapshot`) instead.
- **Shell invocation**: `Command::new("sh").arg("-c")` → `Command::new("cmd").arg("/C")` or `Command::new("powershell").arg("-Command")`.

### 3.6 Terminal I/O Trait

```rust
pub trait TerminalMode: Send + Sync {
    fn enable_raw_mode(&self) -> Result<()>;
    fn disable_raw_mode(&self) -> Result<()>;
    fn get_size(&self) -> Result<TerminalSize>;
}
```

**Current consumers:**
- `into_raw_mode()` / `unset_raw_mode()` in client — nix termios
- `get_terminal_size_using_fd()` in client — libc ioctl
- `get_client_os_input()` / `get_server_os_input()` — `termios::tcgetattr(0)`

**Replacement:** `crossterm` provides `enable_raw_mode()`, `disable_raw_mode()`, `terminal::size()` that work on both Unix and Windows.

### 3.7 Filesystem Helpers

```rust
pub fn get_user_temp_dir() -> PathBuf;      // Unix: /tmp/zellij-{uid}, Windows: %TEMP%\zellij
pub fn get_socket_dir() -> PathBuf;         // Unix: XDG_RUNTIME_DIR, Windows: \\.\pipe\ prefix
pub fn is_session_endpoint(path: &Path) -> bool;  // Unix: is_socket(), Windows: pipe exists
pub fn system_config_dir() -> PathBuf;      // Unix: /etc/zellij, Windows: %ProgramData%\zellij
```

---

## 4. Dependency Migration Plan

### 4.1 `nix` → per-function replacements

| nix Function | Current Use | Replacement | Risk |
|-------------|------------|-------------|------|
| `nix::pty::openpty()` | Create PTY pair | `portable-pty::native_pty_system().openpty()` | Medium — API is different but semantically equivalent |
| `nix::pty::Winsize` | Terminal size struct | `portable-pty::PtySize` or custom struct | Low |
| `nix::sys::termios::tcgetattr()` | Capture terminal settings | `crossterm::terminal::enable_raw_mode()` (manages state internally) | Low |
| `nix::sys::termios::tcsetattr()` | Set terminal settings | `crossterm::terminal::disable_raw_mode()` | Low |
| `nix::sys::termios::cfmakeraw()` | Enable raw mode | `crossterm::terminal::enable_raw_mode()` | Low |
| `nix::sys::termios::tcdrain()` | Drain terminal output | No-op on Windows; flush on Unix | Low |
| `nix::sys::signal::kill()` | Send signal to process | Custom `ProcessSignaler` trait | Medium |
| `nix::unistd::dup()` | Duplicate FD for IPC | Windows `DuplicateHandle()` or refactor IPC to not need duplication | High |
| `nix::unistd::read()` / `write()` | Read/write to PTY FD | `portable-pty::MasterPty::read()` / `write()` | Low |
| `nix::unistd::close()` | Close PTY FD | `Drop` on `portable-pty` types | Low |
| `nix::unistd::Pid` | Process ID type | `u32` (standard on both platforms) | Low |
| `nix::unistd::Uid` | User ID | `whoami` crate or Windows `GetUserName` | Low |
| `nix::sys::stat::umask()` | File creation mask | No-op on Windows (ACLs instead) | Low |
| `nix::Error` | Error type in signatures | `std::io::Error` or `anyhow::Error` | Medium — touches many function signatures |

### 4.2 `libc` → replacements

| libc Function | Replacement |
|--------------|-------------|
| `libc::ioctl(fd, TIOCSWINSZ, ...)` | `portable-pty::MasterPty::resize()` |
| `libc::ioctl(fd, TIOCGWINSZ, ...)` | `crossterm::terminal::size()` |
| `libc::login_tty()` | Handled internally by `portable-pty` |
| `libc::TIOCSWINSZ` / `TIOCGWINSZ` | Removed (no longer needed) |

### 4.3 `signal-hook` → `crossterm` events + `ctrlc`

| Current | Replacement |
|---------|------------|
| `signal_hook::iterator::Signals::new(&[SIGWINCH, ...])` | `crossterm::event::EventStream` for resize; `tokio::signal::ctrl_c()` for interrupt |
| `signals.forever()` blocking loop | `crossterm::event::poll()` / `read()` event loop |
| `signals.pending()` non-blocking check | `crossterm::event::poll(Duration::ZERO)` |

### 4.4 `daemonize` → cross-platform background process

| Current | Replacement |
|---------|------------|
| `daemonize::Daemonize::new().start()` | Unix: keep `daemonize` behind `#[cfg(unix)]`; Windows: `Command::new(current_exe).creation_flags(DETACHED_PROCESS).spawn()` |

### 4.5 `interprocess` → upgraded or replaced

| Current (v1.2.1) | Option A: `interprocess` v2+ | Option B: Custom |
|-------------------|------------------------------|------------------|
| `LocalSocketStream` (Unix domain sockets) | Named pipe support on Windows added in v2 | `#[cfg]` with `std::os::unix::net` / Windows named pipes |
| `LocalSocketListener` | Listener support for both | Custom listener trait |
| FD duplication via `dup()` + `from_raw_fd()` | May handle internally | Requires `DuplicateHandle()` on Windows |

**Recommendation:** Upgrade `interprocess` to v2+ which has first-class Windows named pipe support, then create a thin wrapper for the `dup()` pattern.

### 4.6 `close_fds` → conditional compilation

```rust
#[cfg(unix)]
close_fds::close_open_fds(3, &[]);
// No-op on Windows — CreateProcess doesn't inherit handles by default
```

### 4.7 `async-std` Unix extensions → tokio

The codebase is migrating from `async-std` to `tokio` (see TODO in `zellij-client/Cargo.toml:12-14`). The Unix-specific `async_std::os::unix::io::FromRawFd` used in `RawFdAsyncReader` should be replaced with `portable-pty`'s async read support.

### 4.8 Error type migration: `nix::Error` → `std::io::Error`

`nix::Error` appears in many public function signatures:

| Function | File |
|----------|------|
| `get_server_os_input() -> Result<_, nix::Error>` | `zellij-server/src/os_input_output.rs:919` |
| `get_client_os_input() -> Result<_, nix::Error>` | `zellij-client/src/os_input_output.rs:420` |
| `get_cli_client_os_input() -> Result<_, nix::Error>` | `zellij-client/src/os_input_output.rs:433` |
| `unset_raw_mode() -> Result<(), nix::Error>` | `zellij-client/src/os_input_output.rs:130, 195` |
| `get_os_input(fn() -> Result<_, nix::Error>)` | `src/commands.rs:150` |

All should migrate to `std::io::Error` or `anyhow::Error`.

---

## 5. Test Coverage Analysis

### 5.1 OS-Dependent Code Test Coverage

| Component | File(s) | Lines of Code | Tests | Coverage | Risk |
|-----------|---------|--------------|-------|----------|------|
| PTY creation/lifecycle | `zellij-server/src/os_input_output.rs` | ~250 | 1 (`get_cwd`) | **<5%** | **CRITICAL** |
| Terminal raw mode | `zellij-client/src/os_input_output.rs` | ~120 | 0 | **0%** | **HIGH** |
| Signal handling | Both `os_input_output.rs` files | ~80 | 0 | **0%** | **CRITICAL** |
| IPC socket operations | `zellij-utils/src/ipc.rs` | ~150 | Serialization only | **~30%** | **HIGH** |
| Session discovery | `zellij-utils/src/sessions.rs` | ~200 | 0 | **0%** | **MEDIUM** |
| Daemonization | `zellij-server/src/lib.rs` | ~20 | 0 | **0%** | **MEDIUM** |
| Process CWD detection | `zellij-server/src/os_input_output.rs` | ~50 | 1 | **~50%** | **LOW** |

### 5.2 Tests Requiring Platform-Specific Variants

| Test File | Issue |
|-----------|-------|
| `zellij-server/src/unit/os_input_output_tests.rs` | Uses `nix::pty::openpty()` directly in `TestTerminal` helper |
| `zellij-server/src/unit/screen_tests.rs` | Mock `ServerOsApi` uses `RawFd`, `LocalSocketStream` |
| `zellij-server/src/tab/unit/tab_tests.rs` | Mock `ServerOsApi` uses `RawFd`, `LocalSocketStream` |
| `zellij-server/src/tab/unit/tab_integration_tests.rs` | Mock `ServerOsApi` uses `RawFd`, `LocalSocketStream` |
| `zellij-server/src/tab/unit/layout_applier_tests.rs` | Mock `ServerOsApi` uses `RawFd`, `LocalSocketStream` |
| `zellij-client/src/unit/terminal_loop_tests.rs` | Uses `RawFd` in mock |
| `zellij-client/src/web_client/unit/web_client_tests.rs` | Uses `RawFd` in mock |
| E2E tests (`src/tests/e2e/`) | Linux-only, uses SSH, VTE parsing |

### 5.3 Tests Needed Before Refactoring

Before changing any OS-dependent code, these integration tests should be added to prevent regressions:

1. **PTY round-trip test**: Spawn a PTY, write to stdin, read from stdout, verify output
2. **Terminal resize test**: Create PTY, resize, verify new size
3. **Signal forwarding test**: Spawn PTY with a process, send SIGINT, verify process received it
4. **IPC round-trip test**: Create server socket, connect client, send/recv messages
5. **Session discovery test**: Create a socket, verify `get_sessions()` finds it
6. **Raw mode test**: Enable raw mode, verify terminal state, disable, verify restoration

### 5.4 CI Configuration Status

| Platform | Build | Unit Tests | E2E Tests |
|----------|-------|-----------|-----------|
| Ubuntu | ✅ | ✅ | ✅ |
| macOS | ✅ | ✅ | ❌ |
| Windows | ❌ | ❌ | ❌ |

**Needed:** Add `windows-latest` to CI matrix after abstraction layer is complete.

---

## 6. Ordered Work Plan

### Phase 0: Pre-Refactoring Test Safety Net

**PR 0.1: Add integration tests for OS-dependent code paths**
- Files touched: ~5 new test files
- Changes: ~500 lines added
- Dependencies: None
- Done when: `cargo test` passes with new tests covering PTY, IPC, signal, raw mode on Linux
- Priority: **CRITICAL** — all subsequent work depends on this safety net

### Phase 1: Error Type Migration

**PR 1.1: Replace `nix::Error` with `std::io::Error` in public APIs**
- Files touched: ~10
- Changes: ~100 lines
- Dependencies: None
- Done when: All functions returning `nix::Error` now return `std::io::Error`; `cargo test` passes
- Risk: Low — mechanical replacement

Files to change:
- `zellij-server/src/os_input_output.rs` (function signatures)
- `zellij-client/src/os_input_output.rs` (function signatures, `ClientOsApi` trait)
- `src/commands.rs` (the `get_os_input` helper)
- Test mock files that reference `nix::Error`

### Phase 2: Terminal I/O Migration (termios → crossterm)

**PR 2.1: Replace client-side termios with crossterm for raw mode and terminal size**
- Files touched: ~3
- Changes: ~150 lines
- Dependencies: PR 1.1
- Done when: Client terminal control uses `crossterm`; no more `termios` imports in client; `cargo test` passes; manual testing confirms raw mode works

Files to change:
- `zellij-client/src/os_input_output.rs` — `into_raw_mode()`, `unset_raw_mode()`, `get_terminal_size_using_fd()`, `get_client_os_input()`
- `zellij-client/Cargo.toml` — add `crossterm`, remove `nix` (if no other uses remain)
- `ClientOsApi` trait — change `fd: RawFd` parameter to something platform-agnostic

**Key change:** The `orig_termios` field in `ClientOsInputOutput` goes away. `crossterm` manages terminal state internally.

**PR 2.2: Replace server-side terminal size ioctl with portable-pty resize**
- Files touched: ~2
- Changes: ~80 lines
- Dependencies: PR 3.1 (PTY abstraction)
- Done when: `set_terminal_size_using_fd()` uses `portable-pty` resize API; tests pass

### Phase 3: PTY Abstraction

**PR 3.1: Introduce portable-pty and abstract PTY operations**
- Files touched: ~5
- Changes: ~400 lines
- Dependencies: PR 1.1
- Done when: PTY creation, read, write, resize all go through `portable-pty`; `ServerOsApi` trait no longer uses `RawFd` for PTY operations; tests pass; manual testing confirms pane spawning works

This is the **largest and riskiest single change**. It replaces:
- `nix::pty::openpty()` → `portable_pty::native_pty_system().openpty()`
- `handle_openpty()` with `pre_exec` + `login_tty()` → `portable_pty::CommandBuilder` + `master.spawn_command()`
- `RawFdAsyncReader` → reader from `portable_pty::MasterPty`
- `nix::unistd::read/write/close` on PTY FDs → `MasterPty` read/write + Drop
- `set_terminal_size_using_fd()` → `MasterPty::resize()`
- `tcdrain()` → flush or no-op

Files to change:
- `zellij-server/src/os_input_output.rs` — core rewrite of PTY functions
- `zellij-server/Cargo.toml` — add `portable-pty`; remove `close_fds` (conditionally)
- `ServerOsApi` trait — change `RawFd` to opaque PTY handle type
- Test mocks referencing `RawFd`

**PR 3.2: Update all test mocks to use new PTY types**
- Files touched: ~6 test files
- Changes: ~200 lines
- Dependencies: PR 3.1
- Done when: All screen_tests, tab_tests, layout_applier_tests compile and pass with new types

### Phase 4: Signal Handling Abstraction

**PR 4.1: Abstract signal handling behind OsEvent trait**
- Files touched: ~4
- Changes: ~250 lines
- Dependencies: PR 2.1 (crossterm for resize events)
- Done when: `AsyncSignalListener` and `handle_signals()` use the `OsEventListener` trait; Unix impl wraps `tokio::signal::unix`; tests pass

Files to change:
- `zellij-client/src/os_input_output.rs` — `AsyncSignalListener`, `handle_signals()`
- `zellij-server/src/os_input_output.rs` — `handle_command_exit()`, signal sending
- New: `zellij-utils/src/os_events.rs` or similar

**PR 4.2: Add Windows signal/event implementation (behind cfg)**
- Dependencies: PR 4.1
- Uses: `crossterm::event::Event::Resize` for window size, `tokio::signal::ctrl_c()` for Ctrl+C, `SetConsoleCtrlHandler` for console events
- Done when: Windows implementation compiles (no runtime testing yet)

### Phase 5: IPC Abstraction

**PR 5.1: Upgrade `interprocess` to v2+ with cross-platform support**
- Files touched: ~8
- Changes: ~300 lines
- Dependencies: None (can be done in parallel with Phase 3-4)
- Done when: All `LocalSocketStream`/`LocalSocketListener` usage works with `interprocess` v2 API; tests pass

**PR 5.2: Abstract FD duplication in IPC**
- Files touched: ~2
- Changes: ~100 lines
- Dependencies: PR 5.1
- Done when: `IpcSenderWithContext::get_receiver()` and `IpcReceiverWithContext::get_sender()` no longer use `nix::unistd::dup()` directly; uses platform-abstract duplication

Files to change:
- `zellij-utils/src/ipc.rs` — replace `dup()` + `from_raw_fd()` with platform-abstract `duplicate_stream()`

**PR 5.3: Abstract session discovery**
- Files touched: ~3
- Changes: ~150 lines
- Dependencies: PR 5.1
- Done when: `zellij-utils/src/sessions.rs` no longer uses `FileTypeExt::is_socket()`; session detection works through IPC abstraction

### Phase 6: Process Management

**PR 6.1: Abstract daemonization**
- Files touched: ~3
- Changes: ~100 lines
- Dependencies: None
- Done when: Server startup uses platform-abstract background process spawning; `daemonize` only used behind `#[cfg(unix)]`

Files to change:
- `zellij-server/src/lib.rs` — `start_server()` daemonize section
- `zellij-client/src/web_client/mod.rs` — `daemonize_web_server()`

**PR 6.2: Abstract shell invocation and process listing**
- Files touched: ~2
- Changes: ~80 lines
- Dependencies: None
- Done when: `run_command_hook()` uses platform-appropriate shell; `get_all_cmds_by_ppid()` doesn't depend on Unix `ps`

### Phase 7: Filesystem / Constants

**PR 7.1: Platform-abstract path constants**
- Files touched: ~3
- Changes: ~100 lines
- Dependencies: PR 5.1 (IPC path format)
- Done when: `zellij-utils/src/consts.rs` has `#[cfg(windows)]` block alongside `#[cfg(unix)]` for temp dirs, socket dirs, config paths

The `#[cfg(unix)]` module in `consts.rs` needs a `#[cfg(windows)]` counterpart:
- `ZELLIJ_TMP_DIR` → `%TEMP%\zellij-{username}` (no UID on Windows)
- `ZELLIJ_SOCK_DIR` → Named pipe namespace or a configuration directory
- `ZELLIJ_SOCK_MAX_LENGTH` → Different limit for named pipes
- `SYSTEM_DEFAULT_CONFIG_DIR` → `%ProgramData%\zellij`
- `SYSTEM_DEFAULT_DATA_DIR_PREFIX` → `%ProgramFiles%\zellij`

### Phase 8: Windows Compilation Gate

**PR 8.1: Attempt Windows compilation, fix remaining issues**
- Dependencies: All Phase 1-7 PRs
- Done when: `cargo build --target x86_64-pc-windows-msvc` succeeds
- This will likely surface additional platform-specific code not caught in the audit

**PR 8.2: Add Windows to CI**
- Dependencies: PR 8.1
- Done when: `windows-latest` in GitHub Actions matrix; builds pass; unit tests pass

### Phase 9: Windows Runtime Testing

**PR 9.1: Manual testing on Windows**
- Test: basic pane creation, split, resize, close
- Test: session attach/detach
- Test: plugin loading
- Test: configuration loading

**PR 9.2: Add Windows-specific E2E tests**
- Dependencies: Working Windows binary
- May need a different test harness than SSH

### Summary Timeline

| Phase | PRs | Estimated Files | Key Risk |
|-------|-----|----------------|----------|
| 0: Test safety net | 1 | ~5 | None |
| 1: Error types | 1 | ~10 | Low |
| 2: Terminal I/O | 2 | ~5 | Low |
| 3: PTY abstraction | 2 | ~11 | **High** |
| 4: Signal handling | 2 | ~6 | Medium |
| 5: IPC abstraction | 3 | ~13 | **High** |
| 6: Process management | 2 | ~5 | Medium |
| 7: Filesystem/constants | 1 | ~3 | Low |
| 8: Windows compilation | 2 | ~? | Medium |
| 9: Windows testing | 2 | ~5 | Medium |
| **Total** | **~18 PRs** | **~63+ files** | |

---

## 7. Risk Register

### 7.1 ConPTY Behavioral Differences

| Issue | Unix Behavior | Windows ConPTY Behavior | Impact | Mitigation |
|-------|--------------|------------------------|--------|-----------|
| **Output processing** | Raw PTY output goes directly to Zellij's VT parser | ConPTY may pre-process output, applying its own VT parsing | **HIGH** — could cause double-rendering, incorrect escape sequence handling | Use ConPTY in "raw" mode if available; test thoroughly with complex TUI apps |
| **Resize timing** | `SIGWINCH` + `ioctl(TIOCSWINSZ)` is synchronous | ConPTY resize is asynchronous; may see output at old size briefly | **MEDIUM** — could cause temporary rendering artifacts | Buffer resize events, debounce like the existing `SIGWINCH_CB_THROTTLE_DURATION` |
| **Input encoding** | UTF-8 bytes written directly to PTY stdin | ConPTY may expect Windows input records or UTF-16 | **MEDIUM** — non-ASCII input could break | Verify `portable-pty` handles encoding properly |
| **Exit code propagation** | `waitpid()` returns exact exit code | `GetExitCodeProcess()` returns DWORD | **LOW** — types differ but values should match | Cast appropriately |
| **Cursor/mouse reporting** | Terminal-dependent (xterm, etc.) | Windows Terminal supports it; legacy conhost may not | **MEDIUM** — mouse support may not work in all terminals | Feature-detect mouse support; graceful degradation |

### 7.2 Abstraction Leaks

| Area | Issue | Severity |
|------|-------|---------|
| **RawFd pervasiveness** | `RawFd` (i32) is used throughout `ServerOsApi` trait and all test mocks. Windows uses `HANDLE` (pointer-sized). Changing the type is viral. | **HIGH** |
| **IPC FD duplication** | The pattern of `dup()` + `from_raw_fd()` to create bidirectional IPC from a single socket is deeply embedded. Windows named pipes are inherently bidirectional, so the duplication pattern doesn't translate cleanly. | **HIGH** |
| **Session discovery** | Unix identifies sessions by socket files in a directory. Windows named pipes aren't in the filesystem. Completely different discovery mechanism needed. | **HIGH** |
| **Terminal state management** | Unix stores/restores `termios` struct. `crossterm` manages state internally but may not capture all settings Zellij relies on (e.g., specific flags). | **MEDIUM** |
| **Process groups** | Unix uses process groups for signal delivery. Windows console groups work differently. Killing a "pane" on Windows may not propagate correctly to all child processes. | **MEDIUM** |

### 7.3 Dependency Risks

| Dependency | Risk | Detail |
|-----------|------|--------|
| `portable-pty` | **MEDIUM** | Well-maintained (part of WezTerm), but Zellij's PTY usage pattern (explicit `openpty` + `login_tty` + `pre_exec`) doesn't map 1:1 to `portable-pty`'s higher-level API. May need to use internal/low-level APIs. |
| `interprocess` v2 | **MEDIUM** | Major version upgrade with API changes. Every call site needs updating. Named pipe support may have edge cases. |
| `wasmi`/`wasmi_wasi` | **LOW** | WASM runtime is platform-independent. The WASI implementation may need Windows path handling, but `wasmi_wasi` should handle this. |
| `isahc`/`curl-sys` | **LOW** | HTTP client with vendored curl. The `vendored_curl` feature compiles OpenSSL from source, which adds Windows build complexity. May need to switch to `rustls` backend. |
| `sysinfo` | **LOW** | Already cross-platform. Process CWD detection on Windows should work. |
| `daemonize` | **LOW** | Only used behind `#[cfg(unix)]` after abstraction. |

### 7.4 Performance Implications

| Area | Concern | Severity |
|------|---------|---------|
| **PTY throughput** | `portable-pty` adds a layer between Zellij and the raw PTY. May add latency on high-throughput output (e.g., `cat large_file`). | **LOW** — `portable-pty` is used by WezTerm which handles this well |
| **IPC overhead** | Named pipes on Windows may have different throughput characteristics than Unix domain sockets. | **LOW** — both are kernel-level IPC |
| **Signal/event polling** | Moving from direct signal handlers to `crossterm` event polling adds a layer. | **LOW** — polling is lightweight |
| **ConPTY overhead** | ConPTY adds overhead vs raw Unix PTY because it runs an internal VT parser. Complex output (many escape sequences) may be slower. | **MEDIUM** — test with heavy output scenarios |

### 7.5 Upstream Zellij Concerns

| Item | Detail |
|------|--------|
| **`RawFd` in `ServerOsApi` trait** | The public trait uses `RawFd` extensively. Changing this to a platform-abstract type is a breaking API change that affects all test mocks (~6 test files). This will likely need upstream buy-in. |
| **`nix::Error` in public APIs** | Similarly, `nix::Error` in function return types is a public API concern. |
| **Plugin ABI** | WASM plugins use `extern "C"` FFI. The plugin ABI should be platform-independent, but verify no assumptions about paths or process IDs leak through. |
| **E2E test infrastructure** | The E2E tests use SSH + VTE parsing on Linux. A completely different approach is needed for Windows E2E testing. |
| **Web client** | Uses `tokio-tungstenite` + `axum` which are cross-platform, but the web server daemonization path is Unix-only. |
| **`async-std` → `tokio` migration** | The codebase has a pending migration from `async-std` to `tokio` (noted in TODO comments). Coordinating the platform abstraction with this async runtime migration could reduce total churn. |

### 7.6 Items That Genuinely Cannot Share an Interface

| Item | Why | Approach |
|------|-----|---------|
| Session socket path construction | Unix: filesystem path to socket file; Windows: `\\.\pipe\zellij-{name}` | Fully separate implementations behind `#[cfg]` |
| Session enumeration | Unix: `readdir()` + `is_socket()`; Windows: enumerate named pipes or use a registry file | Separate implementations |
| Daemonization | Unix: `fork()` + `setsid()`; Windows: `CreateProcess` with `DETACHED_PROCESS` | Separate implementations |
| Controlling terminal setup | Unix: `login_tty()` in `pre_exec`; Windows: ConPTY handles this | Abstracted away by `portable-pty` |
| File permissions (sticky bit) | Unix: `chmod` mode bits; Windows: ACLs | `#[cfg(unix)]` only; skip on Windows |

---

## Appendix A: Complete File Inventory of OS-Dependent Code

| File | OS-Dependent? | Category | Priority |
|------|--------------|----------|----------|
| `zellij-server/src/os_input_output.rs` | **YES** — heavily | PTY, Signals, Terminal | P0 |
| `zellij-client/src/os_input_output.rs` | **YES** — heavily | Terminal I/O, Signals, IPC | P0 |
| `zellij-utils/src/ipc.rs` | **YES** — moderately | IPC (dup, from_raw_fd) | P0 |
| `zellij-utils/src/consts.rs` | **YES** — moderately | Filesystem paths, UID | P1 |
| `zellij-utils/src/sessions.rs` | **YES** — moderately | Session discovery (is_socket) | P1 |
| `zellij-server/src/lib.rs` | **YES** — moderately | Daemonization, socket listener, umask | P1 |
| `zellij-utils/src/shared.rs` | **Partial** — has cfg | Permissions (already abstracted) | P2 |
| `zellij-utils/src/web_server_commands.rs` | **YES** — lightly | Socket connection, is_socket | P2 |
| `zellij-server/src/background_jobs.rs` | **YES** — lightly | FileTypeExt::is_socket | P2 |
| `zellij-server/src/plugins/zellij_exports.rs` | **YES** — lightly | LocalSocketStream connect, FileTypeExt | P2 |
| `zellij-client/src/web_client/mod.rs` | **YES** — moderately | Daemonization, pipe() | P2 |
| `src/commands.rs` | **YES** — lightly | nix::Error type, get_os_input | P1 |
| `zellij-server/src/pty.rs` | **YES** — lightly | Uses Pid type from nix | P1 |
| `zellij-server/src/terminal_bytes.rs` | **YES** — lightly | RawFd usage | P1 |
| Test files (6+) | **YES** | Mock implementations with RawFd | P1 |
| Default plugins | **Minimal** | macOS clipboard, system info | P3 |

## Appendix B: `termwiz` Usage (Already Cross-Platform)

`termwiz` (from WezTerm) is used for input event parsing and is already cross-platform. These files use it but do NOT need changes for the porting effort:

- `zellij-client/src/stdin_handler.rs` — key/mouse event parsing
- `zellij-client/src/input_handler.rs` — input event routing
- `zellij-client/src/lib.rs` — `InputEvent` type
- `zellij-utils/src/input/mod.rs` — input configuration
- `zellij-utils/src/input/mouse.rs` — mouse event types
- `zellij-utils/src/data.rs` — key/modifier data types

No migration needed for `termwiz`. It already supports Windows.

## Appendix C: Summary Statistics

| Metric | Count |
|--------|-------|
| Total `.rs` files with OS-dependent code | ~18 production + ~6 test |
| `nix::` imports | ~15 functions/types |
| `libc::` direct calls | 4 (ioctl×2, login_tty, TIOCSWINSZ/TIOCGWINSZ) |
| `unsafe` blocks touching OS APIs | 5 |
| `#[cfg(unix)]` blocks | 4 |
| `std::os::unix::` imports | 12 files |
| `interprocess` LocalSocket usage | 12 files |
| `signal-hook` / `tokio::signal::unix` usage | 3 files |
| `daemonize` usage | 2 files |
| Public API functions returning `nix::Error` | 6 |
| Test files needing platform-abstract mocks | 6 |
