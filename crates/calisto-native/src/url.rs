//! Calisto::URL — espelho do CGI.escape/CGI.unescape (pure Ruby na stdlib).
//!
//! Semantica verificada contra o CRuby 3.4:
//!   - escape: mantem [a-zA-Z0-9_.-], espaco -> `+`, qualquer OUTRO byte ->
//!     `%XX` maiusculo (entrada tratada como bytes — UTF-8 vira 2 hex por
//!     byte, como o `string.b` do CGI);
//!   - unescape: `+` -> espaco, sequencias `%xx` (hex case-insensitive) ->
//!     byte; `%` invalido (`%zz`) fica como esta; bytes fora de % passam
//!     intactos.

pub const HEX: &[u8; 16] = b"0123456789ABCDEF";

pub fn escape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &b in s {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' | b'~' => out.push(b),
            b' ' => out.push(b'+'),
            b => {
                out.push(b'%');
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 0xf) as usize]);
            }
        }
    }
    out
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if b == b'%' && i + 2 < s.len() {
            if let (Some(h), Some(l)) = (hexval(s[i + 1]), hexval(s[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_matches_cgi() {
        assert_eq!(escape(b"a b-c.d_e~f"), b"a+b-c.d_e~f");
        assert_eq!(escape("café & <tag>".as_bytes()), b"caf%C3%A9+%26+%3Ctag%3E");
        assert_eq!(escape(b"+%/=?"), b"%2B%25%2F%3D%3F");
        assert_eq!(escape(b""), b"");
    }

    #[test]
    fn unescape_matches_cgi() {
        assert_eq!(unescape(b"a+b-c.d_e~f"), b"a b-c.d_e~f");
        assert_eq!(unescape(b"caf%C3%A9+%26+%3Ctag%3E"), "café & <tag>".as_bytes());
        assert_eq!(unescape(b"%2F+%3D+%3F"), b"/ = ?");
        assert_eq!(unescape(b"%zz"), b"%zz");
        assert_eq!(unescape(b"%c3%a9"), "é".as_bytes()); // hex minusculo
    }

    #[test]
    fn roundtrip() {
        let s = "a b&c=d?e+f%g<h>i\"j'k~l.m_n-".as_bytes();
        assert_eq!(unescape(&escape(s)), s);
    }
}
