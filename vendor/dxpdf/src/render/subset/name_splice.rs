//! Splice the original SFNT `name` table back into a subsetted font.
//!
//! `fontcull` (via `klippa`) drops every record from the `name` table because
//! its public API hard-codes an empty `name_ids` set. The resulting subset has
//! no family/full/PostScript name, so Skia falls back to a synthetic
//! `font<hex>` identifier and the PDF embeds that synthetic name. Splicing the
//! original `name` table back in restores the real PostScript name (with the
//! standard `ABCDEF+` subset prefix that Skia adds at PDF write time).
//!
//! The `name` table is independent of the glyph order — it stores font
//! metadata, not per-glyph data — so substituting it cannot affect text
//! shaping or rendering.
//!
//! The SFNT assembler this needs — table directory, §5.2 ordering, per-table
//! checksums and `head.checksumAdjustment` — is
//! `crate::render::fonts::opentype::sfnt::rebuild_sfnt`, shared with
//! collection-face carving rather than written twice. It lives outside this
//! `subset-fonts`-gated module because carving must work without the feature
//! too — see that function's own doc.

use crate::render::fonts::opentype::sfnt::rebuild_sfnt;

const NAME_TAG: &[u8; 4] = b"name";
const SFNT_HEADER_SIZE: usize = 12;
const TABLE_RECORD_SIZE: usize = 16;

/// Replace the `name` table in `subsetted` with the one from `original`.
/// Returns the rebuilt SFNT bytes. If either input lacks a `name` table the
/// subsetted bytes are returned unchanged.
pub fn splice_original_name(subsetted: &[u8], original: &[u8]) -> Result<Vec<u8>, String> {
    let Some(name_bytes) = find_table(original, NAME_TAG)? else {
        return Ok(subsetted.to_vec());
    };
    replace_table(subsetted, NAME_TAG, &name_bytes)
}

/// Locate a table by tag and return its raw bytes (unpadded).
fn find_table(sfnt: &[u8], tag: &[u8; 4]) -> Result<Option<Vec<u8>>, String> {
    let num_tables = read_num_tables(sfnt)?;
    for i in 0..num_tables {
        let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
        if rec + TABLE_RECORD_SIZE > sfnt.len() {
            return Err("table directory overflow".into());
        }
        if &sfnt[rec..rec + 4] == tag.as_slice() {
            let off = read_u32(sfnt, rec + 8) as usize;
            let len = read_u32(sfnt, rec + 12) as usize;
            if off.checked_add(len).is_none_or(|end| end > sfnt.len()) {
                return Err("table data overflow".into());
            }
            return Ok(Some(sfnt[off..off + len].to_vec()));
        }
    }
    Ok(None)
}

/// Build a new SFNT identical to `sfnt` except the table at `tag` is replaced
/// with `new_data`. Recomputes table directory, table checksums, and the
/// `head` table's `checksumAdjustment`.
fn replace_table(sfnt: &[u8], tag: &[u8; 4], new_data: &[u8]) -> Result<Vec<u8>, String> {
    if sfnt.len() < SFNT_HEADER_SIZE {
        return Err("sfnt too short".into());
    }
    let num_tables = read_num_tables(sfnt)?;

    // Collect (tag, data) pairs for every table, substituting `new_data` for
    // the target tag. Tables that don't appear are added.
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(num_tables);
    let mut found = false;
    for i in 0..num_tables {
        let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
        if rec + TABLE_RECORD_SIZE > sfnt.len() {
            return Err("table directory overflow".into());
        }
        let mut t = [0u8; 4];
        t.copy_from_slice(&sfnt[rec..rec + 4]);
        let data = if &t == tag {
            found = true;
            new_data.to_vec()
        } else {
            let off = read_u32(sfnt, rec + 8) as usize;
            let len = read_u32(sfnt, rec + 12) as usize;
            if off.checked_add(len).is_none_or(|end| end > sfnt.len()) {
                return Err("table data overflow".into());
            }
            sfnt[off..off + len].to_vec()
        };
        tables.push((t, data));
    }
    if !found {
        tables.push((*tag, new_data.to_vec()));
    }
    rebuild_sfnt(&sfnt[0..4], tables)
}

fn read_num_tables(sfnt: &[u8]) -> Result<usize, String> {
    if sfnt.len() < SFNT_HEADER_SIZE {
        return Err("sfnt too short".into());
    }
    Ok(u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize)
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{Data, FontMgr, FontStyle};

    #[test]
    fn splice_preserves_family_name() {
        let mgr = FontMgr::new();
        let Some(tf) = mgr.match_family_style("Carlito", FontStyle::normal()) else {
            // Carlito isn't installed on every CI image — skip rather than fail.
            return;
        };
        let Some((bytes, _)) = tf.to_font_data() else {
            return;
        };
        let unicodes: Vec<u32> = (0x20u32..0x7Fu32).collect();
        let subsetted = match fontcull::subset_font_data_unicode(&bytes, &unicodes, &[]) {
            Ok(s) => s,
            Err(_) => return,
        };

        let pre = mgr
            .new_from_data(&Data::new_copy(&subsetted), 0)
            .expect("subsetted parses");
        assert_eq!(
            pre.family_name(),
            "",
            "fontcull is expected to wipe the name table — if this changes, the splice is unnecessary"
        );

        let spliced = splice_original_name(&subsetted, &bytes).expect("splice");
        let post = mgr
            .new_from_data(&Data::new_copy(&spliced), 0)
            .expect("spliced parses");
        assert_eq!(post.family_name(), "Carlito");
    }

    // `rebuild_sfnt`'s own directory/checksum boundary tests live with its
    // definition now (`fonts::opentype::sfnt`) — this file only needs to pin
    // `replace_table`'s own behavior: substituting a table that's present,
    // and appending one that's absent.

    const HEAD_TAG: &[u8; 4] = b"head";

    fn tbl(tag: &[u8; 4], len: usize) -> ([u8; 4], Vec<u8>) {
        (*tag, vec![0u8; len])
    }

    /// A table absent from the input is appended rather than dropped — the
    /// path `splice_original_name` takes when the subset has no `name` table.
    #[test]
    fn replacing_an_absent_table_appends_it() {
        let original = rebuild_sfnt(b"\x00\x01\x00\x00", vec![tbl(b"head", 54)]).expect("build");
        let out = replace_table(&original, NAME_TAG, &[1, 2, 3, 4]).expect("replace");
        let found = find_table(&out, NAME_TAG)
            .expect("scan")
            .expect("name present");
        assert_eq!(found, vec![1, 2, 3, 4]);
        assert!(
            find_table(&out, HEAD_TAG).expect("scan").is_some(),
            "the existing table must survive"
        );
    }
}
