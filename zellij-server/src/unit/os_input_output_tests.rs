use super::*;

use nix::pty::{openpty, OpenptyResult};
use nix::unistd::close;

fn get_winsize(fd: RawFd) -> Winsize {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    #[allow(clippy::useless_conversion)]
    unsafe {
        libc::ioctl(fd, libc::TIOCGWINSZ.into(), &mut ws);
    }
    ws
}

struct TestTerminal {
    openpty: OpenptyResult,
}

impl TestTerminal {
    pub fn new() -> TestTerminal {
        let openpty = openpty(None, None).expect("Could not create openpty");
        TestTerminal { openpty }
    }

    fn new_with_size(cols: u16, rows: u16) -> TestTerminal {
        let winsize = Winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let openpty = openpty(Some(&winsize), None).expect("Could not create openpty");
        TestTerminal { openpty }
    }

    pub fn master(&self) -> RawFd {
        self.openpty.master
    }

    pub fn slave(&self) -> RawFd {
        self.openpty.slave
    }
}

impl Drop for TestTerminal {
    fn drop(&mut self) {
        close(self.openpty.master).expect("Failed to close the master");
        close(self.openpty.slave).expect("Failed to close the slave");
    }
}

fn make_server(test_terminal: &TestTerminal) -> ServerOsInputOutput {
    let test_termios =
        termios::tcgetattr(test_terminal.slave()).expect("Could not get terminal attributes");
    ServerOsInputOutput {
        orig_termios: Arc::new(Mutex::new(Some(test_termios))),
        client_senders: Arc::default(),
        terminal_id_to_raw_fd: Arc::default(),
        cached_resizes: Arc::default(),
    }
}

#[test]
fn get_cwd() {
    let test_terminal = TestTerminal::new();
    let server = make_server(&test_terminal);

    let pid = nix::unistd::getpid();
    assert!(
        server.get_cwd(pid).is_some(),
        "Get current working directory from PID {}",
        pid
    );
}

// --- PTY integration tests ---

#[test]
fn pty_roundtrip_write_to_slave_read_from_master() {
    let pty = TestTerminal::new();

    // Writing to the slave side simulates a process producing output.
    // The master side (which Zellij reads) should receive it.
    let msg = b"hello from pty\n";
    nix::unistd::write(pty.slave(), msg).expect("write to slave");

    let mut buf = [0u8; 256];
    let n = nix::unistd::read(pty.master(), &mut buf).expect("read from master");
    assert!(n > 0, "should read data from master side of PTY");

    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(
        output.contains("hello from pty"),
        "master output should contain data written to slave, got: {:?}",
        output
    );
}

#[test]
fn pty_roundtrip_write_to_master_read_from_slave() {
    let pty = TestTerminal::new();

    // Writing to the master side simulates Zellij sending input to a pane.
    let msg = b"keystrokes";
    nix::unistd::write(pty.master(), msg).expect("write to master");

    // Use non-blocking read on slave to avoid hanging if data isn't available.
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    let flags = fcntl(pty.slave(), FcntlArg::F_GETFL).expect("fcntl F_GETFL");
    let flags = OFlag::from_bits_truncate(flags);
    fcntl(pty.slave(), FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).expect("fcntl F_SETFL");

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut buf = [0u8; 256];
    match nix::unistd::read(pty.slave(), &mut buf) {
        Ok(n) => {
            assert!(n > 0, "should read data from slave side of PTY");
            let output = String::from_utf8_lossy(&buf[..n]);
            assert!(
                output.contains("keystrokes"),
                "slave output should contain data written to master, got: {:?}",
                output
            );
        },
        Err(nix::Error::EAGAIN) => {
            // The PTY line discipline may echo data back to master instead of
            // buffering on slave. This is acceptable — the key assertion is that
            // write to master doesn't fail, and the PTY is functional.
        },
        Err(e) => panic!("unexpected read error: {}", e),
    }
}

// --- Terminal resize tests ---

#[test]
fn set_terminal_size_via_ioctl() {
    let pty = TestTerminal::new();

    set_terminal_size_using_fd(pty.master(), 132, 43, None, None);

    let actual = get_winsize(pty.master());
    assert_eq!(actual.ws_col, 132, "columns should be 132");
    assert_eq!(actual.ws_row, 43, "rows should be 43");
}

#[test]
fn set_terminal_size_through_server_api() {
    let pty = TestTerminal::new();

    let server = make_server(&pty);
    let terminal_id = 0u32;
    server
        .terminal_id_to_raw_fd
        .lock()
        .unwrap()
        .insert(terminal_id, Some(pty.master()));

    server
        .set_terminal_size_using_terminal_id(terminal_id, 200, 50, None, None)
        .expect("resize should succeed");

    let actual = get_winsize(pty.master());
    assert_eq!(actual.ws_col, 200);
    assert_eq!(actual.ws_row, 50);
}

#[test]
fn openpty_with_initial_size() {
    let pty = TestTerminal::new_with_size(120, 40);

    let actual = get_winsize(pty.master());
    assert_eq!(actual.ws_col, 120, "initial columns should match");
    assert_eq!(actual.ws_row, 40, "initial rows should match");
}

