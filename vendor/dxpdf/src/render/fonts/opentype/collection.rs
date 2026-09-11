//! Carve one face out of a TrueType/OpenType Collection into a standalone SFNT.
//!
//! A collection is a table directory per face over one shared pool of tables —
//! several faces routinely point at the *same* `glyf`, differing only in
//! `cmap` and `name`. Copying the tables a face's own directory names —
//! rather than slicing a byte range — is what makes this correct: the
//! selected face's tables are scattered through the file and are not
//! contiguous.
//!
//! This lives here, not behind the `subset-fonts` feature, because it now has
//! two callers with different lifetimes: [`crate::render::fonts::FontRegistry`]
//! carves at registration (and again whenever a face is reopened) so no
//! platform is ever asked to interpret a non-zero collection index — Skia's
//! CoreText backend declines one outright — and subsetting still carves the
//! selected face out of the original bytes before subsetting it. Same move as
//! [`super::format`]'s `FontFormat::detect`, for the same reason (issue #116).

use super::format::FontFormat;
use super::sfnt::rebuild_sfnt;
use super::SfntFlavor;

/// SFNT bytes verified ready for further use — subsetting, or handing
/// straight to `FontMgr::new_from_data` at index 0. The newtype prevents
/// accidental mixing with the various wrapper formats.
#[derive(Debug, Clone)]
pub struct ExtractedSfnt {
    pub bytes: Vec<u8>,
    pub flavor: SfntFlavor,
}

#[derive(thiserror::Error, Debug)]
pub enum CollectionCarveError {
    #[error("collection has {face_count} face(s); index {index} is out of range")]
    IndexOutOfRange { index: u32, face_count: u32 },
    #[error("could not carve face {index} out of the collection: {reason}")]
    FaceUnreadable { index: u32, reason: String },
}

/// Resolve one face of a collection into a standalone SFNT.
///
/// The collection header is a `numFonts`-long array of offsets, each to an
/// ordinary table directory. Copying the tables that directory names — rather
/// than slicing a byte range — is what makes this correct: faces share table
/// data, so the selected face's tables are scattered through the file and are
/// not contiguous.
pub fn carve_collection_face(
    bytes: &[u8],
    index: u32,
    face_count: u32,
) -> Result<ExtractedSfnt, CollectionCarveError> {
    if index >= face_count {
        return Err(CollectionCarveError::IndexOutOfRange { index, face_count });
    }
    let fail = |reason: String| CollectionCarveError::FaceUnreadable { index, reason };

    // TTC header: 'ttcf', major u16, minor u16, numFonts u32, then numFonts × Offset32.
    let offset_pos = TTC_HEADER_SIZE + index as usize * 4;
    let directory = read_u32(bytes, offset_pos)
        .ok_or_else(|| fail("collection offset table is truncated".into()))?
        as usize;

    let version = bytes
        .get(directory..directory + 4)
        .ok_or_else(|| fail("face directory is past the end of the file".into()))?;
    let flavor = match FontFormat::detect(version) {
        Ok(FontFormat::Sfnt(flavor)) => flavor,
        Ok(other) => return Err(fail(format!("face directory is not an SFNT ({other:?})"))),
        Err(err) => return Err(fail(err.to_string())),
    };

    let num_tables = read_u16(bytes, directory + 4)
        .ok_or_else(|| fail("face directory header is truncated".into()))?
        as usize;
    let mut tables = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let rec = directory + SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
        let tag = bytes
            .get(rec..rec + 4)
            .ok_or_else(|| fail(format!("table record {i} is truncated")))?;
        let offset = read_u32(bytes, rec + 8)
            .ok_or_else(|| fail(format!("table record {i} has no offset")))?
            as usize;
        let length = read_u32(bytes, rec + 12)
            .ok_or_else(|| fail(format!("table record {i} has no length")))?
            as usize;
        let data = offset
            .checked_add(length)
            .and_then(|end| bytes.get(offset..end))
            .ok_or_else(|| fail(format!("table {i} runs past the end of the file")))?;
        let mut t = [0u8; 4];
        t.copy_from_slice(tag);
        tables.push((t, data.to_vec()));
    }

    let rebuilt = rebuild_sfnt(version, tables).map_err(fail)?;
    log::debug!(
        "[font] carved face {index} of {face_count} out of a collection \
         ({} bytes → {} bytes)",
        bytes.len(),
        rebuilt.len()
    );
    Ok(ExtractedSfnt {
        bytes: rebuilt,
        flavor,
    })
}

