//! Session diagnostics: the report behind `request_diagnostics` in the plugin
//! API.
//!
//! Counters cover the lifetime of whatever they describe and are never reset
//! when read, so several consumers can poll independently and compute rates
//! from the difference between two reports.

use crate::data::{ClientId, PaneId};
use serde::{Deserialize, Serialize};

/// A part of the diagnostics report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiagnosticsSection {
    /// Per-pane grid and scrollback state.
    Panes,
    /// Memory held by the server: process total and shared stores.
    Memory,
    /// Running plugins: linear memory, workers, and the executor threads
    /// they run on.
    Plugins,
    /// Messages waiting for each of the server's core threads.
    Queues,
    /// Every connection: what kind of client it is and its output backlog.
    Clients,
    /// A section this build doesn't know, by its wire number. It appears when
    /// a newer peer names a section, so a report can say which sections it
    /// could not fill in. Never request it.
    Unknown(i32),
}

impl DiagnosticsSection {
    /// Every section this build knows, in report order.
    pub const ALL: [DiagnosticsSection; 5] = [
        DiagnosticsSection::Panes,
        DiagnosticsSection::Memory,
        DiagnosticsSection::Plugins,
        DiagnosticsSection::Queues,
        DiagnosticsSection::Clients,
    ];

    pub fn is_known(self) -> bool {
        !matches!(self, DiagnosticsSection::Unknown(_))
    }

    /// The name used on the command line and in text output: `panes`.
    pub fn name(self) -> String {
        match self {
            DiagnosticsSection::Panes => "panes".to_string(),
            DiagnosticsSection::Memory => "memory".to_string(),
            DiagnosticsSection::Plugins => "plugins".to_string(),
            DiagnosticsSection::Queues => "queues".to_string(),
            DiagnosticsSection::Clients => "clients".to_string(),
            DiagnosticsSection::Unknown(number) => format!("section {number}"),
        }
    }

