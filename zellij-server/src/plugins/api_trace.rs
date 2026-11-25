//! The plugin API trace: a record of every plugin command, pushed in
//! batches to plugins subscribed to `PluginApiTrace`.
//!
//! It runs only while some plugin subscribes; otherwise a command pays one
//! relaxed atomic load. Records go into a bounded buffer and are flushed to
//! the subscribers every `FLUSH_EVERY`, or sooner once `FLUSH_AT` are waiting.
//! Alongside the commands, it counts the events each plugin receives and
//! the time it spends on them, per batch.
//! One `ApiTrace` per session, shared by every plugin's environment.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use std::path::Path;

use zellij_utils::data::{CommandType, EventType, HttpVerb, PermissionType, PluginCommand};
use zellij_utils::diagnostics::{
    ApiCommandOutcome, ApiTraceBatch, ApiTraceRecord, EventCount, PluginEventLoad,
};

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
    loads: HashMap<PluginId, Load>,
    /// The current batch's window started here: when the trace came on, or
    /// at the previous batch.
    window_started: Option<Instant>,
}

/// What one plugin received during the current window.
#[derive(Debug, Default)]
struct Load {
    events: HashMap<EventType, u32>,
    update: Duration,
    renders: u32,
    render: Duration,
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
    /// From `summarize`.
    pub summary: Option<String>,
}

/// The longest summary kept.
const SUMMARY_MAX_CHARS: usize = 100;

/// A short description of a command, for the few whose arguments are the
/// point. Everything else gets none.
///
/// Redaction is the point of this function: it never includes command
/// arguments (they may hold secrets), URL paths, queries, headers or
/// bodies, written bytes or keys, or message payloads, and it names files
/// and programs without their directories. Add a command here only with the
/// same care.
pub fn summarize(command: &PluginCommand) -> Option<String> {
    let summary = match command {
        PluginCommand::RunCommand(argv, ..) | PluginCommand::ExecCmd(argv) => program(argv),
        PluginCommand::WebRequest(url, verb, ..) => format!("{} {}", verb_name(verb), host(url)),
        PluginCommand::OpenFile(file, _)
        | PluginCommand::OpenFileFloating(file, ..)
        | PluginCommand::OpenFileInPlace(file, _)
        | PluginCommand::OpenFileNearPlugin(file, _)
        | PluginCommand::OpenFileFloatingNearPlugin(file, ..)
        | PluginCommand::OpenFileInPlaceOfPlugin(file, ..) => file_name(&file.path),
        PluginCommand::ScanHostFolder(path) => file_name(path),
        PluginCommand::Write(bytes) | PluginCommand::WriteToPaneId(bytes, _) => {
            byte_count(bytes.len())
        },
        PluginCommand::WriteChars(chars) | PluginCommand::WriteCharsToPaneId(chars, _) => {
            byte_count(chars.len())
        },
        PluginCommand::WriteToSessionPane { bytes, .. } => byte_count(bytes.len()),
        PluginCommand::SendKeyToSessionPane { .. } => "1 key".to_string(),
        PluginCommand::MessageToPlugin(message) => {
            let target = match (&message.destination_plugin_id, &message.plugin_url) {
                (Some(plugin_id), _) => format!("plugin_{plugin_id}"),
                (None, Some(url)) => plugin_name(url),
                (None, None) => "any plugin".to_string(),
            };
            let payload = message.message_payload.as_deref().map_or(0, str::len);
            format!(
                "to {target} · {} · {}",
                message.message_name,
                byte_count(payload)
            )
        },
        PluginCommand::PostMessageTo(message) => format!(
            "to worker {} · {} · {}",
            message.worker_name.as_deref().unwrap_or("?"),
            message.name,
            byte_count(message.payload.len())
        ),
        PluginCommand::PostMessageToPlugin(message) => {
            format!("{} · {}", message.name, byte_count(message.payload.len()))
        },
        PluginCommand::CliPipeOutput(pipe_name, output) => {
            format!("{pipe_name} · {}", byte_count(output.len()))
        },
        PluginCommand::Subscribe(events) | PluginCommand::Unsubscribe(events) => {
            names(events.iter().map(|event| format!("{event:?}")))
        },
        PluginCommand::RequestPluginPermissions(permissions) => {
            names(permissions.iter().map(|permission| permission.to_string()))
        },
        _ => return None,
    };
    Some(cut(&summary, SUMMARY_MAX_CHARS))
}

