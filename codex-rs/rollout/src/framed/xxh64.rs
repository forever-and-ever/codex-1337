//! XXH64, the content checksum zstd frames carry; sealing computes it over the kept lines.

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

fn round(accumulator: u64, lane: u64) -> u64 {
    accumulator
        .wrapping_add(lane.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(hash: u64, accumulator: u64) -> u64 {
    (hash ^ round(0, accumulator))
        .wrapping_mul(P1)
        .wrapping_add(P4)
}

/// XXH64 with seed 0.
pub(super) fn xxh64(data: &[u8]) -> u64 {
    let (stripes, mut rest) = data.as_chunks::<32>();
    let mut hash = if stripes.is_empty() {
        P5
    } else {
        let mut accumulators = [P1.wrapping_add(P2), P2, 0, 0u64.wrapping_sub(P1)];
        for stripe in stripes {
            for (index, accumulator) in accumulators.iter_mut().enumerate() {
                *accumulator = round(*accumulator, super::u64_le(&stripe[index * 8..]));
            }
        }
        let [a, b, c, d] = accumulators;
        let hash = a
            .rotate_left(1)
            .wrapping_add(b.rotate_left(7))
            .wrapping_add(c.rotate_left(12))
            .wrapping_add(d.rotate_left(18));
        accumulators.into_iter().fold(hash, merge)
    };
    hash = hash.wrapping_add(data.len() as u64);
    while rest.len() >= 8 {
        hash ^= round(0, super::u64_le(rest));
        hash = hash.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        hash ^= u64::from(super::u32_le(rest)).wrapping_mul(P1);
        hash = hash.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        rest = &rest[4..];
    }
    for &byte in rest {
        hash ^= u64::from(byte).wrapping_mul(P5);
        hash = hash.rotate_left(11).wrapping_mul(P1);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(P3);
    hash ^ (hash >> 32)
}
