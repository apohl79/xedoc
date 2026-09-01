use super::super::PreviousSectionState;
use super::super::test_support::render_section_cases;
use super::*;

#[test]
fn snapshots() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;
    use PreviousSectionState::Unknown;

    let empty = AgentsMdState::default();
    let project_formatter = AgentsMdState::new(Some(UserInstructions {
        directory: None,
        text: "use the project formatter".to_string(),
    }));
    let old = AgentsMdState::new(Some(UserInstructions {
        directory: None,
        text: "old instructions".to_string(),
    }));
    let new = AgentsMdState::new(Some(UserInstructions {
        directory: None,
        text: "new instructions".to_string(),
    }));

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Absent),
        (Absent, Known(&empty)),
        (Absent, Known(&project_formatter)),
        (Known(&project_formatter), Known(&project_formatter)),
        (Known(&old), Known(&new)),
        (Known(&new), Known(&empty)),
        (Unknown, Known(&new)),
        (Unknown, Known(&empty)),
    ]));
}
