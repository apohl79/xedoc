use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use futures::future::join_all;
use tokio::sync::Mutex;
use tracing::Instrument;
use xedoc_app_server_protocol::ClientRequestSerializationScope;

use crate::connection_rpc_gate::ConnectionRpcGate;
use crate::outgoing_message::ConnectionId;

type BoxFutureUnit = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RequestSerializationQueueKey {
    Global(&'static str),
    Thread {
        thread_id: String,
    },
    ThreadPath {
        path: PathBuf,
    },
    CommandExecProcess {
        connection_id: ConnectionId,
        process_id: String,
    },
    Process {
        connection_id: ConnectionId,
        process_handle: String,
    },
    FuzzyFileSearchSession {
        session_id: String,
    },
    FsWatch {
        connection_id: ConnectionId,
        watch_id: String,
    },
    McpOauth {
        server_name: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestSerializationAccess {
    Exclusive,
    SharedRead,
}

impl RequestSerializationQueueKey {
    pub fn from_scope(
        connection_id: ConnectionId,
        scope: ClientRequestSerializationScope,
    ) -> (Self, RequestSerializationAccess) {
        match scope {
            ClientRequestSerializationScope::Global(name) => {
                (Self::Global(name), RequestSerializationAccess::Exclusive)
            }
            ClientRequestSerializationScope::GlobalSharedRead(name) => {
                (Self::Global(name), RequestSerializationAccess::SharedRead)
            }
            ClientRequestSerializationScope::Thread { thread_id } => (
                Self::Thread { thread_id },
                RequestSerializationAccess::Exclusive,
            ),
            ClientRequestSerializationScope::ThreadPath { path } => (
                Self::ThreadPath { path },
                RequestSerializationAccess::Exclusive,
            ),
            ClientRequestSerializationScope::CommandExecProcess { process_id } => (
                Self::CommandExecProcess {
                    connection_id,
                    process_id,
                },
                RequestSerializationAccess::Exclusive,
            ),
            ClientRequestSerializationScope::Process { process_handle } => (
                Self::Process {
                    connection_id,
                    process_handle,
                },
                RequestSerializationAccess::Exclusive,
            ),
            ClientRequestSerializationScope::FuzzyFileSearchSession { session_id } => (
                Self::FuzzyFileSearchSession { session_id },
                RequestSerializationAccess::Exclusive,
            ),
            ClientRequestSerializationScope::FsWatch { watch_id } => (
                Self::FsWatch {
                    connection_id,
                    watch_id,
                },
                RequestSerializationAccess::Exclusive,
            ),
            ClientRequestSerializationScope::McpOauth { server_name } => (
                Self::McpOauth { server_name },
                RequestSerializationAccess::Exclusive,
            ),
        }
    }
}

pub struct QueuedInitializedRequest {
    gate: Option<Arc<ConnectionRpcGate>>,
    future: BoxFutureUnit,
}

impl QueuedInitializedRequest {
    pub fn new(
        gate: Arc<ConnectionRpcGate>,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> Self {
        Self {
            gate: Some(gate),
            future: Box::pin(future),
        }
    }

    fn new_background(future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            gate: None,
            future: Box::pin(future),
        }
    }

    pub async fn run(self) {
        let Self { gate, future } = self;
        match gate {
            Some(gate) => gate.run(future).await,
            None => future.await,
        }
    }
}

struct QueuedSerializedRequest {
    access: RequestSerializationAccess,
    request: QueuedInitializedRequest,
}

#[derive(Clone, Default)]
pub struct RequestSerializationQueues {
    inner: Arc<Mutex<HashMap<RequestSerializationQueueKey, VecDeque<QueuedSerializedRequest>>>>,
}

impl RequestSerializationQueues {
    /// Enqueue app-owned work alongside RPCs that mutate the same serialized resource.
    pub async fn enqueue_background(
        &self,
        key: RequestSerializationQueueKey,
        access: RequestSerializationAccess,
        future: impl Future<Output = ()> + Send + 'static,
    ) {
        self.enqueue(
            key,
            access,
            QueuedInitializedRequest::new_background(future),
        )
        .await;
    }

    pub async fn enqueue(
        &self,
        key: RequestSerializationQueueKey,
        access: RequestSerializationAccess,
        request: QueuedInitializedRequest,
    ) {
        let request = QueuedSerializedRequest { access, request };
        let should_spawn = {
            let mut queues = self.inner.lock().await;
            match queues.get_mut(&key) {
                Some(queue) => {
                    queue.push_back(request);
                    false
                }
                None => {
                    let mut queue = VecDeque::new();
                    queue.push_back(request);
                    queues.insert(key.clone(), queue);
                    true
                }
            }
        };

        if should_spawn {
            let queues = self.clone();
            let span = tracing::debug_span!("app_server.serialized_request_queue", ?key);
            tokio::spawn(async move { queues.drain(key).await }.instrument(span));
        }
    }

    async fn drain(self, key: RequestSerializationQueueKey) {
        loop {
            let requests = {
                let mut queues = self.inner.lock().await;
                let Some(queue) = queues.get_mut(&key) else {
                    return;
                };
                match queue.pop_front() {
                    Some(request) => {
                        let access = request.access;
                        let mut requests = vec![request];
                        if access == RequestSerializationAccess::SharedRead {
                            while queue.front().is_some_and(|request| {
                                request.access == RequestSerializationAccess::SharedRead
                            }) {
                                let Some(request) = queue.pop_front() else {
                                    break;
                                };
                                requests.push(request);
                            }
                        }
                        requests
                    }
                    None => {
                        queues.remove(&key);
                        return;
                    }
                }
            };

            join_all(requests.into_iter().map(|request| request.request.run())).await;
        }
    }
}
