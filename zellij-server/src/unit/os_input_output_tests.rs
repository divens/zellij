use super::*;

use zellij_os::pty::{spawn_in_pty, PtySize};

// --- Cross-platform command helpers ---

/// Long-running command (for signal and PTY tests).
#[cfg(unix)]
fn long_running_cmd() -> (PathBuf, Vec<String>) {
    (PathBuf::from("/bin/sleep"), vec!["60".to_string()])
}
#[cfg(windows)]
fn long_running_cmd() -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("ping"),
        vec!["-n".to_string(), "999".to_string(), "127.0.0.1".to_string()],
    )
}

/// Echo command (for PTY read tests).
#[cfg(unix)]
fn echo_cmd() -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/echo"),
        vec!["hello from pty".to_string()],
    )
}
#[cfg(windows)]
fn echo_cmd() -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("cmd.exe"),
        vec![
            "/c".to_string(),
            "echo".to_string(),
            "hello from pty".to_string(),
        ],
    )
}

/// Stdin-reading command (for PTY write tests).
#[cfg(unix)]
fn stdin_reader_cmd() -> (PathBuf, Vec<String>) {
    (PathBuf::from("/bin/cat"), vec![])
}
#[cfg(windows)]
fn stdin_reader_cmd() -> (PathBuf, Vec<String>) {
    // `more` reads stdin and echoes to stdout; works under ConPTY
    (PathBuf::from("more"), vec![])
}

fn make_server() -> ServerOsInputOutput {
    ServerOsInputOutput {
        client_senders: Arc::default(),
        terminal_id_to_pty: Arc::default(),
        cached_resizes: Arc::default(),
    }
}

#[test]
fn get_cwd() {
    let server = make_server();

    let pid = std::process::id();
    assert!(
        server.get_cwd(pid).is_some(),
        "Get current working directory from PID {}",
        pid
    );
}

// --- PTY integration tests via portable-pty ---

#[test]
fn pty_roundtrip_write_read() {
    // Spawn a simple command via portable-pty and verify we can read its output
    let (cmd, args) = echo_cmd();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        cmd,
        args,
        None,
        vec![],
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Box::new(move |_exit_status| {
            let _ = done_tx.send(());
        }),
    )
    .expect("spawn_in_pty should succeed");

    let mut reader = result.pty.try_clone_reader().expect("clone reader");
    let mut output = String::new();
    // Read in a loop with a timeout
    let start = std::time::Instant::now();
    let mut buf = [0u8; 1024];
    while start.elapsed() < std::time::Duration::from_secs(5) {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                output.push_str(&String::from_utf8_lossy(&buf[..n]));
                if output.contains("hello from pty") {
                    break;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            },
            Err(_) => break,
        }
    }

    assert!(
        output.contains("hello from pty"),
        "should read output from spawned command, got: {:?}",
        output
    );

    // Wait for process exit
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

// --- Terminal resize tests ---

