use xedoc_app_server_protocol::WebSearchAction;

use crate::history_cell::HistoryCell;
use crate::history_cell::new_web_search_call;

#[test]
fn completed_direct_web_search_renders_its_query() {
    let cell = new_web_search_call(
        "search-1".to_string(),
        "local models".to_string(),
        WebSearchAction::Search {
            query: Some("local models".to_string()),
            queries: None,
        },
    );
    let rendered = cell.raw_lines()[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    insta::assert_snapshot!(rendered, @"Searched the web for local models");
}
