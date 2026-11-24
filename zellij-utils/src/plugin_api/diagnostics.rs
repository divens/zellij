pub use super::generated_api::api::diagnostics::{
    DiagnosticsPaneId as ProtobufDiagnosticsPaneId, DiagnosticsReport as ProtobufDiagnosticsReport,
    DiagnosticsRequest as ProtobufDiagnosticsRequest,
    DiagnosticsSection as ProtobufDiagnosticsSection,
    ExecutorThreadDiagnostics as ProtobufExecutorThreadDiagnostics,
    GridDiagnostics as ProtobufGridDiagnostics, MemoryDiagnostics as ProtobufMemoryDiagnostics,
    PaneDiagnostics as ProtobufPaneDiagnostics, PluginDiagnostics as ProtobufPluginDiagnostics,
};
use crate::data::PaneId;
use crate::diagnostics::{
    DiagnosticsReport, DiagnosticsRequest, DiagnosticsSection, ExecutorThreadDiagnostics,
    GridDiagnostics, MemoryDiagnostics, PaneDiagnostics, PluginDiagnostics,
};

use std::convert::TryFrom;

/// Wire numbers this build doesn't know become `Unknown`, so a server can
/// report them back as unsupported and a peer can encode them unchanged.
fn decode_section(value: i32) -> DiagnosticsSection {
    match ProtobufDiagnosticsSection::try_from(value) {
        Ok(ProtobufDiagnosticsSection::Panes) => DiagnosticsSection::Panes,
        Ok(ProtobufDiagnosticsSection::Memory) => DiagnosticsSection::Memory,
        Ok(ProtobufDiagnosticsSection::Plugins) => DiagnosticsSection::Plugins,
        Err(_) => DiagnosticsSection::Unknown(value),
    }
}

fn encode_section(section: DiagnosticsSection) -> i32 {
    match section {
        DiagnosticsSection::Panes => ProtobufDiagnosticsSection::Panes as i32,
        DiagnosticsSection::Memory => ProtobufDiagnosticsSection::Memory as i32,
        DiagnosticsSection::Plugins => ProtobufDiagnosticsSection::Plugins as i32,
        DiagnosticsSection::Unknown(value) => value,
    }
}

fn decode_sections(sections: Vec<i32>) -> Vec<DiagnosticsSection> {
    sections.into_iter().map(decode_section).collect()
}

fn encode_sections(sections: Vec<DiagnosticsSection>) -> Vec<i32> {
    sections.into_iter().map(encode_section).collect()
}

impl From<ProtobufDiagnosticsRequest> for DiagnosticsRequest {
    fn from(request: ProtobufDiagnosticsRequest) -> Self {
        // Unknown sections are kept: the server skips them but reports them.
        DiagnosticsRequest {
            sections: decode_sections(request.sections),
        }
    }
}

impl From<DiagnosticsRequest> for ProtobufDiagnosticsRequest {
    fn from(request: DiagnosticsRequest) -> Self {
        ProtobufDiagnosticsRequest {
            sections: encode_sections(request.sections),
        }
    }
}

