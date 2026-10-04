//! Host configuration for the remote-agent broker.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::net::Ipv6Addr;

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::InstanceType;
use schemars::schema::ObjectValidation;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use schemars::schema::StringValidation;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as SerdeError;
use xedoc_utils_absolute_path::AbsolutePathBuf;

const MAX_ENDPOINT_LENGTH: usize = 2048;
const MAX_WORKSPACES: usize = 128;
const MAX_WORKSPACE_ID_LENGTH: usize = 64;
const MAX_STATIC_PEERS: usize = 128;
const MAX_MANAGED_COORDINATOR_CERTIFICATE_PATHS: usize = 128;
const ENDPOINT_HOST_PATTERN: &str = r"[\x21\x22\x24-\x2e\x30-\x39\x3b-\x3e\x41-\x5a\x5c\x5e-\x7e]+";
const ADVERTISED_ENDPOINT_HOST_PATTERN: &str =
    r"[\x21\x22\x24-\x29\x2b-\x2e\x30-\x39\x3b-\x3e\x41-\x5a\x5c\x5e-\x7e]+";
const ENDPOINT_PORT_PATTERN: &str = r"(?:[1-9]|[1-9][0-9]{1,3}|[1-5][0-9]{4}|6[0-4][0-9]{3}|65[0-4][0-9]{2}|655[0-2][0-9]|6553[0-5])";
const IPV6_LITERAL_PATTERN: &str = r"(?:(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}|(?:[0-9A-Fa-f]{1,4}:){1,7}:|(?:[0-9A-Fa-f]{1,4}:){1,6}:[0-9A-Fa-f]{1,4}|(?:[0-9A-Fa-f]{1,4}:){1,5}(?::[0-9A-Fa-f]{1,4}){1,2}|(?:[0-9A-Fa-f]{1,4}:){1,4}(?::[0-9A-Fa-f]{1,4}){1,3}|(?:[0-9A-Fa-f]{1,4}:){1,3}(?::[0-9A-Fa-f]{1,4}){1,4}|(?:[0-9A-Fa-f]{1,4}:){1,2}(?::[0-9A-Fa-f]{1,4}){1,5}|[0-9A-Fa-f]{1,4}:(?:(?::[0-9A-Fa-f]{1,4}){1,6})|:(?:(?::[0-9A-Fa-f]{1,4}){1,7}|:)|(?:(?:[0-9A-Fa-f]{1,4}:){6}|::(?:[0-9A-Fa-f]{1,4}:){0,5}|(?:[0-9A-Fa-f]{1,4}:){1,5}:)(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9]))";

/// The host role used by the remote-agent broker.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RemoteAgentRole {
    Coordinator,
    Managed,
}

/// Host-wide remote-agent configuration.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteAgentConfigToml {
    pub role: RemoteAgentRole,
    #[schemars(schema_with = "workspaces_schema")]
    pub workspaces: BTreeMap<String, AbsolutePathBuf>,
    pub limits: RemoteAgentLimitsToml,
    #[serde(default)]
    pub peer_listener: Option<RemoteAgentPeerListenerToml>,
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub static_peers: Vec<RemoteAgentStaticPeerToml>,
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub managed_coordinator_certificate_paths: Vec<AbsolutePathBuf>,
}

/// Local listener configuration for trusted remote-agent peers.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteAgentPeerListenerToml {
    #[schemars(schema_with = "endpoint_schema")]
    pub endpoint: String,
    #[serde(default)]
    #[schemars(schema_with = "endpoint_schema")]
    pub pairing_endpoint: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "advertised_endpoint_schema")]
    pub advertised_endpoint: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "advertised_endpoint_schema")]
    pub advertised_pairing_endpoint: Option<String>,
}

/// A trusted peer configured without network discovery.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteAgentStaticPeerToml {
    #[schemars(schema_with = "endpoint_schema")]
    pub endpoint: String,
    pub certificate_path: AbsolutePathBuf,
}

/// Explicit bounds for local remote-agent broker operations.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteAgentLimitsToml {
    #[schemars(range(min = 1, max = 1024))]
    pub max_attached_sessions: u64,
    #[schemars(range(min = 1, max = 1048576))]
    pub max_message_bytes: u64,
    #[schemars(range(min = 1, max = 4194304))]
    pub max_result_bytes: u64,
    #[schemars(range(min = 1, max = 3600))]
    pub max_wait_seconds: u64,
    #[schemars(range(min = 1, max = 300))]
    pub max_discovery_seconds: u64,
    #[schemars(range(min = 1, max = 3650))]
    pub audit_retention_days: u64,
}

