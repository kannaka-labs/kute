//! Compact binary form of a KVF vocabulary (`.kvfb`).
//!
//! The same model as the JSON file and the same digest, in fewer bytes. A
//! learned token is stored as its merge rule alone; its payload is the
//! concatenation of two earlier tokens and is rebuilt on load. Everything
//! that is not the token table travels as one small JSON header.
//!
//! Layout (specified in `specifications/kvf/KVF-0.1.md`, section 7):
//!
//! ```text
//! "KVFB"  format byte (1)  flags byte (bit 0: body is a zlib stream)
//! body:
//!   varint header length, header JSON (the document without tokens and merges)
//!   varint token count
//!   per token, in id order:
//!     tag byte: kind in the low 3 bits, 0x80 when a frequency follows
//!     [varint frequency]
//!     merge tokens:  varint left id, varint right id
//!     other tokens:  payload byte (0 bytes, 1 symbols), then
//!                    bytes:   varint length, the bytes
//!                    symbols: varint count, then per symbol varint length, UTF-8
//! ```

use kute_types::vocab::{MAX_SYMBOL_BYTES, MAX_TOKENS, MAX_TOKEN_BYTES, MAX_TOKEN_SYMBOLS};
use kute_types::{KuteError, MergeRule, Result, TokenEntry, TokenKind, TokenPayload, Vocabulary};
use std::io::{Read, Write};

/// First four bytes of a compact vocabulary file.
pub const MAGIC: &[u8; 4] = b"KVFB";
/// The compact format version this build reads and writes.
pub const FORMAT: u8 = 1;
const FLAG_DEFLATE: u8 = 1;
const MAX_HEADER: usize = 64 * 1024 * 1024;

/// True when `bytes` starts like a compact vocabulary file.
pub fn is_compact(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == MAGIC
}

fn kind_code(kind: TokenKind) -> u8 {
    match kind {
        TokenKind::Byte => 0,
        TokenKind::Char => 1,
        TokenKind::Symbol => 2,
        TokenKind::Entry => 3,
        TokenKind::Merge => 4,
        TokenKind::Special => 5,
    }
}

