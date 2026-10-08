//! Identifier generation.
//!
//! Chrome uses 32-character uppercase hex strings for target ids, session ids
//! and browser context ids. Clients parse them as opaque strings, but matching
//! the shape avoids surprises in libraries that pattern-match on length.

use std::time::{SystemTime, UNIX_EPOCH};

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Seeds a counter so ids differ between runs.
pub fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5DEE_CE66_D1CE_1234);
    nanos ^ (std::process::id() as u64).rotate_left(17)
}

/// Advances the counter and returns a fresh 32-character hex id.
pub fn token(counter: &mut u64) -> String {
    let high = splitmix64(counter);
    let low = splitmix64(counter);
    format!("{high:016X}{low:016X}")
}
