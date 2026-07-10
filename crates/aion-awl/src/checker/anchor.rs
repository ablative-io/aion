//! Path-aware anchoring for inline schema-door diagnostics.
//!
//! A projection failure names a keyword token and the projector path it
//! failed at; anchoring the diagnostic at the FIRST occurrence of the token
//! in the raw body lies whenever the token repeats (nearly every real schema
//! repeats `"type"`). This module walks the raw JSON text — already known
//! valid, the projector runs only after `serde_json` accepts it — to the
//! value the path names, and finds the token within that value's own bytes.

/// Locate the byte range (offset and length within `body`) anchoring a
/// diagnostic about `token` at the projector `path`. Returns `None` when
/// the body cannot be navigated (the caller falls back to the body span).
pub(super) fn locate(body: &str, path: &[String], token: &str) -> Option<(usize, usize)> {
    let scan = Scanner { text: body };
    let root = scan.skip_ws(0)?;
    let defs = scan
        .entries(root)
        .unwrap_or_default()
        .into_iter()
        .find(|entry| entry.key == "$defs")
        .map(|entry| entry.value_at);

    // Navigate the projector path: object properties live under
    // `properties`; `items` is the array-element segment; `$ref` hops into
    // `$defs` without consuming a segment (mirroring the projector).
    let mut at = root;
    for segment in path {
        let mut hops = 0;
        loop {
            let entries = scan.entries(at)?;
            if let Some(properties) = entries.iter().find(|entry| entry.key == "properties")
                && let Some(entries) = scan.entries(properties.value_at)
                && let Some(property) = entries.into_iter().find(|entry| &entry.key == segment)
            {
                at = property.value_at;
                break;
            }
            if segment == "items"
                && let Some(items) = entries.iter().find(|entry| entry.key == "items")
            {
                at = items.value_at;
                break;
            }
            let referenced = scan.ref_target(&entries, defs)?;
            at = referenced;
            hops += 1;
            if hops > 32 {
                return None;
            }
        }
    }

    // Find the token within the located value, following `$ref` hops the
    // projector may have taken past the last segment.
    let needle = format!("\"{token}\"");
    let mut hops = 0;
    loop {
        let end = scan.skip_value(at)?;
        if let Some(entries) = scan.entries(at)
            && let Some(entry) = entries.iter().find(|entry| entry.key == token)
        {
            return Some((entry.key_at, entry.key_end - entry.key_at));
        }
        if let Some(found) = body.get(at..end).and_then(|scope| scope.find(&needle)) {
            return Some((at + found, needle.len()));
        }
        let Some(referenced) = scan.entries(at).and_then(|e| scan.ref_target(&e, defs)) else {
            // No token inside the value (e.g. "expected a schema object" on
            // a non-object): anchor at the value itself.
            return Some((at, end - at));
        };
        at = referenced;
        hops += 1;
        if hops > 32 {
            return None;
        }
    }
}

/// One top-level entry of a JSON object in the raw text.
struct Entry {
    /// Decoded key.
    key: String,
    /// Byte offset of the key's opening quote.
    key_at: usize,
    /// Byte offset just past the key's closing quote.
    key_end: usize,
    /// Byte offset of the entry's value.
    value_at: usize,
}

/// A position scanner over raw JSON text known to be valid.
struct Scanner<'b> {
    text: &'b str,
}

impl Scanner<'_> {
    fn byte(&self, at: usize) -> Option<u8> {
        self.text.as_bytes().get(at).copied()
    }

    fn skip_ws(&self, mut at: usize) -> Option<usize> {
        while matches!(self.byte(at)?, b' ' | b'\t' | b'\n' | b'\r') {
            at += 1;
        }
        Some(at)
    }

    /// `at` points at an opening quote; returns the offset past the closing
    /// quote.
    fn skip_string(&self, at: usize) -> Option<usize> {
        let mut cursor = at + 1;
        loop {
            match self.byte(cursor)? {
                b'"' => return Some(cursor + 1),
                b'\\' => cursor += 2,
                _ => cursor += 1,
            }
        }
    }

    /// `at` points at a value; returns the offset just past it.
    fn skip_value(&self, at: usize) -> Option<usize> {
        match self.byte(at)? {
            b'"' => self.skip_string(at),
            open @ (b'{' | b'[') => {
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0_usize;
                let mut cursor = at;
                loop {
                    match self.byte(cursor)? {
                        b'"' => {
                            cursor = self.skip_string(cursor)?;
                            continue;
                        }
                        byte if byte == open => depth += 1,
                        byte if byte == close => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(cursor + 1);
                            }
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
            }
            _ => {
                let mut cursor = at;
                while let Some(byte) = self.byte(cursor) {
                    if matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    cursor += 1;
                }
                Some(cursor)
            }
        }
    }

    /// Top-level entries of the object at `at`; `None` when the value is
    /// not an object.
    fn entries(&self, at: usize) -> Option<Vec<Entry>> {
        if self.byte(at)? != b'{' {
            return None;
        }
        let mut found = Vec::new();
        let mut cursor = self.skip_ws(at + 1)?;
        if self.byte(cursor)? == b'}' {
            return Some(found);
        }
        loop {
            let key_at = cursor;
            let key_end = self.skip_string(key_at)?;
            let key = serde_json::from_str::<String>(self.text.get(key_at..key_end)?).ok()?;
            let colon = self.skip_ws(key_end)?;
            let value_at = self.skip_ws(colon + 1)?;
            found.push(Entry {
                key,
                key_at,
                key_end,
                value_at,
            });
            cursor = self.skip_ws(self.skip_value(value_at)?)?;
            match self.byte(cursor)? {
                b',' => cursor = self.skip_ws(cursor + 1)?,
                _ => return Some(found),
            }
        }
    }

    /// When the entries carry a `$defs`-local `$ref`, the offset of the
    /// referenced definition's value.
    fn ref_target(&self, entries: &[Entry], defs: Option<usize>) -> Option<usize> {
        let reference = entries.iter().find(|entry| entry.key == "$ref")?;
        let end = self.skip_string(reference.value_at)?;
        let raw = self.text.get(reference.value_at..end)?;
        let target = serde_json::from_str::<String>(raw).ok()?;
        let name = target.strip_prefix("#/$defs/")?;
        self.entries(defs?)?
            .into_iter()
            .find(|entry| entry.key == name)
            .map(|entry| entry.value_at)
    }
}
