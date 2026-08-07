//! BLAKE3 (spec 1.0) hand-rolled — Fase P do roadmap (calisto/hash).
//! Port fiel da implementacao de referencia oficial (BLAKE3-team/BLAKE3,
//! reference_impl.rs), adaptada para one-shot (hash de um slice so).
//! Validada contra os vetores oficiais (test_vectors.json) em test/native.rs:
//! comprimentos 0, 1, 1024, 1025, 2048, 2049, 3072, 3073, 4096, 4097, ...,
//! 102400 (padrao ciclico de 251 bytes) — cobre chunk unico, chunk cheio
//! (bloco final vazio com CHUNK_END), arvore de varios niveis e raiz.

const CHUNK_LEN: usize = 1024;
const BLOCK_LEN: usize = 64;

const CHUNK_START: u32 = 1 << 0;
const CHUNK_END: u32 = 1 << 1;
const PARENT: u32 = 1 << 2;
const ROOT: u32 = 1 << 3;

const IV: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];

const MSG_PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

/// Hash BLAKE3 de um slice de bytes -> 32 bytes.
pub fn hash(input: &[u8]) -> [u8; 32] {
    // cv_stack: chaining values das subarvores completas a esquerda. O
    // ULTIMO chunk nunca entra na pilha — o Output dele vira a raiz
    // (finalize do reference_impl: so os chunks completados sobem).
    let mut cv_stack: Vec<[u32; 8]> = Vec::with_capacity(16);
    let n_chunks = input.len().div_ceil(CHUNK_LEN).max(1);
    let mut last_output = ChunkOutput::default();

    for idx in 0..n_chunks {
        let chunk = &input[idx * CHUNK_LEN..((idx + 1) * CHUNK_LEN).min(input.len())];
        let (cv, out) = process_chunk(chunk, idx as u64);
        if idx + 1 == n_chunks {
            last_output = out;
            break;
        }
        // add_chunk_chaining_value: o numero de subarvores completadas =
        // trailing zeros de (idx+1) — cada merge combina o topo da pilha
        // (esquerda) com o cv atual (direita) e o resultado sobe.
        let mut total = (idx + 1) as u64;
        let mut new_cv = cv;
        while total & 1 == 0 {
            let left = cv_stack.pop().expect("blake3: pilha vazia no merge");
            new_cv = parent_cv(left, new_cv);
            total >>= 1;
        }
        cv_stack.push(new_cv);
    }

    // raiz: sobe a borda direita da arvore ate o Output do chunk raiz
    let mut out = last_output;
    let mut remaining = cv_stack.len();
    while remaining > 0 {
        remaining -= 1;
        out = parent_output(cv_stack[remaining], out.chaining_value());
    }
    root_output_bytes(&out)
}

/// Estado do bloco final de um chunk — o que a compressao da raiz reusa
/// (com counter=0 e a flag ROOT). `counter` = indice do chunk na arvore
/// (0 para parents).
#[derive(Clone, Copy, Default)]
struct ChunkOutput {
    input_cv: [u32; 8],
    block_words: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
}

impl ChunkOutput {
    fn chaining_value(&self) -> [u32; 8] {
        first_8_words(compress(&self.input_cv, &self.block_words, self.counter, self.block_len, self.flags))
    }
}

