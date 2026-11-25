//! The plugin API trace: a record of every plugin command, pushed in
//! batches to plugins subscribed to `PluginApiTrace`.
//!
//! It runs only while some plugin subscribes; otherwise a command pays one
//! relaxed atomic load. Records go into a bounded buffer and are flushed to
//! the subscribers every `FLUSH_EVERY`, or sooner once `FLUSH_AT` are waiting.
//! One `ApiTrace` per session, shared by every plugin's environment.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zellij_utils::data::{CommandType, PermissionType};
use zellij_utils::diagnostics::{ApiCommandOutcome, ApiTraceBatch, ApiTraceRecord};

use super::{PluginId, PluginInstruction};
use crate::thread_bus::ThreadSenders;

/// Records kept while waiting for a flush; older ones are dropped, and the
/// next batch says how many.
const CAPACITY: usize = 4096;
/// A flush is asked for as soon as this many records are waiting.
const FLUSH_AT: usize = 256;
/// Otherwise records wait at most this long.
pub const FLUSH_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
struct Buffer {
    records: VecDeque<ApiTraceRecord>,
    dropped: u64,
}

#[derive(Debug)]
struct Inner {
    active: AtomicBool,
    /// Times are taken as `Instant`s and turned into Unix time from this
    /// pair, so recording never asks the OS for the wall clock.
    started_at: Instant,
    started_at_unix_us: u64,
    next_seq: AtomicU64,
    buffer: Mutex<Buffer>,
    flush_requested: AtomicBool,
    flusher_running: AtomicBool,
}

#[derive(Debug, Clone)]
pub struct ApiTrace {
    inner: Arc<Inner>,
}

impl Default for ApiTrace {
    fn default() -> Self {
        let started_at_unix_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_micros() as u64)
            .unwrap_or(0);
        ApiTrace {
            inner: Arc::new(Inner {
                active: AtomicBool::new(false),
                started_at: Instant::now(),
                started_at_unix_us,
                next_seq: AtomicU64::new(0),
                buffer: Mutex::new(Buffer::default()),
                flush_requested: AtomicBool::new(false),
                flusher_running: AtomicBool::new(false),
            }),
        }
    }
}

/// What happened to a command, for its record.
pub struct CommandOutcome {
    pub outcome: ApiCommandOutcome,
    pub permission: Option<PermissionType>,
    pub message: Option<String>,
}

impl ApiTrace {
    /// The only cost of the trace while nobody subscribes.
    pub fn is_active(&self) -> bool {
        self.inner.active.load(Ordering::Relaxed)
    }

    /// Turns the trace on or off. Turning it off drops what is waiting.
    /// Returns whether it changed.
    pub fn set_active(&self, active: bool) -> bool {
        let was_active = self.inner.active.swap(active, Ordering::Relaxed);
        if was_active && !active {
            if let Ok(mut buffer) = self.inner.buffer.lock() {
                *buffer = Buffer::default();
            }
        }
        was_active != active
    }

    pub fn record(
        &self,
        senders: &ThreadSenders,
        plugin_id: PluginId,
        command: Option<CommandType>,
        outcome: CommandOutcome,
        started: Instant,
    ) {
        let duration_us = started.elapsed().as_micros() as u64;
        let since_start = started.saturating_duration_since(self.inner.started_at);
        let record = ApiTraceRecord {
            seq: self.inner.next_seq.fetch_add(1, Ordering::Relaxed),
            unix_time_us: self.inner.started_at_unix_us + since_start.as_micros() as u64,
            plugin_id,
            command: command.map(|command| format!("{command:?}")),
            outcome: outcome.outcome,
            permission: outcome.permission.map(|permission| permission.to_string()),
            message: outcome.message,
            duration_us,
        };
        let waiting = {
            let Ok(mut buffer) = self.inner.buffer.lock() else {
                return;
            };
            buffer.records.push_back(record);
            if buffer.records.len() > CAPACITY {
                buffer.records.pop_front();
                buffer.dropped += 1;
            }
            buffer.records.len()
        };
        if waiting >= FLUSH_AT && !self.inner.flush_requested.swap(true, Ordering::Relaxed) {
            let _ = senders.send_to_plugin(PluginInstruction::FlushApiTrace);
        }
    }

