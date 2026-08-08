//! Calisto::HTML — espelho do ERB::Util.html_escape / CGI.escapeHTML.
//!
//! Semantica verificada contra o CRuby 3.4 (erb stdlib): & < > " ' ->
//! &amp; &lt; &gt; &quot; &#39;; todo o resto (incl. UTF-8 multibyte) passa
//! intacto. E o `Bun.escapeHTML` do Ruby.

pub fn escape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &b in s {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b'\'' => out.extend_from_slice(b"&#39;"),
            b => out.push(b),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_matches_erb() {
        assert_eq!(escape(b"a&b<c>d\"e'f"), b"a&amp;b&lt;c&gt;d&quot;e&#39;f");
        assert_eq!(escape("café".as_bytes()), "café".as_bytes());
        assert_eq!(escape(b""), b"");
        assert_eq!(escape(b"no specials"), b"no specials");
    }
}