    /// A known section by its `name`, ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        DiagnosticsSection::ALL
            .iter()
            .copied()
            .find(|section| section.name().eq_ignore_ascii_case(name.trim()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsRequest {
    /// The sections to fill in. Empty means all of them.
    pub sections: Vec<DiagnosticsSection>,
}

impl DiagnosticsRequest {
    pub fn new(sections: &[DiagnosticsSection]) -> Self {
        DiagnosticsRequest {
            sections: sections.to_vec(),
        }
    }

    pub fn includes(&self, section: DiagnosticsSection) -> bool {
        if self.sections.is_empty() {
            section.is_known()
        } else {
            self.sections.contains(&section)
        }
    }

    /// The sections this request covers, in report order and without
    /// duplicates.
    pub fn resolved_sections(&self) -> Vec<DiagnosticsSection> {
        DiagnosticsSection::ALL
            .iter()
            .copied()
            .filter(|section| self.includes(*section))
            .collect()
    }

    /// The requested sections this build doesn't know, without duplicates.
    pub fn unsupported_sections(&self) -> Vec<DiagnosticsSection> {
        let mut unsupported: Vec<DiagnosticsSection> = self
            .sections
            .iter()
            .copied()
            .filter(|section| !section.is_known())
            .collect();
        unsupported.sort();
        unsupported.dedup();
        unsupported
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    /// The sections that were filled in. The fields of other sections are
    /// left empty.
    pub sections: Vec<DiagnosticsSection>,
    /// Requested sections the server doesn't know, so it couldn't fill them
    /// in. When every requested section is unsupported, `sections` is empty
    /// and this lists them all: the server is older than the request.
    pub unsupported_sections: Vec<DiagnosticsSection>,
    pub panes: Vec<PaneDiagnostics>,
    pub memory: Option<MemoryDiagnostics>,
    /// Ordered by plugin id.
    pub plugins: Vec<PluginDiagnostics>,
    /// The threads plugins run on, ordered by index. Filled in with
    /// `plugins`.
    pub plugin_executor_threads: Vec<ExecutorThreadDiagnostics>,
    pub queues: Vec<QueueDiagnostics>,
    /// Ordered by client id.
    pub clients: Vec<ClientDiagnostics>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDiagnostics {
    /// Resident memory of the server process, when the OS reports it.
    pub process_rss_bytes: Option<u64>,
    /// The sum of every pane's `scrollback_bytes_estimate`.
    pub scrollback_bytes_estimate: u64,
    /// Grapheme clusters too long to store inside a cell, shared by every
    /// pane, against the table's limit.
    pub grapheme_cluster_bytes: u64,
    pub grapheme_cluster_entries: u64,
    pub grapheme_cluster_limit_bytes: u64,
    /// Decoded kitty graphics images, shared by every pane, against the
    /// store's quota.
    pub kitty_image_bytes: u64,
    pub kitty_image_count: u64,
    pub kitty_image_quota_bytes: u64,
    /// Linear memory of every plugin instance and worker.
    pub plugin_linear_memory_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDiagnostics {
    /// For a shared plugin, the id of its one instance. The panes it renders
    /// into have ids of their own: `slot_ids`.
    pub plugin_id: u32,
    /// Where the plugin was loaded from, as a URL: `zellij:tab-bar`,
    /// `file:/path/to/plugin.wasm` or `https://…`.
    pub location: String,
    pub is_shared: bool,
    /// For a shared plugin, the plugin ids of the panes and background slots
    /// it serves, ordered. Empty otherwise.
    pub slot_ids: Vec<u32>,
    /// Instances of the plugin: one per client, or one if it is shared.
    pub instances: u32,
    /// Workers, across all instances. Each has its own linear memory.
    pub workers: u32,
    /// Linear memory of all instances and workers. It never shrinks, so it
    /// is a high-water mark.
    pub linear_memory_bytes: u64,
    /// The largest single instance or worker. The limit applies to each one
    /// separately, so this is the one that will hit it first.
    pub largest_linear_memory_bytes: u64,
    /// The size past which an instance or worker can't grow its memory: the
    /// plugin traps.
    pub linear_memory_limit_bytes: u64,
    /// The executor thread the plugin runs on.
    pub executor_thread: Option<u32>,
}

/// What a client is, pieced together from how it connected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientKind {
    /// A terminal running `zellij attach`.
    #[default]
    Terminal,
    /// The `zellij terminal` desktop window.
    DesktopWindow,
    /// A browser, through the web server.
    WebBrowser,
    /// A read-only watcher.
    Watcher,
    /// A connection that never attaches, such as `zellij action` or
    /// `zellij subscribe`.
    Cli,
    /// A kind this build doesn't know, by its wire number.
    Unknown(i32),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientDiagnostics {
    /// As in `list_clients`.
    pub client_id: ClientId,
    pub kind: ClientKind,
    /// Watchers: the attached client whose screen they mirror. A watcher has
    /// no plugin instances of its own; it sees that client's.
    pub watching: Option<ClientId>,
    /// Renders reach this client as structured frames, each acknowledged,
    /// rather than as ANSI.
    pub structured_frames: bool,
    /// Messages waiting to be written to the client. `None` if the server
    /// keeps no buffer for it.
    pub output_backlog: Option<u64>,
    /// The backlog at which the server gives up on the client and
    /// disconnects it.
    pub output_backlog_limit: u64,
    /// Structured clients: how long the client took to acknowledge its last
    /// frame, in microseconds.
    pub last_frame_ack_us: Option<u64>,
    /// Structured clients: how long the frame in flight has waited for its
    /// acknowledgement, in microseconds. Renders to the client wait too.
    pub frame_awaiting_ack_us: Option<u64>,
}

/// One of the server's message queues, at the moment the report was made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueDiagnostics {
    /// The thread that reads it: `screen`, `screen_bounded` (what the pty and
    /// plugin threads send to the screen), `screen_priority`, `pty`,
    /// `plugin`, `pty_writer`, `background_jobs` or `server`. Newer servers
    /// may add more.
    pub name: String,
    /// Messages waiting. A plugin's own request is measured while the plugin
    /// thread is still finishing that `update` call, so `screen_bounded`
    /// usually counts the one message it sends the screen afterwards.
    pub depth: u64,
    /// `None` for an unbounded queue. A bounded queue that is full blocks its
    /// senders.
    pub capacity: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorThreadDiagnostics {
    pub index: u32,
    /// Jobs queued or running. Above 1, plugins on this thread are waiting
    /// for each other.
    pub jobs_in_flight: u32,
    /// Ordered by id.
    pub plugin_ids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDiagnostics {
    pub pane_id: PaneId,
    /// The position of the pane's tab, as in `PaneManifest`.
    pub tab_position: usize,
    /// Grids the pane keeps: one for a terminal pane, and one per client
    /// that has seen it for a plugin pane.
    pub grid_count: u32,
    /// `None` for panes without a terminal grid, such as plugin panes.
    pub grid: Option<GridDiagnostics>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridDiagnostics {
    pub viewport_rows: u32,
    pub viewport_cols: u32,
    /// Lines of history kept outside the viewport. While the alternate screen
    /// is active, this includes the primary screen's history it keeps aside.
    pub scrollback_lines: u64,
    /// The `scroll_buffer_size` at which the oldest lines are dropped.
    pub scrollback_limit: u64,
    /// Cells stored in those lines. Lines keep only their non-blank cells.
    pub scrollback_cells: u64,
    /// Approximate bytes those lines take, excluding styles and grapheme
    /// clusters, which are shared.
    pub scrollback_bytes_estimate: u64,
    /// Lines dropped at the limit since the pane was created.
    pub lines_discarded: u64,
    pub in_alternate_screen: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_request_covers_every_section() {
        let request = DiagnosticsRequest::default();
        assert!(request.includes(DiagnosticsSection::Panes));
        assert_eq!(
            request.resolved_sections(),
            DiagnosticsSection::ALL.to_vec()
        );
    }

    #[test]
    fn resolved_sections_are_ordered_and_deduplicated() {
        let request =
            DiagnosticsRequest::new(&[DiagnosticsSection::Panes, DiagnosticsSection::Panes]);
        assert_eq!(request.resolved_sections(), vec![DiagnosticsSection::Panes]);
    }

    #[test]
    fn unknown_sections_are_not_served_but_are_reported() {
        let request = DiagnosticsRequest::new(&[
            DiagnosticsSection::Unknown(9),
            DiagnosticsSection::Panes,
            DiagnosticsSection::Unknown(9),
        ]);
        assert_eq!(request.resolved_sections(), vec![DiagnosticsSection::Panes]);
        assert_eq!(
            request.unsupported_sections(),
            vec![DiagnosticsSection::Unknown(9)]
        );
    }

    #[test]
    fn a_request_for_only_unknown_sections_resolves_to_nothing() {
        let request = DiagnosticsRequest::new(&[DiagnosticsSection::Unknown(9)]);
        assert!(request.resolved_sections().is_empty());
        assert!(!request.includes(DiagnosticsSection::Panes));
    }

    #[test]
    fn sections_go_by_their_names() {
        for section in DiagnosticsSection::ALL {
            assert_eq!(
                DiagnosticsSection::from_name(&section.name()),
                Some(section)
            );
        }
        assert_eq!(
            DiagnosticsSection::from_name(" Memory "),
            Some(DiagnosticsSection::Memory)
        );
        assert_eq!(DiagnosticsSection::from_name("threads"), None);
        assert_eq!(DiagnosticsSection::Unknown(9).name(), "section 9");
    }

    #[test]
    fn an_empty_request_never_includes_unknown_sections() {
        let request = DiagnosticsRequest::default();
        assert!(!request.includes(DiagnosticsSection::Unknown(9)));
        assert!(request.unsupported_sections().is_empty());
    }
}
