"""Bounded, local-only orchestration for the Xedoc remote-agent broker.

The public objects validate host configuration, call an initialized local
controller, keep session operations bounded, and own a foreground daemon
lifecycle.
"""

from .catalog import SessionCatalog
from .controller import BrokerController
from .daemon import Daemon, DaemonPaths, RemoteAgentDaemon
from .errors import BrokerError, ErrorCode
from .ipc import LocalIpcClient, LocalIpcServer
from .models import (
    BrokerConfig,
    BrokerLimits,
    OperationHandle,
    OperationRecord,
    OperationState,
    Role,
    SearchResult,
    SessionPage,
    SessionSummary,
    Workspace,
)
from .operations import SessionOperations
from .peer import PeerEndpoint, PeerService
from .peer_state import HostIdentity, PeerState
from .workspaces import (
    BootstrapDescriptor,
    WorkspaceRegistry,
    load_bootstrap_descriptor,
    load_broker_config,
)

__all__ = [
    "BootstrapDescriptor",
    "BrokerConfig",
    "BrokerController",
    "BrokerError",
    "BrokerLimits",
    "Daemon",
    "DaemonPaths",
    "ErrorCode",
    "LocalIpcClient",
    "LocalIpcServer",
    "OperationRecord",
    "OperationHandle",
    "OperationState",
    "HostIdentity",
    "PeerEndpoint",
    "PeerService",
    "PeerState",
    "Role",
    "RemoteAgentDaemon",
    "SearchResult",
    "SessionCatalog",
    "SessionPage",
    "SessionSummary",
    "SessionOperations",
    "Workspace",
    "WorkspaceRegistry",
    "load_bootstrap_descriptor",
    "load_broker_config",
]
