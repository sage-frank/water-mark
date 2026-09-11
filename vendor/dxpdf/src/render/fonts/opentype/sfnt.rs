//! Assemble an SFNT from a table list — table directory, §5.2 ordering,
//! per-table checksums, `head.checksumAdjustment`.
//!
//! Lives here rather than behind `subset-fonts` because it has two callers
//! now: [`super::collection::carve_collection_face`], needed unconditionally
//! so `FontRegistry` never asks a platform to open a non-zero collection
//! index, and `subset::name_splice::replace_table`, which splices the
//! original `name` table back into a subsetted font. Same move as
//! [`super::format`]'s `FontFormat::detect` — see the module doc.

const HEAD_TAG: &[u8; 4] = b"head";
const SFNT_HEADER_SIZE: usize = 12;
const TABLE_RECORD_SIZE: usize = 16;
const HEAD_CHECKSUM_ADJUSTMENT_OFFSET: usize = 8;
const HEAD_MAGIC: u32 = 0xB1B0_AFBA;

/// Largest table count whose directory search fields are expressible.
///
/// OpenType §5.2 defines `searchRange = 2^floor(log2(numTables)) * 16` as a
/// `uint16`. At 4096 tables that is 65 536 — one past the field's range — and
/// `rangeShift = numTables * 16 - searchRange` goes with it. The format cannot
/// describe such a directory at all, so `rebuild_sfnt` declines rather than
/// writing wrapped values. Real fonts carry on the order of ten tables.
const MAX_TABLES: usize = 4095;

