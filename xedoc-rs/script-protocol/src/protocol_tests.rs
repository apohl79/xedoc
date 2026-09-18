use crate::ResponseOutcome;
use crate::ScriptResponse;
use crate::ScriptResult;

#[test]
fn deserializes_classifier_request() {
    let response: ScriptResponse = serde_json::from_str(
        r#"{
            "protocol": "xedoc.script/v1",
            "requestId": "request-1",
            "result": {
                "kind": "classifierRequest",
                "classifier": {
                    "continuation": "classifier:state",
                    "route": {
                        "providerId": "openai",
                        "model": "gpt-5.6-luna",
                        "reasoningEffort": "low"
                    },
                    "input": "classify this task"
                }
            }
        }"#,
    )
    .unwrap();

    let ResponseOutcome::Result {
        result: ScriptResult::ClassifierRequest { classifier },
    } = response.outcome
    else {
        panic!("expected classifier request");
    };

    assert_eq!(classifier.continuation.as_str(), "classifier:state");
    assert_eq!(classifier.route.provider_id.as_str(), "openai");
    assert_eq!(classifier.route.model.as_str(), "gpt-5.6-luna");
    assert_eq!(classifier.route.reasoning_effort.as_str(), "low");
    assert_eq!(classifier.input, "classify this task");
}
