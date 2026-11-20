//! Diagnostics reports for `request_diagnostics`.
//!
//! A request travels plugin → screen thread, which fills in the sections it
//! owns, → plugin thread, which delivers the report. Nothing here runs unless
//! someone asks for a report.

use zellij_utils::data::{ClientId, PaneId};
use zellij_utils::diagnostics::PaneDiagnostics;

use crate::plugins::PluginId;
use crate::tab::Tab;

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

/// Every pane's grid state, ordered by tab position, then terminals before
/// plugins, then id.
pub(crate) fn collect_panes<'a>(tabs: impl Iterator<Item = &'a Tab>) -> Vec<PaneDiagnostics> {
    let mut panes = Vec::new();
    for tab in tabs {
        let suppressed = tab
            .get_suppressed_panes()
            .map(|(pane_id, (_is_scrollback_editor, pane))| (pane_id, pane));
        for (pane_id, pane) in tab
            .get_tiled_panes()
            .chain(tab.get_floating_panes())
            .chain(suppressed)
        {
            panes.push(PaneDiagnostics {
                pane_id: (*pane_id).into(),
                tab_position: tab.position,
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
