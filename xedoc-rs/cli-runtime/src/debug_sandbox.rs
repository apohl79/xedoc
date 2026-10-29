#[cfg(target_os = "macos")]
mod pid_tracker;
#[cfg(target_os = "macos")]
mod seatbelt;

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context as _;
use tokio::process::Child;
use tokio::process::Command as TokioCommand;
use toml::Value as TomlValue;
use xedoc_config::LoaderOverrides;
use xedoc_core::config::Config;
use xedoc_core::config::ConfigBuilder;
use xedoc_core::config::ConfigOverrides;
use xedoc_core::config::NetworkProxyAuditMetadata;
use xedoc_core::exec_env::create_env;
#[cfg(target_os = "macos")]
use xedoc_core::spawn::CODEX_SANDBOX_ENV_VAR;
use xedoc_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use xedoc_protocol::config_types::SandboxMode;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::models::SandboxEnforcement;
use xedoc_protocol::permissions::NetworkSandboxPolicy;
use xedoc_sandboxing::landlock::allow_network_for_proxy;
use xedoc_sandboxing::landlock::create_linux_sandbox_command_args_for_permission_profile;
#[cfg(target_os = "macos")]
use xedoc_sandboxing::seatbelt::CreateSeatbeltCommandArgsParams;
#[cfg(target_os = "macos")]
use xedoc_sandboxing::seatbelt::create_seatbelt_command_args;
use xedoc_sandboxing::with_managed_mitm_ca_readable_root;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_cli::CliConfigOverrides;

use crate::LandlockCommand;
use crate::SeatbeltCommand;
use crate::exit_status::handle_exit_status;

#[cfg(target_os = "macos")]
use seatbelt::DenialLogger;

#[cfg(target_os = "macos")]
pub async fn run_command_under_seatbelt(
    command: SeatbeltCommand,
    xedoc_linux_sandbox_exe: Option<PathBuf>,
    loader_overrides: LoaderOverrides,
) -> anyhow::Result<()> {
    let SeatbeltCommand {
        sandbox_state,
        permissions_profile,
        config_profile: _,
        cwd,
        include_managed_config,
        allow_unix_sockets,
        log_denials,
        config_overrides,
        command,
    } = command;
    let managed_requirements_mode = ManagedRequirementsMode::for_profile_invocation(
        &permissions_profile,
        include_managed_config,
    );
    run_command_under_sandbox(
        DebugSandboxConfigOptions {
            sandbox_state,
            permissions_profile,
            cwd,
            managed_requirements_mode,
            loader_overrides,
        },
        command,
        config_overrides,
        xedoc_linux_sandbox_exe,
        SandboxType::Seatbelt,
        log_denials,
        &allow_unix_sockets,
    )
    .await
}

#[cfg(not(target_os = "macos"))]
pub async fn run_command_under_seatbelt(
    _command: SeatbeltCommand,
    _xedoc_linux_sandbox_exe: Option<PathBuf>,
    _loader_overrides: LoaderOverrides,
) -> anyhow::Result<()> {
    anyhow::bail!("Seatbelt sandbox is only available on macOS");
}

pub async fn run_command_under_landlock(
    command: LandlockCommand,
    xedoc_linux_sandbox_exe: Option<PathBuf>,
    loader_overrides: LoaderOverrides,
) -> anyhow::Result<()> {
    let LandlockCommand {
        sandbox_state,
        permissions_profile,
        config_profile: _,
        cwd,
        include_managed_config,
        config_overrides,
        command,
    } = command;
    let managed_requirements_mode = ManagedRequirementsMode::for_profile_invocation(
        &permissions_profile,
        include_managed_config,
    );
    run_command_under_sandbox(
        DebugSandboxConfigOptions {
            sandbox_state,
            permissions_profile,
            cwd,
            managed_requirements_mode,
            loader_overrides,
        },
        command,
        config_overrides,
        xedoc_linux_sandbox_exe,
        SandboxType::Landlock,
        /*log_denials*/ false,
        &[],
    )
    .await
}

enum SandboxType {
    #[cfg(target_os = "macos")]
    Seatbelt,
    Landlock,
}

#[derive(Debug)]
struct DebugSandboxConfigOptions {
    sandbox_state: crate::SandboxStateArgs,
    permissions_profile: Option<String>,
    cwd: Option<PathBuf>,
    managed_requirements_mode: ManagedRequirementsMode,
    loader_overrides: LoaderOverrides,
}

#[derive(Debug, Clone, Copy)]
enum ManagedRequirementsMode {
    Include,
    Ignore,
}

