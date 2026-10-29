use crate::exec_command::relativize_to_home;
use crate::legacy_core::config::Config;
use crate::status::StatusAccountDisplay;
use crate::text_formatting;
use chrono::DateTime;
use chrono::Local;
use std::path::Path;
use unicode_width::UnicodeWidthStr;
use xedoc_protocol::account::PlanType;
use xedoc_utils_path_uri::PathConvention;
use xedoc_utils_path_uri::PathUri;

fn normalize_agents_display_path(path: &Path) -> String {
    dunce::simplified(path).display().to_string()
}

pub fn compose_model_display(
    model_name: &str,
    entries: &[(&str, String)],
) -> (String, Vec<String>) {
    let mut details: Vec<String> = Vec::new();
    if let Some((_, effort)) = entries.iter().find(|(k, _)| *k == "reasoning effort") {
        details.push(format!("reasoning {}", effort.to_ascii_lowercase()));
    }
    if let Some((_, summary)) = entries.iter().find(|(k, _)| *k == "reasoning summaries") {
        let summary = summary.trim();
        if summary.eq_ignore_ascii_case("none") || summary.eq_ignore_ascii_case("off") {
            details.push("summaries off".to_string());
        } else if !summary.is_empty() {
            details.push(format!("summaries {}", summary.to_ascii_lowercase()));
        }
    }

    (model_name.to_string(), details)
}

pub fn compose_agents_summary(config: &Config, paths: &[PathUri]) -> String {
    let mut rels: Vec<String> = Vec::new();

    for path in paths {
        // TODO(anp): Rationalize instruction-source summaries with the TUI's broader foreign-path
        // display strategy once other status surfaces can retain environment-native paths.
        if path.infer_path_convention() != Some(PathConvention::native()) {
            rels.push(path.inferred_native_path_string());
            continue;
        }
        let Ok(p) = path.to_abs_path() else {
            rels.push(path.inferred_native_path_string());
            continue;
        };
        let p = p.as_path();
        let file_name = p
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let display = if let Some(parent) = p.parent() {
            if parent == config.cwd.as_path() {
                file_name.clone()
            } else {
                let mut cur = config.cwd.as_path();
                let mut ups = 0usize;
                let mut reached = false;
                while let Some(c) = cur.parent() {
                    if cur == parent {
                        reached = true;
                        break;
                    }
                    cur = c;
                    ups += 1;
                }
                if reached {
                    let up = format!("..{}", std::path::MAIN_SEPARATOR);
                    format!("{}{}", up.repeat(ups), file_name)
                } else if let Ok(stripped) = p.strip_prefix(&config.cwd) {
                    normalize_agents_display_path(stripped)
                } else {
                    normalize_agents_display_path(p)
                }
            }
        } else {
            normalize_agents_display_path(p)
        };
        rels.push(display);
    }

    if rels.is_empty() {
        "<none>".to_string()
    } else {
        rels.join(", ")
    }
}

pub fn compose_account_display(
    account_display: Option<&StatusAccountDisplay>,
) -> Option<StatusAccountDisplay> {
    account_display.cloned()
}

pub fn plan_type_display_name(plan_type: PlanType) -> String {
    if plan_type.is_team_like() {
        "Business".to_string()
    } else if plan_type.is_business_like() {
        "Enterprise".to_string()
    } else if plan_type == PlanType::ProLite {
        "Pro Lite".to_string()
    } else {
        title_case(format!("{plan_type:?}").as_str())
    }
}

pub fn format_directory_display(directory: &Path, max_width: Option<usize>) -> String {
    let formatted = if let Some(rel) = relativize_to_home(directory) {
        if rel.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rel.display())
        }
    } else {
        directory.display().to_string()
    };

    if let Some(max_width) = max_width {
        if max_width == 0 {
            return String::new();
        }
        if UnicodeWidthStr::width(formatted.as_str()) > max_width {
            return text_formatting::center_truncate_path(&formatted, max_width);
        }
    }

    formatted
}

pub fn format_reset_timestamp(dt: DateTime<Local>, captured_at: DateTime<Local>) -> String {
    let time = dt.format("%H:%M").to_string();
    if dt.date_naive() == captured_at.date_naive() {
        time
    } else {
        format!("{time} on {}", dt.format("%-d %b"))
    }
}

fn title_case(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest = chars.as_str().to_ascii_lowercase();
    first.to_uppercase().collect::<String>() + &rest
}
