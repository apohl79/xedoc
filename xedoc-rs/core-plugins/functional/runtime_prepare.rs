use std::path::PathBuf;
use std::sync::Arc;
use xedoc_config::ConfigLayerEntry;
use xedoc_config::ConfigLayerSource;
use xedoc_config::ConfigLayerStack;
use xedoc_core_plugins::PluginInstallRequest;
use xedoc_core_plugins::PluginReadRequest;
use xedoc_core_plugins::PluginsConfigInput;
use xedoc_core_plugins::PluginsManager;
use xedoc_core_plugins::marketplace_add::MarketplaceAddRequest;
use xedoc_core_plugins::marketplace_add::add_marketplace;
use xedoc_utils_absolute_path::AbsolutePathBuf;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let home = PathBuf::from(std::env::var("XEDOC_HOME")?);
    let path = home.join("config.toml");
    let config = toml::from_str(&std::fs::read_to_string(&path)?)?;
    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: AbsolutePathBuf::try_from(path)?,
                profile: None,
            },
            config,
        )],
        Default::default(),
        Default::default(),
    )?;
    if args[2] == "marketplace" && args[3] == "add" {
        add_marketplace(
            home,
            Default::default(),
            MarketplaceAddRequest {
                source: args[4].clone(),
                ref_name: None,
                sparse_paths: vec![],
            },
        )
        .await?;
        return Ok(());
    }
    let manager = Arc::new(PluginsManager::new(home));
    let input = PluginsConfigInput::new(stack, true);
    if args[2] == "marketplace" {
        let result = manager
            .upgrade_configured_marketplaces_for_config(&input, Some(args[4].as_str()))
            .map_err(anyhow::Error::msg)?;
        if !result.errors.is_empty() {
            anyhow::bail!(
                "{}",
                result
                    .errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        println!("{{\"errors\":[]}}");
    } else if args[2] == "list" {
        manager.maybe_start_non_curated_plugin_cache_refresh(&input, &[]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while Arc::strong_count(&manager) > 1 {
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "refresh worker timed out"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        for marketplace in manager
            .list_marketplaces_for_config(&input, &[], false)?
            .marketplaces
        {
            for plugin in marketplace.plugins {
                manager
                    .read_plugin_for_config(
                        &input,
                        &PluginReadRequest {
                            plugin_name: plugin.name,
                            marketplace_path: marketplace.path.clone(),
                        },
                    )
                    .await?;
            }
        }
        println!("{{}}");
    } else {
        let (plugin, market) = args[3].split_once('@').unwrap();
        let marketplace = manager
            .list_marketplaces_for_config(&input, &[], false)?
            .marketplaces
            .into_iter()
            .find(|entry| entry.name == market)
            .unwrap();
        manager
            .install_plugin(
                &input.config_layer_stack,
                PluginInstallRequest {
                    plugin_name: plugin.to_string(),
                    marketplace_path: marketplace.path,
                },
            )
            .await?;
        println!("{{}}");
    }
    Ok(())
}
