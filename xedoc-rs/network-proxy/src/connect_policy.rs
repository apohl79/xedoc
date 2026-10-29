use crate::policy::is_non_public_ip;
use crate::state::NetworkProxyState;
use rama_core::Service;
use rama_core::error::BoxError;
use rama_core::error::ErrorExt as _;
use rama_core::error::OpaqueError;
use rama_core::extensions::ExtensionsMut;
use rama_net::address::ProxyAddress;
use rama_net::client::EstablishedClientConnection;
use rama_net::transport::TryRefIntoTransportContext;
use rama_tcp::TcpStream;
use rama_tcp::client::TcpStreamConnector;
use rama_tcp::client::service::TcpConnector;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct TargetCheckedTcpConnector {
    policy: TargetPolicy,
}

impl TargetCheckedTcpConnector {
    pub(crate) fn new(state: Arc<NetworkProxyState>) -> Self {
        Self {
            policy: TargetPolicy::State(state),
        }
    }

    pub(crate) fn from_allow_local_binding(allow_local_binding: bool) -> Self {
        Self {
            policy: TargetPolicy::Config {
                allow_local_binding,
            },
        }
    }
}

impl<Input> Service<Input> for TargetCheckedTcpConnector
where
    Input: TryRefIntoTransportContext + Send + ExtensionsMut + 'static,
    Input::Error: Into<BoxError> + Send + Sync + 'static,
{
    type Output = EstablishedClientConnection<TcpStream, Input>;
    type Error = BoxError;

    async fn serve(&self, input: Input) -> Result<Self::Output, Self::Error> {
        if input.extensions().get::<ProxyAddress>().is_some() {
            return TcpConnector::new().serve(input).await;
        }

        TcpConnector::new()
            .with_connector(TargetCheckedStreamConnector {
                policy: self.policy.clone(),
            })
            .serve(input)
            .await
    }
}

#[derive(Clone)]
struct TargetCheckedStreamConnector {
    policy: TargetPolicy,
}

impl TcpStreamConnector for TargetCheckedStreamConnector {
    type Error = BoxError;

    async fn connect(&self, addr: SocketAddr) -> Result<TcpStream, Self::Error> {
        if !self.policy.allow_local_binding().await? && is_non_public_ip(addr.ip()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "network target rejected by policy",
            )
            .into());
        }

        tokio::net::TcpStream::connect(addr)
            .await
            .map(TcpStream::from)
            .map_err(Into::into)
    }
}

#[derive(Clone)]
enum TargetPolicy {
    Config { allow_local_binding: bool },
    State(Arc<NetworkProxyState>),
}

impl TargetPolicy {
    async fn allow_local_binding(&self) -> Result<bool, BoxError> {
        match self {
            Self::Config {
                allow_local_binding,
            } => Ok(*allow_local_binding),
            Self::State(state) => state.allow_local_binding().await.map_err(|err| {
                let err: BoxError = err.into();
                OpaqueError::from_boxed(err)
                    .context("read network proxy config")
                    .into_boxed()
            }),
        }
    }
}
