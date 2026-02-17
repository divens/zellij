use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize as PortablePtySize};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Terminal size in rows and columns, with optional pixel dimensions.
#[derive(Debug, Clone, Copy)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// A handle to the master side of a PTY.
///
/// Wraps `portable-pty`'s `MasterPty`, providing read/write/resize operations.
pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Option<Box<dyn Write + Send>>,
}

impl PtyHandle {
    /// Resize the PTY to the given dimensions.
    pub fn resize(&self, size: PtySize) -> Result<()> {
        self.master
            .resize(PortablePtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: size.pixel_width,
                pixel_height: size.pixel_height,
            })
            .map_err(|e| anyhow::anyhow!("{}", e))
            .context("failed to resize PTY")
    }

    /// Get the current size of the PTY.
    pub fn get_size(&self) -> Result<PtySize> {
        let size = self
            .master
            .get_size()
            .map_err(|e| anyhow::anyhow!("{}", e))
            .context("failed to get PTY size")?;
        Ok(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        })
    }

    /// Clone the reader end of the PTY. Can be called multiple times.
    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master
            .try_clone_reader()
            .map_err(|e| anyhow::anyhow!("{}", e))
            .context("failed to clone PTY reader")
    }

    /// Write bytes to the PTY (i.e., send input to the child process).
    pub fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let writer = self
            .writer
            .as_mut()
            .context("PTY writer has already been taken")?;
        writer.write(buf).context("failed to write to PTY")
    }

    /// Flush the PTY writer, ensuring all buffered data is sent.
    pub fn drain(&mut self) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .context("PTY writer has already been taken")?;
        writer.flush().context("failed to drain PTY")
    }
}

/// Result of spawning a command in a PTY.
pub struct SpawnResult {
    /// Handle to the master side of the PTY.
    pub pty: PtyHandle,
    /// Process ID of the spawned child, if available.
    pub child_pid: Option<u32>,
}

/// Spawn a command in a new PTY.
///
/// Creates a new PTY pair, spawns the given command in the slave side,
/// and returns a handle to the master side along with the child's PID.
///
/// An exit-monitoring thread is started that calls `quit_cb` when the child exits.
pub fn spawn_in_pty(
    cmd: PathBuf,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    size: PtySize,
    quit_cb: Box<dyn FnOnce(Option<i32>) + Send>,
) -> Result<SpawnResult> {
    log::info!("spawn_in_pty: opening PTY for {:?}", cmd);
    let pty_system = native_pty_system();

    let pty_size = PortablePtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    };

    let pair = pty_system
        .openpty(pty_size)
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("failed to open PTY")?;

    let mut command = CommandBuilder::new(&cmd);
    command.args(&args);

    if let Some(cwd) = cwd {
        if cwd.exists() && cwd.is_dir() {
            command.cwd(cwd);
        } else {
            log::error!(
                "Failed to set CWD for new pane. '{}' does not exist or is not a folder",
                cwd.display()
            );
        }
    }

    for (key, value) in &env {
        command.env(key, value);
    }

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| anyhow::anyhow!("{}", e))
        .with_context(|| format!("failed to spawn command '{}'", cmd.to_string_lossy()))?;

    let child_pid = child.process_id();

    // Drop the slave — the child owns its end now
    drop(pair.slave);

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("failed to take PTY writer")?;

    // Spawn exit-monitoring thread
    std::thread::spawn(move || {
        let exit_status = match child.wait() {
            Ok(status) => {
                if status.success() {
                    Some(0)
                } else {
                    Some(status.exit_code() as i32)
                }
            },
            Err(e) => {
                log::error!("Error waiting for child process: {}", e);
                None
            },
        };
        quit_cb(exit_status);
    });

    Ok(SpawnResult {
        pty: PtyHandle {
            master: pair.master,
            writer: Some(writer),
        },
        child_pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Platform-specific echo command.
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

    #[test]
    fn spawn_and_read_output() {
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
        let mut buf = [0u8; 1024];
        let start = std::time::Instant::now();
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
        let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
    }
}
