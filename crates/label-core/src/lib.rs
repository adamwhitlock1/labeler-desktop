//! LT200B wire protocol: pure, hardware-free byte-level job encoding.
//!
//! This is an independent reimplementation of the protocol documented (via
//! reverse engineering) by sgilissen/LT200B-gui and alexhorn/lt200b, both
//! AGPL-3.0. No code from either project is copied here; only the wire
//! format itself (byte layout, checksum, chunking) was studied and ported.

/// A 1-bit raster ready to print.
///
/// `cross` is the print head's fixed cross-feed resolution (32 dots on the
/// LT200B). `feed` is the variable length along the feed direction. `bits`
/// is row-major: `feed` rows of `cross` bits each, `true` = black.
#[derive(Debug, Clone)]
pub struct Raster {
    pub cross: u32,
    pub feed: u32,
    pub bits: Vec<bool>,
}

impl Raster {
    pub fn new(cross: u32, feed: u32, bits: Vec<bool>) -> Self {
        assert_eq!(
            bits.len() as u64,
            cross as u64 * feed as u64,
            "bit buffer length does not match cross*feed"
        );
        Self { cross, feed, bits }
    }
}

/// The print head's fixed cross-feed resolution, in dots.
pub const PRINT_HEAD_DOTS: u32 = 32;

const CHUNK_SIZE: usize = 500;

/// First 4 bytes of the primary GATT service UUID the printer advertises
/// (`be3dd650-xxxx-xxxx-xxxx-xxxxxxxxxxxx`). The middle segment varies per
/// device/firmware and must be discovered at connect time.
pub const SERVICE_UUID_PREFIX: &str = "be3dd650";

/// Builds the printable-data characteristic UUID given the service's
/// second UUID segment (discovered from the advertised service UUID).
pub fn characteristic_uuid(service_second_segment: &str) -> String {
    format!("be3dd651-{service_second_segment}-42f1-99c1-f0f749dd0678")
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

fn pack_bits_lsb_first(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            let mut byte = 0u8;
            for (i, &b) in chunk.iter().enumerate() {
                if b {
                    byte |= 1 << i;
                }
            }
            byte
        })
        .collect()
}

fn start_job() -> Vec<u8> {
    vec![0x1B, 0x73, 0x9A, 0x02, 0x00, 0x00]
}

fn print_data(packed: &[u8], feed: u32, cross: u32) -> Vec<u8> {
    let mut out = vec![0x1B, 0x44, 0x01, 0x02];
    out.extend_from_slice(&feed.to_le_bytes());
    out.extend_from_slice(&cross.to_le_bytes());
    out.extend_from_slice(packed);
    out
}

fn form_feed() -> Vec<u8> {
    vec![0x1B, 0x45]
}

fn status() -> Vec<u8> {
    vec![0x1B, 0x41]
}

fn end() -> Vec<u8> {
    vec![0x1B, 0x51]
}

fn header_bytes(body_len: u32) -> Vec<u8> {
    let mut preamble = vec![0xFFu8, 0xF0, 0x12, 0x34];
    preamble.extend_from_slice(&body_len.to_le_bytes());
    let cksum = checksum(&preamble);
    preamble.push(cksum);
    preamble
}

fn split_chunks(data: &[u8]) -> Vec<Vec<u8>> {
    let mut chunks: Vec<Vec<u8>> = data
        .chunks(CHUNK_SIZE)
        .enumerate()
        .map(|(i, c)| {
            let mut v = Vec::with_capacity(c.len() + 1);
            v.push(i as u8);
            v.extend_from_slice(c);
            v
        })
        .collect();
    if let Some(last) = chunks.last_mut() {
        last.push(0x12);
        last.push(0x34);
    }
    chunks
}

