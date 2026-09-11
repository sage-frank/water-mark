//! OpenType metadata reading — the tables that say what a face *is*.
//!
//! Font resolution used to know only what Skia volunteers about a face
//! (`family_name`, `post_script_name`, `font_style`) and inferred the rest by
//! parsing the family string. This module replaces that inference with the
//! font's own records:
//!
//! | Table | Settles |
//! |---|---|
//! | [`name`] | family (ID 1), subfamily (2), full (4), PostScript (6), typographic (16/17), compatible-full (18), WWS (21/22) — each repeatable per language |
//! | [`os2`] | `usWeightClass`, `usWidthClass`, `fsSelection` |
//! | [`fvar`] | variable-font axes and named instances |
//! | [`stat`] | how axis values map back to style names |
//!
//! # Design constraints
//!
//! **Pure over bytes.** Nothing here takes a Skia type. The caller hands in one
//! table's bytes; these functions do not know or care whether they came from a
//! system typeface, a DOCX-embedded font, or a test fixture. That is what makes
//! the face catalogue testable identically on every host. It is also why this
//! module has no font-parsing crate dependency of its own: `--no-default-features`
//! compiles `fontcull` out, and the metadata index still has to work.
//!
//! **One table at a time.** Callers source bytes with
//! `Typeface::copy_table_data(tag)`, never `Typeface::to_font_data()`. The
//! difference is not stylistic: `to_font_data` on the 183 MB system emoji font
//! costs roughly 549 MB of unreleasable RSS (see the text-shaping note in
//! `AGENTS.md`), and the catalogue touches every face the host has.
//!
//! **Never panic on hostile input.** Fonts arrive from documents. Every offset
//! is bounds-checked through [`Reader`] and every entry point returns
//! [`Result`]`<_, `[`MalformedTable`]`>`; a truncated or self-inconsistent table
//! yields a typed error that the catalogue records and moves past. The `name`
//! table of a real shipping font is routinely a little malformed, so this is
//! the common path, not the exceptional one.
//!
//! **Also assembles, not just reads.** [`collection::carve_collection_face`]
//! and `sfnt::rebuild_sfnt` build standalone SFNT bytes from a table list —
//! the write-side counterpart to the read-only table parsers above, needed
//! because a TrueType Collection's faces are not independently addressable
//! (by any platform, in `FontMgr::new_from_data`'s case) without it.

pub mod collection;
pub mod format;
pub mod fvar;
pub mod name;
pub mod os2;
pub mod sfnt;
pub mod stat;

pub use collection::{carve_collection_face, CollectionCarveError, ExtractedSfnt};
pub use format::{FontFormat, FormatError, SfntFlavor, WoffVersion};
pub use fvar::{Axis, AxisTag, NamedInstance, VariationCoord};
pub use name::{NameId, NameRecords, PlatformId};
pub use os2::Os2Metrics;
pub use stat::StatAxisValues;

/// A four-byte OpenType table tag as Skia wants it — big-endian in a `u32`.
///
/// `skia_safe::FontTableTag` is a bare `u32`, so this exists to keep the
/// byte-order convention in one place rather than at every call site.
pub const fn table_tag(tag: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*tag)
}

/// What went wrong reading a table.
///
/// One variant per *kind* of malformation rather than one per site, so a caller
/// logging these produces a bounded set of messages. `offset` is relative to the
/// start of the table being read, which is what a hex dump of
/// `copy_table_data` output shows.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum MalformedTable {
    /// The table is shorter than its own fixed-size header.
    #[error("{table} table is {actual} bytes, shorter than its {needed}-byte header")]
    TooShort {
        table: &'static str,
        needed: usize,
        actual: usize,
    },
    /// A count/offset pair in the header points past the end of the table.
    #[error("{table} table: {field} at offset {offset} runs past the {len}-byte table")]
    OutOfBounds {
        table: &'static str,
        field: &'static str,
        offset: usize,
        len: usize,
    },
    /// The table's version or format field is one this reader does not know.
    ///
    /// Deliberately distinct from [`Self::OutOfBounds`]: a future format is a
    /// gap in *this code*, whereas an out-of-bounds offset is a broken font.
    /// The two want different reactions from whoever reads the log.
    #[error("{table} table has unsupported {field} {value}")]
    UnsupportedVersion {
        table: &'static str,
        field: &'static str,
        value: u32,
    },
}

