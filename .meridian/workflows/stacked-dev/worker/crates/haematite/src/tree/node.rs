use std::fmt;

use crate::Error;

const LEAF_TAG: u8 = 1;
const INTERNAL_TAG: u8 = 2;
const HASH_LEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash([u8; HASH_LEN]);

impl Hash {
    pub const fn from_bytes(bytes: [u8; HASH_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; HASH_LEN] {
        &self.0
    }

    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafNode {
    entries: Vec<(Vec<u8>, Vec<u8>)>,
}

impl LeafNode {
    pub fn new(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Result<Self, Error> {
        validate_sorted_keys(entries.iter().map(|(key, _)| key))?;
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.entries
    }

    pub fn serialise(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(LEAF_TAG);
        write_u64(&mut bytes, self.entries.len());
        for (key, value) in &self.entries {
            write_len_prefixed(&mut bytes, key);
            write_len_prefixed(&mut bytes, value);
        }
        bytes
    }

    pub fn deserialise(bytes: &[u8]) -> Result<Self, Error> {
        match Node::deserialise(bytes)? {
            Node::Leaf(node) => Ok(node),
            Node::Internal(_) => Err(Error::MalformedNode("expected leaf node")),
        }
    }

    pub fn hash(&self) -> Hash {
        Hash::of(&self.serialise())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalNode {
    children: Vec<(Vec<u8>, Hash)>,
}

impl InternalNode {
    pub fn new(children: Vec<(Vec<u8>, Hash)>) -> Result<Self, Error> {
        validate_sorted_keys(children.iter().map(|(key, _)| key))?;
        Ok(Self { children })
    }

    pub fn children(&self) -> &[(Vec<u8>, Hash)] {
        &self.children
    }

    pub fn serialise(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(INTERNAL_TAG);
        write_u64(&mut bytes, self.children.len());
        for (key, child_hash) in &self.children {
            write_len_prefixed(&mut bytes, key);
            bytes.extend_from_slice(child_hash.as_bytes());
        }
        bytes
    }

    pub fn deserialise(bytes: &[u8]) -> Result<Self, Error> {
        match Node::deserialise(bytes)? {
            Node::Internal(node) => Ok(node),
            Node::Leaf(_) => Err(Error::MalformedNode("expected internal node")),
        }
    }

    pub fn hash(&self) -> Hash {
        Hash::of(&self.serialise())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Leaf(LeafNode),
    Internal(InternalNode),
}

impl Node {
    pub fn hash(&self) -> Hash {
        match self {
            Self::Leaf(node) => node.hash(),
            Self::Internal(node) => node.hash(),
        }
    }

    pub fn serialise(&self) -> Vec<u8> {
        match self {
            Self::Leaf(node) => node.serialise(),
            Self::Internal(node) => node.serialise(),
        }
    }

    pub fn deserialise(bytes: &[u8]) -> Result<Self, Error> {
        let mut cursor = Cursor::new(bytes);
        let tag = cursor.read_u8()?;
        let count = cursor.read_u64_as_usize()?;
        let node = match tag {
            LEAF_TAG => Self::Leaf(read_leaf(&mut cursor, count)?),
            INTERNAL_TAG => Self::Internal(read_internal(&mut cursor, count)?),
            _ => return Err(Error::MalformedNode("unknown node tag")),
        };
        cursor.finish()?;
        Ok(node)
    }
}

fn validate_sorted_keys<'a>(keys: impl IntoIterator<Item = &'a Vec<u8>>) -> Result<(), Error> {
    let mut previous: Option<&Vec<u8>> = None;
    for key in keys {
        if let Some(previous_key) = previous {
            if key == previous_key {
                return Err(Error::DuplicateKey);
            }
            if key < previous_key {
                return Err(Error::UnsortedKeys);
            }
        }
        previous = Some(key);
    }
    Ok(())
}

fn read_leaf(cursor: &mut Cursor<'_>, count: usize) -> Result<LeafNode, Error> {
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let key = cursor.read_len_prefixed()?.to_vec();
        let value = cursor.read_len_prefixed()?.to_vec();
        entries.push((key, value));
    }
    LeafNode::new(entries)
}

fn read_internal(cursor: &mut Cursor<'_>, count: usize) -> Result<InternalNode, Error> {
    let mut children = Vec::with_capacity(count);
    for _ in 0..count {
        let key = cursor.read_len_prefixed()?.to_vec();
        let hash = Hash::from_bytes(cursor.read_hash()?);
        children.push((key, hash));
    }
    InternalNode::new(children)
}

fn write_u64(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(&(value as u64).to_be_bytes());
}

fn write_len_prefixed(bytes: &mut Vec<u8>, value: &[u8]) {
    write_u64(bytes, value.len());
    bytes.extend_from_slice(value);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, Error> {
        let bytes = self.read_exact(1)?;
        Ok(bytes[0])
    }

    fn read_u64_as_usize(&mut self) -> Result<usize, Error> {
        let bytes = self.read_exact(8)?;
        let value = u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        usize::try_from(value).map_err(|_| Error::MalformedNode("length does not fit usize"))
    }

    fn read_len_prefixed(&mut self) -> Result<&'a [u8], Error> {
        let length = self.read_u64_as_usize()?;
        self.read_exact(length)
    }

    fn read_hash(&mut self) -> Result<[u8; HASH_LEN], Error> {
        let bytes = self.read_exact(HASH_LEN)?;
        let mut hash = [0; HASH_LEN];
        hash.copy_from_slice(bytes);
        Ok(hash)
    }

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(Error::MalformedNode("node length overflow"))?;
        if end > self.bytes.len() {
            return Err(Error::MalformedNode("truncated node bytes"));
        }
        let bytes = &self.bytes[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn finish(&self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::MalformedNode("trailing node bytes"))
        }
    }
}
