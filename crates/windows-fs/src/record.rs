use localsearch_platform_core::{PlatformError, PlatformErrorKind, PlatformResult};

const V2_FIXED_BYTES: usize = 60;
const MAX_RECORD_BYTES: usize = 1_048_576;

pub(crate) const REASON_DATA_CHANGE: u32 = 0x0000_0007;
pub(crate) const REASON_FILE_CREATE: u32 = 0x0000_0100;
pub(crate) const REASON_FILE_DELETE: u32 = 0x0000_0200;
pub(crate) const REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub(crate) const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub(crate) const REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub(crate) const REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SanitizedUsnRecord {
    pub file_reference: u64,
    pub parent_reference: u64,
    pub position: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: String,
    pub had_invalid_utf16: bool,
}

pub(crate) fn decode_v2(input: &[u8]) -> PlatformResult<(SanitizedUsnRecord, usize)> {
    let length =
        usize::try_from(read_u32(input, 0)?).map_err(|_| malformed("record length overflow"))?;
    if !(V2_FIXED_BYTES..=MAX_RECORD_BYTES).contains(&length)
        || length > input.len()
        || length % 8 != 0
    {
        return Err(malformed(
            "invalid, truncated, oversized, or misaligned record length",
        ));
    }
    if read_u16(input, 4)? != 2 {
        return Err(PlatformError::new(
            PlatformErrorKind::Unsupported,
            "decode_usn_record",
            "only USN record version 2 is supported by this spike",
        ));
    }
    let name_len = usize::from(read_u16(input, 56)?);
    let name_offset = usize::from(read_u16(input, 58)?);
    if name_len % 2 != 0
        || name_offset < V2_FIXED_BYTES
        || name_offset
            .checked_add(name_len)
            .is_none_or(|end| end > length)
    {
        return Err(malformed("filename range is outside the record"));
    }
    let units = input[name_offset..name_offset + name_len]
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    let had_invalid_utf16 = char::decode_utf16(units.iter().copied()).any(|item| item.is_err());
    let name = String::from_utf16_lossy(&units);
    Ok((
        SanitizedUsnRecord {
            file_reference: read_u64(input, 8)?,
            parent_reference: read_u64(input, 16)?,
            position: read_i64(input, 24)?,
            reason: read_u32(input, 40)?,
            attributes: read_u32(input, 52)?,
            name,
            had_invalid_utf16,
        },
        length,
    ))
}

pub(crate) fn decode_page(input: &[u8]) -> PlatformResult<(i64, Vec<SanitizedUsnRecord>)> {
    let next_position = read_i64(input, 0)?;
    let mut records = Vec::new();
    let mut offset = 8;
    while offset < input.len() {
        let (record, consumed) = decode_v2(&input[offset..])?;
        offset = offset
            .checked_add(consumed)
            .ok_or_else(|| malformed("page offset overflow"))?;
        records.push(record);
    }
    Ok((next_position, records))
}

fn bytes(input: &[u8], offset: usize, length: usize) -> PlatformResult<&[u8]> {
    input
        .get(
            offset
                ..offset
                    .checked_add(length)
                    .ok_or_else(|| malformed("field offset overflow"))?,
        )
        .ok_or_else(|| malformed("field outside input"))
}
fn read_u16(input: &[u8], offset: usize) -> PlatformResult<u16> {
    let value: [u8; 2] = bytes(input, offset, 2)?
        .try_into()
        .map_err(|_| malformed("u16 width"))?;
    Ok(u16::from_le_bytes(value))
}
fn read_u32(input: &[u8], offset: usize) -> PlatformResult<u32> {
    let value: [u8; 4] = bytes(input, offset, 4)?
        .try_into()
        .map_err(|_| malformed("u32 width"))?;
    Ok(u32::from_le_bytes(value))
}
fn read_u64(input: &[u8], offset: usize) -> PlatformResult<u64> {
    let value: [u8; 8] = bytes(input, offset, 8)?
        .try_into()
        .map_err(|_| malformed("u64 width"))?;
    Ok(u64::from_le_bytes(value))
}
fn read_i64(input: &[u8], offset: usize) -> PlatformResult<i64> {
    let value: [u8; 8] = bytes(input, offset, 8)?
        .try_into()
        .map_err(|_| malformed("i64 width"))?;
    Ok(i64::from_le_bytes(value))
}
fn malformed(detail: &'static str) -> PlatformError {
    PlatformError::new(PlatformErrorKind::Io, "decode_usn_record", detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &[u16]) -> Vec<u8> {
        let raw_len = V2_FIXED_BYTES + name.len() * 2;
        let len = raw_len.next_multiple_of(8);
        let mut bytes = vec![0; len];
        bytes[0..4].copy_from_slice(&(u32::try_from(len).expect("fixture fits u32")).to_le_bytes());
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        bytes[8..16].copy_from_slice(&42_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&7_u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&99_i64.to_le_bytes());
        bytes[56..58].copy_from_slice(
            &(u16::try_from(name.len() * 2).expect("fixture fits u16")).to_le_bytes(),
        );
        bytes[58..60].copy_from_slice(&60_u16.to_le_bytes());
        for (target, unit) in bytes[60..].chunks_exact_mut(2).zip(name) {
            target.copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn decodes_sanitized_v2_and_rejects_hostile_ranges() -> PlatformResult<()> {
        let bytes = record(&"safe.txt".encode_utf16().collect::<Vec<_>>());
        let (decoded, consumed) = decode_v2(&bytes)?;
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded.name, "safe.txt");
        let mut truncated = bytes;
        truncated[56..58].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            decode_v2(&truncated).expect_err("range must fail").kind,
            PlatformErrorKind::Io
        );
        Ok(())
    }

    #[test]
    fn malformed_utf16_is_marked_and_lossily_sanitized() -> PlatformResult<()> {
        let bytes = record(&[0xD800]);
        let (decoded, _) = decode_v2(&bytes)?;
        assert!(decoded.had_invalid_utf16);
        assert_eq!(decoded.name, "�");
        Ok(())
    }

    #[test]
    fn decodes_bounded_page_header_and_records() -> PlatformResult<()> {
        let first = record(&"one.txt".encode_utf16().collect::<Vec<_>>());
        let second = record(&"two.txt".encode_utf16().collect::<Vec<_>>());
        let mut page = 123_i64.to_le_bytes().to_vec();
        page.extend_from_slice(&first);
        page.extend_from_slice(&second);
        let (next, decoded) = decode_page(&page)?;
        assert_eq!(next, 123);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].name, "one.txt");
        assert_eq!(decoded[1].name, "two.txt");
        Ok(())
    }
}
