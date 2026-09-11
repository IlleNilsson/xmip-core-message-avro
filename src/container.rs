//! The Avro object container file: the magic, the metadata map, the sync
//! marker, then blocks of objects each followed by the marker again. This
//! is the walk that finds where each block's bytes lie; nothing in a block
//! is decoded, because the schema in the header is what a contract reads
//! and a shape only sections.

use std::ops::Range;

/// Where the walk stopped and why.
pub type Stop = (&'static str, usize);

/// The four bytes every object container file opens with.
pub const MAGIC: &[u8] = b"Obj\x01";

/// The header of a container file: its metadata, in the order written, and
/// its sync marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub metadata: Vec<(String, Vec<u8>)>,
    pub sync: [u8; 16],
    /// The byte at which the first block begins.
    pub end: usize,
}

impl Header {
    /// The metadata value under `key`, when the header carries it.
    #[must_use]
    pub fn metadata(&self, key: &str) -> Option<&[u8]> {
        self.metadata
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_slice())
    }
}

/// One block of objects: how many, and where their bytes lie, as written
/// and as compressed by the header's codec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// One-based, in the order of the file.
    pub number: usize,
    pub objects: i64,
    pub range: Range<usize>,
}

/// A zig-zag varint long at `at`: the value and the byte after it.
///
/// # Errors
/// The bytes end inside the number, or it runs past ten bytes.
pub fn long(bytes: &[u8], at: usize) -> Result<(i64, usize), Stop> {
    let mut value: u64 = 0;
    for (index, byte) in bytes.get(at..).unwrap_or(&[]).iter().enumerate() {
        if index >= 10 {
            return Err(("a number runs past ten bytes", at));
        }
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            let decoded = i64::try_from(value >> 1).unwrap_or(i64::MAX);
            let signed = if value & 1 == 0 {
                decoded
            } else {
                -decoded - 1
            };
            return Ok((signed, at + index + 1));
        }
    }
    Err(("the file ends inside a number", bytes.len()))
}

/// A length-prefixed byte sequence at `at`: its range and the byte after it.
fn sized(bytes: &[u8], at: usize) -> Result<(Range<usize>, usize), Stop> {
    let (length, start) = long(bytes, at)?;
    let length = usize::try_from(length).map_err(|_| ("a negative length", at))?;
    let end = start + length;
    if end > bytes.len() {
        return Err(("the file ends inside a value", bytes.len()));
    }
    Ok((start..end, end))
}

/// The header of a container file.
///
/// # Errors
/// The magic is not Avro's, or the file ends inside the header.
pub fn header(bytes: &[u8]) -> Result<Header, Stop> {
    if !bytes.starts_with(MAGIC) {
        return Err(("not an Avro object container file", 0));
    }
    let mut at = MAGIC.len();
    let mut metadata = Vec::new();
    loop {
        let (count, next) = long(bytes, at)?;
        at = next;
        if count == 0 {
            break;
        }
        if count < 0 {
            // A negative count is followed by the byte size of the map block.
            at = long(bytes, at)?.1;
        }
        for _ in 0..count.unsigned_abs() {
            let (key, next) = sized(bytes, at)?;
            let (value, next) = sized(bytes, next)?;
            metadata.push((
                String::from_utf8_lossy(&bytes[key]).into_owned(),
                bytes[value].to_vec(),
            ));
            at = next;
        }
    }
    let sync = bytes
        .get(at..at + 16)
        .and_then(|marker| <[u8; 16]>::try_from(marker).ok())
        .ok_or(("the file ends inside the sync marker", bytes.len()))?;
    Ok(Header {
        metadata,
        sync,
        end: at + 16,
    })
}

/// The blocks after the header, each closed by the header's sync marker.
///
/// # Errors
/// The file ends inside a block, or a block is not followed by the marker.
pub fn blocks(bytes: &[u8], header: &Header) -> Result<Vec<Block>, Stop> {
    let mut at = header.end;
    let mut blocks = Vec::new();
    while at < bytes.len() {
        let (objects, next) = long(bytes, at)?;
        let (range, next) = sized(bytes, next)?;
        let marker = bytes.get(next..next + 16);
        if marker != Some(&header.sync) {
            return Err(("the sync marker does not match", next));
        }
        blocks.push(Block {
            number: blocks.len() + 1,
            objects,
            range,
        });
        at = next + 16;
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_is_zig_zag_and_stops_where_it_cannot_end() {
        assert_eq!(long(&[0x00], 0), Ok((0, 1)));
        assert_eq!(long(&[0x01], 0), Ok((-1, 1)));
        assert_eq!(long(&[0x02], 0), Ok((1, 1)));
        assert_eq!(long(&[0xff, 0x01], 0), Ok((-128, 2)));
        assert_eq!(long(&[0xac, 0x02], 0), Ok((150, 2)));
        assert_eq!(long(&[0x80], 0), Err(("the file ends inside a number", 1)));
        assert_eq!(
            long(&[0x80; 11], 0),
            Err(("a number runs past ten bytes", 0))
        );
        assert_eq!(long(&[], 3), Err(("the file ends inside a number", 0)));
    }

    #[test]
    fn a_negative_map_count_carries_a_byte_size_and_a_short_marker_stops() {
        let mut file = MAGIC.to_vec();
        file.extend_from_slice(&[0x01, 0x10]);
        file.extend_from_slice(&[0x02, b'k', 0x02, b'v']);
        file.push(0x00);
        file.extend_from_slice(&[7u8; 16]);
        let header = header(&file).expect("a header");
        assert_eq!(header.metadata("k"), Some(b"v".as_slice()));
        assert_eq!(header.sync, [7u8; 16]);
        assert_eq!(header.end, file.len());
        assert_eq!(blocks(&file, &header), Ok(vec![]));

        let short = &file[..file.len() - 1];
        assert_eq!(
            super::header(short),
            Err(("the file ends inside the sync marker", short.len()))
        );
        assert_eq!(
            super::header(b"Obj\x02"),
            Err(("not an Avro object container file", 0))
        );
    }
}