/// `git (+3 args)`: the program without its directory, and how many
/// arguments, never which.
fn program(argv: &[String]) -> String {
    let Some(program) = argv.first() else {
        return "no program".to_string();
    };
    let name = file_name(Path::new(program));
    match argv.len() - 1 {
        0 => name,
        1 => format!("{name} (+1 arg)"),
        args => format!("{name} (+{args} args)"),
    }
}

fn verb_name(verb: &HttpVerb) -> &'static str {
    match verb {
        HttpVerb::Get => "GET",
        HttpVerb::Post => "POST",
        HttpVerb::Put => "PUT",
        HttpVerb::Delete => "DELETE",
    }
}

/// The URL's host alone: no scheme, user, path, query or fragment.
fn host(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(url) => url
            .host_str()
            .map(str::to_string)
            .unwrap_or_else(|| "(no host)".to_string()),
        Err(_) => "(unreadable URL)".to_string(),
    }
}

/// The last part of a path, on either platform's separators.
fn file_name(path: &Path) -> String {
    let text = path.to_string_lossy();
    let name = text
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("");
    if name.is_empty() {
        "(no name)".to_string()
    } else {
        name.to_string()
    }
}

/// A plugin URL by its name: `zellij:tab-bar` stays, a file or URL keeps
/// its last part.
fn plugin_name(url: &str) -> String {
    if url.starts_with("zellij:") {
        return url.to_string();
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    file_name(Path::new(path.strip_prefix("file:").unwrap_or(path)))
}

fn byte_count(n: usize) -> String {
    if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{n} bytes")
    }
}

/// Sorted, and at most six before `+N more`.
fn names(names: impl Iterator<Item = String>) -> String {
    let mut names: Vec<String> = names.collect();
    names.sort();
    const SHOWN: usize = 6;
    let more = names.len().saturating_sub(SHOWN);
    names.truncate(SHOWN);
    let mut text = names.join(", ");
    if more > 0 {
        text.push_str(&format!(" +{more} more"));
    }
    text
}

