use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use tempfile::TempDir;
use toml::Value as TomlValue;
use xedoc_exec::CommandExecutionStatus;
use xedoc_exec::ThreadEvent;
use xedoc_exec::ThreadItemDetails;
use xedoc_http_client::ClientRouteClass;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_login::AuthManager;
use xedoc_protocol::config_types::ModelProviderAuthInfo;
use xedoc_utils_absolute_path::AbsolutePathBufGuard;

const REFERENCE_HOME_ENV: &str = "XEDOC_PROVIDER_REFERENCE_HOME";
const PROVIDERS: [&str; 3] = ["anthropic", "google", "deepseek"];
const RETIRED_REFERENCE_MODELS: [(&str, &str); 1] = [("google", "gemini-3-pro-preview")];
const TEXT_MARKER: &str = "XEDOC_PROVIDER_E2E_OK";
const TOOL_MARKER: &str = "XEDOC_PROVIDER_TOOL_OK";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Text,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CaseOutcome {
    provider: String,
    model: String,
    flow: Flow,
    process_succeeded: bool,
    marker_seen: bool,
    turn_completed: bool,
    usage_reported: bool,
    command_completed: Option<bool>,
    diagnostic: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ContractState {
    process_succeeded: bool,
    marker_seen: bool,
    turn_completed: bool,
    usage_reported: bool,
    command_completed: Option<bool>,
}

impl CaseOutcome {
    fn expected(&self) -> Self {
        Self {
            provider: self.provider.clone(),
            model: self.model.clone(),
            flow: self.flow,
            process_succeeded: true,
            marker_seen: true,
            turn_completed: true,
            usage_reported: true,
            command_completed: (self.flow == Flow::Tool).then_some(true),
            diagnostic: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModelCatalog {
    data: Vec<CatalogModel>,
}

#[derive(Debug, Deserialize)]
struct CatalogModel {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ReferenceProviderConfig {
    base_url: String,
    auth: ModelProviderAuthInfo,
    #[serde(default)]
    query_params: HashMap<String, String>,
}

struct ProviderFixture {
    provider: String,
    reference: ReferenceProviderConfig,
    home: TempDir,
    cwd: TempDir,
}

#[tokio::test]
async fn proxy_reference_matches_all_provider_model_contracts() -> Result<()> {
    let source_home = reference_home()?;
    let source_config = read_source_config(&source_home)?;
    let mut actual = Vec::new();
    for provider in PROVIDERS {
        actual.extend(
            run_provider_cases(&source_config, &source_home, provider)
                .await
                .with_context(|| format!("run {provider} reference cases"))?,
        );
    }
    let expected = actual.iter().map(CaseOutcome::expected).collect::<Vec<_>>();

    assert_eq!(actual, expected);
    Ok(())
}

fn reference_home() -> Result<PathBuf> {
    let path = std::env::var_os(REFERENCE_HOME_ENV)
        .map(PathBuf::from)
        .context(format!(
            "set {REFERENCE_HOME_ENV} to the working proxy-backed XEDOC_HOME"
        ))?;
    if !path.join("config.toml").is_file() {
        bail!("{REFERENCE_HOME_ENV} must contain config.toml");
    }
    Ok(path)
}

fn read_source_config(source_home: &Path) -> Result<TomlValue> {
    let path = source_home.join("config.toml");
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))
}

async fn run_provider_cases(
    source_config: &TomlValue,
    source_home: &Path,
    provider: &str,
) -> Result<Vec<CaseOutcome>> {
    let fixture = ProviderFixture::new(source_config, source_home, provider)?;
    let models = fixture.discover_models().await?;
    let mut cases = models
        .iter()
        .map(|model| fixture.run_case(model, Flow::Text))
        .collect::<Result<Vec<_>>>()?;
    let tool_model = preferred_tool_model(&models);
    cases.push(fixture.run_case(tool_model, Flow::Tool)?);
    Ok(cases)
}

impl ProviderFixture {
    fn new(source_config: &TomlValue, source_home: &Path, provider: &str) -> Result<Self> {
        let home = tempfile::tempdir().context("create isolated XEDOC_HOME")?;
        let cwd = tempfile::tempdir().context("create isolated provider workspace")?;
        let reference = reference_provider_config(source_config, source_home, provider)?;
        let config = isolated_provider_config(source_config, provider)?;
        let path = home.path().join("config.toml");
        fs::write(&path, toml::to_string(&config)?)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(Self {
            provider: provider.to_string(),
            reference,
            home,
            cwd,
        })
    }

    async fn discover_models(&self) -> Result<Vec<String>> {
        let provider_param = self
            .reference
            .query_params
            .get("provider")
            .context("reference provider has no provider query parameter")?;
        if provider_param != &self.provider {
            bail!("reference provider query parameter does not match provider");
        }
        let url = format!(
            "{}/models?provider={provider_param}",
            self.reference.base_url.trim_end_matches('/')
        );
        let auth = AuthManager::external_bearer_only(self.reference.auth.clone())
            .auth()
            .await
            .context("reference provider auth command returned no token")?;
        let token = auth.get_token().context("read reference provider token")?;
        let client = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
            .build_client(&url, ClientRouteClass::Api)?;
        let response = client.get(&url).bearer_auth(token).send().await?;
        if !response.status().is_success() {
            bail!("provider catalog request failed: {}", response.status());
        }
        let catalog: ModelCatalog = response.json().await.context("parse model catalog")?;
        let models = catalog
            .data
            .into_iter()
            .map(|model| model.id)
            .filter(|model| {
                !RETIRED_REFERENCE_MODELS.contains(&(self.provider.as_str(), model.as_str()))
            })
            .collect::<Vec<_>>();
        if models.is_empty() {
            bail!("provider {} advertised no models", self.provider);
        }
        Ok(models)
    }

    fn run_case(&self, model: &str, flow: Flow) -> Result<CaseOutcome> {
        let marker = flow.marker();
        let output = self.run_exec(model, flow.prompt(marker))?;
        observe_case(&self.provider, model, flow, marker, output)
    }

    fn run_exec(&self, model: &str, prompt: String) -> Result<Output> {
        self.base_command("xedoc-exec")?
            .args(exec_args(model))
            .arg(prompt)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("run {} {model}", self.provider))
    }

    fn base_command(&self, binary: &str) -> Result<Command> {
        let mut command = Command::new(xedoc_utils_cargo_bin::cargo_bin(binary)?);
        command
            .current_dir(self.cwd.path())
            .env("XEDOC_HOME", self.home.path())
            .env("XEDOC_SQLITE_HOME", self.home.path());
        Ok(command)
    }
}

fn reference_provider_config(
    source_config: &TomlValue,
    source_home: &Path,
    provider: &str,
) -> Result<ReferenceProviderConfig> {
    let _guard = AbsolutePathBufGuard::new(source_home);
    configured_provider(source_config, provider)?
        .clone()
        .try_into()
        .with_context(|| format!("parse model_providers.{provider}"))
}

fn configured_provider<'a>(source_config: &'a TomlValue, provider: &str) -> Result<&'a TomlValue> {
    source_config
        .get("model_providers")
        .and_then(TomlValue::as_table)
        .and_then(|providers| providers.get(provider))
        .with_context(|| format!("model_providers.{provider} is not configured"))
}

