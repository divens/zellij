//! Minimal ConPTY reproduction to diagnose 0xc0000142 (STATUS_DLL_INIT_FAILED).
//!
//! Run on Windows:
//!   cargo run -p zellij-os --example conpty_repro
//!
//! This spawns a simple command via portable-pty's ConPTY backend and reports
//! whether it succeeds or fails with 0xc0000142.
//!
//! Test matrix (edit COMMANDS below to try different programs):
//! - cmd.exe /c echo hello    (console app, most common)
//! - ping -n 1 127.0.0.1     (console app)
//! - powershell -c "echo hi"  (console host)

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;

fn main() {
    let commands: Vec<(&str, Vec<&str>)> = vec![
        ("cmd.exe", vec!["/c", "echo", "hello from cmd"]),
        ("ping", vec!["-n", "1", "127.0.0.1"]),
    ];

    // Add platform-specific commands
    #[cfg(unix)]
    let commands: Vec<(&str, Vec<&str>)> = vec![
        ("/bin/echo", vec!["hello from echo"]),
        ("/bin/sh", vec!["-c", "echo hello from sh"]),
    ];

    let pty_system = native_pty_system();

    for (program, args) in &commands {
        println!("\n{}", "=".repeat(60));
        println!("Testing: {} {}", program, args.join(" "));
        println!("{}", "=".repeat(60));

        // Open a new PTY pair
        let pair = match pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(pair) => pair,
            Err(e) => {
                println!("  FAIL: openpty() failed: {:#}", e);
                continue;
            },
        };

        // Build the command
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }

        // Clone reader before spawning (for reading output)
        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                println!("  FAIL: try_clone_reader() failed: {:#}", e);
                continue;
            },
        };

        // Spawn the command
        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(child) => {
                println!("  OK: spawn_command() succeeded");
                if let Some(pid) = child.process_id() {
                    println!("  PID: {}", pid);
                }
                child
            },
            Err(e) => {
                println!("  FAIL: spawn_command() failed: {:#}", e);
                // Check for 0xc0000142
                let err_str = format!("{:#}", e);
                if err_str.contains("c0000142") || err_str.contains("3221225794") {
                    println!("  >>> This is STATUS_DLL_INIT_FAILED (0xc0000142)");
                    println!("  >>> ConPTY child process could not initialize its DLLs");
                }
                continue;
            },
        };

        // Drop the slave so the child owns it
        drop(pair.slave);

        // Read output with timeout
        let mut output = String::new();
        let mut buf = [0u8; 4096];
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(5);

        while start.elapsed() < timeout {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    output.push_str(&String::from_utf8_lossy(&buf[..n]));
                    // Stop early if we got some output
                    if output.contains("hello")
                        || output.contains("Reply from")
                        || output.len() > 200
                    {
                        break;
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                },
                Err(e) => {
                    println!("  Read error: {}", e);
                    break;
                },
            }
        }

        if !output.is_empty() {
            println!(
                "  Output (first 200 chars): {:?}",
                &output[..output.len().min(200)]
            );
        } else {
            println!("  (no output received within timeout)");
        }

        // Check process status
        match child.try_wait() {
            Ok(Some(status)) => {
                println!("  Exit code: {}", status.exit_code());
                if !status.success() {
                    println!("  >>> Process exited with non-zero status");
                }
            },
            Ok(None) => {
                println!("  Process still running, killing...");
                let _ = child.kill();
                let _ = child.wait();
            },
            Err(e) => println!("  try_wait error: {}", e),
        }
    }

    println!("\nDone.");
}