impl TryFrom<ProtobufDiagnosticsReport> for DiagnosticsReport {
    type Error = &'static str;
    fn try_from(report: ProtobufDiagnosticsReport) -> Result<Self, &'static str> {
        Ok(DiagnosticsReport {
            // A newer server may fill in sections this build doesn't know;
            // there are no fields to put them in, so they are dropped.
            sections: decode_sections(report.sections)
                .into_iter()
                .filter(|section| section.is_known())
                .collect(),
            unsupported_sections: decode_sections(report.unsupported_sections),
            panes: report
                .panes
                .into_iter()
                .map(PaneDiagnostics::try_from)
                .collect::<Result<_, _>>()?,
            memory: report.memory.map(Into::into),
            plugins: report.plugins.into_iter().map(Into::into).collect(),
            plugin_executor_threads: report
                .plugin_executor_threads
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }
}

impl From<DiagnosticsReport> for ProtobufDiagnosticsReport {
    fn from(report: DiagnosticsReport) -> Self {
        ProtobufDiagnosticsReport {
            sections: encode_sections(report.sections),
            unsupported_sections: encode_sections(report.unsupported_sections),
            panes: report.panes.into_iter().map(Into::into).collect(),
            memory: report.memory.map(Into::into),
            plugins: report.plugins.into_iter().map(Into::into).collect(),
            plugin_executor_threads: report
                .plugin_executor_threads
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl TryFrom<ProtobufPaneDiagnostics> for PaneDiagnostics {
    type Error = &'static str;
    fn try_from(pane: ProtobufPaneDiagnostics) -> Result<Self, &'static str> {
        let pane_id = pane.pane_id.ok_or("Pane diagnostics without a pane id")?;
        Ok(PaneDiagnostics {
            pane_id: if pane_id.is_plugin {
                PaneId::Plugin(pane_id.id)
            } else {
                PaneId::Terminal(pane_id.id)
            },
            tab_position: pane.tab_position as usize,
            grid_count: pane.grid_count,
            grid: pane.grid.map(Into::into),
        })
    }
}

impl From<PaneDiagnostics> for ProtobufPaneDiagnostics {
    fn from(pane: PaneDiagnostics) -> Self {
        let (id, is_plugin) = match pane.pane_id {
            PaneId::Terminal(id) => (id, false),
            PaneId::Plugin(id) => (id, true),
        };
        ProtobufPaneDiagnostics {
            pane_id: Some(ProtobufDiagnosticsPaneId { id, is_plugin }),
            tab_position: pane.tab_position as u32,
            grid_count: pane.grid_count,
            grid: pane.grid.map(Into::into),
        }
    }
}

impl From<ProtobufGridDiagnostics> for GridDiagnostics {
    fn from(grid: ProtobufGridDiagnostics) -> Self {
        GridDiagnostics {
            viewport_rows: grid.viewport_rows,
            viewport_cols: grid.viewport_cols,
            scrollback_lines: grid.scrollback_lines,
            scrollback_limit: grid.scrollback_limit,
            scrollback_cells: grid.scrollback_cells,
            scrollback_bytes_estimate: grid.scrollback_bytes_estimate,
            lines_discarded: grid.lines_discarded,
            in_alternate_screen: grid.in_alternate_screen,
        }
    }
}

impl From<GridDiagnostics> for ProtobufGridDiagnostics {
    fn from(grid: GridDiagnostics) -> Self {
        ProtobufGridDiagnostics {
            viewport_rows: grid.viewport_rows,
            viewport_cols: grid.viewport_cols,
            scrollback_lines: grid.scrollback_lines,
            scrollback_limit: grid.scrollback_limit,
            scrollback_cells: grid.scrollback_cells,
            scrollback_bytes_estimate: grid.scrollback_bytes_estimate,
            lines_discarded: grid.lines_discarded,
            in_alternate_screen: grid.in_alternate_screen,
        }
    }
}

impl From<ProtobufMemoryDiagnostics> for MemoryDiagnostics {
    fn from(memory: ProtobufMemoryDiagnostics) -> Self {
        MemoryDiagnostics {
            process_rss_bytes: memory.process_rss_bytes,
            scrollback_bytes_estimate: memory.scrollback_bytes_estimate,
            grapheme_cluster_bytes: memory.grapheme_cluster_bytes,
            grapheme_cluster_entries: memory.grapheme_cluster_entries,
            grapheme_cluster_limit_bytes: memory.grapheme_cluster_limit_bytes,
            kitty_image_bytes: memory.kitty_image_bytes,
            kitty_image_count: memory.kitty_image_count,
            kitty_image_quota_bytes: memory.kitty_image_quota_bytes,
            plugin_linear_memory_bytes: memory.plugin_linear_memory_bytes,
        }
    }
}

impl From<MemoryDiagnostics> for ProtobufMemoryDiagnostics {
    fn from(memory: MemoryDiagnostics) -> Self {
        ProtobufMemoryDiagnostics {
            process_rss_bytes: memory.process_rss_bytes,
            scrollback_bytes_estimate: memory.scrollback_bytes_estimate,
            grapheme_cluster_bytes: memory.grapheme_cluster_bytes,
            grapheme_cluster_entries: memory.grapheme_cluster_entries,
            grapheme_cluster_limit_bytes: memory.grapheme_cluster_limit_bytes,
            kitty_image_bytes: memory.kitty_image_bytes,
            kitty_image_count: memory.kitty_image_count,
            kitty_image_quota_bytes: memory.kitty_image_quota_bytes,
            plugin_linear_memory_bytes: memory.plugin_linear_memory_bytes,
        }
    }
}

impl From<ProtobufPluginDiagnostics> for PluginDiagnostics {
    fn from(plugin: ProtobufPluginDiagnostics) -> Self {
        PluginDiagnostics {
            plugin_id: plugin.plugin_id,
            location: plugin.location,
            is_shared: plugin.is_shared,
            slot_ids: plugin.slot_ids,
            instances: plugin.instances,
            workers: plugin.workers,
            linear_memory_bytes: plugin.linear_memory_bytes,
            largest_linear_memory_bytes: plugin.largest_linear_memory_bytes,
            linear_memory_limit_bytes: plugin.linear_memory_limit_bytes,
            executor_thread: plugin.executor_thread,
        }
    }
}

impl From<PluginDiagnostics> for ProtobufPluginDiagnostics {
    fn from(plugin: PluginDiagnostics) -> Self {
        ProtobufPluginDiagnostics {
            plugin_id: plugin.plugin_id,
            location: plugin.location,
            is_shared: plugin.is_shared,
            slot_ids: plugin.slot_ids,
            instances: plugin.instances,
            workers: plugin.workers,
            linear_memory_bytes: plugin.linear_memory_bytes,
            largest_linear_memory_bytes: plugin.largest_linear_memory_bytes,
            linear_memory_limit_bytes: plugin.linear_memory_limit_bytes,
            executor_thread: plugin.executor_thread,
        }
    }
}

impl From<ProtobufExecutorThreadDiagnostics> for ExecutorThreadDiagnostics {
    fn from(thread: ProtobufExecutorThreadDiagnostics) -> Self {
        ExecutorThreadDiagnostics {
            index: thread.index,
            jobs_in_flight: thread.jobs_in_flight,
            plugin_ids: thread.plugin_ids,
        }
    }
}

impl From<ExecutorThreadDiagnostics> for ProtobufExecutorThreadDiagnostics {
    fn from(thread: ExecutorThreadDiagnostics) -> Self {
        ProtobufExecutorThreadDiagnostics {
            index: thread.index,
            jobs_in_flight: thread.jobs_in_flight,
            plugin_ids: thread.plugin_ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    /// A section number no build knows yet, standing in for one added later.
    const FUTURE_SECTION: i32 = 999;

    fn sample_report() -> DiagnosticsReport {
        DiagnosticsReport {
            sections: DiagnosticsSection::ALL.to_vec(),
            unsupported_sections: vec![],
            memory: Some(MemoryDiagnostics {
                process_rss_bytes: Some(212 * 1024 * 1024),
                scrollback_bytes_estimate: 6_874_048,
                grapheme_cluster_bytes: 4_096,
                grapheme_cluster_entries: 12,
                grapheme_cluster_limit_bytes: 32 * 1024 * 1024,
                kitty_image_bytes: 18 * 1024 * 1024,
                kitty_image_count: 3,
                kitty_image_quota_bytes: 320 * 1024 * 1024,
                plugin_linear_memory_bytes: 9 * 1024 * 1024,
            }),
            plugins: vec![
                PluginDiagnostics {
                    plugin_id: 2,
                    location: "zellij:status-bar".to_string(),
                    is_shared: false,
                    slot_ids: vec![],
                    instances: 2,
                    workers: 1,
                    linear_memory_bytes: 6 * 1024 * 1024,
                    largest_linear_memory_bytes: 3 * 1024 * 1024,
                    linear_memory_limit_bytes: 16 * 1024 * 1024,
                    executor_thread: Some(1),
                },
                PluginDiagnostics {
                    plugin_id: 7,
                    location: "file:/plugins/bars.wasm".to_string(),
                    is_shared: true,
                    slot_ids: vec![8, 9],
                    instances: 1,
                    workers: 0,
                    linear_memory_bytes: 3 * 1024 * 1024,
                    largest_linear_memory_bytes: 3 * 1024 * 1024,
                    linear_memory_limit_bytes: 16 * 1024 * 1024,
                    executor_thread: None,
                },
            ],
            plugin_executor_threads: vec![
                ExecutorThreadDiagnostics {
                    index: 0,
                    jobs_in_flight: 0,
                    plugin_ids: vec![],
                },
                ExecutorThreadDiagnostics {
                    index: 1,
                    jobs_in_flight: 3,
                    plugin_ids: vec![2, 5],
                },
            ],
            panes: vec![
                PaneDiagnostics {
                    pane_id: PaneId::Terminal(3),
                    tab_position: 1,
                    grid_count: 1,
                    grid: Some(GridDiagnostics {
                        viewport_rows: 40,
                        viewport_cols: 120,
                        scrollback_lines: 9_876,
                        scrollback_limit: 10_000,
                        scrollback_cells: 400_000,
                        scrollback_bytes_estimate: 6_874_048,
                        lines_discarded: 123_456,
                        in_alternate_screen: true,
                    }),
                },
                PaneDiagnostics {
                    pane_id: PaneId::Plugin(2),
                    tab_position: 0,
                    grid_count: 3,
                    grid: None,
                },
            ],
        }
    }

    fn over_the_wire(report: DiagnosticsReport) -> DiagnosticsReport {
        let bytes = ProtobufDiagnosticsReport::from(report).encode_to_vec();
        let decoded = ProtobufDiagnosticsReport::decode(bytes.as_slice()).unwrap();
        DiagnosticsReport::try_from(decoded).unwrap()
    }

    #[test]
    fn a_report_survives_the_wire() {
        let report = sample_report();
        assert_eq!(over_the_wire(report.clone()), report);
    }

    #[test]
    fn a_request_survives_the_wire() {
        let request = DiagnosticsRequest::new(&[DiagnosticsSection::Panes]);
        let bytes = ProtobufDiagnosticsRequest::from(request.clone()).encode_to_vec();
        let decoded = ProtobufDiagnosticsRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(DiagnosticsRequest::from(decoded), request);
    }

    #[test]
    fn a_server_keeps_unknown_requested_sections_to_report_them() {
        let request = ProtobufDiagnosticsRequest {
            sections: vec![ProtobufDiagnosticsSection::Panes as i32, FUTURE_SECTION],
        };
        let request = DiagnosticsRequest::from(request);
        assert_eq!(request.resolved_sections(), vec![DiagnosticsSection::Panes]);
        assert_eq!(
            request.unsupported_sections(),
            vec![DiagnosticsSection::Unknown(FUTURE_SECTION)]
        );
    }

    #[test]
    fn unsupported_sections_reach_the_plugin_by_their_wire_number() {
        let report = DiagnosticsReport {
            unsupported_sections: vec![DiagnosticsSection::Unknown(FUTURE_SECTION)],
            ..Default::default()
        };
        let encoded = ProtobufDiagnosticsReport::from(report.clone());
        assert_eq!(encoded.unsupported_sections, vec![FUTURE_SECTION]);
        assert_eq!(over_the_wire(report.clone()), report);
    }

    #[test]
    fn a_plugin_drops_filled_in_sections_it_does_not_know() {
        let report = ProtobufDiagnosticsReport {
            sections: vec![ProtobufDiagnosticsSection::Panes as i32, FUTURE_SECTION],
            ..Default::default()
        };
        assert_eq!(
            DiagnosticsReport::try_from(report).unwrap().sections,
            vec![DiagnosticsSection::Panes]
        );
    }

    #[test]
    fn an_unknown_process_size_stays_unknown() {
        let report = DiagnosticsReport {
            sections: vec![DiagnosticsSection::Memory],
            memory: Some(MemoryDiagnostics::default()),
            ..Default::default()
        };
        let decoded = over_the_wire(report);
        assert_eq!(decoded.memory.unwrap().process_rss_bytes, None);
    }

    #[test]
    fn pane_diagnostics_need_a_pane_id() {
        let pane = ProtobufPaneDiagnostics {
            pane_id: None,
            ..Default::default()
        };
        assert!(PaneDiagnostics::try_from(pane).is_err());
    }
}
