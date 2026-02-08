use anyhow::{Context, Result};

/// Signals that can be sent to a process.
#[derive(Debug, Clone, Copy)]
pub enum ProcessSignal {
    /// SIGHUP on Unix
    HangUp,
    /// SIGKILL on Unix
    Kill,
    /// SIGINT on Unix
    Interrupt,
}

/// Send a signal to a process by PID.
#[cfg(unix)]
pub fn signal_process(pid: u32, signal: ProcessSignal) -> Result<()> {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    let nix_signal = match signal {
        ProcessSignal::HangUp => Signal::SIGHUP,
        ProcessSignal::Kill => Signal::SIGKILL,
        ProcessSignal::Interrupt => Signal::SIGINT,
    };

    signal::kill(Pid::from_raw(pid as i32), Some(nix_signal))
        .with_context(|| format!("failed to send {:?} to pid {}", signal, pid))
}

/// Send a signal to a process by PID.
#[cfg(not(unix))]
pub fn signal_process(pid: u32, signal: ProcessSignal) -> Result<()> {
    anyhow::bail!(
        "signal_process not implemented on this platform (pid={}, signal={:?})",
        pid,
        signal
    )
}
