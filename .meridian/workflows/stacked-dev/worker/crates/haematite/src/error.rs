use std::fmt;

#[derive(Debug)]
pub enum Error {
    UnsortedKeys,
    DuplicateKey,
    NodeNotFound,
    WalCorrupted,
    ShardNotFound,
    SequenceConflict,
    CasMismatch,
    BranchConflict,
    MalformedNode(&'static str),
    StoreIo(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsortedKeys => write!(f, "keys must be sorted"),
            Self::DuplicateKey => write!(f, "duplicate key"),
            Self::NodeNotFound => write!(f, "node not found in store"),
            Self::WalCorrupted => write!(f, "WAL entry corrupted"),
            Self::ShardNotFound => write!(f, "shard not found"),
            Self::SequenceConflict => write!(f, "sequence conflict on append"),
            Self::CasMismatch => write!(f, "compare-and-swap mismatch"),
            Self::BranchConflict => write!(f, "merge conflict on branch"),
            Self::MalformedNode(reason) => write!(f, "malformed node: {reason}"),
            Self::StoreIo(e) => write!(f, "store I/O error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StoreIo(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::StoreIo(e)
    }
}
