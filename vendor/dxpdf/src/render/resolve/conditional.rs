//! §17.7.6: Table conditional formatting resolution.
//!
//! Determines which conditional formatting regions apply to each cell
//! based on its position, tblLook flags, and band sizes. Overlays
//! applicable tblStylePr overrides in priority order.

use crate::model::Dup;
use crate::model::{
    ParagraphProperties, RunProperties, TableCellProperties, TableLook, TableStyleOverride,
    TableStyleOverrideType,
};

/// Resolved conditional formatting for a single cell.
#[derive(Clone, Debug, Default)]
pub struct CellConditionalFormatting {
    pub cell_properties: Option<TableCellProperties>,
    pub run_properties: Option<RunProperties>,
    pub paragraph_properties: Option<ParagraphProperties>,
}

/// §17.4.55 `<w:tblLook>` with every question answered.
///
/// [`TableLook`] records what the file *said* — each flag is `None` when the
/// document left it out. This records what the renderer must *do*, which is a
/// different question, because an unstated flag still has an answer. Keeping
/// the two apart is what lets the default live in exactly one place instead of
/// being restated as six `unwrap_or`s at the read site.
///
/// The band flags are held in their *positive* sense (`h_band` = banding on),
/// which is the inverse of the file's `w:noHBand` / `w:noVBand`, so the
/// double negative is resolved once here rather than at every use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveRegions {
    pub first_row: bool,
    pub last_row: bool,
    pub first_column: bool,
    pub last_column: bool,
    pub h_band: bool,
    pub v_band: bool,
}

impl ActiveRegions {
    /// What Word assumes when `<w:tblLook>` is omitted: the bitmask **0x04A0**.
    ///
    /// [MS-OI29500] Part 1 §17.4.55 note (a) is explicit that the standard and
    /// Word disagree here — the standard says an omitted element means 0x0000,
    /// every region off, and states that in Word it is assumed to be 0x04A0.
    /// Word's reading is the one documents are authored against, so it is the
    /// one used. (Neither reading is "all regions on", which is what this
    /// resolver assumed before and which no source supports.)
    ///
    /// 0x04A0 decodes against the bit constants in
    /// `docx::parse::properties::schema::table::TblLookHex` as
    /// firstRow (0x020) + firstColumn (0x080) + noVBand (0x400):
    ///
    /// | flag        | mask    | 0x04A0 | region                    |
    /// |-------------|---------|--------|---------------------------|
    /// | firstRow    | `0x020` | set    | active                    |
    /// | lastRow     | `0x040` | clear  | inactive                  |
    /// | firstColumn | `0x080` | set    | active                    |
    /// | lastColumn  | `0x100` | clear  | inactive                  |
    /// | noHBand     | `0x200` | clear  | row banding **active**    |
    /// | noVBand     | `0x400` | set    | column banding inactive   |
    pub const WORD_DEFAULT: Self = Self {
        first_row: true,
        last_row: false,
        first_column: true,
        last_column: false,
        h_band: true,
        v_band: false,
    };

    /// Resolve a table's `<w:tblLook>`, absent or partial, to the six answers.
    ///
    /// An absent element is [`Self::WORD_DEFAULT`] per note (a). A flag the
    /// element leaves unstated falls back the same way, which is the same rule
    /// and not a second one — though nothing now reaches it, because the parse
    /// seam (`docx::parse::properties::schema::table::tbl_look`) answers every
    /// flag as soon as the element states anything at all, and drops the
    /// occurrence entirely when it states nothing. That drop is what keeps an
    /// empty `<w:tblLook/>` from shadowing the table style's; this function
    /// sees `None` for both spellings and cannot tell them apart, which is the
    /// point.
    pub fn resolve(look: Option<&TableLook>) -> Self {
        let d = Self::WORD_DEFAULT;
        let Some(look) = look else { return d };
        Self {
            first_row: look.first_row.unwrap_or(d.first_row),
            last_row: look.last_row.unwrap_or(d.last_row),
            first_column: look.first_column.unwrap_or(d.first_column),
            last_column: look.last_column.unwrap_or(d.last_column),
            h_band: !look.no_h_band.unwrap_or(!d.h_band),
            v_band: !look.no_v_band.unwrap_or(!d.v_band),
        }
    }
}

/// Grid position and dimension context for a single cell.
///
/// The horizontal position is a **grid column** (§17.4.16), not the cell's
/// index within its row's `<w:tc>` list. The two coincide only while every row
/// maps one cell to one column; `w:gridSpan` (§17.4.17) and `w:gridBefore`
/// (§17.4.15) break that, and it is the grid column Word conditions on — see
/// `applicable_regions` for the evidence.
pub struct CellGridPosition {
    pub row_idx: usize,
    /// First grid column the cell covers.
    pub grid_col: usize,
    /// How many grid columns it covers — `w:gridSpan`, at least 1.
    pub grid_span: usize,
    pub num_rows: usize,
    /// The table's `<w:tblGrid>` column count, *not* any one row's cell count.
    pub num_cols: usize,
    pub row_band_size: u32,
    pub col_band_size: u32,
}

