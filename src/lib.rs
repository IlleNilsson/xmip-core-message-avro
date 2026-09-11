#![forbid(unsafe_code)]

//! Avro: an object container file, one part per block. The shape reads the
//! file's header — the `Obj\x01` magic, the metadata map with the schema
//! under `avro.schema` and the codec under `avro.codec`, the sync marker —
//! and hands back one part per block that follows, named by the block's
//! number, with the block's bytes as written and `avro/binary` as the media
//! type, `codec=<name>` added when the header names a codec other than
//! `null`. The announced type is the full name of the schema's top-level
//! type: `namespace.name` for a record, an enum or a fixed (ADR-0047).
//!
//! No object is decoded and no block is decompressed: a shape sections and
//! names, a contract reads the schema and validates against it. What cannot
//! be sectioned — a wrong magic, a header without a schema, a file that ends
//! inside a block, a block not closed by the sync marker — is refused with
//! the byte where the walk stopped.

mod container;
mod schema;

pub use container::{Block, Header, MAGIC};

use message::{Part, Shape, ShapeError, Shaped};
use stream::Stream;

/// The Avro shape.
#[derive(Clone, Copy, Debug, Default)]
pub struct Avro;

/// The media type of a block's bytes, with the codec when there is one.
fn block_media_type(header: &Header) -> String {
    match header.metadata("avro.codec") {
        Some(codec) if codec != b"null" && !codec.is_empty() => {
            format!("avro/binary; codec={}", String::from_utf8_lossy(codec))
        }
        _ => "avro/binary".to_string(),
    }
}