fn kind_of(code: u8) -> Option<TokenKind> {
    Some(match code {
        0 => TokenKind::Byte,
        1 => TokenKind::Char,
        2 => TokenKind::Symbol,
        3 => TokenKind::Entry,
        4 => TokenKind::Merge,
        5 => TokenKind::Special,
        _ => return None,
    })
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Serialize to the compact form. The vocabulary is sealed and validated
/// first, exactly as for the JSON writer.
pub fn to_vec(vocabulary: &Vocabulary, deflate: bool) -> Result<Vec<u8>> {
    let mut sealed = vocabulary.clone();
    sealed.seal()?;
    sealed.validate()?;

    // Validation guarantees one rule per merge token. The digest covers the
    // order of the rules, and this form stores them in id order only.
    let mut rule_of = vec![None; sealed.tokens.len()];
    let mut last = None;
    for m in &sealed.merges {
        if last.is_some_and(|l| m.id <= l) {
            return Err(KuteError::Unsupported(
                "merge rules are not listed in token id order; the compact form cannot keep that order".into(),
            ));
        }
        last = Some(m.id);
        rule_of[m.id as usize] = Some(*m);
    }

    let mut header = serde_json::to_value(&sealed)?;
    let map = header.as_object_mut().ok_or_else(|| KuteError::Serde("vocabulary did not serialize to an object".into()))?;
    map.remove("tokens");
    map.remove("merges");
    let header = serde_json::to_vec(&header)?;

    let mut body = Vec::with_capacity(header.len() + sealed.tokens.len() * 6);
    put_varint(&mut body, header.len() as u64);
    body.extend_from_slice(&header);
    put_varint(&mut body, sealed.tokens.len() as u64);
    for (i, token) in sealed.tokens.iter().enumerate() {
        let mut tag = kind_code(token.kind);
        if token.frequency.is_some() {
            tag |= 0x80;
        }
        body.push(tag);
        if let Some(f) = token.frequency {
            put_varint(&mut body, f);
        }
        if token.kind == TokenKind::Merge {
            let rule: MergeRule = rule_of[i].ok_or_else(|| KuteError::InvalidVocabulary(format!("merge token {i} has no merge rule")))?;
            put_varint(&mut body, rule.left as u64);
            put_varint(&mut body, rule.right as u64);
            continue;
        }
        match &token.payload {
            TokenPayload::Bytes(b) => {
                body.push(0);
                put_varint(&mut body, b.len() as u64);
                body.extend_from_slice(b);
            }
            TokenPayload::Symbols(s) => {
                body.push(1);
                put_varint(&mut body, s.len() as u64);
                for symbol in s {
                    put_varint(&mut body, symbol.len() as u64);
                    body.extend_from_slice(symbol.as_bytes());
                }
            }
        }
    }

    let mut out = Vec::with_capacity(body.len() + 6);
    out.extend_from_slice(MAGIC);
    out.push(FORMAT);
    if deflate {
        out.push(FLAG_DEFLATE);
        let mut encoder = flate2::write::ZlibEncoder::new(out, flate2::Compression::new(9));
        encoder.write_all(&body)?;
        out = encoder.finish()?;
    } else {
        out.push(0);
        out.extend_from_slice(&body);
    }
    Ok(out)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

fn bad(why: impl Into<String>) -> KuteError {
    KuteError::InvalidVocabulary(format!("compact vocabulary: {}", why.into()))
}

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.pos).ok_or_else(|| bad("truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            let low = (byte & 0x7f) as u64;
            if shift == 63 && low > 1 {
                return Err(bad("number does not fit in 64 bits"));
            }
            value |= low << shift;
            if byte & 0x80 == 0 {
                // One value, one encoding: no padded forms.
                if byte == 0 && shift > 0 {
                    return Err(bad("number has a padded encoding"));
                }
                return Ok(value);
            }
            shift += 7;
            if shift > 63 {
                return Err(bad("number does not fit in 64 bits"));
            }
        }
    }

    fn length(&mut self, limit: usize, what: &str) -> Result<usize> {
        let n = self.varint()?;
        if n > limit as u64 {
            return Err(bad(format!("{what} of {n} is over the limit of {limit}")));
        }
        Ok(n as usize)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or_else(|| bad("truncated"))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn id(&mut self, below: usize) -> Result<u32> {
        let n = self.varint()?;
        if n >= below as u64 {
            return Err(bad(format!("merge token {below} refers to token {n}, which is not earlier in the table")));
        }
        Ok(n as u32)
    }
}

/// Parse and validate a compact vocabulary. The result is checked exactly as
/// a JSON file is, including the recorded digest; `max_bytes` bounds both the
/// inflated body and the total size of the rebuilt token payloads.
pub fn from_slice(bytes: &[u8], max_bytes: u64) -> Result<Vocabulary> {
    if !is_compact(bytes) {
        return Err(bad("missing KVFB signature"));
    }
    if bytes.len() < 6 {
        return Err(bad("truncated"));
    }
    if bytes[4] != FORMAT {
        return Err(KuteError::Unsupported(format!("compact vocabulary format {}; this build reads {FORMAT}", bytes[4])));
    }
    let flags = bytes[5];
    if flags & !FLAG_DEFLATE != 0 {
        return Err(KuteError::Unsupported(format!("compact vocabulary flags {flags:#04x}")));
    }
    let inflated;
    let body: &[u8] = if flags & FLAG_DEFLATE != 0 {
        let mut buffer = Vec::new();
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[6..]).take(max_bytes + 1);
        decoder.read_to_end(&mut buffer).map_err(|e| bad(format!("body does not inflate: {e}")))?;
        if buffer.len() as u64 > max_bytes {
            return Err(KuteError::LimitExceeded("compact vocabulary inflates past the size limit".into()));
        }
        if decoder.into_inner().total_in() as usize != bytes.len() - 6 {
            return Err(bad("bytes after the end of the compressed body"));
        }
        inflated = buffer;
        &inflated
    } else {
        &bytes[6..]
    };

    let mut cursor = Cursor { bytes: body, pos: 0 };
    let header_len = cursor.length(MAX_HEADER, "header length")?;
    let mut header: serde_json::Value =
        serde_json::from_slice(cursor.take(header_len)?).map_err(|e| bad(format!("header is not JSON: {e}")))?;
    let map = header.as_object_mut().ok_or_else(|| bad("header is not an object"))?;
    if map.contains_key("tokens") || map.contains_key("merges") {
        return Err(bad("header carries a token table"));
    }
    map.insert("tokens".into(), serde_json::Value::Array(Vec::new()));
    let mut vocabulary: Vocabulary = serde_json::from_value(header).map_err(|e| bad(format!("header: {e}")))?;

    let count = cursor.length(MAX_TOKENS, "token count")?;
    // Every token takes at least two bytes, so a count the data cannot hold
    // is refused before anything is allocated for it.
    if count > (body.len() - cursor.pos) / 2 {
        return Err(bad("declared token count exceeds the data present"));
    }
    let mut tokens: Vec<TokenEntry> = Vec::with_capacity(count);
    let mut merges: Vec<MergeRule> = Vec::new();
    let mut budget = max_bytes;
    let mut spend = |n: usize| -> Result<()> {
        budget = budget.checked_sub(n as u64).ok_or_else(|| KuteError::LimitExceeded("compact vocabulary expands past the size limit".into()))?;
        Ok(())
    };
    for i in 0..count {
        let tag = cursor.byte()?;
        if tag & 0x78 != 0 {
            return Err(bad(format!("token {i} has an unknown tag {tag:#04x}")));
        }
        let kind = kind_of(tag & 0x07).ok_or_else(|| bad(format!("token {i} has an unknown kind")))?;
        let frequency = if tag & 0x80 != 0 { Some(cursor.varint()?) } else { None };
        let payload = if kind == TokenKind::Merge {
            let left = cursor.id(i)?;
            let right = cursor.id(i)?;
            merges.push(MergeRule { left, right, id: i as u32 });
            match (&tokens[left as usize].payload, &tokens[right as usize].payload) {
                (TokenPayload::Bytes(a), TokenPayload::Bytes(b)) => {
                    if a.len() + b.len() > MAX_TOKEN_BYTES {
                        return Err(bad(format!("token {i} is longer than {MAX_TOKEN_BYTES} bytes")));
                    }
                    spend(a.len() + b.len())?;
                    let mut v = Vec::with_capacity(a.len() + b.len());
                    v.extend_from_slice(a);
                    v.extend_from_slice(b);
                    TokenPayload::Bytes(v)
                }
                (TokenPayload::Symbols(a), TokenPayload::Symbols(b)) => {
                    if a.len() + b.len() > MAX_TOKEN_SYMBOLS {
                        return Err(bad(format!("token {i} holds more than {MAX_TOKEN_SYMBOLS} symbols")));
                    }
                    spend(a.iter().chain(b).map(|s| s.len() + 24).sum())?;
                    TokenPayload::Symbols(a.iter().chain(b).cloned().collect())
                }
                _ => return Err(bad(format!("token {i} joins a byte token and a symbol token"))),
            }
        } else {
            match cursor.byte()? {
                0 => {
                    let n = cursor.length(MAX_TOKEN_BYTES, "token length")?;
                    spend(n)?;
                    TokenPayload::Bytes(cursor.take(n)?.to_vec())
                }
                1 => {
                    let n = cursor.length(MAX_TOKEN_SYMBOLS, "symbol count")?;
                    if n > body.len() - cursor.pos {
                        return Err(bad("declared symbol count exceeds the data present"));
                    }
                    let mut symbols = Vec::with_capacity(n);
                    for _ in 0..n {
                        let len = cursor.length(MAX_SYMBOL_BYTES, "symbol length")?;
                        spend(len + 24)?;
                        let text = std::str::from_utf8(cursor.take(len)?).map_err(|_| bad(format!("token {i} has a symbol that is not UTF-8")))?;
                        symbols.push(text.to_string());
                    }
                    TokenPayload::Symbols(symbols)
                }
                other => return Err(bad(format!("token {i} has an unknown payload type {other}"))),
            }
        };
        tokens.push(TokenEntry { id: i as u32, kind, payload, frequency });
    }
    if cursor.pos != body.len() {
        return Err(bad("bytes after the last token"));
    }
    vocabulary.tokens = tokens;
    vocabulary.merges = merges;
    vocabulary.validate()?;
    Ok(vocabulary)
}
