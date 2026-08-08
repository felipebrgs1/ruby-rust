//! Calisto::Base64 — espelho do gem base64 0.3 (pure Ruby na stdlib).
//!
//! Semantica verificada contra o CRuby 3.4 (base64 0.3.0):
//!   - encode64: newline a cada 60 chars + newline final (vazio -> "");
//!   - strict_encode64: sem newlines, com padding;
//!   - urlsafe_encode64(padding: true): alfabeto `-_`; padding=false remove
//!     os `=` finais;
//!   - decode64 (lenient): ignora caracteres fora do alfabeto, agrupa em 4s
//!     na stream filtrada, grupo parcial final vira 1-2 bytes (1 char -> 0),
//!     `=` PARA o processamento (resto ignorado);
//!   - strict_decode64: ArgumentError "invalid base64" — exige multiplo de 4,
//!     `=` so no grupo final com 2-3 chars de dados (0/1 invalidos; grupo do
//!     meio nao pode ter `=`), nada fora do alfabeto;
//!   - urlsafe_decode64: strict apos tr `-_` -> `+/`.

pub const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// encode com alfabeto e padding (`=` no fim do ultimo grupo incompleto).
fn encode_raw(data: &[u8], table: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(table[((n >> 18) & 63) as usize]);
        out.push(table[((n >> 12) & 63) as usize]);
        out.push(if chunk.len() > 1 { table[((n >> 6) & 63) as usize] } else { b'=' });
        out.push(if chunk.len() > 2 { table[(n & 63) as usize] } else { b'=' });
    }
    out
}

pub fn strict_encode64(data: &[u8]) -> Vec<u8> {
    encode_raw(data, B64)
}

/// Newline a cada 60 chars de saida + newline final (espelho do pack "m").
pub fn encode64(data: &[u8]) -> Vec<u8> {
    let raw = encode_raw(data, B64);
    if raw.is_empty() {
        return raw;
    }
    let mut out = Vec::with_capacity(raw.len() + raw.len().div_ceil(60) + 1);
    for (i, &b) in raw.iter().enumerate() {
        if i > 0 && i % 60 == 0 {
            out.push(b'\n');
        }
        out.push(b);
    }
    out.push(b'\n');
    out
}

pub fn urlsafe_encode64(data: &[u8], padding: bool) -> Vec<u8> {
    let mut out = encode_raw(data, B64URL);
    if !padding {
        while out.last() == Some(&b'=') {
            out.pop();
        }
    }
    out
}

fn decode_table() -> [i16; 256] {
    let mut t = [-1i16; 256];
    for (i, b) in B64.iter().enumerate() {
        t[*b as usize] = i as i16;
    }
    t
}

/// Lenient (unpack "m"): filtra lixo, `=` para, grupo parcial final decodifica.
pub fn decode64(s: &[u8]) -> Vec<u8> {
    let t = decode_table();
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut quad = [0u8; 4];
    let mut qn = 0;
    for &b in s {
        if b == b'=' {
            break; // padding: para o processamento (resto ignorado)
        }
        let v = t[b as usize];
        if v < 0 {
            continue; // fora do alfabeto: ignorado
        }
        quad[qn] = v as u8;
        qn += 1;
        if qn == 4 {
            out.push((quad[0] << 2) | (quad[1] >> 4));
            out.push(((quad[1] & 0x0f) << 4) | (quad[2] >> 2));
            out.push(((quad[2] & 0x03) << 6) | quad[3]);
            qn = 0;
        }
    }
    if qn == 3 {
        out.push((quad[0] << 2) | (quad[1] >> 4));
        out.push(((quad[1] & 0x0f) << 4) | (quad[2] >> 2));
    } else if qn == 2 {
        out.push((quad[0] << 2) | (quad[1] >> 4));
    }
    out
}

/// Estrito (unpack "m0"): Err(()) = ArgumentError "invalid base64" do Ruby.
pub fn strict_decode64(s: &[u8]) -> Result<Vec<u8>, ()> {
    let t = decode_table();
    if s.len() % 4 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for (gi, chunk) in s.chunks(4).enumerate() {
        let is_last = (gi + 1) * 4 == s.len();
        let mut data = [0u8; 4];
        let mut ndata = 0;
        let mut seen_pad = false;
        for &b in chunk {
            if b == b'=' {
                if !is_last {
                    return Err(()); // `=` so no grupo final
                }
                seen_pad = true; // padding consecutivo no fim e valido
                continue;
            }
            if seen_pad {
                return Err(()); // dados depois do padding
            }
            let v = t[b as usize];
            if v < 0 {
                return Err(());
            }
            data[ndata] = v as u8;
            ndata += 1;
        }
        if is_last {
            // grupo final valido: 2 ou 3 chars de dados (+ padding) ou 4
            if ndata == 0 || ndata == 1 {
                return Err(());
            }
        } else if ndata != 4 {
            return Err(()); // grupo do meio incompleto (pad no meio)
        }
        out.push((data[0] << 2) | (data[1] >> 4));
        if ndata > 2 {
            out.push(((data[1] & 0x0f) << 4) | (data[2] >> 2));
        }
        if ndata > 3 {
            out.push(((data[2] & 0x03) << 6) | data[3]);
        }
    }
    Ok(out)
}

