use crate::city_lights::CityLightsStylize;
use color_eyre::eyre::Result;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::StreamExt;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line_for_keymap;
use crate::config_update::format_config_error;
use crate::hooks_rpc::HookTrustUpdate;
use crate::hooks_rpc::fetch_hooks_list;
use crate::hooks_rpc::hook_needs_review;
use crate::hooks_rpc::hooks_list_entry_for_cwd;
use crate::hooks_rpc::write_hook_trusts;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use std::path::PathBuf;
use xedoc_app_server_client::AppServerRequestHandle;
use xedoc_app_server_protocol::HooksListEntry;

pub(crate) enum StartupHooksReviewOutcome {
    Continue,
    OpenHooksBrowser(HooksListEntry),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupHooksReviewSelection {
    ReviewHooks,
    TrustAllAndContinue,
    ContinueWithoutTrusting,
}

pub(crate) async fn load_startup_hooks_review_entry(
    request_handle: AppServerRequestHandle,
    cwd: PathBuf,
) -> HooksListEntry {
    let response = match fetch_hooks_list(request_handle, cwd.clone()).await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!("failed to load startup hook review state: {err:#}");
            return HooksListEntry {
                cwd,
                hooks: Vec::new(),
                warnings: Vec::new(),
                errors: Vec::new(),
            };
        }
    };
    hooks_list_entry_for_cwd(response, &cwd)
}

pub(crate) async fn maybe_run_startup_hooks_review(
    app_server: &mut AppServerSession,
    tui: &mut Tui,
    config: &Config,
    bypass_hook_trust: bool,
    entry: HooksListEntry,
) -> Result<StartupHooksReviewOutcome> {
    if !review_is_needed(bypass_hook_trust, &entry) {
        return Ok(StartupHooksReviewOutcome::Continue);
    }

    run_startup_hooks_review_app(app_server, tui, config, entry).await
}

async fn run_startup_hooks_review_app(
    app_server: &mut AppServerSession,
    tui: &mut Tui,
    config: &Config,
    entry: HooksListEntry,
) -> Result<StartupHooksReviewOutcome> {
    let keymap = RuntimeKeymap::from_config(&config.tui_keymap)
        .map_err(|err| color_eyre::eyre::eyre!(err))?;
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let mut trust_all_error = None;
    let mut view = selection_view(
        &entry,
        trust_all_error.as_deref(),
        /*trusting_all*/ false,
        app_event_tx.clone(),
        &keymap,
    );
    draw_view(tui, &view)?;

    let tui_events = tui.event_stream();
    tokio::pin!(tui_events);

    loop {
        let Some(event) = tui_events.next().await else {
            return Ok(StartupHooksReviewOutcome::Continue);
        };
        match event {
            TuiEvent::Key(key_event) => {
                if matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    view.handle_key_event(key_event);
                }
                let Some(selection) = selected_choice(&mut view) else {
                    draw_view(tui, &view)?;
                    continue;
                };
                match selection {
                    StartupHooksReviewSelection::ReviewHooks => {
                        return Ok(StartupHooksReviewOutcome::OpenHooksBrowser(entry));
                    }
                    StartupHooksReviewSelection::ContinueWithoutTrusting => {
                        return Ok(StartupHooksReviewOutcome::Continue);
                    }
                    StartupHooksReviewSelection::TrustAllAndContinue => {
                        view = selection_view(
                            &entry,
                            trust_all_error.as_deref(),
                            /*trusting_all*/ true,
                            app_event_tx.clone(),
                            &keymap,
                        );
                        draw_view(tui, &view)?;
                        let result = write_hook_trusts(
                            app_server.request_handle(),
                            entry
                                .hooks
                                .iter()
                                .filter(|hook| hook_needs_review(hook))
                                .map(|hook| HookTrustUpdate {
                                    key: hook.key.clone(),
                                    current_hash: hook.current_hash.clone(),
                                })
                                .collect(),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|err| {
                            format!("Failed to trust hooks: {}", format_config_error(&err))
                        });
                        match result {
                            Ok(()) => return Ok(StartupHooksReviewOutcome::Continue),
                            Err(err) => {
                                trust_all_error = Some(err);
                                view = selection_view(
                                    &entry,
                                    trust_all_error.as_deref(),
                                    /*trusting_all*/ false,
                                    app_event_tx.clone(),
                                    &keymap,
                                );
                                draw_view(tui, &view)?;
                            }
                        }
                    }
                }
            }
            TuiEvent::Paste(_) => {}
            TuiEvent::Draw | TuiEvent::Resize => draw_view(tui, &view)?,
        }
    }
}

