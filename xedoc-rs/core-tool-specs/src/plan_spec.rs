use serde_json::json;
use std::collections::BTreeMap;
use xedoc_tools::JsonSchema;
use xedoc_tools::ResponsesApiTool;
use xedoc_tools::ToolSpec;

const UPDATE_PLAN_DESCRIPTION: &str = "\
Updates a plan for work with several distinct steps.
Provide optional context and concise, achievable steps with `pending`, `in_progress`, or `completed` status.
Keep exactly one step `in_progress` until all steps are complete.
Before beginning the next step, mark the previous step complete.
When the work is done, mark every step complete.
Skip this tool for simple work that does not benefit from a plan.";

const _: () = assert!(UPDATE_PLAN_DESCRIPTION.len() <= 1_024);

pub fn create_update_plan_tool() -> ToolSpec {
    let plan_item_properties = BTreeMap::from([
        (
            "step".to_string(),
            JsonSchema::string(Some("Task step text.".to_string())),
        ),
        (
            "status".to_string(),
            JsonSchema::string_enum(
                vec![json!("pending"), json!("in_progress"), json!("completed")],
                Some("Step status.".to_string()),
            ),
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "explanation".to_string(),
            JsonSchema::string(Some(
                "Optional explanation for this plan update.".to_string(),
            )),
        ),
        (
            "plan".to_string(),
            JsonSchema::array(
                JsonSchema::object(
                    plan_item_properties,
                    Some(vec!["step".to_string(), "status".to_string()]),
                    Some(false.into()),
                ),
                Some("The list of steps".to_string()),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "update_plan".to_string(),
        description: UPDATE_PLAN_DESCRIPTION.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["plan".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}