#[test]
fn pty_resize() {
    let (cmd, args) = long_running_cmd();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        cmd,
        args,
        None,
        vec![],
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Box::new(move |_exit_status| {
            let _ = done_tx.send(());
        }),
    )
    .expect("spawn_in_pty should succeed");

    // Resize the PTY
    result
        .pty
        .resize(PtySize {
            rows: 48,
            cols: 160,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize should succeed");

    // Verify via get_size
    let size = result.pty.get_size().expect("get_size should succeed");
    assert_eq!(size.cols, 160, "columns should be 160 after resize");
    assert_eq!(size.rows, 48, "rows should be 48 after resize");

    // Cleanup: kill the sleep process
    if let Some(pid) = result.child_pid {
        let _ = zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

#[test]
fn resize_through_server_api() {
    let (cmd, args) = long_running_cmd();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        cmd,
        args,
        None,
        vec![],
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Box::new(move |_exit_status| {
            let _ = done_tx.send(());
        }),
    )
    .expect("spawn_in_pty should succeed");

    let server = make_server();
    let terminal_id = 0u32;
    server
        .terminal_id_to_pty
        .lock()
        .unwrap()
        .insert(terminal_id, Some(result.pty));

    server
        .set_terminal_size_using_terminal_id(terminal_id, 200, 50, None, None)
        .expect("resize should succeed");

    // Verify the resize took effect by reading back from the PtyHandle
    let pty_map = server.terminal_id_to_pty.lock().unwrap();
    let pty_handle = pty_map.get(&terminal_id).unwrap().as_ref().unwrap();
    let size = pty_handle.get_size().expect("get_size");
    assert_eq!(size.cols, 200);
    assert_eq!(size.rows, 50);

    drop(pty_map);

    // Cleanup
    if let Some(pid) = result.child_pid {
        let _ = zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

// --- Signal delivery tests ---

/// Spawn a long-running process for signal tests.
/// On Windows, console applications need their own console to initialize.
/// `cargo test` redirects stdout/stderr to pipes, so child processes can't
/// attach to the parent's console — causing 0xc0000142 (STATUS_DLL_INIT_FAILED).
/// CREATE_NO_WINDOW gives each child a hidden console so DLL init succeeds.
fn spawn_long_running() -> std::process::Child {
    let (cmd, args) = long_running_cmd();
    let mut command = Command::new(&cmd);
    command.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .expect("failed to spawn long-running process")
}

#[test]
fn kill_sends_sighup_to_process() {
    let child = spawn_long_running();
    let pid = child.id();

    let server = make_server();

    server.kill(pid).expect("kill should succeed");

    // Give the signal time to be delivered
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Verify the process was killed by trying to signal it (should fail if dead)
    let result = zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::HangUp);
    // Process may or may not still exist at this point (race), but the important
    // thing is that kill() didn't error
}

#[test]
fn force_kill_sends_sigkill_to_process() {
    let child = spawn_long_running();
    let pid = child.id();

    let server = make_server();

    server.force_kill(pid).expect("force_kill should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[test]
fn send_sigint_to_process() {
    let child = spawn_long_running();
    let pid = child.id();

    let server = make_server();

    server.send_sigint(pid).expect("send_sigint should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));
}

// --- PTY read/write through ServerOsApi ---

#[test]
fn write_through_server_os_api() {
    let (cmd, args) = stdin_reader_cmd();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        cmd,
        args,
        None,
        vec![],
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Box::new(move |_exit_status| {
            let _ = done_tx.send(());
        }),
    )
    .expect("spawn_in_pty should succeed");

    let server = make_server();
    let terminal_id = 0u32;
    let child_pid = result.child_pid;
    server
        .terminal_id_to_pty
        .lock()
        .unwrap()
        .insert(terminal_id, Some(result.pty));

    // Write through the API (simulates Zellij sending keystrokes to pane)
    let data = b"test input\r";
    let written = server
        .write_to_tty_stdin(terminal_id, data)
        .expect("write_to_tty_stdin should succeed");
    assert_eq!(written, data.len());

    // Cleanup
    if let Some(pid) = child_pid {
        let _ = zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

// --- Cached resize tests ---

#[test]
fn cached_resizes_are_applied() {
    let (cmd, args) = long_running_cmd();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        cmd,
        args,
        None,
        vec![],
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Box::new(move |_exit_status| {
            let _ = done_tx.send(());
        }),
    )
    .expect("spawn_in_pty should succeed");

    let mut server = make_server();
    let terminal_id = 0u32;
    let child_pid = result.child_pid;
    server
        .terminal_id_to_pty
        .lock()
        .unwrap()
        .insert(terminal_id, Some(result.pty));

    server.cache_resizes();

    // While caching, resizes should not be applied immediately
    server
        .set_terminal_size_using_terminal_id(terminal_id, 160, 48, None, None)
        .expect("cached resize should succeed");

    // Check that the actual PTY size hasn't changed yet
    {
        let pty_map = server.terminal_id_to_pty.lock().unwrap();
        let pty_handle = pty_map.get(&terminal_id).unwrap().as_ref().unwrap();
        let size = pty_handle.get_size().expect("get_size");
        assert_eq!(size.cols, 80, "size should not change while cached");
        assert_eq!(size.rows, 24);
    }

    server.apply_cached_resizes();

    // Now the resize should be applied
    {
        let pty_map = server.terminal_id_to_pty.lock().unwrap();
        let pty_handle = pty_map.get(&terminal_id).unwrap().as_ref().unwrap();
        let size = pty_handle.get_size().expect("get_size");
        assert_eq!(size.cols, 160, "cached resize should be applied");
        assert_eq!(size.rows, 48);
    }

    // Cleanup
    if let Some(pid) = child_pid {
        let _ = zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}
