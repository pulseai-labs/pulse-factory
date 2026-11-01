//! Money text. Cost is whole micro-dollars: no float touches it.

/// `1234567` micro-dollars as `$1.234567`.
pub fn dollars(micros: u64) -> String {
    format!("${}.{:06}", micros / 1_000_000, micros % 1_000_000)
}
