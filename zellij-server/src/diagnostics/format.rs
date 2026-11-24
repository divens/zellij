//! Output of `zellij action dump-diagnostics`: a text summary, or the whole
//! report as JSON.

use std::time::{SystemTime, UNIX_EPOCH};

use zellij_utils::consts::VERSION;
use zellij_utils::data::PaneId;
use zellij_utils::diagnostics::{ClientKind, DiagnosticsReport, DiagnosticsSection};

/// The report, with what a bug report also needs: which server made it, and
/// when.
pub(crate) fn as_json(report: &DiagnosticsReport) -> Vec<String> {
    let unix_time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let document = serde_json::json!({
        "zellij_version": VERSION,
        "unix_time_ms": unix_time_ms,
        "report": report,
    });
    let text = serde_json::to_string_pretty(&document)
        .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }).to_string());
    vec![text]
}

/// One block per section, in report order.
pub(crate) fn as_text(report: &DiagnosticsReport) -> Vec<String> {
    let mut lines = vec![format!("Zellij {VERSION} diagnostics")];
    if !report.unsupported_sections.is_empty() {
        lines.push(format!(
            "This server can't report: {}",
            section_names(&report.unsupported_sections)
        ));
    }
    // A server fills in only sections it knows, so `Unknown` never appears.
    for section in report.sections.iter().filter(|section| section.is_known()) {
        lines.push(String::new());
        match section {
            DiagnosticsSection::Panes => panes(report, &mut lines),
            DiagnosticsSection::Memory => memory(report, &mut lines),
            DiagnosticsSection::Plugins => plugins(report, &mut lines),
            DiagnosticsSection::Queues => queues(report, &mut lines),
            DiagnosticsSection::Clients => clients(report, &mut lines),
            DiagnosticsSection::Unknown(_) => {},
        }
    }
    lines
}

fn section_names(sections: &[DiagnosticsSection]) -> String {
    let names: Vec<String> = sections.iter().map(|section| section.name()).collect();
    names.join(", ")
}

fn panes(report: &DiagnosticsReport, lines: &mut Vec<String>) {
    lines.push("Panes".to_string());
    let rows = report
        .panes
        .iter()
        .map(|pane| {
            let id = pane_label(pane.pane_id);
            let tab = pane.tab_position.to_string();
            let grids = pane.grid_count.to_string();
            match &pane.grid {
                Some(grid) => vec![
                    id,
                    tab,
                    format!("{}x{}", grid.viewport_cols, grid.viewport_rows),
                    format!("{} / {}", grid.scrollback_lines, grid.scrollback_limit),
                    bytes(grid.scrollback_bytes_estimate),
                    grid.lines_discarded.to_string(),
                    grids,
                    if grid.in_alternate_screen {
                        "alternate screen".to_string()
                    } else {
                        String::new()
                    },
                ],
                None => vec![
                    id,
                    tab,
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    grids,
                    String::new(),
                ],
            }
        })
        .collect();
    table(
        lines,
        &[
            "PANE",
            "TAB",
            "VIEWPORT",
            "SCROLLBACK",
            "SIZE",
            "DISCARDED",
            "GRIDS",
            "",
        ],
        rows,
    );
}

fn memory(report: &DiagnosticsReport, lines: &mut Vec<String>) {
    lines.push("Memory".to_string());
    let Some(memory) = &report.memory else {
        return;
    };
    let rows = vec![
        vec![
            "process".to_string(),
            memory
                .process_rss_bytes
                .map(bytes)
                .unwrap_or_else(|| "unknown".to_string()),
        ],
        vec![
            "scrollback".to_string(),
            format!("{} (estimate)", bytes(memory.scrollback_bytes_estimate)),
        ],
        vec![
            "plugins".to_string(),
            bytes(memory.plugin_linear_memory_bytes),
        ],
        vec![
            "grapheme clusters".to_string(),
            format!(
                "{} of {}, {} entries",
                bytes(memory.grapheme_cluster_bytes),
                bytes(memory.grapheme_cluster_limit_bytes),
                memory.grapheme_cluster_entries
            ),
        ],
        vec![
            "kitty images".to_string(),
            format!(
                "{} of {}, {} images",
                bytes(memory.kitty_image_bytes),
                bytes(memory.kitty_image_quota_bytes),
                memory.kitty_image_count
            ),
        ],
    ];
    for row in rows {
        lines.push(format!("  {:<18} {}", row[0], row[1]));
    }
}

