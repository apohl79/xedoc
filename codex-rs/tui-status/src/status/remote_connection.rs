#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteConnectionStatus {
    pub address: String,
    pub version: String,
}
