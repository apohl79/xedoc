use super::*;
use crate::bottom_pane::preview_line_for_title_items;
use pretty_assertions::assert_eq;
use ratatui::text::Line;

fn line_text(line: Line<'static>) -> String {
    line.spans
        .into_iter()
        .map(|span| span.content.into_owned())
        .collect()
}

fn status_preview_line_option(chat: &mut ChatWidget, items: &[StatusLineItem]) -> Option<String> {
    let preview_data = chat.status_surface_preview_data();
    preview_data
        .status_line_for_items(items.iter().copied(), /*use_theme_colors*/ true)
        .map(line_text)
}

fn status_preview_line(chat: &mut ChatWidget, items: &[StatusLineItem]) -> String {
    status_preview_line_option(chat, items).expect("status preview line")
}

fn title_preview_line(chat: &mut ChatWidget, items: &[TerminalTitleItem]) -> String {
    let preview_data = chat.terminal_title_preview_data();
    let preview =
        preview_line_for_title_items(items, &preview_data).expect("terminal title preview line");
    line_text(preview)
}

#[tokio::test]
async fn thread_title_falls_back_to_thread_id_when_unnamed() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);

    // Status line ThreadTitle still falls back to thread ID.
    assert_eq!(
        status_preview_line(&mut chat, &[StatusLineItem::ThreadTitle]),
        thread_id.to_string()
    );
    // Terminal title Thread falls back to project (cwd dir) name.
    assert_eq!(
        title_preview_line(&mut chat, &[TerminalTitleItem::Thread]),
        "project".to_string()
    );
}

#[tokio::test]
async fn missing_project_root_uses_different_status_and_title_preview_sources() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let status_preview = status_preview_line(&mut chat, &[StatusLineItem::ProjectRoot]);
    let title_preview = title_preview_line(&mut chat, &[TerminalTitleItem::Project]);

    assert_eq!(status_preview, "my-project");
    assert_eq!(title_preview, "project");
}

#[tokio::test]
async fn terminal_title_preview_uses_title_truncation_for_live_values() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let long_thread = "This thread title is intentionally much longer than forty-eight characters";
    let long_branch = "feature/this-branch-name-is-intentionally-longer-than-thirty-two";
    chat.thread_name = Some(long_thread.to_string());
    chat.status_line_branch = Some(long_branch.to_string());

    let preview = title_preview_line(
        &mut chat,
        &[TerminalTitleItem::Thread, TerminalTitleItem::GitBranch],
    );
    let truncated_thread =
        ChatWidget::truncate_terminal_title_part(long_thread.to_string(), /*max_chars*/ 48);
    let truncated_branch =
        ChatWidget::truncate_terminal_title_part(long_branch.to_string(), /*max_chars*/ 32);

    assert_eq!(preview, format!("{truncated_thread} | {truncated_branch}"));
}