/// Assemble an SFNT from `tables`. Takes ownership so it can enforce the
/// ascending-tag order OpenType §5.2 requires of the table directory — the sort
/// used to sit in `replace_table`, which left this function silently producing
/// an invalid directory for any other caller.
pub(crate) fn rebuild_sfnt(
    version: &[u8],
    mut tables: Vec<([u8; 4], Vec<u8>)>,
) -> Result<Vec<u8>, String> {
    // Both bounds below used to be handled inline and neither worked. An empty
    // set took an `if n == 0` branch that set `entry_selector = 0` and then
    // underflowed on `range_shift = 0*16 - 16`; a large set overflowed
    // `search_range` in `u16`. Both panicked in debug and wrapped in release.
    // Returning `Err` instead feeds the caller's existing best-effort path —
    // `apply.rs` falls back to the unnamed subset rather than aborting.
    if tables.is_empty() {
        return Err("cannot build an SFNT with no tables".into());
    }
    if tables.len() > MAX_TABLES {
        return Err(format!(
            "{} tables exceeds the {MAX_TABLES} a table directory can describe",
            tables.len()
        ));
    }
    let n = tables.len() as u16;
    // §5.2: the directory is sorted by tag.
    tables.sort_by_key(|t| t.0);

    // OpenType §5.2: entrySelector = floor(log2(numTables));
    // searchRange = 2^entrySelector * 16; rangeShift = numTables*16 - searchRange.
    // `n >= 1` is guaranteed above, so `leading_zeros() <= 15` and the
    // subtraction cannot underflow.
    let entry_selector = 15 - n.leading_zeros() as u16;
    let search_range = (1u16 << entry_selector) * 16;
    let range_shift = n * 16 - search_range;

    let dir_size = SFNT_HEADER_SIZE + tables.len() * TABLE_RECORD_SIZE;
    let mut offsets = Vec::with_capacity(tables.len());
    let mut cur = dir_size;
    for (_, data) in &tables {
        offsets.push(cur);
        cur += data.len();
        cur = (cur + 3) & !3; // 4-byte alignment between tables
    }
    let total_size = cur;

    let mut out = vec![0u8; total_size];
    out[0..4].copy_from_slice(version);
    out[4..6].copy_from_slice(&n.to_be_bytes());
    out[6..8].copy_from_slice(&search_range.to_be_bytes());
    out[8..10].copy_from_slice(&entry_selector.to_be_bytes());
    out[10..12].copy_from_slice(&range_shift.to_be_bytes());

    for (i, ((t, data), &off)) in tables.iter().zip(offsets.iter()).enumerate() {
        let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
        out[rec..rec + 4].copy_from_slice(t);
        // checksum filled below
        out[rec + 8..rec + 12].copy_from_slice(&(off as u32).to_be_bytes());
        out[rec + 12..rec + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
        out[off..off + data.len()].copy_from_slice(data);
    }

    // §5.2: `head.checksumAdjustment` must read zero for *both* checksums that
    // cover it — the `head` table's own directory entry and the whole-file
    // total. Zero it before the loop below, not after: computing the table
    // checksums first and then changing the field leaves the stored `head`
    // checksum describing bytes that no longer exist. The whole-file invariant
    // held either way, which is why nothing downstream noticed.
    let head_adj_pos = match tables.iter().position(|(t, _)| t == HEAD_TAG) {
        Some(i) => {
            // A `head` too short to contain the field would otherwise have its
            // "adjustment" written into padding or the following table.
            if tables[i].1.len() < HEAD_CHECKSUM_ADJUSTMENT_OFFSET + 4 {
                return Err("head table too short for checksumAdjustment".into());
            }
            let pos = offsets[i] + HEAD_CHECKSUM_ADJUSTMENT_OFFSET;
            out[pos..pos + 4].copy_from_slice(&0u32.to_be_bytes());
            Some(pos)
        }
        None => None,
    };

    // Per-table checksums are computed over the padded table region.
    for (i, ((_, data), &off)) in tables.iter().zip(offsets.iter()).enumerate() {
        let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
        let padded_len = (data.len() + 3) & !3;
        let checksum = sfnt_checksum(&out[off..off + padded_len]);
        out[rec + 4..rec + 8].copy_from_slice(&checksum.to_be_bytes());
    }

    // §5.2: head.checksumAdjustment = 0xB1B0AFBA - whole-file checksum, taken
    // with every directory entry final and the field itself still zero. Writing
    // it is the last mutation, so the file then sums to the magic exactly.
    if let Some(pos) = head_adj_pos {
        let total = sfnt_checksum(&out);
        let adjustment = HEAD_MAGIC.wrapping_sub(total);
        out[pos..pos + 4].copy_from_slice(&adjustment.to_be_bytes());
    }

    Ok(out)
}

/// Sum the buffer as big-endian u32 values, with zero-padding at the tail
/// if the length isn't 4-aligned.
fn sfnt_checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let chunks = data.chunks_exact(4);
    let rem = chunks.remainder();
    for c in chunks {
        sum = sum.wrapping_add(u32::from_be_bytes([c[0], c[1], c[2], c[3]]));
    }
    if !rem.is_empty() {
        let mut padded = [0u8; 4];
        padded[..rem.len()].copy_from_slice(rem);
        sum = sum.wrapping_add(u32::from_be_bytes(padded));
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32(buf: &[u8], offset: usize) -> u32 {
        u32::from_be_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ])
    }

    // ── `rebuild_sfnt` directory fields ──────────────────────────────────

    fn tbl(tag: &[u8; 4], len: usize) -> ([u8; 4], Vec<u8>) {
        (*tag, vec![0u8; len])
    }

    /// Synthetic table sets of arbitrary size, for the boundary cases.
    fn many(n: usize) -> Vec<([u8; 4], Vec<u8>)> {
        (0..n)
            .map(|i| {
                let mut t = [0u8; 4];
                t.copy_from_slice(&format!("{i:04x}").as_bytes()[..4]);
                (t, vec![0u8; 4])
            })
            .collect()
    }

    fn dir_fields(sfnt: &[u8]) -> (u16, u16, u16, u16) {
        let g = |o: usize| u16::from_be_bytes([sfnt[o], sfnt[o + 1]]);
        (g(4), g(6), g(8), g(10)) // numTables, searchRange, entrySelector, rangeShift
    }

    /// OpenType §5.2's own formulas, spelled out independently of the
    /// implementation so the test cannot inherit its arithmetic.
    #[test]
    fn directory_search_fields_follow_the_spec_formulas() {
        for n in [1usize, 2, 3, 4, 7, 8, 15, 16, 17, 100, 4095] {
            let out = rebuild_sfnt(b"\x00\x01\x00\x00", many(n)).expect("n={n}");
            let (num, search_range, entry_selector, range_shift) = dir_fields(&out);
            let expected_sel = (usize::BITS - 1 - n.leading_zeros()) as u16; // floor(log2 n)
            let expected_range = (1u32 << expected_sel) * 16;
            assert_eq!(num as usize, n, "numTables for n={n}");
            assert_eq!(entry_selector, expected_sel, "entrySelector for n={n}");
            assert_eq!(search_range as u32, expected_range, "searchRange for n={n}");
            assert_eq!(
                range_shift as u32,
                n as u32 * 16 - expected_range,
                "rangeShift for n={n}"
            );
        }
    }

    /// 4096 is where `searchRange` would be 65 536 — one past `uint16`. The
    /// old code panicked here in debug and wrapped in release; it now declines,
    /// and 4095 still succeeds so the bound is exact rather than conservative.
    #[test]
    fn the_table_count_bound_is_exact() {
        assert!(
            rebuild_sfnt(b"\x00\x01\x00\x00", many(MAX_TABLES)).is_ok(),
            "{MAX_TABLES} tables must still build"
        );
        let err = rebuild_sfnt(b"\x00\x01\x00\x00", many(MAX_TABLES + 1))
            .expect_err("one past the bound must be declined, not wrapped");
        assert!(err.contains("exceeds"), "got: {err}");
    }

    /// The empty set took a guard that set `entry_selector = 0` and then
    /// underflowed computing `rangeShift`. Unreachable from `replace_table`,
    /// which always pushes at least one table — but the function is callable
    /// on its own, and a guard that panics is not a guard.
    #[test]
    fn an_empty_table_set_is_declined_not_underflowed() {
        let err = rebuild_sfnt(b"\x00\x01\x00\x00", Vec::new()).expect_err("must decline");
        assert!(err.contains("no tables"), "got: {err}");
    }

    /// Tables are laid out in tag order with 4-byte alignment between them,
    /// and every directory entry must point at its own data.
    #[test]
    fn tables_are_sorted_by_tag_and_four_byte_aligned() {
        // Deliberately out of order, with a length that forces padding.
        let tables = vec![tbl(b"zzzz", 5), tbl(b"aaaa", 3), tbl(b"mmmm", 4)];
        let out = rebuild_sfnt(b"\x00\x01\x00\x00", tables.clone()).expect("build");
        let n = u16::from_be_bytes([out[4], out[5]]) as usize;
        assert_eq!(n, 3);

        let mut seen = Vec::new();
        for i in 0..n {
            let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
            let tag = &out[rec..rec + 4];
            let off = read_u32(&out, rec + 8) as usize;
            let len = read_u32(&out, rec + 12) as usize;
            assert_eq!(off % 4, 0, "table {i} is not 4-byte aligned");
            assert!(off + len <= out.len(), "table {i} runs past the buffer");
            seen.push(tag.to_vec());
            // The recorded length is the *unpadded* one.
            let expected = tables.iter().find(|(t, _)| t == tag).expect("known tag");
            assert_eq!(len, expected.1.len(), "length for {:?}", tag);
        }
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(seen, sorted, "directory must be sorted by tag");
    }

    // ── OpenType §5.2 checksums ──────────────────────────────────────────

    /// Recompute a table's directory checksum the way a validator would.
    fn stored_and_actual(sfnt: &[u8], tag: &[u8; 4]) -> Option<(u32, u32)> {
        let n = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
        (0..n).find_map(|i| {
            let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
            (&sfnt[rec..rec + 4] == tag).then(|| {
                let stored = read_u32(sfnt, rec + 4);
                let off = read_u32(sfnt, rec + 8) as usize;
                let len = read_u32(sfnt, rec + 12) as usize;
                let padded = (len + 3) & !3;
                (stored, sfnt_checksum(&sfnt[off..off + padded]))
            })
        })
    }

    /// §5.2: the whole font, summed as big-endian `u32`s with
    /// `checksumAdjustment` in place, must equal `0xB1B0AFBA`.
    #[test]
    fn the_whole_file_sums_to_the_head_magic() {
        let mut head = vec![0u8; 54];
        head[8..12].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        let out = rebuild_sfnt(
            b"\x00\x01\x00\x00",
            vec![(*b"head", head), (*b"name", vec![1, 2, 3, 4, 5])],
        )
        .expect("build");
        assert_eq!(sfnt_checksum(&out), HEAD_MAGIC);
    }

    /// §5.2: `head`'s *directory* checksum is the one taken with
    /// `checksumAdjustment` zeroed.
    ///
    /// Previously the per-table loop ran first and the field was rewritten
    /// afterwards, so the stored value described bytes that no longer existed —
    /// it came out as the caller's incoming garbage (`0xdeadbeef` here) rather
    /// than the spec's value. The whole-file invariant above still held, which
    /// is exactly why this went unnoticed.
    #[test]
    fn head_directory_checksum_is_taken_with_the_adjustment_zeroed() {
        let mut head = vec![0u8; 54];
        head[8..12].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        let out = rebuild_sfnt(
            b"\x00\x01\x00\x00",
            vec![(*b"head", head), (*b"name", vec![9, 9, 9, 9])],
        )
        .expect("build");

        let (stored, _) = stored_and_actual(&out, HEAD_TAG).expect("head present");
        // Reconstruct the spec value: head's bytes with the field zeroed.
        let n = u16::from_be_bytes([out[4], out[5]]) as usize;
        let (off, len) = (0..n)
            .find_map(|i| {
                let rec = SFNT_HEADER_SIZE + i * TABLE_RECORD_SIZE;
                (&out[rec..rec + 4] == HEAD_TAG).then(|| {
                    (
                        read_u32(&out, rec + 8) as usize,
                        read_u32(&out, rec + 12) as usize,
                    )
                })
            })
            .expect("head record");
        let mut zeroed = out[off..off + ((len + 3) & !3)].to_vec();
        zeroed[HEAD_CHECKSUM_ADJUSTMENT_OFFSET..HEAD_CHECKSUM_ADJUSTMENT_OFFSET + 4]
            .copy_from_slice(&0u32.to_be_bytes());

        assert_eq!(stored, sfnt_checksum(&zeroed), "§5.2 head checksum");
        assert_ne!(
            stored, 0xDEAD_BEEF,
            "must not be the caller's incoming adjustment"
        );
    }

    /// Every non-`head` table's stored checksum must match its bytes exactly —
    /// nothing rewrites those after the loop.
    #[test]
    fn non_head_directory_checksums_match_their_bytes() {
        let out = rebuild_sfnt(
            b"\x00\x01\x00\x00",
            vec![
                (*b"head", vec![0u8; 54]),
                (*b"name", vec![7; 13]),
                (*b"cmap", vec![3; 8]),
            ],
        )
        .expect("build");
        for tag in [b"name", b"cmap"] {
            let (stored, actual) = stored_and_actual(&out, tag).expect("table present");
            assert_eq!(
                stored,
                actual,
                "checksum for {:?}",
                std::str::from_utf8(tag)
            );
        }
    }

    /// A font with no `head` still builds — the adjustment step is simply
    /// skipped, and no whole-file invariant applies without the field.
    #[test]
    fn a_font_without_head_still_builds() {
        let out = rebuild_sfnt(b"\x00\x01\x00\x00", vec![(*b"name", vec![1, 2, 3, 4])])
            .expect("build without head");
        let (stored, actual) = stored_and_actual(&out, b"name").expect("name present");
        assert_eq!(stored, actual);
    }

    /// A `head` too short to hold `checksumAdjustment` is declined. Writing at
    /// its nominal offset would have landed in padding or the next table.
    #[test]
    fn a_truncated_head_is_declined_not_written_past() {
        let err = rebuild_sfnt(
            b"\x00\x01\x00\x00",
            vec![(*b"head", vec![0u8; 8]), (*b"name", vec![1, 2, 3, 4])],
        )
        .expect_err("must decline");
        assert!(err.contains("head table too short"), "got: {err}");
    }
}
