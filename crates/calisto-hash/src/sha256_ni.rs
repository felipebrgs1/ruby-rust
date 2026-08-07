//! SHA-256 com SHA-NI (Intel SHA extensions) — Fase P do roadmap.
//!
//! Port fiel do codigo de referencia do whitepaper da Intel
//! (noloader/SHA-Intrinsics, sha256-x86.c — public domain, baseado no
//! codigo do Sean Gulley para o miTLS). O escalar puro do `sha256.rs`
//! empata com o Digest::SHA256 do ruby (~300 MB/s); o SHA-NI leva a
//! 1-3 GB/s — o "10x" do marco da Fase P contra a stdlib.
//!
//! Layout do estado da instrucao: xmm1 = {C,D,G,H} (in/out), xmm2 =
//! {A,B,E,F} — cada `sha256rnds2` computa 4 rodadas e devolve UMA metade;
//! as duas chamadas por grupo usam o MSG com as metades trocadas
//! (shuffle 0x0E), como no codigo de referencia.

use std::arch::x86_64::*;

/// Pares de constantes K na ordem de uso (hi, lo) — espelha os
/// `_mm_set_epi64x` do codigo de referencia (grupos de 4 rodadas).
static K: [[u64; 2]; 16] = [
    [0xE9B5DBA5B5C0FBCF, 0x71374491428A2F98],
    [0xAB1C5ED5923F82A4, 0x59F111F13956C25B],
    [0x550C7DC3243185BE, 0x12835B01D807AA98],
    [0xC19BF1749BDC06A7, 0x80DEB1FE72BE5D74],
    [0x240CA1CC0FC19DC6, 0xEFBE4786E49B69C1],
    [0x76F988DA5CB0A9DC, 0x4A7484AA2DE92C6F],
    [0xBF597FC7B00327C8, 0xA831C66D983E5152],
    [0x1429296706CA6351, 0xD5A79147C6E00BF3],
    [0x53380D134D2C6DFC, 0x2E1B213827B70A85],
    [0x92722C8581C2C92E, 0x766A0ABB650A7354],
    [0xC76C51A3C24B8B70, 0xA81A664BA2BFE8A1],
    [0x106AA070F40E3585, 0xD6990624D192E819],
    [0x34B0BCB52748774C, 0x1E376C0819A4C116],
    [0x682E6FF35B9CCA4F, 0x4ED8AA4A391C0CB3],
    [0x8CC7020884C87814, 0x78A5636F748F82EE],
    [0xC67178F2BEF9A3F7, 0xA4506CEB90BEFFFA],
];