/// urlsafe_decode64: espelho do base64 0.3.0 — entrada sem `=` final e com
/// len % 4 != 0 e COMPLETADA com `=` (RFC 4648: padding excessivo ignorado,
/// logo unpadded e aceitavel); depois tr `-_` -> `+/` e strict.
pub fn urlsafe_decode64(s: &[u8]) -> Result<Vec<u8>, ()> {
    let mut t: Vec<u8> = Vec::with_capacity(s.len() + 3);
    for &b in s {
        t.push(match b {
            b'-' => b'+',
            b'_' => b'/',
            b => b,
        });
    }
    if !t.ends_with(&[b'=']) && t.len() % 4 != 0 {
        t.extend(std::iter::repeat(b'=').take(4 - (t.len() % 4)));
    }
    strict_decode64(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (data, expected) in [
            (&b""[..], ""),
            (&b"f"[..], "Zg=="),
            (&b"fo"[..], "Zm8="),
            (&b"foo"[..], "Zm9v"),
            (&b"foob"[..], "Zm9vYg=="),
            (&b"fooba"[..], "Zm9vYmE="),
            (&b"foobar"[..], "Zm9vYmFy"),
        ] {
            assert_eq!(strict_encode64(data), expected.as_bytes());
        }
    }

    #[test]
    fn wrap_and_urlsafe() {
        assert_eq!(encode64(b""), b"");
        // 57 bytes -> 76 chars base64 ("eHh4" x19): newline apos 60 + final
        let mut exp = "eHh4".repeat(19).into_bytes();
        exp.insert(60, b'\n');
        exp.push(b'\n');
        assert_eq!(encode64(&[b'x'; 57]), exp);
        assert_eq!(urlsafe_encode64(&[0xfb, 0xff], true), b"-_8=");
        assert_eq!(urlsafe_encode64(&[0xfb, 0xff], false), b"-_8");
    }

    #[test]
    fn lenient_decode_matches_ruby() {
        assert_eq!(decode64(b"aGVsbG8="), b"hello");
        assert_eq!(decode64(b"aGVs bG8="), b"hello");
        assert_eq!(decode64(b"YQ bG8="), b"a\x06\xc6"); // lixo nao re-alinha o quantum
        assert_eq!(decode64(b"!!aGVsbG8@@##"), b"hello");
        assert_eq!(decode64(b"aGVsbG8"), b"hello");
        assert_eq!(decode64(b"===="), b"");
        assert_eq!(decode64(b"YQ"), b"a");
        assert_eq!(decode64(b"Y"), b"");
        assert_eq!(decode64(b""), b"");
        assert_eq!(decode64(b"YQ=YQ"), b"a"); // `=` para tudo
        assert_eq!(decode64(b"aGVsbG8=extra"), b"hello");
    }

    #[test]
    fn strict_decode_matches_ruby() {
        assert_eq!(strict_decode64(b"YQ=="), Ok(b"a".to_vec()));
        assert_eq!(strict_decode64(b"YWI="), Ok(b"ab".to_vec()));
        assert_eq!(strict_decode64(b"aGVsbG8X"), Ok(b"hello\x17".to_vec()));
        assert_eq!(strict_decode64(b""), Ok(b"".to_vec()));
        for bad in [&b"YQ"[..], &b"YWI"[..], &b"aGVsbG8"[..], &b"===="[..], &b"Y==="[..], &b"aGVsbG8!"[..], &b"aGVs bG8="[..], &b"YQ==="[..]] {
            assert!(strict_decode64(bad).is_err(), "{bad:?} deveria ser invalido");
        }
    }

    #[test]
    fn urlsafe_roundtrip() {
        assert_eq!(urlsafe_decode64(b"-_8="), Ok(vec![0xfb, 0xff]));
        assert_eq!(urlsafe_decode64(b"-_8"), Ok(vec![0xfb, 0xff])); // unpadded: completa com =
        assert!(urlsafe_decode64(b"+/8=").is_ok()); // tr para o alfabeto normal
        assert!(urlsafe_decode64(b"-__8=").is_err()); // termina com = e len nao multiplo
    }
}
