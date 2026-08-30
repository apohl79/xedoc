use super::ContextualUserFragment;

#[derive(Debug, Clone, PartialEq)]
pub struct UserInstructions {
    pub directory: Option<String>,
    pub text: String,
}

impl ContextualUserFragment for UserInstructions {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("# AGENTS.md instructions", "</INSTRUCTIONS>")
    }

    fn body(&self) -> String {
        let directory = self
            .directory
            .as_ref()
            .map(|directory| format!(" for {directory}"))
            .unwrap_or_default();
        format!("{directory}\n\n<INSTRUCTIONS>\n{}\n", self.text)
    }
}
