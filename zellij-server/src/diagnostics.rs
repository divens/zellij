//! Diagnostics reports for `request_diagnostics`.
//!
//! A request travels plugin → screen thread, which fills in the sections it
//! owns, → plugin thread, which adds what it owns and delivers the report.
//! Nothing here runs unless someone asks for a report.

use std::collections::BTreeMap;
use std::time::Duration;

use zellij_utils::channels::SenderWithContext;
use zellij_utils::data::{ClientId, PaneId};
use zellij_utils::diagnostics::{
    ClientDiagnostics, ClientKind, ExecutorThreadDiagnostics, MemoryDiagnostics, PaneDiagnostics,
    PluginDiagnostics, QueueDiagnostics,
};

use crate::panes::cluster_table;
use crate::panes::kitty_graphics::KittyImageStore;
use crate::panes::PaneId as ServerPaneId;
use crate::plugins::{PluginId, PluginLinearMemory};
use crate::screen::ScreenInstruction;
use crate::tab::{Pane, Tab};
use crate::thread_bus::Bus;

/// Where a finished report goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsReplyTo {
    /// The plugin that called `request_diagnostics`, for the client it acted
    /// for.
    Plugin {
        plugin_id: PluginId,
        client_id: ClientId,
    },
}

/// Every pane of a tab: tiled, floating and suppressed.
fn panes_of(tab: &Tab) -> impl Iterator<Item = (&ServerPaneId, &Box<dyn Pane>)> {
    let suppressed = tab
        .get_suppressed_panes()
        .map(|(pane_id, (_is_scrollback_editor, pane))| (pane_id, pane));
    tab.get_tiled_panes()
        .chain(tab.get_floating_panes())
        .chain(suppressed)
}

/// Every pane's grid state, ordered by tab position, then terminals before
/// plugins, then id.
pub(crate) fn collect_panes<'a>(tabs: impl Iterator<Item = &'a Tab>) -> Vec<PaneDiagnostics> {
    let mut panes = Vec::new();
    for tab in tabs {
        for (pane_id, pane) in panes_of(tab) {
            panes.push(PaneDiagnostics {
                pane_id: (*pane_id).into(),
                tab_position: tab.position,
                grid_count: pane.grid_count() as u32,
                grid: pane.grid_diagnostics(),
            });
        }
    }
    panes.sort_by_key(|pane| (pane.tab_position, sort_key(pane.pane_id)));
    panes
}

fn sort_key(pane_id: PaneId) -> (bool, u32) {
    match pane_id {
        PaneId::Terminal(id) => (false, id),
        PaneId::Plugin(id) => (true, id),
    }
}

/// The screen thread's share of the Memory section. The grapheme cluster
/// table is thread-local, so this must run on the screen thread, which owns
/// every grid. `process_rss_bytes` is left for the plugin thread.
pub(crate) fn collect_memory<'a>(
    tabs: impl Iterator<Item = &'a Tab>,
    kitty_images: &KittyImageStore,
) -> MemoryDiagnostics {
    let scrollback_bytes_estimate = tabs
        .flat_map(panes_of)
        .filter_map(|(_, pane)| pane.grid_diagnostics())
        .map(|grid| grid.scrollback_bytes_estimate)
        .sum();
    MemoryDiagnostics {
        process_rss_bytes: None,
        scrollback_bytes_estimate,
        grapheme_cluster_bytes: cluster_table::bytes() as u64,
        grapheme_cluster_entries: cluster_table::live_entries() as u64,
        grapheme_cluster_limit_bytes: cluster_table::byte_limit() as u64,
        kitty_image_bytes: kitty_images.total_bytes() as u64,
        kitty_image_count: kitty_images.image_count() as u64,
        kitty_image_quota_bytes: kitty_images.quota_bytes() as u64,
        plugin_linear_memory_bytes: 0,
    }
}

/// What the screen thread knows about one client, for the Clients section.
#[derive(Debug, Default)]
pub(crate) struct ClientObservation {
    /// `Some(is_web_client)` for an attached client.
    pub attached_as_web: Option<bool>,
    pub is_watcher: bool,
    /// Watchers: the client they mirror.
    pub watching: Option<ClientId>,
    /// It reported `ZELLIJ_DESKTOP_TERMINAL` in its host environment.
    pub in_desktop_window: bool,
    pub structured_frames: bool,
    pub last_frame_ack: Option<Duration>,
    pub frame_awaiting_ack: Option<Duration>,
    pub output_backlog: Option<usize>,
}

fn client_kind(client: &ClientObservation) -> ClientKind {
    if client.is_watcher {
        return ClientKind::Watcher;
    }
    match client.attached_as_web {
        Some(true) => ClientKind::WebBrowser,
        Some(false) if client.in_desktop_window => ClientKind::DesktopWindow,
        Some(false) => ClientKind::Terminal,
        // Registered (it has an output buffer) but neither attached nor
        // watching.
        None => ClientKind::Cli,
    }
}

