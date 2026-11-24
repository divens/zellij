//! Each plugin's latest errors: failed, denied, forbidden and unreadable
//! commands, and crashes.
//!
//! Errors are always recorded, so ones from before anyone looked are still
//! there. They are rare, so this costs nothing on the success path: the
//! thread that hit the error sends it to the plugin thread, which owns the
//! logs.

use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use zellij_utils::data::{CommandType, PermissionType};
use zellij_utils::diagnostics::{PluginErrorKind, PluginErrorRecord};

use super::{PluginId, PluginInstruction};
use crate::thread_bus::ThreadSenders;

/// The most errors kept per plugin; older ones are dropped, though still
/// counted in the total.
pub const PLUGIN_ERROR_LOG_SIZE: usize = 64;

/// One panic reaches us twice: zellij-tile's panic hook reports the panic's
/// message, then the trap it causes fails the call that was running. A crash
/// this soon after another is the same one.
const SAME_CRASH_WINDOW_MS: u64 = 1_000;

#[derive(Debug, Default)]
struct Log {
    total: u64,
    records: VecDeque<PluginErrorRecord>,
}

/// Every plugin's log, by plugin id (for a shared plugin, its instance id).
#[derive(Debug, Default)]
pub struct PluginErrorLogs {
    logs: BTreeMap<PluginId, Log>,
}

impl PluginErrorLogs {
    /// The same error as the newest one only bumps its count and time, so a
    /// plugin retrying a denied command in a loop can't flush the log.
    pub fn record(&mut self, plugin_id: PluginId, record: PluginErrorRecord) {
        let log = self.logs.entry(plugin_id).or_default();
        let same_crash = log.records.back().is_some_and(|newest| {
            newest.kind == PluginErrorKind::Crashed
                && record.kind == PluginErrorKind::Crashed
                && record.unix_time_ms.saturating_sub(newest.unix_time_ms) < SAME_CRASH_WINDOW_MS
        });
        if same_crash {
            // Keep the first report: it has the panic's own message.
            return;
        }
        log.total += u64::from(record.repeats.max(1));
        match log.records.back_mut() {
            Some(newest) if newest.repeats_as(&record) => {
                newest.repeats = newest.repeats.saturating_add(record.repeats.max(1));
                newest.unix_time_ms = record.unix_time_ms;
            },
            _ => {
                log.records.push_back(record);
                if log.records.len() > PLUGIN_ERROR_LOG_SIZE {
                    log.records.pop_front();
                }
            },
        }
    }

    pub fn forget(&mut self, plugin_id: PluginId) {
        self.logs.remove(&plugin_id);
    }

    /// `(plugin id, total, records oldest first)` for every plugin with
    /// errors, by plugin id.
    pub fn logs(&self) -> impl Iterator<Item = (PluginId, u64, Vec<PluginErrorRecord>)> + '_ {
        self.logs
            .iter()
            .map(|(plugin_id, log)| (*plugin_id, log.total, log.records.iter().cloned().collect()))
    }
}

/// Sends an error to the plugin thread, from whichever thread hit it.
pub fn report(
    senders: &ThreadSenders,
    plugin_id: PluginId,
    kind: PluginErrorKind,
    command: Option<CommandType>,
    permission: Option<PermissionType>,
    message: String,
) {
    let record = PluginErrorRecord {
        unix_time_ms: unix_time_ms(),
        kind,
        command: command.map(|command| format!("{command:?}")),
        permission: permission.map(|permission| permission.to_string()),
        message: condensed_panic(&message).unwrap_or_else(|| one_line(&message)),
        repeats: 1,
    };
    let _ = senders.send_to_plugin(PluginInstruction::RecordPluginError(plugin_id, record));
}

/// zellij-tile reports a panic as its message followed by the pretty-printed
/// `PanicHookInfo`, which suits the crash screen in the plugin's pane. For the
/// log, keep the message and where it happened.
fn condensed_panic(message: &str) -> Option<String> {
    let (payload, info) = message.split_once("PanicHookInfo")?;
    let file = info.split_once("file: \"")?.1.split_once('"')?.0;
    let number_after = |label: &str| -> Option<String> {
        let rest = info.split_once(label)?.1;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (!digits.is_empty()).then_some(digits)
    };
    let line = number_after("line: ")?;
    let column = number_after("column: ")?;
    let file = file.replace("\\\\", "\\");
    Some(format!("{} (at {file}:{line}:{column})", one_line(payload)))
}