fn plugins(report: &DiagnosticsReport, lines: &mut Vec<String>) {
    lines.push("Plugins".to_string());
    let rows = report
        .plugins
        .iter()
        .map(|plugin| {
            let instances = if plugin.is_shared {
                let slots: Vec<String> = plugin.slot_ids.iter().map(u32::to_string).collect();
                format!("shared, slots {}", slots.join(" "))
            } else {
                plugin.instances.to_string()
            };
            vec![
                plugin.plugin_id.to_string(),
                plugin.location.clone(),
                instances,
                plugin.workers.to_string(),
                bytes(plugin.linear_memory_bytes),
                format!(
                    "{} of {}",
                    bytes(plugin.largest_linear_memory_bytes),
                    bytes(plugin.linear_memory_limit_bytes)
                ),
                plugin
                    .executor_thread
                    .map(|thread| format!("#{thread}"))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    table(
        lines,
        &[
            "ID",
            "LOCATION",
            "INSTANCES",
            "WORKERS",
            "MEMORY",
            "LARGEST",
            "THREAD",
        ],
        rows,
    );
    if report.plugin_executor_threads.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push("Plugin executor threads".to_string());
    let rows = report
        .plugin_executor_threads
        .iter()
        .map(|thread| {
            let ids: Vec<String> = thread.plugin_ids.iter().map(u32::to_string).collect();
            vec![
                format!("#{}", thread.index),
                thread.jobs_in_flight.to_string(),
                ids.join(" "),
            ]
        })
        .collect();
    table(lines, &["THREAD", "JOBS", "PLUGINS"], rows);
}

fn queues(report: &DiagnosticsReport, lines: &mut Vec<String>) {
    lines.push("Queues".to_string());
    let rows = report
        .queues
        .iter()
        .map(|queue| {
            vec![
                queue.name.clone(),
                queue.depth.to_string(),
                queue
                    .capacity
                    .map(|capacity| capacity.to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    table(lines, &["QUEUE", "WAITING", "LIMIT"], rows);
}

fn clients(report: &DiagnosticsReport, lines: &mut Vec<String>) {
    lines.push("Clients".to_string());
    let rows = report
        .clients
        .iter()
        .map(|client| {
            let mut notes = vec![];
            if client.structured_frames {
                notes.push("structured frames".to_string());
            }
            if let Some(followed) = client.watching {
                notes.push(format!("watching client {followed}"));
            }
            vec![
                client.client_id.to_string(),
                kind_name(client.kind),
                client
                    .output_backlog
                    .map(|backlog| format!("{backlog} / {}", client.output_backlog_limit))
                    .unwrap_or_default(),
                client.last_frame_ack_us.map(duration).unwrap_or_default(),
                client
                    .frame_awaiting_ack_us
                    .map(duration)
                    .unwrap_or_default(),
                notes.join(", "),
            ]
        })
        .collect();
    table(
        lines,
        &[
            "CLIENT",
            "KIND",
            "BACKLOG",
            "LAST FRAME ACK",
            "WAITING FOR ACK",
            "",
        ],
        rows,
    );
}

/// Left-aligned columns, two spaces apart, indented by two.
fn table(lines: &mut Vec<String>, header: &[&str], rows: Vec<Vec<String>>) {
    let mut widths: Vec<usize> = header.iter().map(|cell| cell.chars().count()).collect();
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let render = |cells: Vec<String>| -> String {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        format!("  {}", padded.join("  ")).trim_end().to_string()
    };
    lines.push(render(header.iter().map(|cell| cell.to_string()).collect()));
    for row in rows {
        lines.push(render(row));
    }
}

fn pane_label(pane_id: PaneId) -> String {
    match pane_id {
        PaneId::Terminal(id) => format!("terminal_{id}"),
        PaneId::Plugin(id) => format!("plugin_{id}"),
    }
}

fn kind_name(kind: ClientKind) -> String {
    match kind {
        ClientKind::Terminal => "terminal".to_string(),
        ClientKind::DesktopWindow => "desktop window".to_string(),
        ClientKind::WebBrowser => "web browser".to_string(),
        ClientKind::Watcher => "watcher".to_string(),
        ClientKind::Cli => "cli".to_string(),
        ClientKind::Unknown(number) => format!("kind {number}"),
    }
}

/// Binary units with one decimal above a KiB: `1.5 MiB`.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `850 µs`, `1.9 ms`, `2.4 s`.
fn duration(us: u64) -> String {
    if us < 1_000 {
        format!("{us} µs")
    } else if us < 1_000_000 {
        format!("{:.1} ms", us as f64 / 1_000.0)
    } else {
        format!("{:.1} s", us as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::diagnostics::{
        ClientDiagnostics, GridDiagnostics, MemoryDiagnostics, PaneDiagnostics, PluginDiagnostics,
        QueueDiagnostics,
    };

    fn sample() -> DiagnosticsReport {
        DiagnosticsReport {
            sections: DiagnosticsSection::ALL.to_vec(),
            unsupported_sections: vec![DiagnosticsSection::Unknown(9)],
            panes: vec![
                PaneDiagnostics {
                    pane_id: PaneId::Terminal(0),
                    tab_position: 0,
                    grid_count: 1,
                    grid: Some(GridDiagnostics {
                        viewport_rows: 44,
                        viewport_cols: 99,
                        scrollback_lines: 10_000,
                        scrollback_limit: 10_000,
                        scrollback_cells: 90_000,
                        scrollback_bytes_estimate: 1_920_000,
                        lines_discarded: 20_000,
                        in_alternate_screen: true,
                    }),
                },
                PaneDiagnostics {
                    pane_id: PaneId::Plugin(5),
                    tab_position: 0,
                    grid_count: 2,
                    grid: None,
                },
            ],
            memory: Some(MemoryDiagnostics {
                process_rss_bytes: Some(212 * 1024 * 1024),
                plugin_linear_memory_bytes: 9 * 1024 * 1024,
                ..Default::default()
            }),
            plugins: vec![PluginDiagnostics {
                plugin_id: 5,
                location: "zellij:tab-bar".to_string(),
                instances: 2,
                linear_memory_bytes: 3 * 1024 * 1024,
                largest_linear_memory_bytes: 1536 * 1024,
                linear_memory_limit_bytes: 16 * 1024 * 1024,
                executor_thread: Some(0),
                ..Default::default()
            }],
            queues: vec![QueueDiagnostics {
                name: "screen_bounded".to_string(),
                depth: 50,
                capacity: Some(50),
            }],
            clients: vec![ClientDiagnostics {
                client_id: 6,
                kind: ClientKind::Watcher,
                watching: Some(1),
                output_backlog: Some(0),
                output_backlog_limit: 5_000,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn the_text_summary_has_a_block_per_section() {
        let text = as_text(&sample()).join("\n");
        assert!(
            text.contains("This server can't report: section 9"),
            "{text}"
        );
        for heading in ["Panes", "Memory", "Plugins", "Queues", "Clients"] {
            assert!(
                text.lines().any(|line| line == heading),
                "{heading} in {text}"
            );
        }
        assert!(text.contains("terminal_0"), "{text}");
        assert!(text.contains("10000 / 10000"), "{text}");
        assert!(text.contains("alternate screen"), "{text}");
        assert!(text.contains("process            212.0 MiB"), "{text}");
        assert!(text.contains("1.5 MiB of 16.0 MiB"), "{text}");
        assert!(text.contains("screen_bounded  50       50"), "{text}");
        assert!(text.contains("watching client 1"), "{text}");
        assert!(
            text.lines().all(|line| line == line.trim_end()),
            "no trailing spaces"
        );
    }

    #[test]
    fn json_wraps_the_report_with_the_server_version() {
        let json = as_json(&sample()).join("\n");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["zellij_version"], VERSION);
        assert!(value["unix_time_ms"].as_u64().unwrap() > 0);
        let report: DiagnosticsReport = serde_json::from_value(value["report"].clone()).unwrap();
        assert_eq!(report, sample());
    }

    #[test]
    fn units_are_readable() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(duration(850), "850 µs");
        assert_eq!(duration(1_850), "1.9 ms");
    }
}
