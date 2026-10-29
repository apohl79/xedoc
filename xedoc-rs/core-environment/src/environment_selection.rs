use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::sync::OnceLock;

use arc_swap::ArcSwap;
use async_channel::Sender;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use tokio_util::task::AbortOnDropHandle;
use xedoc_exec_server::Environment;
use xedoc_exec_server::EnvironmentConnectionState;
use xedoc_exec_server::EnvironmentManager;
use xedoc_exec_server::ExecServerError;
use xedoc_exec_server::ExecutorFileSystem;
use xedoc_protocol::protocol::EnvironmentConnectionEvent;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::TurnEnvironmentSelection;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path_uri::PathUri;

use crate::TurnEnvironment;
use crate::shell::Shell;
use crate::shell_snapshot::ShellSnapshot;

pub fn default_thread_environment_selections(
    environment_manager: &EnvironmentManager,
    cwd: &AbsolutePathBuf,
    workspace_roots: &[AbsolutePathBuf],
) -> Vec<TurnEnvironmentSelection> {
    environment_manager
        .default_environment_ids()
        .into_iter()
        .map(|environment_id| TurnEnvironmentSelection {
            environment_id,
            cwd: PathUri::from_abs_path(cwd),
            workspace_roots: workspace_roots.iter().map(PathUri::from_abs_path).collect(),
        })
        .collect()
}

type TurnEnvironmentResult = Result<TurnEnvironment, Arc<ExecServerError>>;
type TurnEnvironmentResolution = Shared<BoxFuture<'static, TurnEnvironmentResult>>;

#[derive(Clone)]
struct SelectedTurnEnvironment {
    selection: TurnEnvironmentSelection,
    environment: Arc<Environment>,
    // Selection clones share one listener; the final handle drop aborts it.
    connection_events_task: Option<Arc<AbortOnDropHandle<()>>>,
    resolution: TurnEnvironmentResolution,
}

#[derive(Clone)]
pub struct StartingTurnEnvironment {
    pub selection: TurnEnvironmentSelection,
    resolution: TurnEnvironmentResolution,
}

impl fmt::Debug for StartingTurnEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartingTurnEnvironment")
            .field("selection", &self.selection)
            .field("resolved", &self.resolution.peek().is_some())
            .finish_non_exhaustive()
    }
}

impl StartingTurnEnvironment {
    pub async fn wait_until_ready(&self) -> Result<(), Arc<ExecServerError>> {
        self.resolution.clone().await.map(|_| ())
    }
}

pub struct ThreadEnvironments {
    environment_manager: Arc<EnvironmentManager>,
    local_shell: Shell,
    shell_snapshot: ShellSnapshot,
    non_blocking_snapshots: bool,
    environments: ArcSwap<Vec<SelectedTurnEnvironment>>,
    connection_event_tx: OnceLock<Sender<Event>>,
}

impl ThreadEnvironments {
    pub fn new(
        environment_manager: Arc<EnvironmentManager>,
        local_shell: Shell,
        shell_snapshot: ShellSnapshot,
        current: TurnEnvironmentSnapshot,
        non_blocking_snapshots: bool,
    ) -> Self {
        // Reuse only attached environments from the supplied snapshot; drop starting entries.
        let environments = current
            .environments
            .into_iter()
            .filter_map(|environment| {
                let TurnEnvironmentState::Ready(environment) = environment else {
                    return None;
                };
                let selection = environment.selection();
                let selected_environment = Arc::clone(&environment.environment);
                let resolution: TurnEnvironmentResolution =
                    futures::future::ready(Ok(environment)).boxed().shared();
                Some(SelectedTurnEnvironment {
                    selection,
                    environment: selected_environment,
                    connection_events_task: None,
                    resolution,
                })
            })
            .collect();
        Self {
            environment_manager,
            local_shell,
            shell_snapshot,
            non_blocking_snapshots,
            environments: ArcSwap::from_pointee(environments),
            connection_event_tx: OnceLock::new(),
        }
    }

