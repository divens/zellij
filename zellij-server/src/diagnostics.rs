//! Diagnostics reports for `request_diagnostics`.
//!
//! A request travels plugin → screen thread, which fills in the sections it
//! owns, → plugin thread, which adds what it owns and delivers the report.
//! Nothing here runs unless someone asks for a report.

use std::collections::BTreeMap;

use zellij_utils::data::{ClientId, PaneId};
use zellij_utils::diagnostics::{
    ExecutorThreadDiagnostics, MemoryDiagnostics, PaneDiagnostics, PluginDiagnostics,
};

use crate::panes::cluster_table;
use crate::panes::kitty_graphics::KittyImageStore;
use crate::panes::PaneId as ServerPaneId;
use crate::plugins::{PluginId, PluginLinearMemory};
use crate::tab::{Pane, Tab};

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
    fn this_process_reports_its_memory() {
        let rss = super::process_rss_bytes().expect("the OS reports our memory");
        assert!(
            rss > 1024 * 1024,
            "a running test binary uses more than 1 MiB: {rss}"
        );
    }
}