/// One row per client, ordered by id.
pub(crate) fn collect_clients(
    clients: BTreeMap<ClientId, ClientObservation>,
    output_backlog_limit: usize,
) -> Vec<ClientDiagnostics> {
    let micros = |duration: Duration| duration.as_micros().min(u64::MAX as u128) as u64;
    clients
        .into_iter()
        .map(|(client_id, client)| ClientDiagnostics {
            client_id,
            kind: client_kind(&client),
            watching: client.watching,
            structured_frames: client.structured_frames,
            output_backlog: client.output_backlog.map(|backlog| backlog as u64),
            output_backlog_limit: output_backlog_limit as u64,
            last_frame_ack_us: client.last_frame_ack.map(micros),
            frame_awaiting_ack_us: client.frame_awaiting_ack.map(micros),
        })
        .collect()
}

/// The screen's receivers, in the order `screen_bus` is built with in
/// `lib.rs`: its main queue, then the bounded one the pty and plugin threads
/// send through.
const SCREEN_QUEUES: [&str; 2] = ["screen", "screen_bounded"];

/// Messages waiting for each core thread. Runs on the screen thread: its own
/// queues are read from its receivers, the others through its senders.
pub(crate) fn collect_queues(bus: &Bus<ScreenInstruction>) -> Vec<QueueDiagnostics> {
    let mut queues: Vec<QueueDiagnostics> = bus
        .receiver_depths()
        .into_iter()
        .enumerate()
        .map(|(index, (depth, capacity))| {
            let name = SCREEN_QUEUES
                .get(index)
                .map(|name| name.to_string())
                .unwrap_or_else(|| format!("screen_{index}"));
            queue(name, depth, capacity)
        })
        .collect();
    if let Some(depth) = bus.priority_depth() {
        queues.push(queue("screen_priority".to_string(), depth, None));
    }
    let senders = &bus.senders;
    queues.extend(sender_queue("pty", senders.to_pty.as_ref()));
    queues.extend(sender_queue("plugin", senders.to_plugin.as_ref()));
    queues.extend(sender_queue("pty_writer", senders.to_pty_writer.as_ref()));
    queues.extend(sender_queue(
        "background_jobs",
        senders.to_background_jobs.as_ref(),
    ));
    queues.extend(sender_queue("server", senders.to_server.as_ref()));
    queues
}

fn sender_queue<T>(name: &str, sender: Option<&SenderWithContext<T>>) -> Option<QueueDiagnostics> {
    sender.map(|sender| queue(name.to_string(), sender.len(), sender.capacity()))
}

fn queue(name: String, depth: usize, capacity: Option<usize>) -> QueueDiagnostics {
    QueueDiagnostics {
        name,
        depth: depth as u64,
        capacity: capacity.map(|capacity| capacity as u64),
    }
}

/// Who a plugin is, for its row in the Plugins section.
pub(crate) struct PluginIdentity {
    pub location: String,
    /// `Some` for a shared plugin: the slots its one instance serves.
    pub shared_slots: Option<Vec<u32>>,
}

/// One row per plugin, ordered by id. Runs on the plugin thread.
pub(crate) fn collect_plugins(
    identities: BTreeMap<PluginId, PluginIdentity>,
    linear_memory: &BTreeMap<PluginId, PluginLinearMemory>,
    executor_threads: &[ExecutorThreadDiagnostics],
    linear_memory_limit_bytes: u64,
) -> Vec<PluginDiagnostics> {
    let executor_thread_of: BTreeMap<u32, u32> = executor_threads
        .iter()
        .flat_map(|thread| thread.plugin_ids.iter().map(|id| (*id, thread.index)))
        .collect();
    identities
        .into_iter()
        .map(|(plugin_id, identity)| {
            let memory = linear_memory.get(&plugin_id).cloned().unwrap_or_default();
            PluginDiagnostics {
                plugin_id,
                location: identity.location,
                is_shared: identity.shared_slots.is_some(),
                slot_ids: identity.shared_slots.unwrap_or_default(),
                instances: memory.instances,
                workers: memory.workers,
                linear_memory_bytes: memory.total_bytes,
                largest_linear_memory_bytes: memory.largest_bytes,
                linear_memory_limit_bytes,
                executor_thread: executor_thread_of.get(&plugin_id).copied(),
            }
        })
        .collect()
}