    pub fn update_selections(&self, environments: &[TurnEnvironmentSelection]) {
        let previous = self.environments.load();
        let mut seen_environment_ids = HashSet::with_capacity(environments.len());
        let mut next = Vec::with_capacity(environments.len());
        for selected_environment in environments {
            if !seen_environment_ids.insert(selected_environment.environment_id.as_str()) {
                continue;
            }
            if let Some(environment) = previous
                .iter()
                .find(|environment| environment.selection == *selected_environment)
                && !matches!(environment.resolution.clone().now_or_never(), Some(Err(_)))
            {
                next.push(environment.clone());
                continue;
            }

            let environment_id = &selected_environment.environment_id;
            let Some(environment) = self.environment_manager.get_environment(environment_id) else {
                tracing::warn!("skipping unknown turn environment `{environment_id}`");
                continue;
            };
            // Connection state belongs to the environment instance, not its cwd or roots.
            let connection_events_task = previous
                .iter()
                .find(|previous| {
                    previous.selection.environment_id.as_str() == environment_id.as_str()
                        && Arc::ptr_eq(&previous.environment, &environment)
                })
                .and_then(|previous| previous.connection_events_task.clone())
                .or_else(|| {
                    self.connection_event_tx.get().and_then(|tx_event| {
                        Self::spawn_connection_event_listener(
                            environment.as_ref(),
                            environment_id.clone(),
                            tx_event.clone(),
                        )
                    })
                });
            let (resolution_task, resolution) = Self::resolve_environment(
                selected_environment.clone(),
                Arc::clone(&environment),
                self.local_shell.clone(),
                self.shell_snapshot.clone(),
            )
            .remote_handle();
            drop(tokio::spawn(resolution_task));
            let resolution = resolution.boxed().shared();
            next.push(SelectedTurnEnvironment {
                selection: selected_environment.clone(),
                environment,
                connection_events_task,
                resolution,
            });
        }
        let removed_connection_tasks = previous
            .iter()
            .filter_map(|previous| {
                let task = previous.connection_events_task.as_ref()?;
                (!next.iter().any(|next| {
                    next.connection_events_task
                        .as_ref()
                        .is_some_and(|next_task| Arc::ptr_eq(task, next_task))
                }))
                .then(|| Arc::clone(task))
            })
            .collect::<Vec<_>>();
        self.environments.store(Arc::new(next));
        // ArcSwap readers may retain removed selections, so abort at logical removal.
        for task in removed_connection_tasks {
            task.abort();
        }
    }

