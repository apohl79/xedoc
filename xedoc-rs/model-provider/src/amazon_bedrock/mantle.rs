use xedoc_aws_auth::AwsAuthConfig;
use xedoc_login::auth::BedrockApiKeyAuth;
use xedoc_model_provider_info::ModelProviderAwsAuthInfo;
use xedoc_protocol::error::Result;
use xedoc_protocol::error::XedocErr;

use super::auth::BedrockAuthMethod;
use super::auth::resolve_auth_method;

const BEDROCK_MANTLE_SERVICE_NAME: &str = "bedrock-mantle";
const BEDROCK_MANTLE_SUPPORTED_REGIONS: [&str; 12] = [
    "us-east-2",
    "us-east-1",
    "us-west-2",
    "ap-southeast-3",
    "ap-south-1",
    "ap-northeast-1",
    "eu-central-1",
    "eu-west-1",
    "eu-west-2",
    "eu-south-1",
    "eu-north-1",
    "sa-east-1",
];

pub(super) fn aws_auth_config(aws: &ModelProviderAwsAuthInfo) -> AwsAuthConfig {
    AwsAuthConfig {
        profile: aws.profile.clone(),
        region: region_from_config(aws),
        service: BEDROCK_MANTLE_SERVICE_NAME.to_string(),
    }
}

pub(super) fn region_from_config(aws: &ModelProviderAwsAuthInfo) -> Option<String> {
    aws.region
        .as_deref()
        .map(str::trim)
        .filter(|region| !region.is_empty())
        .map(str::to_string)
}

/// Returns whether Amazon Bedrock Mantle is available in `region`.
pub fn is_supported_amazon_bedrock_region(region: &str) -> bool {
    BEDROCK_MANTLE_SUPPORTED_REGIONS.contains(&region)
}

pub(super) fn base_url(region: &str) -> Result<String> {
    if is_supported_amazon_bedrock_region(region) {
        Ok(format!("https://bedrock-mantle.{region}.api.aws/openai/v1"))
    } else {
        Err(XedocErr::Fatal(format!(
            "Amazon Bedrock Mantle does not support region `{region}`"
        )))
    }
}

pub(super) async fn bedrock_mantle_runtime_base_url(
    managed_auth: Option<&BedrockApiKeyAuth>,
    aws: &ModelProviderAwsAuthInfo,
) -> Result<String> {
    let region = resolve_region(managed_auth, aws).await?;
    base_url(&region)
}

async fn resolve_region(
    managed_auth: Option<&BedrockApiKeyAuth>,
    aws: &ModelProviderAwsAuthInfo,
) -> Result<String> {
    match resolve_auth_method(managed_auth, aws).await? {
        BedrockAuthMethod::ManagedBearerToken { region, .. }
        | BedrockAuthMethod::EnvBearerToken { region, .. } => Ok(region),
        BedrockAuthMethod::AwsSdkAuth { context } => Ok(context.region().to_string()),
    }
}