impl<'de> Deserialize<'de> for RemoteAgentLimitsToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RemoteAgentLimitsTomlInput {
            max_attached_sessions: u64,
            max_message_bytes: u64,
            max_result_bytes: u64,
            max_wait_seconds: u64,
            max_discovery_seconds: u64,
            audit_retention_days: u64,
        }

        let input = RemoteAgentLimitsTomlInput::deserialize(deserializer)?;
        let limits = Self {
            max_attached_sessions: input.max_attached_sessions,
            max_message_bytes: input.max_message_bytes,
            max_result_bytes: input.max_result_bytes,
            max_wait_seconds: input.max_wait_seconds,
            max_discovery_seconds: input.max_discovery_seconds,
            audit_retention_days: input.audit_retention_days,
        };
        let invalid_limit = [
            ("max_attached_sessions", limits.max_attached_sessions, 1024),
            ("max_message_bytes", limits.max_message_bytes, 1048576),
            ("max_result_bytes", limits.max_result_bytes, 4194304),
            ("max_wait_seconds", limits.max_wait_seconds, 3600),
            ("max_discovery_seconds", limits.max_discovery_seconds, 300),
            ("audit_retention_days", limits.audit_retention_days, 3650),
        ]
        .into_iter()
        .find(|(_, value, max)| !(1..=*max).contains(value));
        if let Some((name, _, max)) = invalid_limit {
            return Err(D::Error::custom(format!(
                "{name} must be between 1 and {max}"
            )));
        }
        Ok(limits)
    }
}

impl<'de> Deserialize<'de> for RemoteAgentPeerListenerToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RemoteAgentPeerListenerTomlInput {
            endpoint: String,
            #[serde(default)]
            pairing_endpoint: Option<String>,
            #[serde(default)]
            advertised_endpoint: Option<String>,
            #[serde(default)]
            advertised_pairing_endpoint: Option<String>,
        }

        let input = RemoteAgentPeerListenerTomlInput::deserialize(deserializer)?;
        validate_endpoint(&input.endpoint).map_err(D::Error::custom)?;
        if let Some(endpoint) = input.pairing_endpoint.as_deref() {
            validate_endpoint(endpoint).map_err(D::Error::custom)?;
        }
        if let Some(endpoint) = input.advertised_endpoint.as_deref() {
            validate_advertised_endpoint(endpoint).map_err(D::Error::custom)?;
        }
        if let Some(endpoint) = input.advertised_pairing_endpoint.as_deref() {
            validate_advertised_endpoint(endpoint).map_err(D::Error::custom)?;
        }
        if input.advertised_endpoint.is_some() != input.advertised_pairing_endpoint.is_some() {
            return Err(D::Error::custom(
                "advertised_endpoint and advertised_pairing_endpoint must be supplied together",
            ));
        }
        Ok(Self {
            endpoint: input.endpoint,
            pairing_endpoint: input.pairing_endpoint,
            advertised_endpoint: input.advertised_endpoint,
            advertised_pairing_endpoint: input.advertised_pairing_endpoint,
        })
    }
}

impl<'de> Deserialize<'de> for RemoteAgentStaticPeerToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RemoteAgentStaticPeerTomlInput {
            endpoint: String,
            certificate_path: AbsolutePathBuf,
        }

        let input = RemoteAgentStaticPeerTomlInput::deserialize(deserializer)?;
        validate_endpoint(&input.endpoint).map_err(D::Error::custom)?;
        Ok(Self {
            endpoint: input.endpoint,
            certificate_path: input.certificate_path,
        })
    }
}

impl<'de> Deserialize<'de> for RemoteAgentConfigToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RemoteAgentConfigTomlInput {
            role: RemoteAgentRole,
            workspaces: BTreeMap<String, AbsolutePathBuf>,
            limits: RemoteAgentLimitsToml,
            #[serde(default)]
            peer_listener: Option<RemoteAgentPeerListenerToml>,
            #[serde(default)]
            static_peers: Vec<RemoteAgentStaticPeerToml>,
            #[serde(default)]
            managed_coordinator_certificate_paths: Vec<AbsolutePathBuf>,
        }

        let input = RemoteAgentConfigTomlInput::deserialize(deserializer)?;
        if input.workspaces.len() > MAX_WORKSPACES {
            return Err(D::Error::custom(format!(
                "workspaces must contain at most {MAX_WORKSPACES} entries"
            )));
        }
        if let Some(workspace_id) = input
            .workspaces
            .keys()
            .find(|workspace_id| !is_valid_workspace_id(workspace_id))
        {
            return Err(D::Error::custom(format!(
                "workspace ID `{workspace_id}` must match \
                 [A-Za-z0-9][A-Za-z0-9_-]{{0,63}}"
            )));
        }
        if input.static_peers.len() > MAX_STATIC_PEERS {
            return Err(D::Error::custom(format!(
                "static_peers must contain at most {MAX_STATIC_PEERS} entries"
            )));
        }
        if input.managed_coordinator_certificate_paths.len()
            > MAX_MANAGED_COORDINATOR_CERTIFICATE_PATHS
        {
            return Err(D::Error::custom(format!(
                "managed_coordinator_certificate_paths must contain at most \
                 {MAX_MANAGED_COORDINATOR_CERTIFICATE_PATHS} entries"
            )));
        }
        Ok(Self {
            role: input.role,
            workspaces: input.workspaces,
            limits: input.limits,
            peer_listener: input.peer_listener,
            static_peers: input.static_peers,
            managed_coordinator_certificate_paths: input.managed_coordinator_certificate_paths,
        })
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    if !endpoint.is_ascii()
        || endpoint.is_empty()
        || endpoint.len() > MAX_ENDPOINT_LENGTH
        || endpoint.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
    {
        return Err(format!(
            "endpoint must be ASCII and contain between 1 and {MAX_ENDPOINT_LENGTH} bytes"
        ));
    }

    parse_endpoint(endpoint).map(|_| ())
}