    fn spawn_connection_event_listener(
        environment: &Environment,
        environment_id: String,
        tx_event: Sender<Event>,
    ) -> Option<Arc<AbortOnDropHandle<()>>> {
        let mut connection_state = environment.subscribe_connection_state()?;
        let task = tokio::spawn(async move {
            loop {
                let state = tokio::select! {
                    _ = tx_event.closed() => return,
                    changed = connection_state.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        *connection_state.borrow_and_update()
                    }
                };
                let msg = match state {
                    EnvironmentConnectionState::Connected => {
                        EventMsg::EnvironmentConnected(EnvironmentConnectionEvent {
                            environment_id: environment_id.clone(),
                        })
                    }
                    EnvironmentConnectionState::Disconnected => {
                        EventMsg::EnvironmentDisconnected(EnvironmentConnectionEvent {
                            environment_id: environment_id.clone(),
                        })
                    }
                };
                if tx_event
                    .send(Event {
                        id: String::new(),
                        msg,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Some(Arc::new(AbortOnDropHandle::new(task)))
    }

    pub fn start_connection_event_forwarding(&self, tx_event: Sender<Event>) {
        let tx_event = self.connection_event_tx.get_or_init(|| tx_event);
        let current = self.environments.load_full();
        let environments = current
            .iter()
            .map(|selected| {
                let mut selected = selected.clone();
                if selected.connection_events_task.is_none() {
                    selected.connection_events_task = Self::spawn_connection_event_listener(
                        selected.environment.as_ref(),
                        selected.selection.environment_id.clone(),
                        tx_event.clone(),
                    );
                }
                selected
            })
            .collect();
        self.environments.store(Arc::new(environments));
    }

    fn resolve_environment(
        selection: TurnEnvironmentSelection,
        environment: Arc<Environment>,
        local_shell: Shell,
        shell_snapshot: ShellSnapshot,
    ) -> BoxFuture<'static, TurnEnvironmentResult> {
        async move {
            let environment_id = &selection.environment_id;
            if let Err(err) = environment.wait_until_ready().await {
                tracing::warn!("turn environment `{environment_id}` failed to start: {err}");
                return Err(Arc::new(err));
            }
            let shell = if environment.is_remote() {
                match environment.info().await {
                    Ok(info) => match Shell::from_environment_shell_info(info.shell) {
                        Ok(shell) => Some(shell),
                        Err(err) => {
                            tracing::warn!(
                                "failed to resolve shell for environment `{environment_id}`: {err}"
                            );
                            None
                        }
                    },
                    Err(err) => {
                        tracing::warn!(
                            "failed to get info for environment `{environment_id}`: {err}"
                        );
                        None
                    }
                }
            } else {
                Some(local_shell)
            };
            let mut turn_environment = TurnEnvironment::new(
                selection.environment_id,
                environment,
                selection.cwd,
                selection.workspace_roots,
                shell,
            );
            let task = shell_snapshot
                .build(turn_environment.clone())
                .boxed()
                .shared();
            drop(tokio::spawn(task.clone()));
            turn_environment.shell_snapshot = task;
            Ok(turn_environment)
        }
        .boxed()
    }

    #[tracing::instrument(name = "environments.snapshot", skip_all)]
    pub async fn snapshot(&self) -> TurnEnvironmentSnapshot {
        let selected = self.environments.load_full();
        let mut environments = Vec::with_capacity(selected.len());
        for environment in selected.iter() {
            let resolved = if self.non_blocking_snapshots {
                environment.resolution.clone().now_or_never()
            } else {
                Some(environment.resolution.clone().await)
            };
            if let Some(environment) = TurnEnvironmentState::from_resolution(
                StartingTurnEnvironment {
                    selection: environment.selection.clone(),
                    resolution: environment.resolution.clone(),
                },
                resolved,
            ) {
                environments.push(environment);
            }
        }
        TurnEnvironmentSnapshot { environments }
    }

    pub fn environment_manager(&self) -> Arc<EnvironmentManager> {
        Arc::clone(&self.environment_manager)
    }
}

#[derive(Clone, Debug)]
pub enum TurnEnvironmentState {
    Ready(TurnEnvironment),
    Starting(StartingTurnEnvironment),
}

impl TurnEnvironmentState {
    fn from_resolution(
        starting: StartingTurnEnvironment,
        resolved: Option<TurnEnvironmentResult>,
    ) -> Option<Self> {
        match resolved {
            Some(Ok(environment)) => Some(Self::Ready(environment)),
            Some(Err(err)) => {
                tracing::debug!(
                    environment_id = %starting.selection.environment_id,
                    "skipping failed turn environment: {err}"
                );
                None
            }
            None => Some(Self::Starting(starting)),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TurnEnvironmentSnapshot {
    // Keep ready and starting environments in their original selection order.
    pub environments: Vec<TurnEnvironmentState>,
}

impl TurnEnvironmentSnapshot {
    /// Promotes completed startup work without adopting newer thread selections.
    pub fn refresh_readiness(&self) -> Self {
        let environments = self
            .environments
            .iter()
            .filter_map(|environment| match environment {
                TurnEnvironmentState::Ready(environment) => {
                    Some(TurnEnvironmentState::Ready(environment.clone()))
                }
                TurnEnvironmentState::Starting(environment) => {
                    TurnEnvironmentState::from_resolution(
                        environment.clone(),
                        environment.resolution.clone().now_or_never(),
                    )
                }
            })
            .collect();
        Self { environments }
    }

    pub fn turn_environments(&self) -> impl Iterator<Item = &TurnEnvironment> {
        self.environments.iter().filter_map(|environment| {
            let TurnEnvironmentState::Ready(environment) = environment else {
                return None;
            };
            Some(environment)
        })
    }

    pub fn starting(&self) -> impl Iterator<Item = &StartingTurnEnvironment> {
        self.environments.iter().filter_map(|environment| {
            let TurnEnvironmentState::Starting(environment) = environment else {
                return None;
            };
            Some(environment)
        })
    }

    /// Maps each captured environment to its exact ready handle, or `None` when it was starting.
    pub fn captured_environments(&self) -> HashMap<String, Option<Arc<Environment>>> {
        self.turn_environments()
            .map(|environment| {
                (
                    environment.environment_id.clone(),
                    Some(Arc::clone(&environment.environment)),
                )
            })
            .chain(
                self.starting()
                    .map(|environment| (environment.selection.environment_id.clone(), None)),
            )
            .collect()
    }

    pub fn primary(&self) -> Option<&TurnEnvironment> {
        self.turn_environments().next()
    }

    pub fn local(&self) -> Option<&TurnEnvironment> {
        self.turn_environments()
            .find(|environment| !environment.environment.is_remote())
    }

    pub fn to_selections(&self) -> Vec<TurnEnvironmentSelection> {
        self.turn_environments()
            .map(TurnEnvironment::selection)
            .collect()
    }

    pub fn primary_filesystem(&self) -> Option<Arc<dyn ExecutorFileSystem>> {
        self.primary()
            .map(|environment| environment.environment.get_filesystem())
    }

    pub fn single_local_environment(&self) -> Option<&TurnEnvironment> {
        if self.starting().next().is_some() {
            return None;
        }
        let mut environments = self.turn_environments();
        let environment = environments.next()?;
        if environments.next().is_some() {
            return None;
        }

        (!environment.environment.is_remote()).then_some(environment)
    }

    pub fn single_local_environment_cwd(&self) -> Option<AbsolutePathBuf> {
        // TODO(anp): Migrate local-environment consumers to PathUri so this compatibility
        // conversion can be removed.
        self.single_local_environment()?.cwd().to_abs_path().ok()
    }
}