/// Hash completo (padding incluido) — usa a compressao SHA-NI por bloco.
/// So chamar com `is_x86_feature_detected!("sha")` (o dispatch fica no
/// sha256.rs); sem a feature, a instrucao da SIGILL.
#[target_feature(enable = "sha,sse4.1")]
pub unsafe fn hash(input: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut blocks = input.chunks_exact(64);
    for block in &mut blocks {
        compress_ni(&mut h, block.try_into().unwrap());
    }
    let rem = blocks.remainder();
    let bit_len = (input.len() as u64) << 3;

    // padding (FIPS 180-4, sec. 5.1.1) — mesmo layout do sha256 escalar
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let two_blocks = rem.len() >= 56;
    let pad_end = if two_blocks { 120 } else { 56 };
    tail[pad_end..pad_end + 8].copy_from_slice(&bit_len.to_be_bytes());
    compress_ni(&mut h, tail[..64].try_into().unwrap());
    if two_blocks {
        compress_ni(&mut h, tail[64..128].try_into().unwrap());
    }

    let mut out = [0u8; 32];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

#[target_feature(enable = "sha,sse4.1")]
unsafe fn compress_ni(h: &mut [u32; 8], block: &[u8; 64]) {
    let mask = _mm_set_epi64x(0x0c0d0e0f08090a0b, 0x0405060700010203);

    // carrega o estado no layout interleaved da instrucao
    let tmp = _mm_loadu_si128(h.as_ptr() as *const __m128i);
    let mut state1 = _mm_loadu_si128(h.as_ptr().add(4) as *const __m128i);
    let tmp = _mm_shuffle_epi32(tmp, 0xB1); /* CDAB */
    state1 = _mm_shuffle_epi32(state1, 0x1B); /* EFGH -> HGFE */
    let mut state0 = _mm_alignr_epi8(tmp, state1, 8); /* ABEF */
    state1 = _mm_blend_epi16(state1, tmp, 0xF0); /* CDGH */
    let abef_save = state0;
    let cdgh_save = state1;

    let load_words = |off: usize| {
        let m = _mm_loadu_si128(block.as_ptr().add(off) as *const __m128i);
        _mm_shuffle_epi8(m, mask)
    };

    // Rounds 0-3
    let mut msg0 = load_words(0);
    let mut msg = _mm_add_epi32(msg0, k(0));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

    // Rounds 4-7
    let mut msg1 = load_words(16);
    msg = _mm_add_epi32(msg1, k(1));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg0 = _mm_sha256msg1_epu32(msg0, msg1);

    // Rounds 8-11
    let mut msg2 = load_words(32);
    msg = _mm_add_epi32(msg2, k(2));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg1 = _mm_sha256msg1_epu32(msg1, msg2);

    // Rounds 12-15
    let mut msg3 = load_words(48);
    msg = _mm_add_epi32(msg3, k(3));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    let mut tmp = _mm_alignr_epi8(msg3, msg2, 4);
    msg0 = _mm_add_epi32(msg0, tmp);
    msg0 = _mm_sha256msg2_epu32(msg0, msg3);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg2 = _mm_sha256msg1_epu32(msg2, msg3);

    // Rounds 16-19
    msg = _mm_add_epi32(msg0, k(4));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg0, msg3, 4);
    msg1 = _mm_add_epi32(msg1, tmp);
    msg1 = _mm_sha256msg2_epu32(msg1, msg0);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg3 = _mm_sha256msg1_epu32(msg3, msg0);

    // Rounds 20-23
    msg = _mm_add_epi32(msg1, k(5));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg1, msg0, 4);
    msg2 = _mm_add_epi32(msg2, tmp);
    msg2 = _mm_sha256msg2_epu32(msg2, msg1);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg0 = _mm_sha256msg1_epu32(msg0, msg1);

    // Rounds 24-27
    msg = _mm_add_epi32(msg2, k(6));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg2, msg1, 4);
    msg3 = _mm_add_epi32(msg3, tmp);
    msg3 = _mm_sha256msg2_epu32(msg3, msg2);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg1 = _mm_sha256msg1_epu32(msg1, msg2);

    // Rounds 28-31
    msg = _mm_add_epi32(msg3, k(7));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg3, msg2, 4);
    msg0 = _mm_add_epi32(msg0, tmp);
    msg0 = _mm_sha256msg2_epu32(msg0, msg3);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg2 = _mm_sha256msg1_epu32(msg2, msg3);

    // Rounds 32-35
    msg = _mm_add_epi32(msg0, k(8));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg0, msg3, 4);
    msg1 = _mm_add_epi32(msg1, tmp);
    msg1 = _mm_sha256msg2_epu32(msg1, msg0);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg3 = _mm_sha256msg1_epu32(msg3, msg0);

    // Rounds 36-39
    msg = _mm_add_epi32(msg1, k(9));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg1, msg0, 4);
    msg2 = _mm_add_epi32(msg2, tmp);
    msg2 = _mm_sha256msg2_epu32(msg2, msg1);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg0 = _mm_sha256msg1_epu32(msg0, msg1);

    // Rounds 40-43
    msg = _mm_add_epi32(msg2, k(10));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg2, msg1, 4);
    msg3 = _mm_add_epi32(msg3, tmp);
    msg3 = _mm_sha256msg2_epu32(msg3, msg2);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg1 = _mm_sha256msg1_epu32(msg1, msg2);

    // Rounds 44-47
    msg = _mm_add_epi32(msg3, k(11));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg3, msg2, 4);
    msg0 = _mm_add_epi32(msg0, tmp);
    msg0 = _mm_sha256msg2_epu32(msg0, msg3);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg2 = _mm_sha256msg1_epu32(msg2, msg3);

    // Rounds 48-51
    msg = _mm_add_epi32(msg0, k(12));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg0, msg3, 4);
    msg1 = _mm_add_epi32(msg1, tmp);
    msg1 = _mm_sha256msg2_epu32(msg1, msg0);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
    msg3 = _mm_sha256msg1_epu32(msg3, msg0);

    // Rounds 52-55
    msg = _mm_add_epi32(msg1, k(13));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg1, msg0, 4);
    msg2 = _mm_add_epi32(msg2, tmp);
    msg2 = _mm_sha256msg2_epu32(msg2, msg1);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

    // Rounds 56-59
    msg = _mm_add_epi32(msg2, k(14));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    tmp = _mm_alignr_epi8(msg2, msg1, 4);
    msg3 = _mm_add_epi32(msg3, tmp);
    msg3 = _mm_sha256msg2_epu32(msg3, msg2);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

    // Rounds 60-63
    msg = _mm_add_epi32(msg3, k(15));
    state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
    msg = _mm_shuffle_epi32(msg, 0x0E);
    state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

    // combina com o estado salvo (feed-forward do FIPS)
    state0 = _mm_add_epi32(state0, abef_save);
    state1 = _mm_add_epi32(state1, cdgh_save);

    // extrai o estado no layout {A,B,C,D},{E,F,G,H}
    let tmp = _mm_shuffle_epi32(state0, 0x1B); /* FEBA */
    let state1b = _mm_shuffle_epi32(state1, 0xB1); /* DCHG */
    let out0 = _mm_blend_epi16(tmp, state1b, 0xF0); /* DCBA */
    let out1 = _mm_alignr_epi8(state1b, tmp, 8); /* EFGH */
    _mm_storeu_si128(h.as_mut_ptr() as *mut __m128i, out0);
    _mm_storeu_si128(h.as_mut_ptr().add(4) as *mut __m128i, out1);
}

#[inline(always)]
unsafe fn k(i: usize) -> __m128i {
    _mm_set_epi64x(K[i][0] as i64, K[i][1] as i64)
}
