//! What the shape reads off `avro.schema`: the name the schema's top-level
//! type declares, qualified by its namespace. A record, an enum or a fixed
//! has one; a primitive, an array, a map or a union does not.
//!
//! The schema is JSON, and this is a scan of its top-level object for two
//! string keys, not a parse: nested values are skipped balanced, strings
//! are taken as written. A contract reads the schema in full. The cursor —
//! peek, whitespace — is the estate's one, `codec::cursor::Cursor`, and a
//! string is the Foundation's `message::scan::string` (ADR-0044); what this
//! file makes of the schema is Avro's.

use codec::cursor::Cursor;
use message::scan::string;

/// The full name the schema announces: `namespace.name` when the top-level
/// type has a namespace and its name is not already dotted, else the name.
#[must_use]
pub fn full_name(schema: &[u8]) -> Option<String> {
    let mut scan = Cursor::new(schema);
    scan.skip_whitespace();
    if scan.peek() != Some(b'{') {
        return None;
    }
    scan.advance(1);
    let mut name = None;
    let mut namespace = None;
    loop {
        scan.skip_whitespace();
        match scan.peek() {
            Some(b'}') | None => break,
            Some(b',') => {
                scan.advance(1);
                continue;
            }
            Some(b'"') => {}
            Some(_) => return None,
        }
        let key = string(&mut scan).ok()?;
        scan.skip_whitespace();
        if scan.peek() != Some(b':') {
            return None;
        }
        scan.advance(1);
        scan.skip_whitespace();
        if scan.peek() == Some(b'"') {
            let value = string(&mut scan).ok()?;
            match key {
                b"name" => name = Some(String::from_utf8_lossy(value).into_owned()),
                b"namespace" => namespace = Some(String::from_utf8_lossy(value).into_owned()),
                _ => {}
            }
        } else {
            value(&mut scan)?;
        }
    }
    let name = name?;
    match namespace {
        Some(namespace) if !name.contains('.') && !namespace.is_empty() => {
            Some(format!("{namespace}.{name}"))
        }
        _ => Some(name),
    }
}

/// Past any value under the cursor: a string, or a balanced object or
/// array, or a scalar up to the next comma or closing bracket.
fn value(scan: &mut Cursor<'_>) -> Option<()> {
    let mut depth = 0usize;
    loop {
        match scan.peek()? {
            b'"' => {
                string(scan).ok()?;
            }
            b'{' | b'[' => {
                depth += 1;
                scan.advance(1);
            }
            b'}' | b']' | b',' if depth == 0 => return Some(()),
            b'}' | b']' => {
                depth -= 1;
                scan.advance(1);
            }
            _ => scan.advance(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_full_name_is_the_namespace_and_the_name_of_the_top_level_type() {
        let record = br#"{"type": "record", "namespace": "com.example", "fields": [
            {"name": "id", "type": "long"}, {"name": "tags", "type": {"type": "map",
            "values": "string"}}], "doc": "a \"quoted\" name", "name": "Order"}"#;
        assert_eq!(full_name(record).as_deref(), Some("com.example.Order"));
        assert_eq!(
            full_name(br#"{"name":"a.b.C","namespace":"x"}"#).as_deref(),
            Some("a.b.C")
        );
        assert_eq!(
            full_name(br#"{"type":"enum","name":"Suit","symbols":["S"]}"#).as_deref(),
            Some("Suit")
        );
        assert_eq!(
            full_name(br#"{"type":"record","name":"R","namespace":""}"#).as_deref(),
            Some("R")
        );
    }

    #[test]
    fn a_schema_without_a_named_top_level_type_announces_nothing() {
        assert_eq!(full_name(b"\"string\""), None);
        assert_eq!(
            full_name(b"[\"null\", {\"name\": \"R\", \"type\": \"record\"}]"),
            None
        );
        assert_eq!(full_name(br#"{"type": "array", "items": "long"}"#), None);
        assert_eq!(full_name(br#"{"name": "cut"#), None);
        assert_eq!(full_name(br#"{"name": "bad \x escape"}"#), None);
        assert_eq!(full_name(b""), None);
    }
}