impl ManagedRequirementsMode {
    fn for_profile_invocation(
        permissions_profile: &Option<String>,
        include_managed_config: bool,
    ) -> Self {
        if permissions_profile.is_some() && !include_managed_config {
            Self::Ignore
        } else {
            Self::Include
        }
    }
}

async fn run_command_under_sandbox(
    mut config_options: DebugSandboxConfigOptions,
    command: Vec<String>,
    config_overrides: CliConfigOverrides,
    xedoc_linux_sandbox_exe: Option<PathBuf>,
    sandbox_type: SandboxType,
    log_denials: bool,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    allow_unix_sockets: &[AbsolutePathBuf],
) -> anyhow::Result<()> {
    let sandbox_state = config_options
        .sandbox_state
        .sandbox_state_json
        .as_deref()
        .map(serde_json::from_str::<xedoc_mcp::SandboxState>)
        .transpose()
        .map_err(|err| anyhow::anyhow!("invalid --sandbox-state-json value: {err}"))?;
    let sandbox_state_readable_root = config_options
        .sandbox_state
        .sandbox_state_readable_root
        .clone();
    let sandbox_state_disable_network = config_options.sandbox_state.sandbox_state_disable_network;
    let xedoc_linux_sandbox_exe = match sandbox_state.as_ref() {
        Some(state) => {
            config_options.cwd = Some(
                state
                    .sandbox_cwd
                    .to_abs_path()
                    .context("sandbox state cwd is not native to this host")?
                    .to_path_buf(),
            );
            state
                .xedoc_linux_sandbox_exe
                .clone()
                .or(xedoc_linux_sandbox_exe)
        }
        None => xedoc_linux_sandbox_exe,
    };
    let config = load_debug_sandbox_config(
        config_overrides
            .parse_overrides()
            .map_err(anyhow::Error::msg)?,
        xedoc_linux_sandbox_exe,
        config_options,
        /*strict_config*/ false,
    )
    .await?;

    // In practice, this should be `std::env::current_dir()` because this CLI
    // does not support `--cwd`, but let's use the config value for consistency.
    let cwd = config.cwd.clone();
    // Sandbox launchers still use `sandbox_policy_cwd` for any remaining
    // cwd-dependent policy resolution. `:workspace_roots` entries in the
    // effective profile have already been materialized from config roots.
    let sandbox_policy_cwd = cwd.clone();

    let env = create_env(
        &config.permissions.shell_environment_policy,
        /*thread_id*/ None,
    );
    let mut permission_profile = match sandbox_state.as_ref() {
        Some(state) => match &state.permission_profile {
            PermissionProfile::External { .. } => {
                // `External` only says that the producer relies on an outer sandbox; it does not
                // include filesystem permissions we can recreate here. The consumer may not share
                // that sandbox, so use a locally enforceable read-only profile instead of spawning
                // without a sandbox.
                PermissionProfile::read_only()
            }
            permission_profile => permission_profile.clone(),
        },
        None => config.permissions.effective_permission_profile(),
    };
    if matches!(permission_profile, PermissionProfile::Disabled) && sandbox_state_disable_network {
        anyhow::bail!(
            "--sandbox-state-disable-network cannot be applied to a disabled permission profile"
        );
    }
    if !matches!(permission_profile, PermissionProfile::Disabled)
        && (!sandbox_state_readable_root.is_empty() || sandbox_state_disable_network)
    {
        let file_system = permission_profile
            .file_system_sandbox_policy()
            .with_additional_readable_roots(&cwd, &sandbox_state_readable_root);
        let network = if sandbox_state_disable_network {
            NetworkSandboxPolicy::Restricted
        } else {
            permission_profile.network_sandbox_policy()
        };
        permission_profile = PermissionProfile::from_runtime_permissions(&file_system, network);
    }
    let use_legacy_landlock = sandbox_state.as_ref().map_or_else(
        || config.features.use_legacy_landlock(),
        |state| state.use_legacy_landlock,
    );

    match permission_profile.enforcement() {
        SandboxEnforcement::Managed => {}
        SandboxEnforcement::Disabled | SandboxEnforcement::External => {
            let (program, args) = command
                .split_first()
                .context("sandbox command must not be empty")?;
            let mut child = spawn_debug_sandbox_child(
                PathBuf::from(program),
                args.to_vec(),
                /*arg0*/ None,
                cwd.to_path_buf(),
                permission_profile.network_sandbox_policy(),
                env,
                |_| {},
            )
            .await?;
            handle_exit_status(child.wait().await?);
        }
    }

    #[cfg(target_os = "macos")]
    let mut denial_logger = log_denials.then(DenialLogger::new).flatten();
    #[cfg(not(target_os = "macos"))]
    let _ = log_denials;

    let managed_network_requirements_enabled = config.managed_network_requirements_enabled();

    // This proxy should only live for the lifetime of the child process.
    let network_proxy = match config.permissions.network.as_ref() {
        Some(spec) => Some(
            spec.start_proxy(
                &permission_profile,
                /*policy_decider*/ None,
                /*blocked_request_observer*/ None,
                managed_network_requirements_enabled,
                NetworkProxyAuditMetadata::default(),
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to start managed network proxy: {err}"))?,
        ),
        None => None,
    };
    let network = network_proxy
        .as_ref()
        .map(xedoc_core::config::StartedNetworkProxy::proxy);
    // Proxy containment depends on whether a proxy is active, not whether its
    // policy came from managed requirements.
    let enforce_managed_network = network.is_some();
    let managed_mitm_ca_trust_bundle_path = match network.as_ref() {
        Some(network) => network.managed_mitm_ca_trust_bundle_path(),
        None => None,
    };
    let runtime_permission_profile = with_managed_mitm_ca_readable_root(
        permission_profile,
        managed_mitm_ca_trust_bundle_path.as_ref(),
        sandbox_policy_cwd.as_path(),
    );

    let mut child = match sandbox_type {
        #[cfg(target_os = "macos")]
        SandboxType::Seatbelt => {
            let (file_system_sandbox_policy, network_sandbox_policy) =
                runtime_permission_profile.to_runtime_permissions();
            let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
                command,
                file_system_sandbox_policy: &file_system_sandbox_policy,
                network_sandbox_policy,
                sandbox_policy_cwd: sandbox_policy_cwd.as_path(),
                enforce_managed_network,
                managed_network: None,
                environment_id: None,
                network: network.as_ref(),
                extra_allow_unix_sockets: allow_unix_sockets,
            })
            .map_err(|err| anyhow::anyhow!(err))?;
            spawn_debug_sandbox_child(
                PathBuf::from("/usr/bin/sandbox-exec"),
                args,
                /*arg0*/ None,
                cwd.to_path_buf(),
                network_sandbox_policy,
                env,
                |env_map| {
                    env_map.insert(CODEX_SANDBOX_ENV_VAR.to_string(), "seatbelt".to_string());
                    if let Some(network) = network.as_ref() {
                        network.apply_to_env(env_map);
                    }
                },
            )
            .await?
        }
        SandboxType::Landlock => {
            #[expect(clippy::expect_used)]
            let xedoc_linux_sandbox_exe = config
                .xedoc_linux_sandbox_exe
                .expect("xedoc-linux-sandbox executable not found");
            let network_sandbox_policy = runtime_permission_profile.network_sandbox_policy();
            let args = create_linux_sandbox_command_args_for_permission_profile(
                command,
                cwd.as_path(),
                &runtime_permission_profile,
                sandbox_policy_cwd.as_path(),
                use_legacy_landlock,
                allow_network_for_proxy(enforce_managed_network),
            );
            spawn_debug_sandbox_child(
                xedoc_linux_sandbox_exe,
                args,
                Some("xedoc-linux-sandbox"),
                cwd.to_path_buf(),
                network_sandbox_policy,
                env,
                |env_map| {
                    if let Some(network) = network.as_ref() {
                        network.apply_to_env(env_map);
                    }
                },
            )
            .await?
        }
    };

    #[cfg(target_os = "macos")]
    if let Some(denial_logger) = &mut denial_logger {
        denial_logger.on_child_spawn(&child);
    }

    let status = child.wait().await?;

    #[cfg(target_os = "macos")]
    if let Some(denial_logger) = denial_logger {
        let denials = denial_logger.finish().await;
        eprintln!("\n=== Sandbox denials ===");
        if denials.is_empty() {
            eprintln!("None found.");
        } else {
            for seatbelt::SandboxDenial { name, capability } in denials {
                eprintln!("({name}) {capability}");
            }
        }
    }

    handle_exit_status(status);
}

