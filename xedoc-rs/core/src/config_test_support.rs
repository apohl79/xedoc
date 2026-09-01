use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::config::ConfigOverrides;
use crate::config::LoaderOverrides;
use xedoc_config::ConfigLayerStack;
use xedoc_config::config_toml::ConfigToml;
use xedoc_utils_absolute_path::AbsolutePathBuf;

pub(crate) fn config_builder_without_managed_config() -> ConfigBuilder {
    ConfigBuilder::default().loader_overrides(LoaderOverrides::without_managed_config_for_tests())
}

pub(crate) async fn test_config() -> Config {
    let xedoc_home = tempfile::tempdir().expect("create temp dir");
    Config::load_config_with_layer_stack(
        xedoc_exec_server::LOCAL_FS.as_ref(),
        ConfigToml {
            model: Some("gpt-5.5".to_string()),
            ..Default::default()
        },
        ConfigOverrides::default(),
        AbsolutePathBuf::from_absolute_path(xedoc_home.path()).expect("temp dir should resolve"),
        ConfigLayerStack::default(),
    )
    .await
    .expect("load default test config")
}