impl Shape for Avro {
    fn technology(&self) -> &'static str {
        "avro"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "avro/binary",
            "application/avro",
            "application/vnd.apache.avro+binary",
        ]
    }

    fn recognises(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(MAGIC)
    }

    fn shape(&self, stream: &Stream) -> Result<Shaped, ShapeError> {
        let bytes = stream.bytes();
        let refused = |(reason, at): (&str, usize)| ShapeError::new("avro", reason).at(at);
        let header = container::header(bytes).map_err(refused)?;
        let schema = header
            .metadata("avro.schema")
            .ok_or_else(|| refused(("no schema in the header", MAGIC.len())))?;
        let message_type = schema::full_name(schema);
        let media = block_media_type(&header);
        let parts = container::blocks(bytes, &header)
            .map_err(refused)?
            .into_iter()
            .map(|block| {
                Part::new(
                    Some(block.number.to_string()),
                    &bytes[block.range],
                    Some(media.clone()),
                )
            })
            .collect();
        Ok(Shaped {
            parts,
            message_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcore::StreamId;

    const SYNC: [u8; 16] = *b"0123456789abcdef";

    fn stream(bytes: &[u8], media: Option<&str>) -> Stream {
        Stream::new(StreamId::new(1), bytes.to_vec(), media.map(str::to_string))
    }

    /// A long as Avro writes it: zig-zag, then base-128 little-endian.
    fn long(value: i64) -> Vec<u8> {
        let mut zigzag = ((value << 1) ^ (value >> 63)).cast_unsigned();
        let mut out = Vec::new();
        loop {
            let byte = u8::try_from(zigzag & 0x7f).expect("seven bits");
            zigzag >>= 7;
            if zigzag == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn sized(bytes: &[u8]) -> Vec<u8> {
        let mut out = long(i64::try_from(bytes.len()).expect("fits"));
        out.extend_from_slice(bytes);
        out
    }

    fn file(metadata: &[(&str, &[u8])], blocks: &[(i64, &[u8])]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(long(i64::try_from(metadata.len()).expect("fits")));
        for (key, value) in metadata {
            out.extend(sized(key.as_bytes()));
            out.extend(sized(value));
        }
        out.push(0);
        out.extend_from_slice(&SYNC);
        for (objects, data) in blocks {
            out.extend(long(*objects));
            out.extend(sized(data));
            out.extend_from_slice(&SYNC);
        }
        out
    }

    const SCHEMA: &[u8] = br#"{"type":"record","name":"Order","namespace":"com.example",
        "fields":[{"name":"id","type":"long"}]}"#;

    #[test]
    fn every_block_is_a_numbered_part_and_the_schema_full_name_is_the_announced_type() {
        let bytes = file(
            &[("avro.schema", SCHEMA), ("avro.codec", b"null")],
            &[(2, b"\x02\x04"), (1, b"\x06")],
        );
        let shaped = Avro.shape(&stream(&bytes, None)).expect("a container");
        assert_eq!(shaped.parts.len(), 2);
        assert_eq!(shaped.parts[0].name.as_deref(), Some("1"));
        assert_eq!(shaped.parts[0].bytes, b"\x02\x04");
        assert_eq!(shaped.parts[0].media_type.as_deref(), Some("avro/binary"));
        assert_eq!(shaped.parts[1].name.as_deref(), Some("2"));
        assert_eq!(shaped.parts[1].bytes, b"\x06");
        assert_eq!(shaped.message_type.as_deref(), Some("com.example.Order"));

        let empty = file(&[("avro.schema", b"\"string\"")], &[]);
        let shaped = Avro.shape(&stream(&empty, None)).expect("no blocks");
        assert!(shaped.parts.is_empty());
        assert_eq!(shaped.message_type, None);
    }

    #[test]
    fn a_codec_other_than_null_is_named_on_every_block() {
        let bytes = file(
            &[("avro.codec", b"deflate"), ("avro.schema", SCHEMA)],
            &[(1, b"\x00\x01\x02")],
        );
        let shaped = Avro.shape(&stream(&bytes, None)).expect("a container");
        assert_eq!(
            shaped.parts[0].media_type.as_deref(),
            Some("avro/binary; codec=deflate")
        );
        assert_eq!(shaped.parts[0].bytes, b"\x00\x01\x02");
    }

    #[test]
    fn a_wrong_magic_a_missing_schema_and_a_broken_block_are_refused_where_they_fail() {
        let wrong = Avro
            .shape(&stream(b"Obj\x02rest", None))
            .expect_err("not a container");
        assert_eq!(wrong.offset, Some(0));
        assert_eq!(
            wrong.to_string(),
            "avro: not an Avro object container file at byte 0"
        );

        let no_schema = file(&[("avro.codec", b"null")], &[]);
        let refused = Avro
            .shape(&stream(&no_schema, None))
            .expect_err("no schema");
        assert_eq!(refused.reason, "no schema in the header");
        assert_eq!(refused.offset, Some(4));

        let mut mismatched = file(&[("avro.schema", SCHEMA)], &[(1, b"\x06")]);
        let last = mismatched.len() - 1;
        mismatched[last] ^= 0xff;
        let refused = Avro
            .shape(&stream(&mismatched, None))
            .expect_err("bad marker");
        assert_eq!(refused.reason, "the sync marker does not match");
        assert_eq!(refused.offset, Some(last - 15));

        let mut cut = file(&[("avro.schema", SCHEMA)], &[(1, b"\x06")]);
        cut.truncate(cut.len() - 17);
        let refused = Avro
            .shape(&stream(&cut, None))
            .expect_err("cut inside a block");
        assert_eq!(refused.reason, "the file ends inside a value");
        assert_eq!(refused.offset, Some(cut.len()));
    }

    #[test]
    fn the_shape_claims_avro_and_recognises_the_magic() {
        assert_eq!(Avro.technology(), "avro");
        assert!(Avro.media_types().contains(&"avro/binary"));
        assert!(Avro.recognises(b"Obj\x01\x00"));
        assert!(!Avro.recognises(b"Obj\x00"));
        assert!(!Avro.recognises(b"{}"));
        assert!(!Avro.recognises(b""));

        let shapes: [&dyn Shape; 1] = [&Avro];
        let by_media = message::choose(&shapes, &stream(b"x", Some("Application/Avro")));
        assert_eq!(by_media.map(Shape::technology), Some("avro"));
        let by_look = message::choose(&shapes, &stream(b"Obj\x01", None));
        assert_eq!(by_look.map(Shape::technology), Some("avro"));
        assert!(message::choose(&shapes, &stream(b"plain", None)).is_none());
    }
}
