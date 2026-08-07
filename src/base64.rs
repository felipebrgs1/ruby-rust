//! calisto — base64
//!
//! base64 hand-rolled (cliente e daemon usam o mesmo alfabeto).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — base64 (extraido de src/main.rs na reorg do CLI).






pub const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";



// ---- utils -------------------------------------------------------------------

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}



pub fn b64_decode(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let table: [i16; 256] = {
        let mut t = [-1i16; 256];
        for (i, b) in B64.iter().enumerate() {
            t[*b as usize] = i as i16;
        }
        t
    };
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let c0 = table[bytes[i] as usize];
        let c1 = table[bytes[i + 1] as usize];
        if c0 < 0 || c1 < 0 {
            break;
        }
        out.push(((c0 << 2) | (c1 >> 4)) as u8);
        let c2 = if bytes[i + 2] == b'=' { -1 } else { table[bytes[i + 2] as usize] };
        if c2 >= 0 {
            out.push((((c1 & 0x0f) << 4) | (c2 >> 2)) as u8);
            let c3 = if bytes[i + 3] == b'=' { -1 } else { table[bytes[i + 3] as usize] };
            if c3 >= 0 {
                out.push((((c2 & 0x03) << 6) | c3) as u8);
            }
        }
        i += 4;
    }
    String::from_utf8_lossy(&out).into_owned()
}