fn selected_choice(view: &mut ListSelectionView) -> Option<StartupHooksReviewSelection> {
    if !view.is_complete() {
        return None;
    }
    match view.take_last_selected_index() {
        Some(0) => Some(StartupHooksReviewSelection::ReviewHooks),
        Some(1) => Some(StartupHooksReviewSelection::TrustAllAndContinue),
        Some(2) | None => Some(StartupHooksReviewSelection::ContinueWithoutTrusting),
        Some(_) => None,
    }
}

fn selection_view(
    entry: &HooksListEntry,
    trust_all_error: Option<&str>,
    trusting_all: bool,
    app_event_tx: AppEventSender,
    keymap: &RuntimeKeymap,
) -> ListSelectionView {
    ListSelectionView::new(
        selection_view_params(entry, trust_all_error, trusting_all, keymap),
        app_event_tx,
        keymap.list.clone(),
    )
}

#[allow(clippy::disallowed_methods)]
fn selection_view_params(
    entry: &HooksListEntry,
    trust_all_error: Option<&str>,
    trusting_all: bool,
    keymap: &RuntimeKeymap,
) -> SelectionViewParams {
    let count = review_needed_count(entry);
    let count_line = match count {
        1 => "1 hook is new or changed.".to_string(),
        count => format!("{count} hooks are new or changed."),
    };
    let mut header = ColumnRenderable::new();
    header.push(Line::from("Hooks need review".bold()));
    header.push(Line::from(count_line).yellow());
    header.push(Line::from(
        "Hooks can run outside the sandbox after you trust them.".dim(),
    ));
    if let Some(error) = trust_all_error {
        header.push(
            Paragraph::new(Line::from(error.to_string()).cl_red()).wrap(Wrap { trim: false }),
        );
    } else if trusting_all {
        header.push(Line::from("Trusting hooks...".dim()));
    }

    SelectionViewParams {
        footer_hint: Some(standard_popup_hint_line_for_keymap(&keymap.list)),
        items: vec![
            selection_item("Review hooks", trusting_all),
            selection_item("Trust all and continue", trusting_all),
            selection_item("Continue without trusting (hooks won't run)", trusting_all),
        ],
        header: Box::new(header),
        ..Default::default()
    }
}

fn review_needed_count(entry: &HooksListEntry) -> usize {
    entry
        .hooks
        .iter()
        .filter(|hook| hook_needs_review(hook))
        .count()
}

fn review_is_needed(bypass_hook_trust: bool, entry: &HooksListEntry) -> bool {
    !bypass_hook_trust && review_needed_count(entry) > 0
}

fn selection_item(name: &str, is_disabled: bool) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        dismiss_on_select: true,
        is_disabled,
        ..Default::default()
    }
}

fn draw_view(tui: &mut Tui, view: &ListSelectionView) -> Result<()> {
    tui.draw(u16::MAX, |frame| {
        let area = frame.area();
        frame.render_widget_ref(Clear, area);
        let view_area = Rect::new(
            area.x,
            area.y,
            area.width,
            view.desired_height(area.width).min(area.height),
        );
        frame.render_widget_ref(&StandaloneSelectionView { view }, view_area);
    })?;
    Ok(())
}

struct StandaloneSelectionView<'a> {
    view: &'a ListSelectionView,
}

impl WidgetRef for &StandaloneSelectionView<'_> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        self.view.render(area, buf);
    }
}