const TTC_HEADER_SIZE: usize = 12;
const SFNT_HEADER_SIZE: usize = 12;
const TABLE_RECORD_SIZE: usize = 16;

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = bytes.get(offset..end)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fonts::tests::arbitrary_system_font_bytes;

    /// Build a two-face collection whose faces share every table but `name`,
    /// which is how real collections are laid out — and the reason a face
    /// cannot be extracted by slicing a byte range.
    fn collection_of_two(sfnt: &[u8]) -> Vec<u8> {
        let num_tables = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
        let mut tables = Vec::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            let mut tag = [0u8; 4];
            tag.copy_from_slice(&sfnt[rec..rec + 4]);
            let off = u32::from_be_bytes(sfnt[rec + 8..rec + 12].try_into().unwrap()) as usize;
            let len = u32::from_be_bytes(sfnt[rec + 12..rec + 16].try_into().unwrap()) as usize;
            tables.push((tag, sfnt[off..off + len].to_vec()));
        }

        // Two identical directories over one shared table pool.
        let header = 12 + 2 * 4;
        let dir_size = 12 + num_tables * 16;
        let pool_start = header + 2 * dir_size;

        let mut pool = Vec::new();
        let mut offsets = Vec::new();
        for (_, data) in &tables {
            offsets.push(pool_start + pool.len());
            pool.extend_from_slice(data);
            while !pool.len().is_multiple_of(4) {
                pool.push(0);
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"ttcf");
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&(header as u32).to_be_bytes());
        out.extend_from_slice(&((header + dir_size) as u32).to_be_bytes());
        for _ in 0..2 {
            out.extend_from_slice(&sfnt[0..4]);
            out.extend_from_slice(&sfnt[4..12]);
            for (i, (tag, data)) in tables.iter().enumerate() {
                out.extend_from_slice(tag);
                out.extend_from_slice(&[0u8; 4]); // checksum — rebuilt on carve
                out.extend_from_slice(&(offsets[i] as u32).to_be_bytes());
                out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
        }
        out.extend_from_slice(&pool);
        out
    }

    /// A collection face is carved into a standalone SFNT rather than refused.
    /// Skia accepting the result is the real assertion: it means the rebuilt
    /// directory, checksums and `head.checksumAdjustment` are all right.
    #[test]
    fn a_collection_face_is_carved_into_a_standalone_font() {
        let sfnt = arbitrary_system_font_bytes();
        let ttc = collection_of_two(&sfnt);
        assert!(matches!(
            FontFormat::detect(&ttc),
            Ok(FontFormat::Ttc { face_count: 2 })
        ));

        for index in 0..2 {
            let carved = carve_collection_face(&ttc, index, 2)
                .unwrap_or_else(|e| panic!("face {index} must carve: {e}"));
            assert!(matches!(
                FontFormat::detect(&carved.bytes),
                Ok(FontFormat::Sfnt(_))
            ));
            assert!(
                skia_safe::FontMgr::new()
                    .new_from_data(&carved.bytes[..], 0)
                    .is_some(),
                "Skia must accept the carved face {index}"
            );
        }
    }

    #[test]
    fn a_collection_index_past_the_end_is_reported() {
        let ttc = collection_of_two(&arbitrary_system_font_bytes());
        assert!(matches!(
            carve_collection_face(&ttc, 7, 2),
            Err(CollectionCarveError::IndexOutOfRange {
                index: 7,
                face_count: 2
            })
        ));
    }

    /// A header that promises faces it does not describe must produce a typed
    /// error, not a panic — these bytes come from documents.
    #[test]
    fn a_truncated_collection_is_reported_not_fatal() {
        let mut ttc = b"ttcf".to_vec();
        ttc.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        ttc.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // numFonts = 2, no offsets
        assert!(matches!(
            carve_collection_face(&ttc, 0, 2),
            Err(CollectionCarveError::FaceUnreadable { index: 0, .. })
        ));
    }
}