/// §17.7.6: resolve conditional formatting for a cell at (row, col).
///
/// Overlays applicable `tblStylePr` overrides in ascending priority (later
/// overlays win), per §17.7.6:
/// 0. `wholeTable` — the base layer, applying to every cell
/// 1. Band1/Band2 Vertical (banded columns)
/// 2. Band1/Band2 Horizontal (banded rows — override column banding)
/// 3. First/Last Column
/// 4. First/Last Row
/// 5. Corner cells (highest)
///
/// `wholeTable` is not positional — it applies to every cell — so it is not
/// produced by `applicable_regions`, which answers "which *positional* regions
/// is this cell in?". Seeding the chain with it here keeps that function honest
/// and reuses the existing overlay machinery unchanged, so the positional
/// precedence (including D1's banding fix) is untouched: `wholeTable` simply
/// sits underneath.
///
/// The table-level half — a `wholeTable` override's `tblPr` (borders, cell
/// margins) — is folded into `ResolvedStyle::table` during style resolution,
/// because that is where `build_table` reads them from.
pub fn resolve_cell_conditional(
    pos: &CellGridPosition,
    look: Option<&TableLook>,
    overrides: &[TableStyleOverride],
) -> CellConditionalFormatting {
    let regions = applicable_regions(pos, look);

    let mut result = CellConditionalFormatting::default();

    // §17.7.6: apply overrides in priority order (lowest first, highest last).
    // Later overlays take precedence.
    for region in std::iter::once(&TableStyleOverrideType::WholeTable).chain(regions.iter()) {
        if let Some(ovr) = overrides.iter().find(|o| o.override_type == *region) {
            if let Some(ref tcp) = ovr.table_cell_properties {
                overlay_cell_properties(&mut result, tcp);
            }
            if let Some(ref rp) = ovr.run_properties {
                overlay_run_properties(&mut result, rp);
            }
            if let Some(ref pp) = ovr.paragraph_properties {
                overlay_paragraph_properties(&mut result, pp);
            }
        }
    }

    result
}