    /// Everything waiting, or `None` if there is nothing to send.
    pub fn take_batch(&self) -> Option<ApiTraceBatch> {
        self.inner.flush_requested.store(false, Ordering::Relaxed);
        let mut buffer = self.inner.buffer.lock().ok()?;
        if buffer.records.is_empty() && buffer.dropped == 0 {
            return None;
        }
        let buffer = std::mem::take(&mut *buffer);
        Some(ApiTraceBatch {
            dropped: buffer.dropped,
            records: buffer.records.into(),
        })
    }

    /// Asks for a flush every `FLUSH_EVERY` while the trace is on. At most
    /// one runs at a time; it stops on the first tick after the trace goes
    /// off.
    pub fn start_flusher(&self, senders: ThreadSenders) {
        if self.inner.flusher_running.swap(true, Ordering::Relaxed) {
            return;
        }
        let trace = self.clone();
        crate::global_async_runtime::get_tokio_runtime().spawn(async move {
            loop {
                tokio::time::sleep(FLUSH_EVERY).await;
                if !trace.is_active() {
                    break;
                }
                let _ = senders.send_to_plugin(PluginInstruction::FlushApiTrace);
            }
            trace.inner.flusher_running.store(false, Ordering::Relaxed);
            // Turned back on between the last check and now: keep flushing.
            if trace.is_active() {
                trace.start_flusher(senders);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> CommandOutcome {
        CommandOutcome {
            outcome: ApiCommandOutcome::Ok,
            permission: None,
            message: None,
        }
    }

    #[test]
    fn records_are_numbered_and_timed_from_one_clock() {
        let trace = ApiTrace::default();
        let senders = ThreadSenders::default();
        let first = Instant::now();
        trace.record(&senders, 3, Some(CommandType::Subscribe), ok(), first);
        trace.record(
            &senders,
            4,
            None,
            CommandOutcome {
                outcome: ApiCommandOutcome::Denied,
                permission: Some(PermissionType::RunCommands),
                message: None,
            },
            first + Duration::from_millis(5),
        );
        let batch = trace.take_batch().unwrap();
        assert_eq!(batch.dropped, 0);
        let seqs: Vec<u64> = batch.records.iter().map(|record| record.seq).collect();
        assert_eq!(seqs, vec![0, 1]);
        assert_eq!(batch.records[0].command.as_deref(), Some("Subscribe"));
        assert_eq!(batch.records[1].permission.as_deref(), Some("RunCommands"));
        assert_eq!(
            batch.records[1].unix_time_us - batch.records[0].unix_time_us,
            5_000,
            "times come from the same clock"
        );
        assert!(trace.take_batch().is_none(), "a batch empties the buffer");
    }

    #[test]
    fn a_full_buffer_drops_the_oldest_and_says_how_many() {
        let trace = ApiTrace::default();
        let senders = ThreadSenders::default();
        for _ in 0..CAPACITY + 7 {
            trace.record(
                &senders,
                1,
                Some(CommandType::Subscribe),
                ok(),
                Instant::now(),
            );
        }
        let batch = trace.take_batch().unwrap();
        assert_eq!(batch.dropped, 7);
        assert_eq!(batch.records.len(), CAPACITY);
        assert_eq!(batch.records[0].seq, 7, "the oldest went first");
    }

    #[test]
    fn turning_the_trace_off_drops_what_is_waiting() {
        let trace = ApiTrace::default();
        assert!(trace.set_active(true));
        assert!(!trace.set_active(true), "no change");
        trace.record(
            &ThreadSenders::default(),
            1,
            Some(CommandType::Subscribe),
            ok(),
            Instant::now(),
        );
        assert!(trace.set_active(false));
        assert!(trace.take_batch().is_none());
    }
}
