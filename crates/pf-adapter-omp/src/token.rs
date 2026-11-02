//! A fresh seat token for each launch (ADR-0005).

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 32 hex characters from the kernel's random source. When that source cannot be read, the token
/// comes from the process-seeded hasher, the clock and a counter, so two launches still differ.
pub fn fresh_token() -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        use std::hash::{BuildHasher, Hasher};
        let mut out = Vec::new();
        for _ in 0..2 {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(COUNTER.fetch_add(1, Ordering::SeqCst));
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
            );
            out.extend_from_slice(&h.finish().to_le_bytes());
        }
        bytes.copy_from_slice(&out);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::fresh_token;

    #[test]
    fn tokens_differ() {
        let a = fresh_token();
        assert_eq!(a.len(), 32);
        assert_ne!(a, fresh_token());
    }
}