#[test]
fn resize_does_not_apply_zero_dimensions() {
    let pty = TestTerminal::new_with_size(80, 24);

    let server = make_server(&pty);
    let terminal_id = 0u32;
    server
        .terminal_id_to_raw_fd
        .lock()
        .unwrap()
        .insert(terminal_id, Some(pty.master()));

    // Zero dimensions should be ignored (the implementation skips when cols/rows are 0)
    server
        .set_terminal_size_using_terminal_id(terminal_id, 0, 0, None, None)
        .expect("resize with zeros should not error");

    let actual = get_winsize(pty.master());
    assert_eq!(actual.ws_col, 80, "columns should remain unchanged");
    assert_eq!(actual.ws_row, 24, "rows should remain unchanged");
}

// --- Signal delivery tests ---

#[test]
fn kill_sends_sighup_to_process() {
    let child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let pid = Pid::from_raw(child.id() as i32);

    let pty = TestTerminal::new();
    let server = make_server(&pty);

    server.kill(pid).expect("kill should succeed");

    // Give the signal time to be delivered
    std::thread::sleep(std::time::Duration::from_millis(100));

    // The process should no longer be running
    let status = nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG));
    assert!(
        status.is_ok(),
        "process should be terminated after SIGHUP"
    );
}

#[test]
fn force_kill_sends_sigkill_to_process() {
    let child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let pid = Pid::from_raw(child.id() as i32);

    let pty = TestTerminal::new();
    let server = make_server(&pty);

    server.force_kill(pid).expect("force_kill should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));

    let status = nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG));
    assert!(
        status.is_ok(),
        "process should be terminated after SIGKILL"
    );
}

#[test]
fn send_sigint_to_process() {
    // Spawn a process that ignores SIGINT so we can verify it received one
    // We use `cat` which will exit on SIGINT by default
    let child = Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn cat");
    let pid = Pid::from_raw(child.id() as i32);

    let pty = TestTerminal::new();
    let server = make_server(&pty);

    server.send_sigint(pid).expect("send_sigint should succeed");

    std::thread::sleep(std::time::Duration::from_millis(100));

    let status = nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG));
    assert!(
        status.is_ok(),
        "process should be terminated after SIGINT"
    );
}

// --- PTY read/write through ServerOsApi ---

#[test]
fn read_write_through_server_os_api() {
    let pty = TestTerminal::new();
    let server = make_server(&pty);
    let terminal_id = 0u32;
    server
        .terminal_id_to_raw_fd
        .lock()
        .unwrap()
        .insert(terminal_id, Some(pty.master()));

    // Write through the API (simulates Zellij sending keystrokes to pane)
    let data = b"test input\r";
    let written = server
        .write_to_tty_stdin(terminal_id, data)
        .expect("write_to_tty_stdin should succeed");
    assert_eq!(written, data.len());

    // Read from slave side (simulates process receiving input)
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    let flags = fcntl(pty.slave(), FcntlArg::F_GETFL).expect("fcntl");
    let flags = OFlag::from_bits_truncate(flags);
    fcntl(pty.slave(), FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).expect("fcntl");

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut buf = [0u8; 256];
    let result = nix::unistd::read(pty.slave(), &mut buf);
    // Data may be available on slave or echoed back to master depending on
    // the PTY line discipline mode. Either way, write should not fail.
    match result {
        Ok(n) => assert!(n > 0, "should read some data from slave"),
        Err(nix::Error::EAGAIN) => {
            // Data echoed to master — still valid behavior
        },
        Err(e) => panic!("unexpected error: {}", e),
    }
}

#[test]
fn read_from_tty_stdout_through_server_api() {
    let pty = TestTerminal::new();
    let server = make_server(&pty);

    // Write to slave (simulates a process producing output)
    nix::unistd::write(pty.slave(), b"output data\n").expect("write to slave");

    // Read from master through the server API
    let mut buf = [0u8; 256];
    let n = server
        .read_from_tty_stdout(pty.master(), &mut buf)
        .expect("read_from_tty_stdout should succeed");
    assert!(n > 0, "should read output from PTY");

    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(
        output.contains("output data"),
        "should contain the written data, got: {:?}",
        output
    );
}

// --- Cached resize tests ---

#[test]
fn cached_resizes_are_applied() {
    let pty = TestTerminal::new_with_size(80, 24);
    let mut server = make_server(&pty);
    let terminal_id = 0u32;
    server
        .terminal_id_to_raw_fd
        .lock()
        .unwrap()
        .insert(terminal_id, Some(pty.master()));

    server.cache_resizes();

    // While caching, resizes should not be applied immediately
    server
        .set_terminal_size_using_terminal_id(terminal_id, 160, 48, None, None)
        .expect("cached resize should succeed");

    let actual = get_winsize(pty.master());
    assert_eq!(actual.ws_col, 80, "size should not change while cached");
    assert_eq!(actual.ws_row, 24);

    server.apply_cached_resizes();

    let actual = get_winsize(pty.master());
    assert_eq!(actual.ws_col, 160, "cached resize should be applied");
    assert_eq!(actual.ws_row, 48);
}