/// Bounds-checked big-endian cursor over one table's bytes.
///
/// Every OpenType integer is big-endian, and every table is a header followed by
/// arrays addressed by byte offset. `Reader` gives both: sequential reads for
/// headers, and the `*_at` methods for the random access the arrays need — each
/// returning `Result` rather than panicking, so a malformed font degrades to a
/// missing alias instead of taking the render down.
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    table: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], table: &'static str) -> Self {
        Self {
            bytes,
            pos: 0,
            table,
        }
    }

    /// Length of the table being read, for callers that bound an array against
    /// it. There is deliberately no `is_empty`: an empty table is not a
    /// meaningful state here — [`require`](Self::require) rejects it with the
    /// header size it wanted, which is the answer a caller can act on.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Require the table to be at least `needed` bytes, so a header can then be
    /// read without per-field error handling.
    pub fn require(&self, needed: usize) -> Result<(), MalformedTable> {
        if self.bytes.len() < needed {
            return Err(MalformedTable::TooShort {
                table: self.table,
                needed,
                actual: self.bytes.len(),
            });
        }
        Ok(())
    }

    fn out_of_bounds(&self, field: &'static str, offset: usize) -> MalformedTable {
        MalformedTable::OutOfBounds {
            table: self.table,
            field,
            offset,
            len: self.bytes.len(),
        }
    }

    pub fn u8_at(&self, offset: usize, field: &'static str) -> Result<u8, MalformedTable> {
        self.bytes
            .get(offset)
            .copied()
            .ok_or_else(|| self.out_of_bounds(field, offset))
    }

    pub fn u16_at(&self, offset: usize, field: &'static str) -> Result<u16, MalformedTable> {
        let end = offset
            .checked_add(2)
            .ok_or(self.out_of_bounds(field, offset))?;
        let slice = self
            .bytes
            .get(offset..end)
            .ok_or_else(|| self.out_of_bounds(field, offset))?;
        Ok(u16::from_be_bytes([slice[0], slice[1]]))
    }

    pub fn i16_at(&self, offset: usize, field: &'static str) -> Result<i16, MalformedTable> {
        self.u16_at(offset, field).map(|v| v as i16)
    }

    pub fn u32_at(&self, offset: usize, field: &'static str) -> Result<u32, MalformedTable> {
        let end = offset
            .checked_add(4)
            .ok_or(self.out_of_bounds(field, offset))?;
        let slice = self
            .bytes
            .get(offset..end)
            .ok_or_else(|| self.out_of_bounds(field, offset))?;
        Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    /// A `Fixed` (16.16 signed) as `f32` — the coordinate type `fvar` uses.
    pub fn fixed_at(&self, offset: usize, field: &'static str) -> Result<f32, MalformedTable> {
        self.u32_at(offset, field)
            .map(|raw| raw as i32 as f32 / 65536.0)
    }

    /// An `F2Dot14` (2.14 signed) as `f32` — the normalized coordinate type
    /// `STAT` and `avar` use.
    pub fn f2dot14_at(&self, offset: usize, field: &'static str) -> Result<f32, MalformedTable> {
        self.u16_at(offset, field)
            .map(|raw| raw as i16 as f32 / 16384.0)
    }

    pub fn tag_at(&self, offset: usize, field: &'static str) -> Result<[u8; 4], MalformedTable> {
        let end = offset
            .checked_add(4)
            .ok_or(self.out_of_bounds(field, offset))?;
        let slice = self
            .bytes
            .get(offset..end)
            .ok_or_else(|| self.out_of_bounds(field, offset))?;
        Ok([slice[0], slice[1], slice[2], slice[3]])
    }

    pub fn slice_at(
        &self,
        offset: usize,
        len: usize,
        field: &'static str,
    ) -> Result<&'a [u8], MalformedTable> {
        let end = offset
            .checked_add(len)
            .ok_or(self.out_of_bounds(field, offset))?;
        self.bytes
            .get(offset..end)
            .ok_or_else(|| self.out_of_bounds(field, offset))
    }

    // ── sequential header reads ──────────────────────────────────────────

    pub fn u16(&mut self, field: &'static str) -> Result<u16, MalformedTable> {
        let v = self.u16_at(self.pos, field)?;
        self.pos += 2;
        Ok(v)
    }

    pub fn u32(&mut self, field: &'static str) -> Result<u32, MalformedTable> {
        let v = self.u32_at(self.pos, field)?;
        self.pos += 4;
        Ok(v)
    }

    pub fn position(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_tag_is_big_endian() {
        assert_eq!(table_tag(b"name"), 0x6E616D65);
        assert_eq!(table_tag(b"OS/2"), 0x4F532F32);
        assert_eq!(table_tag(b"fvar"), 0x66766172);
        assert_eq!(table_tag(b"STAT"), 0x53544154);
    }

    #[test]
    fn reader_reads_big_endian_integers() {
        let bytes = [0x12, 0x34, 0x56, 0x78];
        let r = Reader::new(&bytes, "probe");
        assert_eq!(r.u16_at(0, "f").unwrap(), 0x1234);
        assert_eq!(r.u32_at(0, "f").unwrap(), 0x1234_5678);
        assert_eq!(r.tag_at(0, "f").unwrap(), [0x12, 0x34, 0x56, 0x78]);
    }

    /// Signedness matters: an `i16` weight delta or a negative `Fixed`
    /// coordinate read as unsigned would silently become a huge positive.
    #[test]
    fn reader_reads_signed_and_fractional_types() {
        let neg = (-2i16 as u16).to_be_bytes();
        let r = Reader::new(&neg, "probe");
        assert_eq!(r.i16_at(0, "f").unwrap(), -2);

        // Fixed 16.16: 1.5 == 0x0001_8000
        let fixed = 0x0001_8000u32.to_be_bytes();
        let r = Reader::new(&fixed, "probe");
        assert_eq!(r.fixed_at(0, "f").unwrap(), 1.5);

        // Fixed 16.16 negative: -0.5 == 0xFFFF_8000
        let fixed = 0xFFFF_8000u32.to_be_bytes();
        let r = Reader::new(&fixed, "probe");
        assert_eq!(r.fixed_at(0, "f").unwrap(), -0.5);

        // F2Dot14: 1.0 == 0x4000, -1.0 == 0xC000
        let f2 = 0x4000u16.to_be_bytes();
        assert_eq!(Reader::new(&f2, "p").f2dot14_at(0, "f").unwrap(), 1.0);
        let f2 = 0xC000u16.to_be_bytes();
        assert_eq!(Reader::new(&f2, "p").f2dot14_at(0, "f").unwrap(), -1.0);
    }

    /// The whole point of `Reader`: hostile input returns an error rather than
    /// panicking. Fonts arrive from documents, so this is a reachable path.
    #[test]
    fn reader_refuses_to_read_past_the_end() {
        let bytes = [0x00, 0x01];
        let r = Reader::new(&bytes, "probe");

        assert!(matches!(
            r.u32_at(0, "field").unwrap_err(),
            MalformedTable::OutOfBounds {
                table: "probe",
                field: "field",
                offset: 0,
                len: 2
            }
        ));
        assert!(r.u16_at(1, "field").is_err(), "straddling the end");
        assert!(r.slice_at(1, 4, "field").is_err());
    }

    /// An offset near `usize::MAX` must not wrap into a valid range.
    #[test]
    fn reader_offsets_cannot_overflow_into_bounds() {
        let bytes = [0u8; 16];
        let r = Reader::new(&bytes, "probe");
        assert!(r.u32_at(usize::MAX - 1, "field").is_err());
        assert!(r.slice_at(usize::MAX - 1, 8, "field").is_err());
    }

    #[test]
    fn require_reports_the_header_size_it_wanted() {
        let bytes = [0u8; 3];
        let err = Reader::new(&bytes, "name").require(6).unwrap_err();
        assert_eq!(
            err,
            MalformedTable::TooShort {
                table: "name",
                needed: 6,
                actual: 3
            }
        );
    }
}
