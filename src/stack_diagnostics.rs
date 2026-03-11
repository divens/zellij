//! Stack overflow diagnostics for debug builds.
//!
//! Provides:
//! - A stack overflow handler that prints a backtrace (VEH on Windows,
//!   signal handler with alternate stack on Unix)
//! - Stack probe function that reports remaining stack at key points
//!
//! All calls are gated behind `#[cfg(debug_assertions)]` at call sites.

/// Install the stack overflow backtrace handler.
///
/// On Unix, uses `backtrace-on-stack-overflow` (SIGSEGV with sigaltstack).
/// On Windows, uses a Vectored Exception Handler for EXCEPTION_STACK_OVERFLOW.
pub fn enable_overflow_handler() {
    #[cfg(unix)]
    unsafe {
        backtrace_on_stack_overflow::enable();
    }

    #[cfg(windows)]
    unsafe {
        windows_overflow_handler::enable();
    }
}

/// Print the remaining stack space at the current call site.
pub fn probe(label: &str) {
    match stacker::remaining_stack() {
        Some(remaining) => {
            eprintln!(
                "[stack-probe] {label}: {remaining} bytes remaining ({} KB)",
                remaining / 1024
            );
        },
        None => {
            eprintln!("[stack-probe] {label}: remaining_stack() unavailable");
        },
    }
}

#[cfg(windows)]
mod windows_overflow_handler {
    use std::backtrace::Backtrace;

    use windows_sys::Win32::{
        Foundation::STATUS_STACK_OVERFLOW,
        System::{
            Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS},
            Threading::SetThreadStackGuarantee,
        },
    };

    /// Tell the OS to keep searching for other handlers.
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    unsafe extern "system" fn handler(exception_info: *mut EXCEPTION_POINTERS) -> i32 {
        if exception_info.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let record = unsafe { (*exception_info).ExceptionRecord };
        if record.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let code = unsafe { (*record).ExceptionCode };
        if code == STATUS_STACK_OVERFLOW {
            eprintln!("\n=== STACK OVERFLOW DETECTED ===");
            eprintln!(
                "Thread: {:?}",
                std::thread::current().name().unwrap_or("<unnamed>")
            );

            // Best-effort backtrace capture. SetThreadStackGuarantee reserves
            // extra space for the handler, but a very deep overflow may still
            // leave too little room.
            let bt = Backtrace::force_capture();
            eprintln!("{bt}");
            eprintln!("=== END STACK OVERFLOW ===\n");

            std::process::abort();
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Install a Vectored Exception Handler that catches stack overflows.
    ///
    /// Also calls `SetThreadStackGuarantee` to reserve 64 KB of stack space
    /// for the handler, giving it room to capture a backtrace.
    pub unsafe fn enable() {
        // Reserve extra stack for the exception handler.
        let mut guarantee: u32 = 64 * 1024;
        unsafe {
            SetThreadStackGuarantee(&mut guarantee);
            AddVectoredExceptionHandler(1, Some(handler));
        }
    }
}