/// Comprime um chunk (0..=1024 bytes) num CV + o Output do bloco final.
///
/// Regra do spec (reference_impl): todos os blocos cheios MENOS o ultimo
/// sao comprimidos via `update` (counter = indice do chunk); o ULTIMO bloco
/// do chunk — cheio (chunk de 1024) ou parcial — e o "bloco final":
/// comprimido UMA vez com CHUNK_END (counter = indice do chunk, block_len
/// real). Nao ha bloco vazio extra apos um chunk cheio.
fn process_chunk(chunk: &[u8], chunk_counter: u64) -> ([u32; 8], ChunkOutput) {
    let mut cv = IV;
    let full_blocks = chunk.len() / BLOCK_LEN;
    let last_is_full = full_blocks > 0 && chunk.len() % BLOCK_LEN == 0;
    let n_update = if last_is_full { full_blocks - 1 } else { full_blocks };
    for i in 0..n_update {
        let block_words = words(&chunk[i * BLOCK_LEN..(i + 1) * BLOCK_LEN]);
        let flags = if i == 0 { CHUNK_START } else { 0 };
        cv = first_8_words(compress(&cv, &block_words, chunk_counter, BLOCK_LEN as u32, flags));
    }
    // bloco final: os bytes restantes (0..64; 64 quando o chunk e cheio)
    let rem = &chunk[n_update * BLOCK_LEN..];
    let mut block_words = [0u32; 16];
    for (i, four) in rem.chunks_exact(4).enumerate() {
        block_words[i] = u32::from_le_bytes(four.try_into().unwrap());
    }
    if rem.len() % 4 != 0 {
        // ultimos 1-3 bytes: LE parcial (bytes altos zero — o resto do array
        // ja e zero)
        let tail = &rem[rem.len() - rem.len() % 4..];
        let mut buf = [0u8; 4];
        buf[..tail.len()].copy_from_slice(tail);
        block_words[rem.len() / 4] = u32::from_le_bytes(buf);
    }
    let flags = CHUNK_END | if n_update == 0 { CHUNK_START } else { 0 };
    let out = ChunkOutput { input_cv: cv, block_words, counter: chunk_counter, block_len: rem.len() as u32, flags };
    let chunk_cv = out.chaining_value();
    (chunk_cv, out)
}

fn parent_output(left_cv: [u32; 8], right_cv: [u32; 8]) -> ChunkOutput {
    let mut block_words = [0u32; 16];
    block_words[..8].copy_from_slice(&left_cv);
    block_words[8..].copy_from_slice(&right_cv);
    ChunkOutput {
        input_cv: IV,
        block_words,
        counter: 0,
        block_len: BLOCK_LEN as u32,
        flags: PARENT,
    }
}

fn parent_cv(left_cv: [u32; 8], right_cv: [u32; 8]) -> [u32; 8] {
    parent_output(left_cv, right_cv).chaining_value()
}

/// Saida raiz: compressao com counter=0 e ROOT — primeiros 8 words = hash.
fn root_output_bytes(out: &ChunkOutput) -> [u8; 32] {
    let words = compress(&out.input_cv, &out.block_words, 0, out.block_len, out.flags | ROOT);
    let mut digest = [0u8; 32];
    for (i, w) in words[..8].iter().enumerate() {
        digest[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    digest
}

fn first_8_words(out: [u32; 16]) -> [u32; 8] {
    out[..8].try_into().unwrap()
}

fn words(block: &[u8]) -> [u32; 16] {
    let mut w = [0u32; 16];
    for (i, four) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_le_bytes(four.try_into().unwrap());
    }
    w
}

fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(mx);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(my);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

fn round(state: &mut [u32; 16], m: &[u32; 16]) {
    // colunas
    g(state, 0, 4, 8, 12, m[0], m[1]);
    g(state, 1, 5, 9, 13, m[2], m[3]);
    g(state, 2, 6, 10, 14, m[4], m[5]);
    g(state, 3, 7, 11, 15, m[6], m[7]);
    // diagonais
    g(state, 0, 5, 10, 15, m[8], m[9]);
    g(state, 1, 6, 11, 12, m[10], m[11]);
    g(state, 2, 7, 8, 13, m[12], m[13]);
    g(state, 3, 4, 9, 14, m[14], m[15]);
}

fn compress(
    cv: &[u32; 8],
    block_words: &[u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 16] {
    let mut state = [
        cv[0], cv[1], cv[2], cv[3], cv[4], cv[5], cv[6], cv[7],
        IV[0], IV[1], IV[2], IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len,
        flags,
    ];
    let mut block = *block_words;
    for _ in 0..7 {
        round(&mut state, &block);
        let mut permuted = [0u32; 16];
        for (i, &p) in MSG_PERMUTATION.iter().enumerate() {
            permuted[i] = block[p];
        }
        block = permuted;
    }
    for i in 0..8 {
        state[i] ^= state[i + 8];
        state[i + 8] ^= cv[i];
    }
    state
}