fn validate_advertised_endpoint(endpoint: &str) -> Result<(), String> {
    validate_endpoint(endpoint)?;
    let host = parse_endpoint(endpoint)?;
    if host.contains('*') {
        return Err("advertised endpoint host must not contain a wildcard".to_string());
    }
    if host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_unspecified() || address.is_loopback())
    {
        return Err(
            "advertised endpoint host must not be an unspecified or loopback IP address"
                .to_string(),
        );
    }
    Ok(())
}

fn parse_endpoint(endpoint: &str) -> Result<&str, String> {
    if endpoint.contains('\0') {
        return Err("endpoint must not contain a NUL byte".to_string());
    }
    let authority_and_path = endpoint
        .strip_prefix("tls://")
        .ok_or_else(|| "endpoint must use the tls:// scheme".to_string())?;
    if authority_and_path.contains(['?', '#']) {
        return Err("endpoint must not include a query or fragment".to_string());
    }
    let (authority, path) = authority_and_path
        .split_once('/')
        .map_or((authority_and_path, ""), |(authority, path)| {
            (authority, path)
        });
    if !path.is_empty() {
        return Err("endpoint path must be empty or `/`".to_string());
    }
    if authority.is_empty() {
        return Err("endpoint host must not be empty".to_string());
    }
    if authority.contains('@') {
        return Err("endpoint must not include a username or password".to_string());
    }

    let (host, port) = if let Some(bracketed_host) = authority.strip_prefix('[') {
        let (host, port) = bracketed_host
            .split_once(']')
            .ok_or_else(|| "endpoint IPv6 host must end with `]`".to_string())?;
        host.parse::<Ipv6Addr>()
            .map_err(|_| "endpoint bracketed host must be an IPv6 address".to_string())?;
        let port = port
            .strip_prefix(':')
            .ok_or_else(|| "endpoint must include a port".to_string())?;
        (host, port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| "endpoint must include a port".to_string())?;
        if host.contains(':') {
            return Err("endpoint IPv6 hosts must be enclosed in `[` and `]`".to_string());
        }
        (host, port)
    };
    if host.is_empty() {
        return Err("endpoint host must not be empty".to_string());
    }
    if !matches!(port.as_bytes().first(), Some(b'1'..=b'9'))
        || !port.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("endpoint port must be a canonical number between 1 and 65535".to_string());
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| "endpoint port must be a number between 1 and 65535".to_string())?;
    if port == 0 {
        return Err("endpoint port must be a number between 1 and 65535".to_string());
    }
    Ok(host)
}

fn endpoint_schema(_: &mut SchemaGenerator) -> Schema {
    endpoint_schema_with_host_pattern(ENDPOINT_HOST_PATTERN)
}

fn advertised_endpoint_schema(_: &mut SchemaGenerator) -> Schema {
    endpoint_schema_with_host_pattern(ADVERTISED_ENDPOINT_HOST_PATTERN)
}

fn endpoint_schema_with_host_pattern(host_pattern: &str) -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        string: Some(Box::new(StringValidation {
            min_length: Some(1),
            max_length: Some(MAX_ENDPOINT_LENGTH as u32),
            pattern: Some(
                [
                    r"^tls://(?:\[",
                    IPV6_LITERAL_PATTERN,
                    r"\]|",
                    host_pattern,
                    "):",
                    ENDPOINT_PORT_PATTERN,
                    "/?$",
                ]
                .concat(),
            ),
        })),
        ..Default::default()
    })
}

fn is_valid_workspace_id(workspace_id: &str) -> bool {
    let mut characters = workspace_id.bytes();
    matches!(characters.next(), Some(character) if character.is_ascii_alphanumeric())
        && workspace_id.len() <= MAX_WORKSPACE_ID_LENGTH
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-'))
}

fn workspaces_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let workspace_id = Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        string: Some(Box::new(StringValidation {
            min_length: Some(1),
            max_length: Some(64),
            pattern: Some("^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$".to_string()),
        })),
        ..Default::default()
    });
    let validation = ObjectValidation {
        max_properties: Some(128),
        property_names: Some(Box::new(workspace_id)),
        additional_properties: Some(Box::new(schema_gen.subschema_for::<AbsolutePathBuf>())),
        ..Default::default()
    };
    Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        object: Some(Box::new(validation)),
        ..Default::default()
    })
}
