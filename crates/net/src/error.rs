#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("quic connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("quic connection: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("quic write: {0}")]
    Write(#[from] quinn::WriteError),
    #[error("quic read: {0}")]
    Read(#[from] quinn::ReadError),
    #[error("frame: {0}")]
    Frame(#[from] deskbridge_proto::frame::FrameError),
    #[error("platform: {0}")]
    Platform(#[from] deskbridge_platform::PlatformError),
    #[error("peer is not paired (fingerprint {0})")]
    UnknownPeer(String),
    #[error("peer presented an unexpected certificate")]
    FingerprintMismatch,
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("pairing is locked for {0} s after too many wrong codes")]
    Locked(u64),
    #[error("incompatible protocol version (peer {peer:#x}, own {own:#x})")]
    IncompatibleVersion { peer: u32, own: u32 },
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("identity: {0}")]
    Identity(String),
    #[error("config: {0}")]
    Config(String),
    #[error("discovery: {0}")]
    Discovery(String),
}

pub type Result<T> = std::result::Result<T, NetError>;
