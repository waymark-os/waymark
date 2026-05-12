use std::fmt;

const MAGIC: &[u8; 8] = b"HLXAST1\0";
const VERSION: u32 = 1;
const BLOCK_ALIGN: u64 = 512;
const DATA_ALIGN: u64 = 4096;
const HEADER_FIXED_LEN: u64 = 8 + 4 + 16 + 2 + 4 + 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetImage {
    metadata: AssetImageMetadata,
    entries: Vec<AssetEntry>,
}

impl AssetImage {
    pub fn new(entries: Vec<AssetEntry>) -> Result<Self, AssetImageError> {
        Self::with_metadata(AssetImageMetadata::default(), entries)
    }

    pub fn with_metadata(
        metadata: AssetImageMetadata,
        entries: Vec<AssetEntry>,
    ) -> Result<Self, AssetImageError> {
        validate_metadata(&metadata)?;
        validate_owned_entries(&entries)?;
        Ok(Self { metadata, entries })
    }

    pub fn metadata(&self) -> &AssetImageMetadata {
        &self.metadata
    }

    pub fn entries(&self) -> &[AssetEntry] {
        &self.entries
    }

    pub fn encode(&self) -> Result<Vec<u8>, AssetImageError> {
        validate_metadata(&self.metadata)?;
        validate_owned_entries(&self.entries)?;

        let table_len = encoded_table_len(&self.metadata, &self.entries)?;
        let data_start = align_up(table_len, DATA_ALIGN)?;
        let mut data_offset = data_start;
        let mut records = Vec::with_capacity(self.entries.len());

        for entry in &self.entries {
            let len = u64::try_from(entry.content.len()).map_err(|_| {
                AssetImageError::ContentTooLarge {
                    path: entry.path.clone(),
                }
            })?;
            records.push(EntryRecord {
                path: entry.path.as_str(),
                offset: data_offset,
                len,
            });
            data_offset =
                data_offset
                    .checked_add(len)
                    .ok_or_else(|| AssetImageError::ContentTooLarge {
                        path: entry.path.clone(),
                    })?;
        }

        let capacity = usize::try_from(data_offset).map_err(|_| AssetImageError::ImageTooLarge)?;
        let mut out = Vec::with_capacity(capacity);
        out.extend_from_slice(MAGIC);
        push_u32(&mut out, VERSION);
        out.extend_from_slice(&self.metadata.uuid);
        push_u16(
            &mut out,
            self.metadata
                .label
                .len()
                .try_into()
                .map_err(|_| AssetImageError::LabelTooLong)?,
        );
        out.extend_from_slice(self.metadata.label.as_bytes());
        push_u32(
            &mut out,
            self.entries
                .len()
                .try_into()
                .map_err(|_| AssetImageError::TooManyEntries)?,
        );
        push_u64(&mut out, data_start);

        for record in &records {
            let path = record.path.as_bytes();
            push_u32(
                &mut out,
                path.len()
                    .try_into()
                    .map_err(|_| AssetImageError::PathTooLong(record.path.to_string()))?,
            );
            out.extend_from_slice(path);
            push_u64(&mut out, record.offset);
            push_u64(&mut out, record.len);
        }

        let data_start_usize =
            usize::try_from(data_start).map_err(|_| AssetImageError::ImageTooLarge)?;
        out.resize(data_start_usize, 0);
        for entry in &self.entries {
            out.extend_from_slice(&entry.content);
        }
        let padded_len = align_up(
            u64::try_from(out.len()).map_err(|_| AssetImageError::ImageTooLarge)?,
            BLOCK_ALIGN,
        )?;
        out.resize(
            usize::try_from(padded_len).map_err(|_| AssetImageError::ImageTooLarge)?,
            0,
        );

        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AssetImageError> {
        let view = AssetImageView::decode(bytes)?;
        let entries = view
            .entries
            .iter()
            .map(|entry| AssetEntry {
                path: entry.path.to_string(),
                content: entry.content.to_vec(),
            })
            .collect();
        Self::with_metadata(view.metadata.clone(), entries)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetImageProbe {
    pub metadata: AssetImageMetadata,
    pub entry_count: u32,
    pub data_start: u64,
}

impl AssetImageProbe {
    pub fn probe(bytes: &[u8]) -> Result<Self, AssetImageError> {
        let (header, _) = read_header(bytes)?;
        Ok(Self {
            metadata: header.metadata,
            entry_count: header.entry_count,
            data_start: header.data_start,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AssetImageMetadata {
    pub uuid: [u8; 16],
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetImageView<'a> {
    metadata: AssetImageMetadata,
    entries: Vec<AssetEntryRef<'a>>,
}

impl<'a> AssetImageView<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, AssetImageError> {
        let (header, mut cursor) = read_header(bytes)?;

        let mut records = Vec::with_capacity(header.entry_count as usize);
        for _ in 0..header.entry_count {
            let path_len = cursor.read_u32()? as usize;
            let path_bytes = cursor.read_exact(path_len)?;
            let path =
                std::str::from_utf8(path_bytes).map_err(|_| AssetImageError::InvalidUtf8Path)?;
            let offset = cursor.read_u64()?;
            let len = cursor.read_u64()?;
            if offset < header.data_start {
                return Err(AssetImageError::InvalidDataOffset(offset));
            }
            records.push(EntryRecord { path, offset, len });
        }
        if u64::try_from(cursor.offset).unwrap_or(u64::MAX) > header.data_start {
            return Err(AssetImageError::InvalidDataOffset(header.data_start));
        }

        validate_records(&records)?;
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            let start =
                usize::try_from(record.offset).map_err(|_| AssetImageError::ImageTooLarge)?;
            let len = usize::try_from(record.len).map_err(|_| AssetImageError::ImageTooLarge)?;
            let end = start
                .checked_add(len)
                .ok_or(AssetImageError::UnexpectedEof)?;
            let content = bytes
                .get(start..end)
                .ok_or(AssetImageError::UnexpectedEof)?;
            entries.push(AssetEntryRef {
                path: record.path,
                offset: record.offset,
                content,
            });
        }

        Ok(Self {
            metadata: header.metadata,
            entries,
        })
    }

    pub fn metadata(&self) -> &AssetImageMetadata {
        &self.metadata
    }

    pub fn entries(&self) -> &[AssetEntryRef<'a>] {
        &self.entries
    }

    pub fn entry(&self, path: &str) -> Option<&AssetEntryRef<'a>> {
        self.entries
            .binary_search_by(|entry| entry.path.cmp(path))
            .ok()
            .map(|index| &self.entries[index])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetEntry {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetEntryRef<'a> {
    pub path: &'a str,
    pub offset: u64,
    pub content: &'a [u8],
}

#[derive(Clone, Copy)]
struct EntryRecord<'a> {
    path: &'a str,
    offset: u64,
    len: u64,
}

struct Header {
    metadata: AssetImageMetadata,
    entry_count: u32,
    data_start: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub enum AssetImageError {
    ContentTooLarge { path: String },
    DuplicatePath(String),
    EntryNotSorted { previous: String, current: String },
    EntryOverlapsData { path: String },
    ImageTooLarge,
    InvalidDataOffset(u64),
    InvalidLabel(String),
    InvalidMagic,
    InvalidPath(String),
    InvalidUtf8Label,
    InvalidUtf8Path,
    LabelTooLong,
    PathTooLong(String),
    TooManyEntries,
    UnexpectedEof,
    UnsupportedVersion(u32),
}

impl fmt::Display for AssetImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContentTooLarge { path } => write!(f, "content too large for {path}"),
            Self::DuplicatePath(path) => write!(f, "duplicate path {path}"),
            Self::EntryNotSorted { previous, current } => {
                write!(f, "entries must be sorted: {previous} before {current}")
            }
            Self::EntryOverlapsData { path } => write!(f, "entry overlaps previous data {path}"),
            Self::ImageTooLarge => write!(f, "asset image is too large"),
            Self::InvalidDataOffset(offset) => write!(f, "invalid data offset {offset}"),
            Self::InvalidLabel(label) => write!(f, "invalid asset image label {label}"),
            Self::InvalidMagic => write!(f, "invalid asset image magic"),
            Self::InvalidPath(path) => write!(f, "invalid asset image path {path}"),
            Self::InvalidUtf8Label => write!(f, "asset image label is not valid UTF-8"),
            Self::InvalidUtf8Path => write!(f, "asset image path is not valid UTF-8"),
            Self::LabelTooLong => write!(f, "asset image label is too long"),
            Self::PathTooLong(path) => write!(f, "path too long {path}"),
            Self::TooManyEntries => write!(f, "too many asset image entries"),
            Self::UnexpectedEof => write!(f, "unexpected end of asset image"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported asset image version {version}")
            }
        }
    }
}

impl std::error::Error for AssetImageError {}

fn read_header(bytes: &[u8]) -> Result<(Header, Cursor<'_>), AssetImageError> {
    let mut cursor = Cursor::new(bytes);
    if cursor.read_exact(8)? != MAGIC {
        return Err(AssetImageError::InvalidMagic);
    }
    let version = cursor.read_u32()?;
    if version != VERSION {
        return Err(AssetImageError::UnsupportedVersion(version));
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(cursor.read_exact(16)?);
    let label_len = cursor.read_u16()? as usize;
    let label_bytes = cursor.read_exact(label_len)?;
    let label = std::str::from_utf8(label_bytes)
        .map_err(|_| AssetImageError::InvalidUtf8Label)?
        .to_owned();
    let metadata = AssetImageMetadata { uuid, label };
    validate_metadata(&metadata)?;
    let entry_count = cursor.read_u32()?;
    let data_start = cursor.read_u64()?;
    if data_start < HEADER_FIXED_LEN || data_start % DATA_ALIGN != 0 {
        return Err(AssetImageError::InvalidDataOffset(data_start));
    }

    Ok((
        Header {
            metadata,
            entry_count,
            data_start,
        },
        cursor,
    ))
}

fn validate_metadata(metadata: &AssetImageMetadata) -> Result<(), AssetImageError> {
    if metadata.label.len() > u16::MAX as usize {
        return Err(AssetImageError::LabelTooLong);
    }
    if !metadata.label.is_empty()
        && !metadata
            .label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AssetImageError::InvalidLabel(metadata.label.clone()));
    }
    Ok(())
}

fn validate_owned_entries(entries: &[AssetEntry]) -> Result<(), AssetImageError> {
    let records = entries
        .iter()
        .map(|entry| EntryRecord {
            path: entry.path.as_str(),
            offset: 0,
            len: 0,
        })
        .collect::<Vec<_>>();
    validate_records(&records)
}

fn validate_records(entries: &[EntryRecord<'_>]) -> Result<(), AssetImageError> {
    let mut previous: Option<&str> = None;
    let mut previous_end = 0u64;
    for entry in entries {
        if !is_valid_entry_path(entry.path) {
            return Err(AssetImageError::InvalidPath(entry.path.to_string()));
        }
        if let Some(previous_path) = previous {
            if previous_path == entry.path {
                return Err(AssetImageError::DuplicatePath(entry.path.to_string()));
            }
            if previous_path > entry.path {
                return Err(AssetImageError::EntryNotSorted {
                    previous: previous_path.to_string(),
                    current: entry.path.to_string(),
                });
            }
        }
        if entry.offset < previous_end {
            return Err(AssetImageError::EntryOverlapsData {
                path: entry.path.to_string(),
            });
        }
        previous_end = entry
            .offset
            .checked_add(entry.len)
            .ok_or(AssetImageError::ImageTooLarge)?;
        previous = Some(entry.path);
    }
    Ok(())
}

fn is_valid_entry_path(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && !path.ends_with('/')
        && !path.contains("//")
        && path
            .split('/')
            .all(|component| component.is_empty() || component != "." && component != "..")
}

fn encoded_table_len(
    metadata: &AssetImageMetadata,
    entries: &[AssetEntry],
) -> Result<u64, AssetImageError> {
    let label_len =
        u64::try_from(metadata.label.len()).map_err(|_| AssetImageError::LabelTooLong)?;
    let mut len = HEADER_FIXED_LEN
        .checked_add(label_len)
        .ok_or(AssetImageError::ImageTooLarge)?;
    for entry in entries {
        let path_len = u64::try_from(entry.path.len())
            .map_err(|_| AssetImageError::PathTooLong(entry.path.clone()))?;
        len = len
            .checked_add(4)
            .and_then(|len| len.checked_add(path_len))
            .and_then(|len| len.checked_add(8))
            .and_then(|len| len.checked_add(8))
            .ok_or(AssetImageError::ImageTooLarge)?;
    }
    Ok(len)
}

fn align_up(value: u64, align: u64) -> Result<u64, AssetImageError> {
    let mask = align - 1;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or(AssetImageError::ImageTooLarge)
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], AssetImageError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AssetImageError::UnexpectedEof)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(AssetImageError::UnexpectedEof)?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u32(&mut self) -> Result<u32, AssetImageError> {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(self.read_exact(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_u16(&mut self) -> Result<u16, AssetImageError> {
        let mut bytes = [0u8; 2];
        bytes.copy_from_slice(self.read_exact(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64, AssetImageError> {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(self.read_exact(8)?);
        Ok(u64::from_le_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        encoded_table_len, AssetEntry, AssetImage, AssetImageError, AssetImageMetadata,
        AssetImageProbe, AssetImageView,
    };

    #[test]
    fn round_trip_asset_image() {
        let image = AssetImage::new(vec![
            AssetEntry {
                path: "/a.txt".to_string(),
                content: b"alpha".to_vec(),
            },
            AssetEntry {
                path: "/nested/b.txt".to_string(),
                content: b"beta".to_vec(),
            },
        ])
        .unwrap();

        let bytes = image.encode().unwrap();
        let decoded = AssetImage::decode(&bytes).unwrap();

        assert_eq!(decoded, image);
    }

    #[test]
    fn round_trip_metadata() {
        let image = AssetImage::with_metadata(
            AssetImageMetadata {
                uuid: [7; 16],
                label: "terminalbench-hello-world".to_string(),
            },
            vec![AssetEntry {
                path: "/task.yaml".to_string(),
                content: b"version: 1\n".to_vec(),
            }],
        )
        .unwrap();

        let bytes = image.encode().unwrap();
        let view = AssetImageView::decode(&bytes).unwrap();

        assert_eq!(view.metadata().uuid, [7; 16]);
        assert_eq!(view.metadata().label, "terminalbench-hello-world");
        assert_eq!(AssetImage::decode(&bytes).unwrap(), image);
    }

    #[test]
    fn probes_metadata_from_first_page() {
        let image = AssetImage::with_metadata(
            AssetImageMetadata {
                uuid: [9; 16],
                label: "task-assets".to_string(),
            },
            vec![
                AssetEntry {
                    path: "/a.txt".to_string(),
                    content: b"alpha".to_vec(),
                },
                AssetEntry {
                    path: "/b.txt".to_string(),
                    content: b"beta".to_vec(),
                },
            ],
        )
        .unwrap();
        let bytes = image.encode().unwrap();
        let probe = AssetImageProbe::probe(&bytes[..4096]).unwrap();

        assert_eq!(probe.metadata.uuid, [9; 16]);
        assert_eq!(probe.metadata.label, "task-assets");
        assert_eq!(probe.entry_count, 2);
        assert_eq!(probe.data_start, 4096);
    }

    #[test]
    fn borrowed_view_returns_offset_length_content() {
        let image = AssetImage::new(vec![AssetEntry {
            path: "/a.txt".to_string(),
            content: b"alpha".to_vec(),
        }])
        .unwrap();
        let bytes = image.encode().unwrap();
        let view = AssetImageView::decode(&bytes).unwrap();
        let entry = view.entry("/a.txt").unwrap();

        assert_eq!(entry.offset, 4096);
        assert_eq!(entry.content, b"alpha");
        assert!(entry.content.as_ptr() >= bytes.as_ptr());
        assert!(entry.content.as_ptr() < unsafe { bytes.as_ptr().add(bytes.len()) });
    }

    #[test]
    fn encoded_image_is_block_aligned() {
        let image = AssetImage::new(vec![AssetEntry {
            path: "/a.txt".to_string(),
            content: b"alpha".to_vec(),
        }])
        .unwrap();
        let bytes = image.encode().unwrap();

        assert_eq!(bytes.len() % 512, 0);
        assert_eq!(AssetImage::decode(&bytes).unwrap(), image);
    }

    #[test]
    fn rejects_unsorted_paths() {
        let err = AssetImage::new(vec![
            AssetEntry {
                path: "/b.txt".to_string(),
                content: Vec::new(),
            },
            AssetEntry {
                path: "/a.txt".to_string(),
                content: Vec::new(),
            },
        ])
        .unwrap_err();

        assert_eq!(
            err,
            AssetImageError::EntryNotSorted {
                previous: "/b.txt".to_string(),
                current: "/a.txt".to_string(),
            }
        );
    }

    #[test]
    fn rejects_directory_paths() {
        let err = AssetImage::new(vec![AssetEntry {
            path: "/dir/".to_string(),
            content: Vec::new(),
        }])
        .unwrap_err();

        assert_eq!(err, AssetImageError::InvalidPath("/dir/".to_string()));
    }

    #[test]
    fn rejects_overlapping_data_ranges() {
        let image = AssetImage::new(vec![
            AssetEntry {
                path: "/a.txt".to_string(),
                content: b"alpha".to_vec(),
            },
            AssetEntry {
                path: "/b.txt".to_string(),
                content: b"beta".to_vec(),
            },
        ])
        .unwrap();
        let mut bytes = image.encode().unwrap();
        let header_len = encoded_table_len(image.metadata(), &[]).unwrap() as usize;
        let second_offset_position = header_len + 4 + "/a.txt".len() + 8 + 8 + 4 + "/b.txt".len();
        bytes[second_offset_position..second_offset_position + 8]
            .copy_from_slice(&4096u64.to_le_bytes());

        let err = AssetImageView::decode(&bytes).unwrap_err();
        assert_eq!(
            err,
            AssetImageError::EntryOverlapsData {
                path: "/b.txt".to_string()
            }
        );
    }

    #[test]
    fn rejects_invalid_label() {
        let err = AssetImage::with_metadata(
            AssetImageMetadata {
                uuid: [0; 16],
                label: "bad label".to_string(),
            },
            Vec::new(),
        )
        .unwrap_err();

        assert_eq!(err, AssetImageError::InvalidLabel("bad label".to_string()));
    }
}
