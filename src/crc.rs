//! CRC-32/MPEG-2 (poly 0x04C11DB7, init 0xFFFFFFFF, no reflection, no final XOR).

const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// CRC-32/MPEG-2 as used by PSI sections. Running it over a whole section,
/// including its trailing CRC field, yields 0 when the section is intact.
pub fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = (crc << 8) ^ TABLE[usize::from((crc >> 24) as u8 ^ b)];
    }
    crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_value() {
        assert_eq!(super::crc32_mpeg2(b"123456789"), 0x0376_E6E7);
    }
}