async fn spawn_debug_sandbox_child(
    program: PathBuf,
    args: Vec<String>,
    arg0: Option<&str>,
    cwd: PathBuf,
    network_sandbox_policy: NetworkSandboxPolicy,
    mut env: std::collections::HashMap<String, String>,
    apply_env: impl FnOnce(&mut std::collections::HashMap<String, String>),
) -> std::io::Result<Child> {
    let mut cmd = TokioCommand::new(&program);
    #[cfg(unix)]
    cmd.arg0(arg0.map_or_else(|| program.to_string_lossy().to_string(), String::from));
    #[cfg(not(unix))]
    let _ = arg0;
    cmd.args(args);
    cmd.current_dir(cwd);
    apply_env(&mut env);
    cmd.env_clear();
    cmd.envs(env);

    if !network_sandbox_policy.is_enabled() {
        cmd.env(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR, "1");
    }

    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
}

async fn load_debug_sandbox_config(
    cli_overrides: Vec<(String, TomlValue)>,
    xedoc_linux_sandbox_exe: Option<PathBuf>,
    options: DebugSandboxConfigOptions,
    strict_config: bool,
) -> anyhow::Result<Config> {
    load_debug_sandbox_config_with_xedoc_home(
        cli_overrides,
        xedoc_linux_sandbox_exe,
        options,
        /*xedoc_home*/ None,
        strict_config,
    )
    .await
}

