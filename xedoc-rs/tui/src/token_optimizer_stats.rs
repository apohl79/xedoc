use ratatui::style::Stylize;
use ratatui::text::Line;
use xedoc_app_server_protocol::TokenUsageOptimizerReadResponse;

pub(crate) fn stats_lines(response: &TokenUsageOptimizerReadResponse) -> Vec<Line<'static>> {
    let retrieval_rate = if response.insights.spilled == 0 {
        0.0
    } else {
        (response.insights.retrievals as f64 / response.insights.spilled as f64 * 100.0).min(100.0)
    };
    let kinds = response
        .insights
        .by_kind
        .iter()
        .take(3)
        .map(|item| format!("{} {}", item.dimension, group_digits(item.reductions)))
        .collect::<Vec<_>>()
        .join(" · ");

    let mut lines = vec![
        Line::from("Token optimizer stats".bold()),
        Line::from(vec![
            "  Reductions   ".into(),
            group_digits(response.reduction_count).into(),
            "        Tokens saved  ~".into(),
            group_digits(response.tokens_saved).into(),
        ]),
        Line::from(vec![
            "  Cost saved   ".into(),
            format_cost(response.cost_saved_usd).into(),
        ]),
        Line::from(vec![
            "  Retrieval    ".into(),
            format!("{retrieval_rate:.1}%").into(),
            format!(
                " ({} / {})",
                group_digits(response.insights.retrievals),
                group_digits(response.insights.spilled)
            )
            .into(),
        ]),
        Line::from(""),
        Line::from(vec![
            "By kind        ".dim(),
            if kinds.is_empty() {
                "(none)".dim()
            } else {
                kinds.into()
            },
        ]),
    ];

    let models = response
        .insights
        .by_model
        .iter()
        .take(5)
        .map(|item| {
            let cost = item
                .cost_saved_usd
                .map_or_else(|| "cost n/a".to_owned(), format_cost);
            format!(
                "{} {} ({cost})",
                item.dimension,
                group_digits(item.reductions)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    lines.push(Line::from(vec![
        "By model       ".dim(),
        if models.is_empty() {
            "(none)".dim()
        } else {
            models.into()
        },
    ]));

    if !response.insights.top_reductions.is_empty() {
        lines.extend([Line::from(""), Line::from("Top reductions".bold())]);
        lines.extend(response.insights.top_reductions.iter().take(3).map(|item| {
            Line::from(vec![
                format!("  {:<7} {:<5} ", item.tool_name, item.kind).into(),
                truncate_call_id(&item.call_id).into(),
                format!(
                    "  {} → {}  (~{} tok",
                    group_digits(item.bytes_in),
                    group_digits(item.bytes_out),
                    group_digits(item.tokens_saved)
                )
                .into(),
                match item.cost_saved_usd {
                    Some(cost) => format!(", {})", format_cost(cost)).into(),
                    None => ", cost n/a)".into(),
                },
            ])
        }));
    }

    lines
}

fn format_cost(cost_saved_usd: f64) -> String {
    format!("~${cost_saved_usd:.2}")
}

fn group_digits(value: i64) -> String {
    let value = value.to_string();
    let (sign, digits) = value
        .strip_prefix('-')
        .map_or(("", value.as_str()), |digits| ("-", digits));
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{sign}{grouped}")
}

fn truncate_call_id(call_id: &str) -> String {
    const MAX_CHARS: usize = 14;
    if call_id.chars().count() <= MAX_CHARS {
        return call_id.to_owned();
    }
    let prefix = call_id.chars().take(8).collect::<String>();
    let suffix = call_id
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

#[cfg(test)]
#[path = "token_optimizer_stats_tests.rs"]
mod tests;
