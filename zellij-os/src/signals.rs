use async_trait::async_trait;
use std::io;

/// Events that can be received from OS signals.
pub enum SignalEvent {
    Resize,
    Quit,
}

/// Trait for async signal listening, allowing for testable implementations.
#[async_trait]
pub trait AsyncSignals: Send {
    async fn recv(&mut self) -> Option<SignalEvent>;
}

/// Async signal listener that maps OS signals to `SignalEvent` variants.
#[cfg(unix)]
pub struct AsyncSignalListener {
    sigwinch: tokio::signal::unix::Signal,
    sigterm: tokio::signal::unix::Signal,
    sigint: tokio::signal::unix::Signal,
    sigquit: tokio::signal::unix::Signal,
    sighup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl AsyncSignalListener {
    pub fn new() -> io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            sigwinch: signal(SignalKind::window_change())?,
            sigterm: signal(SignalKind::terminate())?,
            sigint: signal(SignalKind::interrupt())?,
            sigquit: signal(SignalKind::quit())?,
            sighup: signal(SignalKind::hangup())?,
        })
    }
}

#[cfg(unix)]
#[async_trait]
impl AsyncSignals for AsyncSignalListener {
    async fn recv(&mut self) -> Option<SignalEvent> {
        tokio::select! {
            result = self.sigwinch.recv() => result.map(|_| SignalEvent::Resize),
            result = self.sigterm.recv() => result.map(|_| SignalEvent::Quit),
            result = self.sigint.recv() => result.map(|_| SignalEvent::Quit),
            result = self.sigquit.recv() => result.map(|_| SignalEvent::Quit),
            result = self.sighup.recv() => result.map(|_| SignalEvent::Quit),
        }
    }
}

#[cfg(not(unix))]
pub struct AsyncSignalListener;

#[cfg(not(unix))]
impl AsyncSignalListener {
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "AsyncSignalListener is not supported on this platform",
        ))
    }
}

#[cfg(not(unix))]
#[async_trait]
impl AsyncSignals for AsyncSignalListener {
    async fn recv(&mut self) -> Option<SignalEvent> {
        None
    }
}

/// Blocking signal iterator that maps OS signals to `SignalEvent` variants.
/// Used by `handle_signals()` on a dedicated thread.
#[cfg(unix)]
pub struct BlockingSignalIterator {
    signals: signal_hook::iterator::Signals,
}

#[cfg(unix)]
impl BlockingSignalIterator {
    pub fn new() -> io::Result<Self> {
        use signal_hook::consts::signal::*;
        let signals =
            signal_hook::iterator::Signals::new([SIGWINCH, SIGTERM, SIGINT, SIGQUIT, SIGHUP])?;
        Ok(Self { signals })
    }
}

#[cfg(unix)]
impl Iterator for BlockingSignalIterator {
    type Item = SignalEvent;

    fn next(&mut self) -> Option<SignalEvent> {
        use signal_hook::consts::signal::*;
        for signal in self.signals.forever() {
            match signal {
                SIGWINCH => return Some(SignalEvent::Resize),
                SIGTERM | SIGINT | SIGQUIT | SIGHUP => return Some(SignalEvent::Quit),
                _ => {},
            }
        }
        None
    }
}

#[cfg(not(unix))]
pub struct BlockingSignalIterator;

#[cfg(not(unix))]
impl BlockingSignalIterator {
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "BlockingSignalIterator is not supported on this platform",
        ))
    }
}

#[cfg(not(unix))]
impl Iterator for BlockingSignalIterator {
    type Item = SignalEvent;

    fn next(&mut self) -> Option<SignalEvent> {
        None
    }
}