async fn load_debug_sandbox_config_with_xedoc_home(
    cli_overrides: Vec<(String, TomlValue)>,
    xedoc_linux_sandbox_exe: Option<PathBuf>,
    options: DebugSandboxConfigOptions,
    xedoc_home: Option<PathBuf>,
    strict_config: bool,
) -> anyhow::Result<Config> {
    let DebugSandboxConfigOptions {
        sandbox_state: _,
        permissions_profile,
        cwd,
        managed_requirements_mode,
        loader_overrides,
    } = options;
    let mut cli_overrides = cli_overrides;

    if let Some(permissions_profile) = permissions_profile {
        cli_overrides.push((
            "default_permissions".to_string(),
            TomlValue::String(permissions_profile),
        ));
    }

    // For legacy configs, `xedoc sandbox` historically defaulted to read-only
    // instead of inheriting ambient `sandbox_mode` settings from user/system
    // config. Keep that behavior unless this invocation explicitly passes a
    // legacy `sandbox_mode` CLI override for compatibility with older callers.
    let uses_legacy_sandbox_mode_override = cli_overrides_use_legacy_sandbox_mode(&cli_overrides);
    let config = build_debug_sandbox_config_with_loader_overrides(
        cli_overrides.clone(),
        ConfigOverrides {
            cwd: cwd.clone(),
            xedoc_linux_sandbox_exe: xedoc_linux_sandbox_exe.clone(),
            ..Default::default()
        },
        xedoc_home.clone(),
        managed_requirements_mode,
        loader_overrides.clone(),
        strict_config,
    )
    .await?;

    if config_uses_permission_profiles(&config) || uses_legacy_sandbox_mode_override {
        return Ok(config);
    }

    build_debug_sandbox_config_with_loader_overrides(
        cli_overrides,
        ConfigOverrides {
            sandbox_mode: Some(SandboxMode::ReadOnly),
            cwd,
            xedoc_linux_sandbox_exe,
            ..Default::default()
        },
        xedoc_home,
        managed_requirements_mode,
        loader_overrides,
        strict_config,
    )
    .await
    .map_err(Into::into)
}

async fn build_debug_sandbox_config_with_loader_overrides(
    cli_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    xedoc_home: Option<PathBuf>,
    managed_requirements_mode: ManagedRequirementsMode,
    mut loader_overrides: LoaderOverrides,
    strict_config: bool,
) -> std::io::Result<Config> {
    let mut builder = ConfigBuilder::default()
        .cli_overrides(cli_overrides)
        .harness_overrides(harness_overrides)
        .strict_config(strict_config);
    if matches!(managed_requirements_mode, ManagedRequirementsMode::Ignore) {
        loader_overrides.ignore_managed_requirements = true;
    }
    builder = builder.loader_overrides(loader_overrides);
    if let Some(xedoc_home) = xedoc_home {
        builder = builder
            .xedoc_home(xedoc_home.clone())
            .fallback_cwd(Some(xedoc_home));
    }
    builder.build().await
}

fn config_uses_permission_profiles(config: &Config) -> bool {
    config
        .config_layer_stack
        .effective_config()
        .get("default_permissions")
        .is_some()
}

fn cli_overrides_use_legacy_sandbox_mode(cli_overrides: &[(String, TomlValue)]) -> bool {
    cli_overrides.iter().any(|(key, _)| key == "sandbox_mode")
}
