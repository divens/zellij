//! Diagnostics reports for `request_diagnostics`.
//!
//! A request travels plugin → screen thread, which fills in the sections it
//! owns, → plugin thread, which adds what it owns and delivers the report.
//! Nothing here runs unless someone asks for a report.

use zellij_utils::data::{ClientId, PaneId};
use zellij_utils::diagnostics::{MemoryDiagnostics, PaneDiagnostics};

use crate::panes::cluster_table;
use crate::panes::kitty_graphics::KittyImageStore;
use crate::panes::PaneId as ServerPaneId;
use crate::plugins::PluginId;
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
    }
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
    #[test]
    fn this_process_reports_its_memory() {
        let rss = super::process_rss_bytes().expect("the OS reports our memory");
        assert!(
            rss > 1024 * 1024,
            "a running test binary uses more than 1 MiB: {rss}"
        );
    }
}
