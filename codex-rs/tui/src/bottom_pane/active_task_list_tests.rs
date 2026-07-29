use super::*;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use pretty_assertions::assert_eq;

#[test]
fn display_lines_header_uses_lighter_gray_background_and_black_foreground() {
    let mut list = ActiveTaskList::new();
    list.set_tasks(vec![PlanItemArg {
        step: "Inspect the task list".to_string(),
        status: StepStatus::InProgress,
    }]);

    let header = &list.display_lines(/*width*/ 80)[0];
    let header_style = crate::city_lights::active_list_header_style();

    assert_eq!(header.spans[0].style.bg, None);
    assert_eq!(header.spans[0].style.fg, None);
    assert_eq!(header.spans[1].style.bg, header_style.bg);
    assert_eq!(header.spans[1].style.fg, header_style.fg);
    assert_eq!(header.spans[2].style.bg, None);
    assert_eq!(header.spans[2].style.fg, None);
    assert_eq!(header.spans[2].content.as_ref(), " 0/1");
}
