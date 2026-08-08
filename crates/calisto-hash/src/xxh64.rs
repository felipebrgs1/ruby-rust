//! xxHash64 (xxh64) — o `Bun.hash` do Ruby (Bun usa xxHash64/wyhash).
//!
//! Hash NAO-criptografico de 64 bits, ~5-8GB/s. A stdlib Ruby nao tem hash
//! de bytes nao-criptografico — apps usam Digest::SHA256 para cache keys
//! (ActiveSupport::Cache), sharding e dedup; o xxh64 e o substituto certo
//! para esses casos (criptografia continua sendo Digest). Implementacao
//! one-shot fiel ao xxhash.c oficial (algoritmo public domain, domínio
//! público — mesmo nome/constantes do reference C).
//!
//! Vetores: os do sanity check oficial do repo Cyan4973/xxHash
//! (cli/xsum_sanity_check.c, XSUM_XXH64_testdata) — buffer pseudo-aleatorio
//! determinístico com seeds 0 e PRIME32.

pub const P1: u64 = 0x9E37_79B1_85EB_CA87;
pub const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
pub const P3: u64 = 0x1656_67B1_9E37_79F9;
pub const P4: u64 = 0x85EB_CA77_C2B2_AE63;
pub const P5: u64 = 0x27D4_EB2F_1656_67C5;

fn rotl(x: u64, r: u32) -> u64 {
    x.rotate_left(r)
}

fn read64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

fn round(acc: u64, input: u64) -> u64 {
    let acc = acc.wrapping_add(input.wrapping_mul(P2));
    rotl(acc, 31).wrapping_mul(P1)
}

fn merge_round(acc: u64, val: u64) -> u64 {
    // C: val = round(0, val); acc ^= val; return acc * P1 + P4 (SEM rotl)
    let v = round(0, val);
    let acc = acc ^ v;
    acc.wrapping_mul(P1).wrapping_add(P4)
}

/// One-shot XXH64(data, seed) — espelho do xxhash.c (update + digest de
/// uma vez, sem estado).
pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let len = data.len();
    let mut p = 0usize;
    let mut h: u64;
    if len >= 32 {
        let mut v1 = seed.wrapping_add(P1).wrapping_add(P2);
        let mut v2 = seed.wrapping_add(P2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(P1);
        let limit = len - 32;
        while p <= limit {
            v1 = round(v1, read64(data, p));
            p += 8;
            v2 = round(v2, read64(data, p));
            p += 8;
            v3 = round(v3, read64(data, p));
            p += 8;
            v4 = round(v4, read64(data, p));
            p += 8;
        }
        h = rotl(v1, 1)
            .wrapping_add(rotl(v2, 7))
            .wrapping_add(rotl(v3, 12))
            .wrapping_add(rotl(v4, 18));
        h = merge_round(h, v1);
        h = merge_round(h, v2);
        h = merge_round(h, v3);
        h = merge_round(h, v4);
    } else {
        h = seed.wrapping_add(P5);
    }
    h = h.wrapping_add(len as u64);

    // cauda (<32 bytes restantes): 8, 4, 1
    while p + 8 <= len {
        h ^= round(0, read64(data, p));
        h = rotl(h, 27).wrapping_mul(P1).wrapping_add(P4);
        p += 8;
    }
    if p + 4 <= len {
        h ^= u64::from(u32::from_le_bytes(data[p..p + 4].try_into().unwrap())).wrapping_mul(P1);
        h = rotl(h, 23).wrapping_mul(P2).wrapping_add(P3);
        p += 4;
    }
    while p < len {
        h ^= (data[p] as u64).wrapping_mul(P5);
        h = rotl(h, 11).wrapping_mul(P1);
        p += 1;
    }

    // avalanche final
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^= h >> 32;
    h
}

/// Buffer do sanity check oficial: 2367 bytes, byte[i] = (gen >> 56) & 0xff
/// com gen comecando em PRIME32 e multiplicando pelo PRIME64 DO TESTE
/// (0x9E3779B185EBCA8D — diferente do P1 do algoritmo!) a cada passo.
#[cfg(test)]
pub fn sanity_buffer() -> Vec<u8> {
    let mut gen: u64 = 2654435761;
    let prime64: u64 = 0x9E37_79B1_85EB_CA8D;
    let mut buf = Vec::with_capacity(2367);
    for _ in 0..2367 {
        buf.push((gen >> 56) as u8);
        gen = gen.wrapping_mul(prime64);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vetores oficiais (Cyan4973/xxHash cli/xsum_sanity_check.c).
    /// (len, seed, hash) sobre o sanity_buffer.
    #[test]
    fn official_vectors() {
        let buf = sanity_buffer();
        let cases: &[(usize, u64, u64)] = &[
            (0, 0, 0xEF46DB3751D8E999),
            (0, 2654435761, 0xAC75FDA2929B17EF),
            (1, 0, 0xE934A84ADB052768),
            (1, 2654435761, 0x5014607643A9B4C3),
            (4, 0, 0x9136A0DCA57457EE),
            (14, 0, 0x8282DCC4994E35C8),
            (14, 2654435761, 0xC3BD6BF63DEB6DF0),
            (222, 0, 0xB641AE8CB691C174),
            (222, 2654435761, 0x20CB8AB7AE10C14A),
        ];
        for &(len, seed, expected) in cases {
            assert_eq!(
                xxh64(&buf[..len], seed),
                expected,
                "xxh64(len={len}, seed={seed})"
            );
        }
    }
}

#[cfg(test)]
mod cground {
    use super::*;

    #[test]
    fn matches_c_reference() {
        let buf = sanity_buffer();
        let cases: &[(usize, u64, u64)] = &[
            (32, 0, 0x18B216492BB44B70),
            (33, 0, 0x55C8DC3E578F5B59),
            (222, 0, 0xB641AE8CB691C174),
            (222, 2654435761, 0x20CB8AB7AE10C14A),
            (512, 0, 0x4358D2FDD62B58A7),
        ];
        for &(len, seed, expected) in cases {
            assert_eq!(xxh64(&buf[..len], seed), expected, "len={len} seed={seed}");
        }
    }
}
