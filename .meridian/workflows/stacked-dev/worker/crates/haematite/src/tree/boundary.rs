pub const DEFAULT_TARGET_SIZE: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryDetector {
    target_size: usize,
    threshold: u64,
}

impl BoundaryDetector {
    pub fn new(target_size: usize) -> Self {
        let target_size = target_size.max(1);
        let threshold = u64::MAX / target_size as u64;
        Self {
            target_size,
            threshold,
        }
    }

    pub const fn target_size(&self) -> usize {
        self.target_size
    }

    pub const fn threshold(&self) -> u64 {
        self.threshold
    }

    pub fn is_boundary(&self, key: &[u8]) -> bool {
        stable_key_hash(key) <= self.threshold
    }
}

impl Default for BoundaryDetector {
    fn default() -> Self {
        Self::new(DEFAULT_TARGET_SIZE)
    }
}

fn stable_key_hash(key: &[u8]) -> u64 {
    let digest = blake3::hash(key);
    let bytes = digest.as_bytes();
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}
