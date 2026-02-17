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
#[cfg(windows)]
pub fn signal_process(pid: u32, signal: ProcessSignal) -> Result<()> {
    use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_C_EVENT};

    match signal {
        ProcessSignal::Interrupt => {
            let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, pid) };
            if ok != 0 {
                Ok(())
            } else {
                // Fallback: if GenerateConsoleCtrlEvent fails (e.g. different
                // process group), terminate the process instead.
                terminate_process(pid)
                    .with_context(|| format!("failed to send Interrupt to pid {}", pid))
            }
        },
        ProcessSignal::Kill | ProcessSignal::HangUp => terminate_process(pid)
            .with_context(|| format!("failed to send {:?} to pid {}", signal, pid)),
    }
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> std::result::Result<(), std::io::Error> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ok = TerminateProcess(handle, 1);
        CloseHandle(handle);
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Send a signal to a process by PID.
#[cfg(not(any(unix, windows)))]
pub fn signal_process(pid: u32, signal: ProcessSignal) -> Result<()> {
    anyhow::bail!(
        "signal_process not implemented on this platform (pid={}, signal={:?})",
        pid,
        signal
    )
}
