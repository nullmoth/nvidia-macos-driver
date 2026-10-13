//! Fast, deterministic hashing for the translator's maps and sets.
//!
//! Almost every key here is a SPIR-V id, an LLVM value name or a small tuple of them. The standard library's
//! SipHash guards against adversarial keys, which a shader compiler does not need, and was 40-50 % of translation
//! time. This is the Fx hash (rustc's own): one multiply per word. Build maps with `HashMap::default()`;
//! `HashMap::from([...])` is [`map_from`].

use std::hash::{BuildHasherDefault, Hash, Hasher};

#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0xf135_7aea_2e62_a9c5;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = self.hash.wrapping_add(word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.add(u64::from_le_bytes(chunk.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut tail = [0u8; 8];
            tail[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(tail) ^ ((rest.len() as u64) << 59));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64)
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64)
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64)
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i)
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64)
    }
    #[inline]
    fn finish(&self) -> u64 {
        // The multiply leaves the low bits weak; hashbrown picks buckets from the low bits.
        self.hash.rotate_left(26)
    }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
pub type HashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;
pub type HashSet<T> = std::collections::HashSet<T, FxBuildHasher>;

/// `HashMap::from([(k, v), ...])` for the Fx-hashed map.
pub fn map_from<K: Eq + Hash, V, I: IntoIterator<Item = (K, V)>>(items: I) -> HashMap<K, V> {
    items.into_iter().collect()
}

/// `HashSet::from([a, b, ...])` for the Fx-hashed set.
pub fn set_from<T: Eq + Hash, I: IntoIterator<Item = T>>(items: I) -> HashSet<T> {
    items.into_iter().collect()
}
