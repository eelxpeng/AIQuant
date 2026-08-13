//! CRC32, the IEEE reflected variant.
//!
//! Hand-rolled rather than pulled in. `types` has zero dependencies and the
//! workspace has three dev-dependencies in total; forty lines with a published
//! test vector is cheaper than another crate in a real-money system's supply
//! chain (ADR, recorded log format, D-6).

/// Reflected IEEE 802.3 polynomial.
const POLY: u32 = 0xEDB8_8320;

/// Built at compile time, so there is no initialization order to get wrong.
static TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut byte = 0usize;
    while byte < 256 {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
}

/// The checksum of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = TABLE[index] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_matches_the_published_check_value() {
        // The standard CRC-32/ISO-HDLC check value. If this fails, the
        // implementation is not the algorithm it claims to be, and every file
        // it has stamped is stamped with something else.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn an_empty_input_has_a_zero_checksum() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_single_flipped_bit_changes_the_checksum() {
        let mut bytes = [0u8; 80];
        let before = crc32(&bytes);
        bytes[40] ^= 0x01;
        assert_ne!(crc32(&bytes), before);
    }

    #[test]
    fn a_trailing_zero_is_not_invisible() {
        // A checksum that ignored length would give these the same value, and
        // a truncated record would pass as an intact one.
        assert_ne!(crc32(b"abc"), crc32(b"abc\0"));
    }
}