impl Flow {
    fn marker(self) -> &'static str {
        match self {
            Self::Text => TEXT_MARKER,
            Self::Tool => TOOL_MARKER,
        }
    }

    fn prompt(self, marker: &str) -> String {
        match self {
            Self::Text => format!("Reply with exactly {marker}."),
            Self::Tool => format!(
                "Use the shell tool to run `printf {marker}`, then reply with exactly {marker}."
            ),
        }
    }
}

fn exec_args(model: &str) -> [&str; 11] {
    [
        "--json",
        "--ephemeral",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "-m",
        model,
        "-c",
        "model_reasoning_effort=\"low\"",
        "--",
    ]
}

fn isolated_provider_config(source: &TomlValue, provider: &str) -> Result<TomlValue> {
    let provider_config = configured_provider(source, provider)?.clone();
    let mut providers = toml::map::Map::new();
    providers.insert(provider.to_string(), provider_config);
    let mut root = toml::map::Map::new();
    root.insert(
        "model_provider".to_string(),
        TomlValue::String(provider.to_string()),
    );
    root.insert("model_providers".to_string(), TomlValue::Table(providers));
    Ok(TomlValue::Table(root))
}

fn preferred_tool_model(models: &[String]) -> &str {
    models
        .iter()
        .find(|model| model.contains("haiku") || model.contains("flash"))
        .or_else(|| models.first())
        .map(String::as_str)
        .expect("model catalog is non-empty")
}