/// §17.7.6: determine which regions apply to a cell, in priority order
/// (lowest priority first). §17.4.55 `tblLook` controls which regions are
/// active.
///
/// # Columns are grid columns
///
/// A cell's column regions follow the **grid columns it covers** (§17.4.16),
/// not its ordinal among its row's `<w:tc>` elements: it is in `firstCol` when
/// it starts at grid column 0, and in `lastCol` when its `w:gridSpan` reaches
/// the last grid column. Under a row that maps one cell to one column the two
/// readings are the same; `w:gridSpan` and `w:gridBefore` separate them.
///
/// This is settled by Word's own output. Word records the regions it assigned
/// in `w:cnfStyle` (§17.3.1.8), so a Word-authored document with a `gridSpan`
/// under a style that defines a `lastCol` layer states the answer outright. In
/// `test-files/sample-docx-files-sample1.docx` the `Calendar3` table declares
/// a 14-column `<w:tblGrid>` and `<w:tblLook w:val="05A0"/>` — lastColumn on,
/// so a missing `lastCol` is signal and not a suppressed region — and its
/// first row is a *single* cell with `w:gridSpan="13"` covering grid columns
/// 0 to 12. Word wrote `001000000000` on it: `firstCol` alone. Read as a cell
/// index that cell is both the first and the last of its row, so the index
/// reading has to claim `lastCol` too. It does not reach column 13, and Word
/// agrees. Rows 1 onward corroborate from the other side: their cell 12 spans
/// grid columns 12 and 13, reaches the last one, and Word marks it `lastCol`.
/// `word_cnf_style_agrees_with_the_grid_column_reading` below asserts that
/// comparison against the fixture rather than restating it.
///
/// That same row settles §17.4.14 `gridAfter` at the same time, which is worth
/// stating because it looks like a separate question and is not: row 0 does not
/// merely stop at column 12, it declares `<w:gridAfter w:val="1"/>` — the row
/// saying outright that its remaining grid column is deliberately empty. A
/// renderer could read that as "this row's last cell *is* its last column", and
/// Word does not: the `lastCol` bit is still absent. So a row kept off the last
/// grid column by `gridAfter` has no `lastCol` cell at all, which is exactly
/// what a rule written on grid columns produces without a `gridAfter` clause
/// anywhere in it.
///
/// # Which band a spanning cell falls in is a choice
///
/// A cell covering several grid columns can be in several vertical bands at
/// once, and neither §17.7.6 nor §17.7.6.5 says which one wins — the spec
/// describes banding over columns and never contemplates a cell that is not
/// one. **The choice taken is the cell's first grid column**, consistent with
/// `firstCol` keying off the same edge, so a cell belongs to the band it
/// starts in.
///
/// The evidence above cannot settle it: `Calendar3`'s `tblLook` sets `noVBand`
/// and its style defines no `band*Vert` layer, so Word had no vertical band to
/// record. **What would settle it**: a Word render (or `w:cnfStyle` capture) of
/// a table with `band1Vert`/`band2Vert` layers, `noVBand` clear, and a cell
/// spanning a band boundary.
fn applicable_regions(
    pos: &CellGridPosition,
    look: Option<&TableLook>,
) -> Vec<TableStyleOverrideType> {
    let CellGridPosition {
        row_idx,
        grid_col,
        grid_span,
        num_rows,
        num_cols,
        row_band_size,
        col_band_size,
    } = *pos;
    // A cell is in the last column region when its span *reaches* the last
    // grid column — nothing else about the cell matters, and in particular not
    // whether it is the last `<w:tc>` in its row.
    //
    // Both directions of that are Word's answer, from `Calendar3` in sample1
    // (see this function's doc): a row falling *short* of the last column,
    // there because of `w:gridSpan="13"` plus `<w:gridAfter w:val="1"/>` in a
    // 14-column grid, has no `lastCol` cell even though its single cell is the
    // last one it has. `>=` rather than `==` handles the other direction, a row
    // addressing *more* grid columns than `tblGrid` declares — real producer
    // output does, and `build_table` clamps it for widths rather than rejecting
    // the table, so this must not silently drop the region for such a cell.
    let reaches_last_col = grid_col + grid_span.max(1) >= num_cols;
    let mut regions = Vec::new();

    // §17.4.55: which regions the table's `tblLook` switches on. The default
    // for anything the file left unstated lives on `ActiveRegions` — there is
    // exactly one of it, and it is *not* "everything on".
    let active = ActiveRegions::resolve(look);
    let first_row_active = active.first_row;
    let last_row_active = active.last_row;
    let first_col_active = active.first_column;
    let last_col_active = active.last_column;
    let h_band_active = active.h_band;
    let v_band_active = active.v_band;

    // §17.7.6 priority 1: vertical banding (banded columns). Pushed *before*
    // horizontal banding so that — per the spec's ascending order (band1Vert,
    // band2Vert, band1Horz, band2Horz) — row banding overrides column banding.
    if v_band_active {
        let band_col = if first_col_active && grid_col > 0 {
            grid_col - 1
        } else {
            grid_col
        };
        let band_size = col_band_size.max(1) as usize;
        let in_first_band = (band_col / band_size).is_multiple_of(2);

        let is_first = first_col_active && grid_col == 0;
        let is_last = last_col_active && reaches_last_col;
        if !is_first && !is_last {
            if in_first_band {
                regions.push(TableStyleOverrideType::Band1Vert);
            } else {
                regions.push(TableStyleOverrideType::Band2Vert);
            }
        }
    }

    // §17.7.6 priority 2: horizontal banding (banded rows) — higher priority
    // than vertical banding.
    if h_band_active {
        // When firstRow is active, banding starts from row 1.
        let band_row = if first_row_active && row_idx > 0 {
            row_idx - 1
        } else {
            row_idx
        };
        let band_size = row_band_size.max(1) as usize;
        let in_first_band = (band_row / band_size).is_multiple_of(2);

        // Don't apply banding to first/last row if those regions are active.
        let is_first = first_row_active && row_idx == 0;
        let is_last = last_row_active && row_idx == num_rows - 1;
        if !is_first && !is_last {
            if in_first_band {
                regions.push(TableStyleOverrideType::Band1Horz);
            } else {
                regions.push(TableStyleOverrideType::Band2Horz);
            }
        }
    }

    // §17.7.6 priority 4: first/last column.
    if first_col_active && grid_col == 0 {
        regions.push(TableStyleOverrideType::FirstCol);
    }
    if last_col_active && reaches_last_col {
        regions.push(TableStyleOverrideType::LastCol);
    }

    // §17.7.6 priority 5: first/last row.
    if first_row_active && row_idx == 0 {
        regions.push(TableStyleOverrideType::FirstRow);
    }
    if last_row_active && row_idx == num_rows - 1 {
        regions.push(TableStyleOverrideType::LastRow);
    }

    // §17.7.6 priority 6: corner cells (highest priority).
    if first_row_active && first_col_active && row_idx == 0 && grid_col == 0 {
        regions.push(TableStyleOverrideType::NwCell);
    }
    if first_row_active && last_col_active && row_idx == 0 && reaches_last_col {
        regions.push(TableStyleOverrideType::NeCell);
    }
    if last_row_active && first_col_active && row_idx == num_rows - 1 && grid_col == 0 {
        regions.push(TableStyleOverrideType::SwCell);
    }
    if last_row_active && last_col_active && row_idx == num_rows - 1 && reaches_last_col {
        regions.push(TableStyleOverrideType::SeCell);
    }

    regions
}

