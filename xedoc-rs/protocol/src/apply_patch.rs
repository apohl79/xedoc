/// Written patch format for providers that translate the freeform `apply_patch` tool to a function.
pub const APPLY_PATCH_TOOL_INSTRUCTIONS: &str =
    include_str!("prompts/apply_patch_tool_instructions.md");

const _: () = assert!(APPLY_PATCH_TOOL_INSTRUCTIONS.len() <= 3_584);