fn observe_case(
    provider: &str,
    model: &str,
    flow: Flow,
    marker: &str,
    output: Output,
) -> Result<CaseOutcome> {
    let events = parse_events(&output.stdout)?;
    let state = ContractState::observe(flow, marker, output.status.success(), &events);
    let diagnostic = state.failed().then(|| bounded_output_diagnostic(&output));
    Ok(CaseOutcome {
        provider: provider.to_string(),
        model: model.to_string(),
        flow,
        process_succeeded: state.process_succeeded,
        marker_seen: state.marker_seen,
        turn_completed: state.turn_completed,
        usage_reported: state.usage_reported,
        command_completed: state.command_completed,
        diagnostic,
    })
}

impl ContractState {
    fn observe(flow: Flow, marker: &str, process_succeeded: bool, events: &[ThreadEvent]) -> Self {
        Self {
            process_succeeded,
            marker_seen: has_agent_marker(events, marker),
            turn_completed: has_completed_turn(events),
            usage_reported: has_usage(events),
            command_completed: (flow == Flow::Tool).then(|| has_completed_command(events, marker)),
        }
    }

    fn failed(self) -> bool {
        !self.process_succeeded
            || !self.marker_seen
            || !self.turn_completed
            || !self.usage_reported
            || self.command_completed == Some(false)
    }
}

fn has_agent_marker(events: &[ThreadEvent], marker: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ThreadEvent::ItemCompleted(completed)
                if matches!(
                    &completed.item.details,
                    ThreadItemDetails::AgentMessage(message) if message.text.contains(marker)
                )
        )
    })
}

fn has_completed_turn(events: &[ThreadEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, ThreadEvent::TurnCompleted(_)))
}

fn has_usage(events: &[ThreadEvent]) -> bool {
    events.iter().any(|event| {
        let ThreadEvent::TurnCompleted(completed) = event else {
            return false;
        };
        completed.usage.input_tokens
            + completed.usage.cached_input_tokens
            + completed.usage.cache_write_input_tokens
            > 0
            && completed.usage.output_tokens + completed.usage.reasoning_output_tokens > 0
    })
}

fn has_completed_command(events: &[ThreadEvent], marker: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ThreadEvent::ItemCompleted(completed)
                if matches!(
                    &completed.item.details,
                    ThreadItemDetails::CommandExecution(command)
                        if command.status == CommandExecutionStatus::Completed
                            && command.exit_code == Some(0)
                            && command.aggregated_output.contains(marker)
                )
        )
    })
}

fn parse_events(stdout: &[u8]) -> Result<Vec<ThreadEvent>> {
    let text = std::str::from_utf8(stdout).context("xedoc exec stdout was not UTF-8")?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parse xedoc exec JSONL event"))
        .collect()
}

fn bounded_output_diagnostic(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        bounded_text(&output.stdout),
        bounded_text(&output.stderr)
    )
}

fn bounded_text(bytes: &[u8]) -> String {
    const LIMIT: usize = 2_000;
    let text = String::from_utf8_lossy(bytes);
    let start = text.floor_char_boundary(text.len().saturating_sub(LIMIT));
    text[start..].trim().to_string()
}
