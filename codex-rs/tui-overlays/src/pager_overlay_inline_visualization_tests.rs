use crate::history_cell::AgentMarkdownCell;
use crate::keymap::RuntimeKeymap;
use crate::pager_overlay::TranscriptOverlay;
use chrono::DateTime;
use codex_protocol::ThreadId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

fn buffer_to_text(buffer: &Buffer, width: u16) -> String {
    buffer
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let symbol = cell.symbol();
                    symbol
                        .strip_prefix("\x1b]8;;")
                        .and_then(|symbol| symbol.split_once('\x07'))
                        .and_then(|(_, symbol)| symbol.strip_suffix("\x1b]8;;\x07"))
                        .unwrap_or(symbol)
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn visualization_thread_dir(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    let thread_id = thread_id.to_string();
    let uuid = Uuid::parse_str(&thread_id).expect("thread id UUID");
    let timestamp = uuid.get_timestamp().expect("UUIDv7 timestamp");
    let (seconds, nanos) = timestamp.to_unix();
    let created_at = DateTime::from_timestamp(i64::try_from(seconds).expect("timestamp"), nanos)
        .expect("valid timestamp");
    codex_home
        .join("visualizations")
        .join(created_at.format("%Y/%m/%d").to_string())
        .join(thread_id)
}

#[test]
fn transcript_overlay_remeasures_visualization_when_artifact_becomes_available() {
    let codex_home = tempfile::tempdir().expect("temp codex home");
    let thread_id = ThreadId::new();
    let thread_dir = visualization_thread_dir(codex_home.path(), thread_id);
    let context =
        crate::inline_visualization::InlineVisualizationContext::new(codex_home.path(), thread_id)
            .expect("UUIDv7 thread id should provide a timestamp");
    fs::create_dir_all(&thread_dir).expect("create visualization directory");

    let cell = AgentMarkdownCell::new_with_inline_visualizations(
        "::codex-inline-vis{file=\"chart.html\"}".to_string(),
        Path::new("/workspace"),
        Some(context),
    );
    let mut overlay = TranscriptOverlay::new(vec![Arc::new(cell)], RuntimeKeymap::defaults().pager);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 240, /*height*/ 12,
    );
    let mut buffer = Buffer::empty(area);

    overlay.render(area, &mut buffer);
    let unavailable = buffer_to_text(&buffer, area.width);
    assert!(unavailable.contains("Visualization unavailable on this device"));

    fs::write(thread_dir.join("chart.html"), "<div>chart</div>")
        .expect("write visualization fragment");
    overlay.insert_cell(Arc::new(AgentMarkdownCell::new(
        "next message".to_string(),
        Path::new("/workspace"),
    )));
    buffer = Buffer::empty(area);
    overlay.render(area, &mut buffer);

    let available = buffer_to_text(&buffer, area.width);
    assert!(available.contains("Open chart visualization in the browser"));
    assert!(
        available.contains("file://"),
        "viewer URL was clipped: {available:?}"
    );

    let available = available
        .lines()
        .map(|line| {
            line.find("file://").map_or_else(
                || line.to_string(),
                |start| format!("{}file://<viewer-path>", &line[..start]),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(
        "transcript_overlay_visualization_becomes_available",
        format!("before:\n{unavailable}\n\nafter:\n{available}")
    );
}
