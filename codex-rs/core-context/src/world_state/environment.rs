use super::PreviousSectionState;
use super::WorldStateSection;
use crate::ContextualUserFragment;
use crate::environment_context::FileSystemContext;
use crate::environment_context::NetworkContext;
use crate::environment_context::push_xml_escaped_text;
use codex_utils_path_uri::PathUri;
use codex_utils_string::approx_token_count;
use codex_utils_string::truncate_middle_with_token_budget;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

const MAX_SUBAGENTS_CONTEXT_TOKENS: usize = 1_000;

/// Environment values visible to the model.
#[derive(Clone, Debug, Default)]
pub struct EnvironmentsState {
    pub environments: BTreeMap<String, EnvironmentState>,
    pub current_date: Option<String>,
    pub timezone: Option<String>,
    pub network: Option<NetworkContext>,
    pub filesystem: Option<FileSystemContext>,
    pub subagents: Option<String>,
}

impl EnvironmentsState {
    pub fn with_subagents(mut self, subagents: String) -> Self {
        if !subagents.is_empty() {
            self.subagents = Some(truncate_to_token_budget(
                &subagents,
                MAX_SUBAGENTS_CONTEXT_TOKENS,
            ));
        }
        self
    }

    fn rendered_full(&self) -> RenderedEnvironments {
        RenderedEnvironments {
            updates: self
                .environments
                .iter()
                .map(|(id, environment)| {
                    (id.clone(), EnvironmentUpdate::Current(environment.clone()))
                })
                .collect(),
            legacy_single: is_legacy_single(&self.environments),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            network: self.network.clone(),
            filesystem: self.filesystem.clone(),
            subagents: self
                .subagents
                .as_deref()
                .map(|subagents| truncate_to_token_budget(subagents, MAX_SUBAGENTS_CONTEXT_TOKENS)),
        }
    }
}

impl WorldStateSection for EnvironmentsState {
    const ID: &'static str = "environments";
    type Snapshot = EnvironmentsSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        EnvironmentsSnapshot {
            environments: self
                .environments
                .iter()
                .map(|(id, environment)| {
                    (
                        id.clone(),
                        EnvironmentSnapshot {
                            cwd: environment.cwd.inferred_native_path_string(),
                            status: environment.status,
                            shell: environment.shell.clone(),
                        },
                    )
                })
                .collect(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            network: self.network.as_ref().map(NetworkContext::render),
            filesystem: self.filesystem.as_ref().map(FileSystemContext::render),
            subagents: self
                .subagents
                .as_deref()
                .map(|subagents| truncate_to_token_budget(subagents, MAX_SUBAGENTS_CONTEXT_TOKENS)),
        }
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let current = self.snapshot();
        let empty = EnvironmentsSnapshot::default();
        let previous = match previous {
            PreviousSectionState::Known(previous) => previous,
            PreviousSectionState::Absent | PreviousSectionState::Unknown => &empty,
        };
        let turn_context_values_changed = current.current_date != previous.current_date
            || current.timezone != previous.timezone
            || current.network != previous.network
            || current.filesystem != previous.filesystem
            || current.subagents != previous.subagents;
        let mut updates = self
            .environments
            .iter()
            .filter(|(id, _)| {
                let environment = &current.environments[*id];
                previous
                    .environments
                    .get(*id)
                    .is_none_or(|previous| !environment.has_same_diff_value(previous))
            })
            .map(|(id, environment)| (id.clone(), EnvironmentUpdate::Current(environment.clone())))
            .collect::<BTreeMap<_, _>>();
        updates.extend(
            previous
                .environments
                .keys()
                .filter(|id| !self.environments.contains_key(*id))
                .map(|id| (id.clone(), EnvironmentUpdate::Unavailable)),
        );
        let legacy_single = is_legacy_single(&self.environments)
            && updates
                .values()
                .all(|update| matches!(update, EnvironmentUpdate::Current(_)));
        (!updates.is_empty() || turn_context_values_changed).then(|| {
            Box::new(RenderedEnvironments {
                updates,
                legacy_single,
                current_date: self.current_date.clone(),
                timezone: self.timezone.clone(),
                network: self.network.clone(),
                filesystem: self.filesystem.clone(),
                subagents: self.subagents.as_deref().map(|subagents| {
                    truncate_to_token_budget(subagents, MAX_SUBAGENTS_CONTEXT_TOKENS)
                }),
            }) as Box<dyn ContextualUserFragment>
        })
    }
}

impl ContextualUserFragment for EnvironmentsState {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        environment_context_markers()
    }

    fn body(&self) -> String {
        self.rendered_full().body()
    }
}

struct RenderedEnvironments {
    updates: BTreeMap<String, EnvironmentUpdate>,
    legacy_single: bool,
    current_date: Option<String>,
    timezone: Option<String>,
    network: Option<NetworkContext>,
    filesystem: Option<FileSystemContext>,
    subagents: Option<String>,
}

enum EnvironmentUpdate {
    Current(EnvironmentState),
    Unavailable,
}