/// Resident memory of this process. A targeted refresh of our own PID, but
/// on some platforms sysinfo still scans the process list, so keep this off
/// the screen thread.
pub(crate) fn process_rss_bytes() -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(std::process::id());
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map(|process| process.memory())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_rows_join_identity_memory_and_executor_thread() {
        let mut identities = BTreeMap::new();
        identities.insert(
            4,
            PluginIdentity {
                location: "zellij:status-bar".to_string(),
                shared_slots: None,
            },
        );
        identities.insert(
            9,
            PluginIdentity {
                location: "file:/plugins/bars.wasm".to_string(),
                shared_slots: Some(vec![10, 11]),
            },
        );
        let mut linear_memory = BTreeMap::new();
        linear_memory.insert(
            4,
            PluginLinearMemory {
                instances: 2,
                workers: 1,
                total_bytes: 3_000,
                largest_bytes: 1_500,
            },
        );
        let threads = vec![ExecutorThreadDiagnostics {
            index: 1,
            jobs_in_flight: 0,
            plugin_ids: vec![4],
        }];
        let rows = collect_plugins(identities, &linear_memory, &threads, 16_000);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].plugin_id, 4);
        assert_eq!((rows[0].instances, rows[0].workers), (2, 1));
        assert_eq!(rows[0].linear_memory_bytes, 3_000);
        assert_eq!(rows[0].largest_linear_memory_bytes, 1_500);
        assert_eq!(rows[0].linear_memory_limit_bytes, 16_000);
        assert_eq!(rows[0].executor_thread, Some(1));
        assert!(!rows[0].is_shared);
        assert!(rows[1].is_shared);
        assert_eq!(rows[1].slot_ids, vec![10, 11]);
        assert_eq!(
            rows[1].instances, 0,
            "no store yet: still loading, or failed to"
        );
        assert_eq!(rows[1].executor_thread, None);
    }

    #[test]
    fn clients_are_told_apart_by_how_they_connected() {
        let mut clients = BTreeMap::new();
        let attached = |is_web| ClientObservation {
            attached_as_web: Some(is_web),
            ..Default::default()
        };
        clients.insert(1, attached(false));
        clients.insert(
            2,
            ClientObservation {
                in_desktop_window: true,
                structured_frames: true,
                last_frame_ack: Some(Duration::from_micros(1_850)),
                frame_awaiting_ack: Some(Duration::from_millis(3)),
                output_backlog: Some(4),
                ..attached(false)
            },
        );
        clients.insert(3, attached(true));
        clients.insert(
            4,
            ClientObservation {
                is_watcher: true,
                watching: Some(1),
                ..Default::default()
            },
        );
        clients.insert(
            5,
            ClientObservation {
                output_backlog: Some(0),
                ..Default::default()
            },
        );
        let rows = collect_clients(clients, 5_000);
        let kinds: Vec<ClientKind> = rows.iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ClientKind::Terminal,
                ClientKind::DesktopWindow,
                ClientKind::WebBrowser,
                ClientKind::Watcher,
                ClientKind::Cli,
            ]
        );
        assert_eq!(rows[1].output_backlog, Some(4));
        assert_eq!(rows[1].output_backlog_limit, 5_000);
        assert_eq!(rows[1].last_frame_ack_us, Some(1_850));
        assert_eq!(rows[1].frame_awaiting_ack_us, Some(3_000));
        assert!(rows[1].structured_frames && !rows[0].structured_frames);
        assert_eq!(rows[0].output_backlog, None, "no buffer was seen for it");
        assert_eq!(rows[3].watching, Some(1));
        assert_eq!(rows[0].watching, None);
    }

    #[test]
    fn queues_are_named_by_the_thread_that_reads_them() {
        use crate::pty::PtyInstruction;
        use zellij_utils::channels;
        let (to_screen, screen_receiver) = channels::unbounded();
        let (to_screen_bounded, bounded_receiver) = channels::bounded(50);
        let (to_priority, priority_receiver) = channels::unbounded();
        let (to_pty, _pty_receiver) = channels::unbounded();
        let to_screen = SenderWithContext::new(to_screen);
        let to_screen_bounded = SenderWithContext::new(to_screen_bounded);
        let to_priority = SenderWithContext::new(to_priority);
        let to_pty = SenderWithContext::new(to_pty);
        for _ in 0..3 {
            to_screen.send(ScreenInstruction::Exit).unwrap();
        }
        to_screen_bounded.send(ScreenInstruction::Exit).unwrap();
        to_pty.send(PtyInstruction::Exit).unwrap();
        to_pty.send(PtyInstruction::Exit).unwrap();
        let bus = Bus::new(
            vec![screen_receiver, bounded_receiver],
            None,
            Some(&to_pty),
            None,
            None,
            None,
            None,
            None,
        )
        .with_priority(priority_receiver, &to_priority);
        let queues = collect_queues(&bus);
        let summary: Vec<(&str, u64, Option<u64>)> = queues
            .iter()
            .map(|queue| (queue.name.as_str(), queue.depth, queue.capacity))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("screen", 3, None),
                ("screen_bounded", 1, Some(50)),
                ("screen_priority", 0, None),
                ("pty", 2, None),
            ],
            "threads without a sender are left out"
        );
    }

    #[test]
    fn this_process_reports_its_memory() {
        let rss = super::process_rss_bytes().expect("the OS reports our memory");
        assert!(
            rss > 1024 * 1024,
            "a running test binary uses more than 1 MiB: {rss}"
        );
    }
}
