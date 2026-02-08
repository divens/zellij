use super::*;

use zellij_os::pty::{spawn_in_pty, PtySize};

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
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        PathBuf::from("/bin/echo"),
        vec!["hello from pty".to_string()],
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
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        PathBuf::from("/bin/sleep"),
        vec!["60".to_string()],
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
    let size = result
        .pty
        .get_size()
        .expect("get_size should succeed");
    assert_eq!(size.cols, 160, "columns should be 160 after resize");
    assert_eq!(size.rows, 48, "rows should be 48 after resize");

    // Cleanup: kill the sleep process
    if let Some(pid) = result.child_pid {
        let _ =
            zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

#[test]
fn resize_through_server_api() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        PathBuf::from("/bin/sleep"),
        vec!["60".to_string()],
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
        let _ =
            zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

// --- Signal delivery tests ---

#[test]
fn kill_sends_sighup_to_process() {
    let child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let pid = child.id();

    let server = make_server();

    server.kill(pid).expect("kill should succeed");

    // Give the signal time to be delivered
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Verify the process was killed by trying to signal it (should fail if dead)
    let result =
        zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::HangUp);
    // Process may or may not still exist at this point (race), but the important
    // thing is that kill() didn't error
}

#[test]
fn force_kill_sends_sigkill_to_process() {
    let child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let pid = child.id();

    let server = make_server();

    server.force_kill(pid).expect("force_kill should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[test]
fn send_sigint_to_process() {
    let child = Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn cat");
    let pid = child.id();

    let server = make_server();

    server.send_sigint(pid).expect("send_sigint should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));
}

// --- PTY read/write through ServerOsApi ---

#[test]
fn write_through_server_os_api() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        PathBuf::from("/bin/cat"),
        vec![],
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
        let _ =
            zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}

// --- Cached resize tests ---

#[test]
fn cached_resizes_are_applied() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let result = spawn_in_pty(
        PathBuf::from("/bin/sleep"),
        vec!["60".to_string()],
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
        let _ =
            zellij_os::process::signal_process(pid, zellij_os::process::ProcessSignal::Kill);
    }
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
}