/// Cuts `text` to `max` characters, ending in `…` when it was longer.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
        if was_active != active {
            if let Ok(mut buffer) = self.inner.buffer.lock() {
                *buffer = Buffer {
                    window_started: active.then(Instant::now),
                    ..Buffer::default()
                };
            }
        }
        was_active != active
    }

    /// Whether a plugin with these subscriptions has its events counted:
    /// while the trace is on, unless it subscribes to the trace itself.
    pub fn counts_events_of(&self, subscriptions: &HashSet<EventType>) -> bool {
        self.is_active() && !subscriptions.contains(&EventType::PluginApiTrace)
    }

    /// An event delivered to a plugin, and the time its `update` took.
    pub fn record_event(&self, plugin_id: PluginId, event_type: EventType, update: Duration) {
        if let Ok(mut buffer) = self.inner.buffer.lock() {
            let load = buffer.loads.entry(plugin_id).or_default();
            *load.events.entry(event_type).or_default() += 1;
            load.update += update;
        }
    }

    /// A render that events led to, and the time it took.
    pub fn record_render(&self, plugin_id: PluginId, render: Duration) {
        if let Ok(mut buffer) = self.inner.buffer.lock() {
            let load = buffer.loads.entry(plugin_id).or_default();
            load.renders += 1;
            load.render += render;
        }
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
            summary: outcome.summary,
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
    /// Nothing to send keeps the window open, so the next batch's window
    /// covers all the time its counts were gathered in.
    pub fn take_batch(&self) -> Option<ApiTraceBatch> {
        self.inner.flush_requested.store(false, Ordering::Relaxed);
        let mut buffer = self.inner.buffer.lock().ok()?;
        if buffer.records.is_empty() && buffer.dropped == 0 && buffer.loads.is_empty() {
            return None;
        }
        let now = Instant::now();
        let window_started = buffer.window_started.replace(now).unwrap_or(now);
        let records = std::mem::take(&mut buffer.records);
        let dropped = std::mem::take(&mut buffer.dropped);
        let loads = std::mem::take(&mut buffer.loads);
        let mut events: Vec<PluginEventLoad> = loads
            .into_iter()
            .map(|(plugin_id, load)| {
                let mut counts: Vec<EventCount> = load
                    .events
                    .into_iter()
                    .map(|(event_type, count)| EventCount {
                        event: format!("{event_type:?}"),
                        count,
                    })
                    .collect();
                counts.sort_by(|a, b| a.event.cmp(&b.event));
                PluginEventLoad {
                    plugin_id,
                    events: counts,
                    update_time_us: load.update.as_micros() as u64,
                    renders: load.renders,
                    render_time_us: load.render.as_micros() as u64,
                }
            })
            .collect();
        events.sort_by_key(|load| load.plugin_id);
        Some(ApiTraceBatch {
            dropped,
            window_us: now.saturating_duration_since(window_started).as_micros() as u64,
            records: records.into(),
            events,
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
            summary: None,
        }
    }

    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use zellij_utils::data::{FileToOpen, MessageToPlugin, PluginMessage};

    #[test]
    fn a_command_is_summarized_without_its_arguments() {
        let run = PluginCommand::RunCommand(
            vec![
                "/usr/bin/git".to_string(),
                "push".to_string(),
                "--token=hunter2".to_string(),
                "origin".to_string(),
            ],
            BTreeMap::from([("SECRET".to_string(), "x".to_string())]),
            PathBuf::from("/home/me/secret-project"),
            BTreeMap::new(),
        );
        let summary = summarize(&run).unwrap();
        assert_eq!(summary, "git (+3 args)");
        for secret in [
            "hunter2",
            "push",
            "origin",
            "SECRET",
            "secret-project",
            "/usr/bin",
        ] {
            assert!(
                !summary.contains(secret),
                "{secret} leaked into {summary:?}"
            );
        }
        assert_eq!(
            summarize(&PluginCommand::ExecCmd(vec!["ls".to_string()])).as_deref(),
            Some("ls")
        );
        assert_eq!(
            summarize(&PluginCommand::ExecCmd(vec![])).as_deref(),
            Some("no program")
        );
    }

    #[test]
    fn a_web_request_shows_only_its_method_and_host() {
        let request = PluginCommand::WebRequest(
            "https://user:pass@api.github.com/repos/me/private?token=abc#frag".to_string(),
            HttpVerb::Post,
            BTreeMap::from([("Authorization".to_string(), "Bearer abc".to_string())]),
            b"secret body".to_vec(),
            BTreeMap::new(),
        );
        assert_eq!(summarize(&request).as_deref(), Some("POST api.github.com"));
        let unreadable = PluginCommand::WebRequest(
            "not a url".to_string(),
            HttpVerb::Get,
            BTreeMap::new(),
            vec![],
            BTreeMap::new(),
        );
        assert_eq!(
            summarize(&unreadable).as_deref(),
            Some("GET (unreadable URL)")
        );
    }

    #[test]
    fn files_are_named_without_their_directories() {
        let file = |path: &str| FileToOpen {
            path: PathBuf::from(path),
            ..Default::default()
        };
        assert_eq!(
            summarize(&PluginCommand::OpenFile(
                file("/home/me/notes.md"),
                BTreeMap::new()
            ))
            .as_deref(),
            Some("notes.md")
        );
        assert_eq!(
            summarize(&PluginCommand::OpenFileInPlace(
                file(r"C:\Users\me\notes.md"),
                BTreeMap::new()
            ))
            .as_deref(),
            Some("notes.md")
        );
        assert_eq!(
            summarize(&PluginCommand::ScanHostFolder(PathBuf::from(
                "/home/me/src/"
            )))
            .as_deref(),
            Some("src")
        );
    }

    #[test]
    fn writes_and_messages_give_sizes_never_content() {
        assert_eq!(
            summarize(&PluginCommand::Write(b"rm -rf ~\n".to_vec())).as_deref(),
            Some("9 bytes")
        );
        assert_eq!(
            summarize(&PluginCommand::WriteChars("a".to_string())).as_deref(),
            Some("1 byte")
        );
        let message = PluginCommand::MessageToPlugin(MessageToPlugin {
            plugin_url: Some("file:/home/me/plugins/notes.wasm".to_string()),
            message_name: "save".to_string(),
            message_payload: Some("the whole secret document".to_string()),
            ..Default::default()
        });
        assert_eq!(
            summarize(&message).as_deref(),
            Some("to notes.wasm · save · 25 bytes")
        );
        let to_worker = PluginCommand::PostMessageTo(PluginMessage {
            name: "index".to_string(),
            payload: "secret".to_string(),
            worker_name: Some("search".to_string()),
        });
        assert_eq!(
            summarize(&to_worker).as_deref(),
            Some("to worker search · index · 6 bytes")
        );
        assert_eq!(
            summarize(&PluginCommand::CliPipeOutput(
                "my-pipe".to_string(),
                "secret".to_string()
            ))
            .as_deref(),
            Some("my-pipe · 6 bytes")
        );
    }

    #[test]
    fn subscriptions_and_permissions_are_listed() {
        let events = PluginCommand::Subscribe(
            [EventType::TabUpdate, EventType::Key, EventType::Timer].into(),
        );
        assert_eq!(summarize(&events).as_deref(), Some("Key, TabUpdate, Timer"));
        let permissions = PluginCommand::RequestPluginPermissions(vec![
            PermissionType::RunCommands,
            PermissionType::ReadApplicationState,
        ]);
        assert_eq!(
            summarize(&permissions).as_deref(),
            Some("ReadApplicationState, RunCommands")
        );
        let many = names((0..9).map(|n| format!("E{n}")));
        assert_eq!(many, "E0, E1, E2, E3, E4, E5 +3 more");
    }

    #[test]
    fn other_commands_have_no_summary() {
        assert_eq!(summarize(&PluginCommand::HideSelf), None);
        assert_eq!(
            cut(&"x".repeat(150), SUMMARY_MAX_CHARS).chars().count(),
            100
        );
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
                summary: None,
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
    fn events_are_tallied_per_plugin_with_the_time_they_cost() {
        let trace = ApiTrace::default();
        trace.set_active(true);
        trace.record_event(5, EventType::Timer, Duration::from_micros(100));
        trace.record_event(5, EventType::Timer, Duration::from_micros(50));
        trace.record_event(5, EventType::TabUpdate, Duration::from_micros(10));
        trace.record_render(5, Duration::from_micros(400));
        trace.record_event(2, EventType::Key, Duration::from_micros(1));
        let batch = trace.take_batch().unwrap();
        let ids: Vec<u32> = batch.events.iter().map(|load| load.plugin_id).collect();
        assert_eq!(ids, vec![2, 5]);
        let load = &batch.events[1];
        assert_eq!(
            load.events,
            vec![
                EventCount {
                    event: "TabUpdate".to_string(),
                    count: 1
                },
                EventCount {
                    event: "Timer".to_string(),
                    count: 2
                },
            ]
        );
        assert_eq!(load.update_time_us, 160);
        assert_eq!((load.renders, load.render_time_us), (1, 400));
        assert!(trace.take_batch().is_none(), "the tallies start over");
    }

    #[test]
    fn a_trace_subscriber_has_no_events_counted() {
        let trace = ApiTrace::default();
        let plain: HashSet<EventType> = [EventType::Timer].into();
        let subscriber: HashSet<EventType> = [EventType::PluginApiTrace].into();
        assert!(!trace.counts_events_of(&plain), "the trace is off");
        trace.set_active(true);
        assert!(trace.counts_events_of(&plain));
        assert!(!trace.counts_events_of(&subscriber));
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