/// Error chains and panic messages can span lines; a log row can't.
fn one_line(message: &str) -> String {
    let parts: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    parts.join(" ")
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied(at: u64) -> PluginErrorRecord {
        PluginErrorRecord {
            unix_time_ms: at,
            kind: PluginErrorKind::Denied,
            command: Some("OpenFile".to_string()),
            permission: Some("OpenFiles".to_string()),
            message: "denied".to_string(),
            repeats: 1,
        }
    }

    fn failed(message: &str) -> PluginErrorRecord {
        PluginErrorRecord {
            kind: PluginErrorKind::Failed,
            message: message.to_string(),
            repeats: 1,
            ..Default::default()
        }
    }

    #[test]
    fn the_same_error_in_a_row_is_counted_not_repeated() {
        let mut logs = PluginErrorLogs::default();
        logs.record(3, denied(10));
        logs.record(3, denied(20));
        logs.record(3, failed("boom"));
        logs.record(3, denied(30));
        let (plugin_id, total, records) = logs.logs().next().unwrap();
        assert_eq!((plugin_id, total), (3, 4));
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].repeats, 2);
        assert_eq!(records[0].unix_time_ms, 20, "the time is the latest");
        assert_eq!(records[2].repeats, 1, "not merged across another error");
    }

    #[test]
    fn the_log_keeps_the_latest_errors_and_counts_them_all() {
        let mut logs = PluginErrorLogs::default();
        for n in 0..PLUGIN_ERROR_LOG_SIZE + 10 {
            logs.record(1, failed(&format!("error {n}")));
        }
        let (_, total, records) = logs.logs().next().unwrap();
        assert_eq!(total, PLUGIN_ERROR_LOG_SIZE as u64 + 10);
        assert_eq!(records.len(), PLUGIN_ERROR_LOG_SIZE);
        assert_eq!(records[0].message, "error 10");
        assert_eq!(
            records.last().unwrap().message,
            format!("error {}", PLUGIN_ERROR_LOG_SIZE + 9)
        );
    }

    fn crash(at: u64, message: &str) -> PluginErrorRecord {
        PluginErrorRecord {
            unix_time_ms: at,
            kind: PluginErrorKind::Crashed,
            message: message.to_string(),
            repeats: 1,
            ..Default::default()
        }
    }

    #[test]
    fn a_panic_and_the_trap_it_causes_are_one_crash() {
        let mut logs = PluginErrorLogs::default();
        logs.record(8, crash(1_000, "devtools-chaos: panic mode"));
        logs.record(8, crash(1_000, "Failed to apply event: wasm unreachable"));
        logs.record(8, crash(9_000, "devtools-chaos: panic mode"));
        let (_, total, records) = logs.logs().next().unwrap();
        assert_eq!(total, 2, "the trap isn't counted");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "devtools-chaos: panic mode");
        assert_eq!(records[0].repeats, 2, "a later crash counts again");
        assert_eq!(records[0].unix_time_ms, 9_000);
    }

    #[test]
    fn messages_are_kept_on_one_line() {
        assert_eq!(
            one_line("failed to load\n\nCaused by:\n    0: not found\n"),
            "failed to load Caused by: 0: not found"
        );
    }

    #[test]
    fn a_panic_report_is_condensed_to_its_message_and_location() {
        // As zellij-tile's panic hook formats it, with its \r\n line ends.
        let reported = "devtools-chaos: panic mode\r\n\r\nPanicHookInfo {\r\n    payload: Any { .. },\r\n    location: Location {\r\n        file: \"fixtures\\\\chaos\\\\src\\\\main.rs\",\r\n        line: 102,\r\n        column: 28,\r\n    },\r\n    can_unwind: true,\r\n}";
        assert_eq!(
            condensed_panic(reported).as_deref(),
            Some(r"devtools-chaos: panic mode (at fixtures\chaos\src\main.rs:102:28)")
        );
        assert_eq!(condensed_panic("Failed to apply event to plugin 8"), None);
    }

    #[test]
    fn an_unloaded_plugin_is_forgotten() {
        let mut logs = PluginErrorLogs::default();
        logs.record(1, failed("a"));
        logs.record(2, failed("b"));
        logs.forget(1);
        let ids: Vec<PluginId> = logs.logs().map(|(plugin_id, _, _)| plugin_id).collect();
        assert_eq!(ids, vec![2]);
    }
}