/// Overlay cell properties (higher priority replaces existing values).
fn overlay_cell_properties(result: &mut CellConditionalFormatting, tcp: &TableCellProperties) {
    let target = result
        .cell_properties
        .get_or_insert_with(TableCellProperties::default);

    // §17.7.6: each non-None field from the overlay replaces the target.
    if tcp.shading.cloned().is_some() {
        target.shading = tcp.shading.clone();
    }
    if let Some(src) = tcp.borders.get() {
        // §17.7.6: when a tblStylePr has tcBorders, it REPLACES all cell
        // borders for that region. Sides not mentioned are implicitly nil.
        target.borders = Dup::from(Some(*src));
    }
    if tcp.vertical_align.cloned().is_some() {
        target.vertical_align = tcp.vertical_align.clone();
    }
}

/// Overlay run properties (higher priority replaces existing values).
fn overlay_run_properties(result: &mut CellConditionalFormatting, rp: &RunProperties) {
    let target = result
        .run_properties
        .get_or_insert_with(RunProperties::default);
    // Higher priority: overlay's values replace target's.
    // Use merge in reverse: merge target into a clone of overlay.
    let mut merged = rp.clone();
    crate::render::resolve::properties::merge_run_properties(&mut merged, target);
    *target = merged;
}

/// Overlay paragraph properties (higher priority replaces existing values).
fn overlay_paragraph_properties(result: &mut CellConditionalFormatting, pp: &ParagraphProperties) {
    let target = result
        .paragraph_properties
        .get_or_insert_with(ParagraphProperties::default);
    let mut merged = pp.clone();
    crate::render::resolve::properties::merge_paragraph_properties(&mut merged, target);
    *target = merged;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn make_override(
        override_type: TableStyleOverrideType,
        shading: Option<Shading>,
        bold: Option<bool>,
    ) -> TableStyleOverride {
        TableStyleOverride {
            override_type,
            paragraph_properties: None,
            run_properties: bold.map(|b| RunProperties {
                bold: Some(b),
                ..Default::default()
            }),
            table_properties: None,
            table_row_properties: None,
            table_cell_properties: shading.map(|s| TableCellProperties {
                shading: Dup::from(Some(s)),
                ..Default::default()
            }),
        }
    }

    fn green_shading() -> Shading {
        Shading {
            fill: Color::Rgb(0x9BBB59),
            pattern: ShadingPattern::Clear,
            color: Color::Auto,
        }
    }

    fn blue_shading() -> Shading {
        Shading {
            fill: Color::Rgb(0xD3DFEE),
            pattern: ShadingPattern::Clear,
            color: Color::Auto,
        }
    }

    /// Every region switched on explicitly — the shape a `<w:tblLook>` has once
    /// the parse seam has answered every flag. Tests that are about something
    /// *other* than the `tblLook` default use this so they keep testing what
    /// they say: an absent `tblLook` is Word's 0x04A0, which leaves lastRow,
    /// lastColumn and vertical banding off.
    fn all_regions_on() -> TableLook {
        TableLook {
            first_row: Some(true),
            last_row: Some(true),
            first_column: Some(true),
            last_column: Some(true),
            no_h_band: Some(false),
            no_v_band: Some(false),
        }
    }

    // ── Region detection ─────────────────────────────────────────────

    #[test]
    fn first_row_detected() {
        let regions = applicable_regions(&pos(0, 2), None);
        assert!(regions.contains(&TableStyleOverrideType::FirstRow));
        assert!(!regions.contains(&TableStyleOverrideType::LastRow));
    }

    #[test]
    fn last_row_detected() {
        let look = all_regions_on();
        let regions = applicable_regions(&pos(5, 2), Some(&look));
        assert!(regions.contains(&TableStyleOverrideType::LastRow));
        assert!(!regions.contains(&TableStyleOverrideType::FirstRow));
    }

    #[test]
    fn first_col_detected() {
        let regions = applicable_regions(&pos(2, 0), None);
        assert!(regions.contains(&TableStyleOverrideType::FirstCol));
    }

    #[test]
    fn last_col_detected() {
        let look = all_regions_on();
        let regions = applicable_regions(&pos(2, 5), Some(&look));
        assert!(regions.contains(&TableStyleOverrideType::LastCol));
    }

    #[test]
    fn nw_corner_detected() {
        let regions = applicable_regions(&pos(0, 0), None);
        assert!(regions.contains(&TableStyleOverrideType::NwCell));
        assert!(regions.contains(&TableStyleOverrideType::FirstRow));
        assert!(regions.contains(&TableStyleOverrideType::FirstCol));
    }

    #[test]
    fn se_corner_detected() {
        let look = all_regions_on();
        let regions = applicable_regions(&pos(5, 5), Some(&look));
        assert!(regions.contains(&TableStyleOverrideType::SeCell));
    }

    // ── §17.4.55: an absent `w:tblLook` is Word's 0x04A0 ─────────────
    //
    // [MS-OI29500] Part 1 §17.4.55 note (a): the standard says an omitted
    // `<w:tblLook>` means the bitmask 0x0000 — every region off. Word assumes
    // **0x04A0** instead, and Word's reading is the one documents are authored
    // against. 0x04A0 = firstRow (0x020) + firstColumn (0x080) + noVBand
    // (0x400), so: first row on, first column on, horizontal banding on
    // (noHBand clear), and last row, last column and vertical banding off.

    #[test]
    fn absent_tbl_look_activates_first_row_first_column_and_horizontal_banding() {
        assert!(
            applicable_regions(&pos(0, 2), None).contains(&TableStyleOverrideType::FirstRow),
            "0x04A0 sets firstRow (0x020)"
        );
        assert!(
            applicable_regions(&pos(2, 0), None).contains(&TableStyleOverrideType::FirstCol),
            "0x04A0 sets firstColumn (0x080)"
        );
        assert!(
            applicable_regions(&pos(1, 2), None).contains(&TableStyleOverrideType::Band1Horz),
            "0x04A0 leaves noHBand (0x200) clear, so row banding is active"
        );
    }

    #[test]
    fn absent_tbl_look_leaves_last_row_inactive() {
        let regions = applicable_regions(&pos(5, 2), None);
        assert!(
            !regions.contains(&TableStyleOverrideType::LastRow),
            "0x04A0 leaves lastRow (0x040) clear: {regions:?}"
        );
    }

    #[test]
    fn absent_tbl_look_leaves_last_column_inactive() {
        let regions = applicable_regions(&pos(2, 5), None);
        assert!(
            !regions.contains(&TableStyleOverrideType::LastCol),
            "0x04A0 leaves lastColumn (0x100) clear: {regions:?}"
        );
    }

    #[test]
    fn absent_tbl_look_leaves_vertical_banding_inactive() {
        let regions = applicable_regions(&pos(2, 2), None);
        assert!(
            !regions.contains(&TableStyleOverrideType::Band1Vert)
                && !regions.contains(&TableStyleOverrideType::Band2Vert),
            "0x04A0 sets noVBand (0x400), so column banding is off: {regions:?}"
        );
    }

    /// A corner region is the intersection of two row/column regions, so the
    /// three corners that need lastRow or lastColumn go with them.
    #[test]
    fn absent_tbl_look_leaves_every_corner_but_nw_inactive() {
        assert!(
            applicable_regions(&pos(0, 0), None).contains(&TableStyleOverrideType::NwCell),
            "firstRow x firstColumn are both set"
        );
        for (region, r, c) in [
            (TableStyleOverrideType::NeCell, 0, 5),
            (TableStyleOverrideType::SwCell, 5, 0),
            (TableStyleOverrideType::SeCell, 5, 5),
        ] {
            let regions = applicable_regions(&pos(r, c), None);
            assert!(
                !regions.contains(&region),
                "{region:?} needs lastRow or lastColumn, which 0x04A0 clears: {regions:?}"
            );
        }
    }

    #[test]
    fn interior_cell_gets_banding() {
        let regions = applicable_regions(&pos(1, 1), None);
        // Row 1 with firstRow active: band_row = 0 → band1
        assert!(regions.contains(&TableStyleOverrideType::Band1Horz));
    }

    #[test]
    fn banding_alternates() {
        // Row 2 with firstRow active: band_row = 1 → band2
        let regions = applicable_regions(&pos(2, 1), None);
        assert!(regions.contains(&TableStyleOverrideType::Band2Horz));
    }

    #[test]
    fn no_h_band_disables_banding() {
        let look = TableLook {
            first_row: Some(true),
            last_row: Some(true),
            first_column: Some(true),
            last_column: Some(true),
            no_h_band: Some(true),
            no_v_band: None,
        };
        let regions = applicable_regions(&pos(1, 1), Some(&look));
        assert!(!regions.contains(&TableStyleOverrideType::Band1Horz));
        assert!(!regions.contains(&TableStyleOverrideType::Band2Horz));
    }

    #[test]
    fn first_row_disabled_by_look() {
        let look = TableLook {
            first_row: Some(false),
            last_row: None,
            first_column: None,
            last_column: None,
            no_h_band: None,
            no_v_band: None,
        };
        let regions = applicable_regions(&pos(0, 2), Some(&look));
        assert!(!regions.contains(&TableStyleOverrideType::FirstRow));
    }

    #[test]
    fn band_size_2() {
        // Row 1 with firstRow: band_row=0, band_size=2 → 0/2=0 → band1
        let r1 = applicable_regions(&banded(1, 1, 10, 6, 2, 1), None);
        assert!(r1.contains(&TableStyleOverrideType::Band1Horz));

        // Row 2: band_row=1, 1/2=0 → band1
        let r2 = applicable_regions(&banded(2, 1, 10, 6, 2, 1), None);
        assert!(r2.contains(&TableStyleOverrideType::Band1Horz));

        // Row 3: band_row=2, 2/2=1 → band2
        let r3 = applicable_regions(&banded(3, 1, 10, 6, 2, 1), None);
        assert!(r3.contains(&TableStyleOverrideType::Band2Horz));
    }

    // ── Priority overlay ─────────────────────────────────────────────

    #[test]
    fn first_row_shading_applied() {
        let overrides = vec![make_override(
            TableStyleOverrideType::FirstRow,
            Some(green_shading()),
            Some(true),
        )];
        let result = resolve_cell_conditional(&pos(0, 2), None, &overrides);
        assert!(result.cell_properties.is_some());
        assert!(result
            .cell_properties
            .as_ref()
            .unwrap()
            .shading
            .cloned()
            .is_some());
        assert!(result.run_properties.as_ref().unwrap().bold == Some(true));
    }

    #[test]
    fn banding_shading_for_interior() {
        let overrides = vec![make_override(
            TableStyleOverrideType::Band1Horz,
            Some(blue_shading()),
            None,
        )];
        let result = resolve_cell_conditional(&pos(1, 2), None, &overrides);
        let shading = result
            .cell_properties
            .as_ref()
            .unwrap()
            .shading
            .get()
            .unwrap();
        assert_eq!(shading.fill, Color::Rgb(0xD3DFEE));
    }

    #[test]
    fn corner_overrides_first_row() {
        let overrides = vec![
            make_override(
                TableStyleOverrideType::FirstRow,
                Some(green_shading()),
                None,
            ),
            make_override(TableStyleOverrideType::NwCell, Some(blue_shading()), None),
        ];
        let result = resolve_cell_conditional(&pos(0, 0), None, &overrides);
        // NW corner has higher priority than FirstRow.
        let shading = result
            .cell_properties
            .as_ref()
            .unwrap()
            .shading
            .get()
            .unwrap();
        assert_eq!(shading.fill, Color::Rgb(0xD3DFEE));
    }

    #[test]
    fn no_overrides_returns_empty() {
        let result = resolve_cell_conditional(&pos(2, 2), None, &[]);
        assert!(result.cell_properties.is_none());
        assert!(result.run_properties.is_none());
    }

    #[test]
    fn horizontal_banding_overrides_vertical() {
        // §17.7.6: band1Horz is applied after band1Vert, so on a cell that is in
        // both an odd row-band and an odd column-band the *row* banding wins.
        let overrides = vec![
            make_override(
                TableStyleOverrideType::Band1Vert,
                Some(blue_shading()),
                None,
            ),
            make_override(
                TableStyleOverrideType::Band1Horz,
                Some(green_shading()),
                None,
            ),
        ];
        // Interior cell (1,1): band_row=0 → Band1Horz, band_col=0 → Band1Vert.
        // Vertical banding has to be switched on explicitly — an absent
        // `tblLook` is 0x04A0, which sets noVBand.
        let look = all_regions_on();
        let result = resolve_cell_conditional(&pos(1, 1), Some(&look), &overrides);
        let shading = result.cell_properties.unwrap().shading.cloned().unwrap();
        assert_eq!(
            shading.fill,
            Color::Rgb(0x9BBB59),
            "row banding (green) must override column banding (blue)"
        );
    }

    // ── wholeTable base layer (§17.7.6, backlog Unit 5) ──────────────

    /// A one-grid-column cell in a 6×6 grid with band sizes of 1.
    fn pos(row_idx: usize, grid_col: usize) -> CellGridPosition {
        CellGridPosition {
            row_idx,
            grid_col,
            grid_span: 1,
            num_rows: 6,
            num_cols: 6,
            row_band_size: 1,
            col_band_size: 1,
        }
    }

    /// The same, over a grid of a stated size and band sizes.
    fn banded(
        row_idx: usize,
        grid_col: usize,
        num_rows: usize,
        num_cols: usize,
        row_band_size: u32,
        col_band_size: u32,
    ) -> CellGridPosition {
        CellGridPosition {
            row_idx,
            grid_col,
            grid_span: 1,
            num_rows,
            num_cols,
            row_band_size,
            col_band_size,
        }
    }

    fn red_shading() -> Shading {
        Shading {
            fill: Color::Rgb(0xFF0000),
            pattern: ShadingPattern::Clear,
            color: Color::Auto,
        }
    }

    /// `wholeTable` applies to every cell — including plain interior cells that
    /// match no positional region at all, which used to resolve to nothing.
    #[test]
    fn whole_table_applies_to_an_interior_cell() {
        let overrides = vec![make_override(
            TableStyleOverrideType::WholeTable,
            Some(red_shading()),
            Some(true),
        )];
        // (2,2) with band sizes of 1 lands in a band, but a table whose style
        // declares no banding override still gets the base layer.
        let result = resolve_cell_conditional(&pos(2, 2), None, &overrides);
        assert_eq!(
            result
                .cell_properties
                .unwrap()
                .shading
                .cloned()
                .unwrap()
                .fill,
            Color::Rgb(0xFF0000)
        );
        assert_eq!(result.run_properties.unwrap().bold, Some(true));
    }

    /// …and to the corners, where the highest-priority region also applies.
    #[test]
    fn whole_table_applies_to_every_position() {
        let overrides = vec![make_override(
            TableStyleOverrideType::WholeTable,
            Some(red_shading()),
            None,
        )];
        for (r, c) in [(0, 0), (0, 5), (5, 0), (5, 5), (0, 3), (3, 0), (3, 3)] {
            let result = resolve_cell_conditional(&pos(r, c), None, &overrides);
            assert_eq!(
                result
                    .cell_properties
                    .and_then(|p| p.shading.cloned())
                    .map(|s| s.fill),
                Some(Color::Rgb(0xFF0000)),
                "cell ({r},{c}) must receive the wholeTable base layer"
            );
        }
    }

    /// It is the *base*: every positional region outranks it.
    #[test]
    fn positional_regions_override_whole_table() {
        for (region, r, c) in [
            (TableStyleOverrideType::Band1Horz, 1, 2),
            (TableStyleOverrideType::FirstRow, 0, 2),
            (TableStyleOverrideType::FirstCol, 2, 0),
            (TableStyleOverrideType::NwCell, 0, 0),
        ] {
            let overrides = vec![
                make_override(
                    TableStyleOverrideType::WholeTable,
                    Some(red_shading()),
                    None,
                ),
                make_override(region, Some(green_shading()), None),
            ];
            let result = resolve_cell_conditional(&pos(r, c), None, &overrides);
            assert_eq!(
                result
                    .cell_properties
                    .unwrap()
                    .shading
                    .cloned()
                    .unwrap()
                    .fill,
                Color::Rgb(0x9BBB59),
                "{region:?} must outrank wholeTable"
            );
        }
    }

    /// A positional override that sets only *some* properties leaves the rest
    /// of the base layer showing through — that is what makes it a layer and
    /// not a replacement.
    #[test]
    fn whole_table_shows_through_a_partial_positional_override() {
        let overrides = vec![
            make_override(
                TableStyleOverrideType::WholeTable,
                Some(red_shading()),
                Some(true),
            ),
            // firstRow sets bold=false but no shading.
            make_override(TableStyleOverrideType::FirstRow, None, Some(false)),
        ];
        let result = resolve_cell_conditional(&pos(0, 2), None, &overrides);
        assert_eq!(
            result
                .cell_properties
                .unwrap()
                .shading
                .cloned()
                .unwrap()
                .fill,
            Color::Rgb(0xFF0000),
            "shading falls through from wholeTable"
        );
        assert_eq!(
            result.run_properties.unwrap().bold,
            Some(false),
            "but firstRow's own bold wins"
        );
    }

    /// D1 fixed horizontal-over-vertical banding precedence (§17.7.6). Adding a
    /// base layer beneath both must not disturb it.
    #[test]
    fn whole_table_does_not_disturb_banding_precedence() {
        let overrides = vec![
            make_override(
                TableStyleOverrideType::WholeTable,
                Some(red_shading()),
                None,
            ),
            make_override(
                TableStyleOverrideType::Band1Vert,
                Some(blue_shading()),
                None,
            ),
            make_override(
                TableStyleOverrideType::Band1Horz,
                Some(green_shading()),
                None,
            ),
        ];
        // Vertical banding is only active when the `tblLook` says so — 0x04A0,
        // the absent-element default, sets noVBand.
        let look = all_regions_on();
        let result = resolve_cell_conditional(&pos(1, 1), Some(&look), &overrides);
        assert_eq!(
            result
                .cell_properties
                .unwrap()
                .shading
                .cloned()
                .unwrap()
                .fill,
            Color::Rgb(0x9BBB59),
            "row banding must still override column banding, and both the base"
        );
    }

    // ── Columns are grid columns: Word's own answer ──────────────────

    /// The oracle test. Word writes `w:cnfStyle` (§17.3.1.8) onto the cells it
    /// applied a conditional region to, so a Word-authored table is Word's
    /// answer to "which region is this cell in?" written into the file. No
    /// reference render is needed and no fixture has to be built: the
    /// comparison is against a document Word itself produced.
    ///
    /// The table is `Calendar3` in `test-files/sample-docx-files-sample1.docx`
    /// — the corpus's only table combining `w:gridSpan` with a style defining
    /// column regions, which is the combination that discriminates. Its
    /// `<w:tblLook w:val="05A0"/>` has lastColumn **on** and the style defines
    /// a `lastCol` layer, so a cell carrying no `lastCol` bit is Word saying
    /// "not in that region" rather than "that region is switched off".
    ///
    /// Only the column bits are compared. Word puts the row regions on
    /// `trPr/cnfStyle` rather than on the cell, and `Calendar3` defines no
    /// `band*Vert`/`band*Horz` layer, so it recorded no band bits for those to
    /// be compared against — asserting on bits Word had no reason to write
    /// would be reading silence as data.
    ///
    /// The two shapes that make the fixture an oracle are asserted rather than
    /// assumed, so a fixture that ever changed could not quietly turn this into
    /// a test of nothing: row 0 must combine `gridSpan` with `gridAfter` (the
    /// §17.4.14 half — a row deliberately short of the last grid column), and
    /// some later row must reach the last column with a span.
    #[test]
    fn word_cnf_style_agrees_with_the_grid_column_reading() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test-files/sample-docx-files-sample1.docx");
        let bytes = std::fs::read(&path).expect("corpus fixture");
        let doc = crate::docx::parse(&bytes).expect("parse");

        let table = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .find(|t| {
                t.properties
                    .style_id
                    .as_ref()
                    .is_some_and(|s| s.as_str() == "Calendar3")
            })
            .expect("sample1 carries the Calendar3 table");

        let look = table.properties.look.get();
        assert_eq!(
            look.and_then(|l| l.last_column),
            Some(true),
            "the fixture only discriminates while lastColumn is an active region"
        );
        let num_cols = table.grid.len();
        assert_eq!(num_cols, 14, "the fixture's grid, as Word wrote it");

        // §17.4.14: the row Word answered the `gridAfter` question on. Its one
        // cell spans 13 of 14 columns and the row declares the fourteenth
        // deliberately empty, so "the last cell in the row" and "the cell
        // reaching the last grid column" name different cells here.
        let row0 = &table.rows[0];
        assert_eq!(row0.properties.grid_after, 1, "the fixture's gridAfter row");
        assert_eq!(row0.cells.len(), 1);
        assert_eq!(row0.cells[0].properties.grid_span.cloned(), Some(13));
        assert!(
            !row0.cells[0]
                .properties
                .cnf_style
                .cloned()
                .unwrap_or_default()
                .contains(CnfStyle::LAST_COLUMN),
            "Word puts no lastCol on a row held off the last column by gridAfter"
        );

        let mut spanning_cells = 0;
        let mut reaching_cells = 0;
        for (row_idx, row) in table.rows.iter().enumerate() {
            // The same walk `build_table` does: gridBefore, then accumulate
            // each preceding cell's gridSpan — `max(1)` included, since a cell
            // covers at least one column (`build::table::grid_span`).
            let mut grid_col = row.properties.grid_before as usize;
            for cell in &row.cells {
                let grid_span = cell.properties.grid_span.cloned().unwrap_or(1).max(1) as usize;
                if grid_span > 1 {
                    spanning_cells += 1;
                    if grid_col + grid_span >= num_cols {
                        reaching_cells += 1;
                    }
                }
                let regions = applicable_regions(
                    &CellGridPosition {
                        row_idx,
                        grid_col,
                        grid_span,
                        num_rows: table.rows.len(),
                        num_cols,
                        row_band_size: 1,
                        col_band_size: 1,
                    },
                    look,
                );
                // Word omits `cnfStyle` entirely on a cell in no region, so an
                // absent one reads as "no bits", not as "unknown".
                let word = cell.properties.cnf_style.cloned().unwrap_or_default();
                let at = format!("row {row_idx}, grid col {grid_col}, span {grid_span}");
                assert_eq!(
                    regions.contains(&TableStyleOverrideType::FirstCol),
                    word.contains(CnfStyle::FIRST_COLUMN),
                    "firstCol disagrees with Word at {at}"
                );
                assert_eq!(
                    regions.contains(&TableStyleOverrideType::LastCol),
                    word.contains(CnfStyle::LAST_COLUMN),
                    "lastCol disagrees with Word at {at}"
                );
                grid_col += grid_span;
            }
        }
        assert!(
            spanning_cells >= 2,
            "the comparison is only worth making over spanning cells; found {spanning_cells}"
        );
        assert!(
            reaching_cells >= 1,
            "and only discriminating while some span *reaches* the last grid \
             column, so both answers are present; found {reaching_cells}"
        );
    }
}