impl ContextualUserFragment for RenderedEnvironments {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        environment_context_markers()
    }

    fn body(&self) -> String {
        let mut rendered = "\n".to_string();
        let bounded_subagents = self
            .subagents
            .as_deref()
            .map(|subagents| truncate_to_token_budget(subagents, MAX_SUBAGENTS_CONTEXT_TOKENS));
        if self.legacy_single {
            if let Some(EnvironmentUpdate::Current(environment)) = self.updates.values().next() {
                push_environment_values(&mut rendered, environment, "  ");
            }
        } else if !self.updates.is_empty() {
            rendered.push_str("  <environments>\n");
            for (id, update) in &self.updates {
                match update {
                    EnvironmentUpdate::Current(environment) => {
                        rendered.push_str("    <environment id=\"");
                        push_xml_escaped_text(&mut rendered, id);
                        rendered.push('"');
                        rendered.push_str(">\n");
                        push_environment_values(&mut rendered, environment, "      ");
                        rendered.push_str("    </environment>\n");
                    }
                    EnvironmentUpdate::Unavailable => {
                        rendered.push_str("    <environment id=\"");
                        push_xml_escaped_text(&mut rendered, id);
                        rendered.push_str("\" status=\"unavailable\" />\n");
                    }
                }
            }
            rendered.push_str("  </environments>\n");
        }
        push_optional_element(&mut rendered, "current_date", self.current_date.as_deref());
        push_optional_element(&mut rendered, "timezone", self.timezone.as_deref());
        if let Some(network) = &self.network {
            rendered.push_str("  ");
            rendered.push_str(&network.render());
            rendered.push('\n');
        }
        if let Some(filesystem) = &self.filesystem {
            rendered.push_str("  ");
            rendered.push_str(&filesystem.render());
            rendered.push('\n');
        }
        if let Some(subagents) = bounded_subagents.as_deref() {
            rendered.push_str("  <subagents>\n");
            for line in subagents.lines() {
                rendered.push_str("    ");
                rendered.push_str(line);
                rendered.push('\n');
            }
            rendered.push_str("  </subagents>\n");
        }
        rendered
    }
}

fn push_environment_values(rendered: &mut String, environment: &EnvironmentState, indent: &str) {
    rendered.push_str(indent);
    rendered.push_str("<cwd>");
    push_xml_escaped_text(rendered, &environment.cwd.inferred_native_path_string());
    rendered.push_str("</cwd>\n");
    if environment.status == EnvironmentStatus::Starting {
        rendered.push_str(indent);
        rendered.push_str("<status>starting</status>\n");
    }
    if let Some(shell) = &environment.shell {
        rendered.push_str(indent);
        rendered.push_str("<shell>");
        push_xml_escaped_text(rendered, shell);
        rendered.push_str("</shell>\n");
    }
}

fn push_optional_element(rendered: &mut String, name: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    rendered.push_str("  <");
    rendered.push_str(name);
    rendered.push('>');
    push_xml_escaped_text(rendered, value);
    rendered.push_str("</");
    rendered.push_str(name);
    rendered.push_str(">\n");
}

fn truncate_to_token_budget(text: &str, max_tokens: usize) -> String {
    let mut budget = max_tokens;
    loop {
        let (candidate, _) = truncate_middle_with_token_budget(text, budget);
        let candidate_tokens = approx_token_count(&candidate);
        if candidate_tokens <= max_tokens {
            return candidate;
        }
        if budget == 0 {
            return String::new();
        }
        budget = budget.saturating_sub((candidate_tokens - max_tokens).max(1));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentState {
    cwd: PathUri,
    status: EnvironmentStatus,
    shell: Option<String>,
}

impl EnvironmentState {
    pub fn available(cwd: PathUri, shell: Option<String>) -> Self {
        Self {
            cwd,
            status: EnvironmentStatus::Available,
            shell,
        }
    }

    pub fn starting(cwd: PathUri) -> Self {
        Self {
            cwd,
            status: EnvironmentStatus::Starting,
            shell: None,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
pub struct EnvironmentsSnapshot {
    environments: BTreeMap<String, EnvironmentSnapshot>,
    current_date: Option<String>,
    timezone: Option<String>,
    network: Option<String>,
    filesystem: Option<String>,
    subagents: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct EnvironmentSnapshot {
    cwd: String,
    status: EnvironmentStatus,
    shell: Option<String>,
}

impl EnvironmentSnapshot {
    fn has_same_diff_value(&self, other: &Self) -> bool {
        self.cwd == other.cwd
            && self.status == other.status
            && self
                .shell
                .as_ref()
                .zip(other.shell.as_ref())
                .is_none_or(|(current, previous)| current == previous)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EnvironmentStatus {
    Starting,
    Available,
}

fn is_legacy_single(environments: &BTreeMap<String, EnvironmentState>) -> bool {
    environments.len() == 1
        && environments
            .values()
            .all(|environment| environment.status == EnvironmentStatus::Available)
}

fn environment_context_markers() -> (&'static str, &'static str) {
    (
        codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG,
        codex_protocol::protocol::ENVIRONMENT_CONTEXT_CLOSE_TAG,
    )
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "environment_render_tests.rs"]
mod render_tests;
