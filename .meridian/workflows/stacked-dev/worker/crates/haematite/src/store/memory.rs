use std::collections::HashMap;

use crate::tree::{Hash, Node};

pub trait NodeStore {
    fn get(&self, hash: &Hash) -> Option<Node>;

    fn put(&mut self, node: &Node) -> Hash;
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    nodes: HashMap<Hash, Vec<u8>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl NodeStore for MemoryStore {
    fn get(&self, hash: &Hash) -> Option<Node> {
        self.nodes
            .get(hash)
            .and_then(|bytes| Node::deserialise(bytes).ok())
    }

    fn put(&mut self, node: &Node) -> Hash {
        let hash = node.hash();
        self.nodes.insert(hash, node.serialise());
        hash
    }
}