/// Encodes a raster into the full sequence of BLE writes needed to print
/// it: the 9-byte header first, then one write per 500-byte body chunk
/// (each prefixed with its chunk index; the final chunk carries a 2-byte
/// `12 34` trailer).
pub fn create_job(raster: &Raster) -> Vec<Vec<u8>> {
    let packed = pack_bits_lsb_first(&raster.bits);
    let mut body = Vec::new();
    body.extend(start_job());
    body.extend(print_data(&packed, raster.feed, raster.cross));
    body.extend(form_feed());
    body.extend(status());
    body.extend(end());

    let header = header_bytes(body.len() as u32);
    let mut out = vec![header];
    out.extend(split_chunks(&body));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_hand_computed_value() {
        // FF + F0 + 12 + 34 = 255 + 240 + 18 + 52 = 565; 565 & 0xFF = 0x35
        assert_eq!(checksum(&[0xFF, 0xF0, 0x12, 0x34]), 0x35);
    }

    #[test]
    fn header_bytes_for_zero_length_body() {
        assert_eq!(
            header_bytes(0),
            vec![0xFF, 0xF0, 0x12, 0x34, 0x00, 0x00, 0x00, 0x00, 0x35]
        );
    }

    #[test]
    fn header_bytes_checksum_includes_length() {
        // length = 1 -> preamble FF F0 12 34 01 00 00 00, sum = 565 + 1 = 566
        // 566 & 0xFF = 0x36
        assert_eq!(
            header_bytes(1),
            vec![0xFF, 0xF0, 0x12, 0x34, 0x01, 0x00, 0x00, 0x00, 0x36]
        );
    }

    #[test]
    fn pack_bits_is_lsb_first() {
        // first bit -> least significant bit of the byte
        assert_eq!(pack_bits_lsb_first(&[true, false, false, false, false, false, false, false]), vec![0x01]);
        assert_eq!(pack_bits_lsb_first(&[false, false, false, false, false, false, false, true]), vec![0x80]);
        assert_eq!(pack_bits_lsb_first(&[true, true, false, false, false, false, false, false]), vec![0x03]);
    }

    #[test]
    fn split_chunks_single_short_chunk_gets_trailer() {
        let data = vec![0xAA, 0xBB, 0xCC];
        let chunks = split_chunks(&data);
        assert_eq!(chunks, vec![vec![0x00, 0xAA, 0xBB, 0xCC, 0x12, 0x34]]);
    }

    #[test]
    fn split_chunks_exact_boundary_produces_two_chunks() {
        let data = vec![0x01u8; 1000];
        let chunks = split_chunks(&data);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 501); // index byte + 500 data bytes, no trailer
        assert_eq!(chunks[0][0], 0x00);
        assert_eq!(chunks[1].len(), 503); // index byte + 500 data bytes + 2 trailer bytes
        assert_eq!(chunks[1][0], 0x01);
        assert_eq!(&chunks[1][498..], &[0x01, 0x01, 0x01, 0x12, 0x34]);
    }

    #[test]
    fn create_job_header_length_matches_body() {
        // 32 cross x 8 feed rows, all white (false) -> 32 bytes packed data
        let raster = Raster::new(32, 8, vec![false; 32 * 8]);
        let job = create_job(&raster);
        let header = &job[0];
        let declared_len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);

        let body_len: usize = job[1..]
            .iter()
            .enumerate()
            .map(|(i, c)| {
                // undo the index-byte and (for the last chunk) trailer we added
                let is_last = i == job.len() - 2;
                c.len() - 1 - if is_last { 2 } else { 0 }
            })
            .sum();
        assert_eq!(declared_len as usize, body_len);

        // start_job(6) + print_data header(4+4+4=12) + packed(32) + form_feed(2) + status(2) + end(2)
        assert_eq!(body_len, 6 + 12 + 32 + 2 + 2 + 2);
    }

    #[test]
    #[should_panic]
    fn raster_new_panics_on_mismatched_length() {
        Raster::new(32, 8, vec![false; 10]);
    }
}
