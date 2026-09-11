//! The OpenType `STAT` table — style attributes, the bridge from axis values
//! back to the words a person types.
//!
//! [`fvar`](super::fvar) says a face can sit at `wght = 600`; `STAT` says that
//! `wght = 600` is spelled `"SemiBold"`. That mapping is what lets a request for
//! `"Fixture Sans Condensed SemiBold"` be *read* rather than parsed: the
//! catalogue composes the family with one axis-value name per axis and matches
//! the result, instead of chopping words off the end of the string and hoping.
//!
//! It also covers combinations `fvar` does not name. A font may declare nine
//! weights and three widths in `STAT` while naming only nine instances; the
//! twenty-seven combinations are all legitimate faces, and `STAT` is the only
//! place that says what to call them.
//!
//! # Layout
//!
//! ```text
//! majorVersion u16  minorVersion u16
//! designAxisSize u16  designAxisCount u16  designAxesOffset Offset32
//! axisValueCount u16  offsetToAxisValueOffsets Offset32
//! (1.1+) elidedFallbackNameID u16
//! ... designAxes:   [ axisTag Tag  axisNameID u16  axisOrdering u16 ] × designAxisCount
//! ... axisValues:   Offset16 × axisValueCount, each to a format 1–4 record
//! ```
//!
//! The four axis-value formats differ in what they can express — a single value,
//! a range, a value with a "linked" bold counterpart, or one name covering
//! several axes at once. They are normalized here into [`AxisValue`], because
//! every consumer in this engine wants the same thing from all four: *which
//! coordinates does this name stand for*.

use super::fvar::{AxisTag, VariationCoord};
use super::{MalformedTable, Reader};

const TABLE: &str = "STAT";
const HEADER_SIZE: usize = 20;
const MIN_DESIGN_AXIS_SIZE: usize = 8;

/// `OLDER_SIBLING_FONT_ATTRIBUTE` — the value describes a face kept only for
/// backwards compatibility and should not be offered as a style.
const AXIS_VALUE_FLAG_OLDER_SIBLING: u16 = 0x0001;
/// `ELIDABLE_AXIS_VALUE_NAME` — the name is omitted when composing a face name.
/// This is how `"Regular"` and `"Normal"` disappear: a font marks the default
/// weight and width elidable so the composed name is `"Fixture Sans"` rather
/// than `"Fixture Sans Regular Normal"`.
const AXIS_VALUE_FLAG_ELIDABLE: u16 = 0x0002;

/// One design axis as `STAT` declares it. Distinct from an
/// [`fvar` axis](super::fvar::Axis): a static font can carry `STAT` with no
/// `fvar` at all, which is how a family of static faces declares that it is one
/// family spanning several weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesignAxis {
    pub tag: AxisTag,
    pub name_id: u16,
    /// Sort order for composing a name from several axis values. Lower first —
    /// this is what puts `"Condensed"` before `"Bold"` rather than after.
    pub ordering: u16,
}

/// One named point (or region) in the design space, normalized across the four
/// on-disk formats.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisValue {
    /// Name id of this value's display name (`"SemiBold"`, `"Condensed"`).
    pub name_id: u16,
    /// The coordinates this name stands for. One entry for formats 1–3, several
    /// for format 4.
    pub coords: Vec<VariationCoord>,
    /// The name is omitted when composing a face name — the font's way of
    /// saying "this is the unmarked default".
    pub elidable: bool,
    /// The value exists for backwards compatibility and should not be offered.
    pub older_sibling: bool,
}

/// A font's style-attribute declarations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatAxisValues {
    pub axes: Vec<DesignAxis>,
    pub values: Vec<AxisValue>,
    /// Name id used when *every* axis value is elided — the name of the plain
    /// face, usually `"Regular"`. Absent in version 1.0.
    pub elided_fallback_name_id: Option<u16>,
}

impl StatAxisValues {
    pub fn is_empty(&self) -> bool {
        self.axes.is_empty() && self.values.is_empty()
    }

    /// Axis values that should take part in composing a face name, in the
    /// font's declared axis order.
    ///
    /// Filters out both flags the spec defines for the purpose: an elided value
    /// contributes nothing to the name, and an older-sibling value names a face
    /// that exists only so old documents keep working.
    pub fn nameable(&self) -> impl Iterator<Item = &AxisValue> {
        self.values
            .iter()
            .filter(|v| !v.elidable && !v.older_sibling)
    }

    /// Sort key for an axis value, so composed names order axes the way the
    /// font says rather than the order the table happens to list them.
    ///
    /// Values naming several axes at once (format 4) take the lowest ordering
    /// among them, which puts a combined name where its most significant axis
    /// would have gone.
    pub fn ordering_of(&self, value: &AxisValue) -> u16 {
        value
            .coords
            .iter()
            .filter_map(|c| self.axes.iter().find(|a| a.tag == c.axis))
            .map(|a| a.ordering)
            .min()
            .unwrap_or(u16::MAX)
    }
}

pub fn read(bytes: &[u8]) -> Result<StatAxisValues, MalformedTable> {
    let mut r = Reader::new(bytes, TABLE);
    // Version 1.0 stops two bytes short of the 1.1 header. Require only what
    // 1.0 defines, then read the extra field conditionally.
    r.require(HEADER_SIZE - 2)?;

    let major = r.u16("majorVersion")?;
    let minor = r.u16("minorVersion")?;
    if major != 1 {
        return Err(MalformedTable::UnsupportedVersion {
            table: TABLE,
            field: "majorVersion",
            value: u32::from(major),
        });
    }

    let design_axis_size = r.u16("designAxisSize")? as usize;
    let design_axis_count = r.u16("designAxisCount")? as usize;
    let design_axes_offset = r.u32("designAxesOffset")? as usize;
    let axis_value_count = r.u16("axisValueCount")? as usize;
    let axis_values_offset = r.u32("offsetToAxisValueOffsets")? as usize;
    let elided_fallback_name_id = if minor >= 1 {
        Some(r.u16("elidedFallbackNameID")?)
    } else {
        None
    };

    if design_axis_count > 0 && design_axis_size < MIN_DESIGN_AXIS_SIZE {
        return Err(MalformedTable::UnsupportedVersion {
            table: TABLE,
            field: "designAxisSize",
            value: design_axis_size as u32,
        });
    }

    let mut axes = Vec::with_capacity(design_axis_count);
    for i in 0..design_axis_count {
        let rec = design_axes_offset + i * design_axis_size;
        axes.push(DesignAxis {
            tag: AxisTag(r.tag_at(rec, "axisTag")?),
            name_id: r.u16_at(rec + 4, "axisNameID")?,
            ordering: r.u16_at(rec + 6, "axisOrdering")?,
        });
    }

    let mut values = Vec::with_capacity(axis_value_count);
    for i in 0..axis_value_count {
        let offset = r.u16_at(axis_values_offset + i * 2, "axisValueOffset")? as usize;
        let Some(rec) = axis_values_offset.checked_add(offset) else {
            continue;
        };
        // A single unreadable axis value costs its name, not the whole table —
        // the other values still compose usable aliases.
        if let Some(value) = read_axis_value(&r, rec, &axes)? {
            values.push(value);
        }
    }

    Ok(StatAxisValues {
        axes,
        values,
        elided_fallback_name_id,
    })
}

/// Normalize one axis-value record. `Ok(None)` for a format this reader does
/// not know — a future format 5 is a gap here, not a broken font, and the rest
/// of the table stays usable.
fn read_axis_value(
    r: &Reader<'_>,
    rec: usize,
    axes: &[DesignAxis],
) -> Result<Option<AxisValue>, MalformedTable> {
    let format = r.u16_at(rec, "axisValue format")?;

    // Formats 1–3 share their first four fields; only format 4 differs.
    let axis_tag_at =
        |index: u16| -> Option<AxisTag> { axes.get(usize::from(index)).map(|a| a.tag) };

    let (name_id, flags, coords) = match format {
        // Format 1: one axis, one value.
        // Format 3: same, plus a `linkedValue` naming the bold counterpart.
        // The linked value is not a face of its own — it tells a style-linking
        // UI which value `<w:b/>` should jump to — so it is read past, not kept.
        1 | 3 => {
            let axis_index = r.u16_at(rec + 2, "axisIndex")?;
            let flags = r.u16_at(rec + 4, "flags")?;
            let name_id = r.u16_at(rec + 6, "valueNameID")?;
            let value = r.fixed_at(rec + 8, "value")?;
            let Some(axis) = axis_tag_at(axis_index) else {
                return Ok(None);
            };
            (name_id, flags, vec![VariationCoord { axis, value }])
        }
        // Format 2: one axis, a range with a nominal value. The nominal value
        // is the one a face sits at; the range is for matching a user's
        // arbitrary position back to this name.
        2 => {
            let axis_index = r.u16_at(rec + 2, "axisIndex")?;
            let flags = r.u16_at(rec + 4, "flags")?;
            let name_id = r.u16_at(rec + 6, "valueNameID")?;
            let nominal = r.fixed_at(rec + 8, "nominalValue")?;
            let Some(axis) = axis_tag_at(axis_index) else {
                return Ok(None);
            };
            (
                name_id,
                flags,
                vec![VariationCoord {
                    axis,
                    value: nominal,
                }],
            )
        }
        // Format 4: one name covering a point across several axes, e.g. a
        // single "Display" naming a specific optical-size and weight pairing.
        4 => {
            let axis_count = r.u16_at(rec + 2, "axisCount")? as usize;
            let flags = r.u16_at(rec + 4, "flags")?;
            let name_id = r.u16_at(rec + 6, "valueNameID")?;
            let mut coords = Vec::with_capacity(axis_count);
            for i in 0..axis_count {
                let entry = rec + 8 + i * 6;
                let axis_index = r.u16_at(entry, "axisIndex")?;
                let value = r.fixed_at(entry + 2, "value")?;
                if let Some(axis) = axis_tag_at(axis_index) {
                    coords.push(VariationCoord { axis, value });
                }
            }
            if coords.is_empty() {
                return Ok(None);
            }
            (name_id, flags, coords)
        }
        _ => return Ok(None),
    };

    Ok(Some(AxisValue {
        name_id,
        coords,
        elidable: flags & AXIS_VALUE_FLAG_ELIDABLE != 0,
        older_sibling: flags & AXIS_VALUE_FLAG_OLDER_SIBLING != 0,
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn fixed(v: f32) -> [u8; 4] {
        ((v * 65536.0) as i32).to_be_bytes()
    }

    /// One axis-value record, in whichever format the test needs.
    pub(crate) enum Value {
        /// format 1 — `(axisIndex, flags, nameID, value)`
        Single(u16, u16, u16, f32),
        /// format 2 — `(axisIndex, flags, nameID, nominal, min, max)`
        Range(u16, u16, u16, f32, f32, f32),
        /// format 3 — `(axisIndex, flags, nameID, value, linkedValue)`
        Linked(u16, u16, u16, f32, f32),
        /// format 4 — `(flags, nameID, [(axisIndex, value)])`
        Multi(u16, u16, Vec<(u16, f32)>),
        /// A format this reader does not know, to prove it is skipped.
        UnknownFormat(u16),
    }

    impl Value {
        fn encode(&self) -> Vec<u8> {
            let mut out = Vec::new();
            match self {
                Value::Single(axis, flags, name, value) => {
                    for v in [1u16, *axis, *flags, *name] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                    out.extend_from_slice(&fixed(*value));
                }
                Value::Range(axis, flags, name, nominal, min, max) => {
                    for v in [2u16, *axis, *flags, *name] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                    out.extend_from_slice(&fixed(*nominal));
                    out.extend_from_slice(&fixed(*min));
                    out.extend_from_slice(&fixed(*max));
                }
                Value::Linked(axis, flags, name, value, linked) => {
                    for v in [3u16, *axis, *flags, *name] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                    out.extend_from_slice(&fixed(*value));
                    out.extend_from_slice(&fixed(*linked));
                }
                Value::Multi(flags, name, entries) => {
                    for v in [4u16, entries.len() as u16, *flags, *name] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                    for (axis, value) in entries {
                        out.extend_from_slice(&axis.to_be_bytes());
                        out.extend_from_slice(&fixed(*value));
                    }
                }
                Value::UnknownFormat(format) => {
                    for v in [*format, 0u16, 0u16, 0u16] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                    out.extend_from_slice(&fixed(0.0));
                }
            }
            out
        }
    }

    /// Build a `STAT` table. `axes` are `(tag, nameID, ordering)`.
    pub(crate) fn build_stat(
        minor: u16,
        axes: &[(&[u8; 4], u16, u16)],
        values: &[Value],
        elided_fallback: u16,
    ) -> Vec<u8> {
        let header = if minor >= 1 {
            HEADER_SIZE
        } else {
            HEADER_SIZE - 2
        };
        let design_axes_offset = header;
        let axis_values_offset = design_axes_offset + axes.len() * MIN_DESIGN_AXIS_SIZE;
        let records_start = axis_values_offset + values.len() * 2;

        let mut records = Vec::new();
        let mut offsets = Vec::new();
        for value in values {
            offsets.push(records_start - axis_values_offset + records.len());
            records.extend_from_slice(&value.encode());
        }

        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        out.extend_from_slice(&minor.to_be_bytes());
        out.extend_from_slice(&(MIN_DESIGN_AXIS_SIZE as u16).to_be_bytes());
        out.extend_from_slice(&(axes.len() as u16).to_be_bytes());
        out.extend_from_slice(&(design_axes_offset as u32).to_be_bytes());
        out.extend_from_slice(&(values.len() as u16).to_be_bytes());
        out.extend_from_slice(&(axis_values_offset as u32).to_be_bytes());
        if minor >= 1 {
            out.extend_from_slice(&elided_fallback.to_be_bytes());
        }
        for (tag, name_id, ordering) in axes {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&name_id.to_be_bytes());
            out.extend_from_slice(&ordering.to_be_bytes());
        }
        for offset in &offsets {
            out.extend_from_slice(&(*offset as u16).to_be_bytes());
        }
        out.extend_from_slice(&records);
        out
    }

    /// A weight + width fixture in the shape a real family declares.
    pub(crate) fn weight_width_stat() -> Vec<u8> {
        build_stat(
            1,
            &[(b"wdth", 256, 0), (b"wght", 257, 1)],
            &[
                // wdth: Normal (elidable), Condensed
                Value::Single(0, AXIS_VALUE_FLAG_ELIDABLE, 300, 100.0),
                Value::Single(0, 0, 301, 75.0),
                // wght: Regular (elidable, style-linked to Bold), SemiBold, Bold
                Value::Linked(1, AXIS_VALUE_FLAG_ELIDABLE, 302, 400.0, 700.0),
                Value::Single(1, 0, 303, 600.0),
                Value::Single(1, 0, 304, 700.0),
            ],
            302,
        )
    }

    #[test]
    fn reads_design_axes_with_their_ordering() {
        let stat = read(&weight_width_stat()).unwrap();
        assert_eq!(stat.axes.len(), 2);
        assert_eq!(stat.axes[0].tag, AxisTag::WIDTH);
        assert_eq!(stat.axes[0].ordering, 0);
        assert_eq!(stat.axes[1].tag, AxisTag::WEIGHT);
        assert_eq!(stat.axes[1].ordering, 1);
        assert_eq!(stat.elided_fallback_name_id, Some(302));
    }

    #[test]
    fn normalizes_all_four_formats_to_coordinates() {
        let table = build_stat(
            1,
            &[(b"wght", 256, 0), (b"opsz", 257, 1)],
            &[
                Value::Single(0, 0, 300, 600.0),
                Value::Range(0, 0, 301, 700.0, 650.0, 750.0),
                Value::Linked(0, 0, 302, 400.0, 700.0),
                Value::Multi(0, 303, vec![(0, 500.0), (1, 14.0)]),
            ],
            300,
        );
        let stat = read(&table).unwrap();
        assert_eq!(stat.values.len(), 4);

        assert_eq!(stat.values[0].coords[0].value, 600.0, "format 1");
        assert_eq!(
            stat.values[1].coords[0].value, 700.0,
            "format 2 keeps the nominal value, not the range bounds"
        );
        assert_eq!(
            stat.values[2].coords,
            vec![VariationCoord {
                axis: AxisTag::WEIGHT,
                value: 400.0
            }],
            "format 3's linkedValue is not a face of its own"
        );
        assert_eq!(stat.values[3].coords.len(), 2, "format 4 spans two axes");
        assert_eq!(stat.values[3].coords[1].axis, AxisTag::OPTICAL_SIZE);
    }

    /// Elided names are how `"Regular"` and `"Normal"` stay out of a composed
    /// face name. Without honouring the flag every face would be called
    /// `"Family Regular Normal"`.
    #[test]
    fn elidable_and_older_sibling_values_are_excluded_from_naming() {
        let stat = read(&weight_width_stat()).unwrap();
        let nameable: Vec<u16> = stat.nameable().map(|v| v.name_id).collect();
        assert_eq!(
            nameable,
            vec![301, 303, 304],
            "Normal (300) and Regular (302) are elided"
        );

        let table = build_stat(
            1,
            &[(b"wght", 256, 0)],
            &[Value::Single(0, AXIS_VALUE_FLAG_OLDER_SIBLING, 300, 400.0)],
            300,
        );
        let stat = read(&table).unwrap();
        assert!(stat.values[0].older_sibling);
        assert_eq!(stat.nameable().count(), 0);
    }

    /// Composing `"Family Condensed SemiBold"` rather than
    /// `"Family SemiBold Condensed"` depends on the font's declared ordering,
    /// not on table order.
    #[test]
    fn axis_ordering_comes_from_the_font_not_the_table_order() {
        let stat = read(&weight_width_stat()).unwrap();
        let condensed = stat.values.iter().find(|v| v.name_id == 301).unwrap();
        let semibold = stat.values.iter().find(|v| v.name_id == 303).unwrap();
        assert!(
            stat.ordering_of(condensed) < stat.ordering_of(semibold),
            "wdth is declared before wght"
        );
    }

    /// A format-4 value takes its most significant axis's ordering, so a
    /// combined name lands where that axis would have put it.
    #[test]
    fn a_multi_axis_value_orders_by_its_lowest_axis() {
        let table = build_stat(
            1,
            &[(b"wdth", 256, 0), (b"wght", 257, 5)],
            &[Value::Multi(0, 300, vec![(1, 700.0), (0, 75.0)])],
            300,
        );
        let stat = read(&table).unwrap();
        assert_eq!(stat.ordering_of(&stat.values[0]), 0);
    }

    /// Version 1.0 has no `elidedFallbackNameID`; reading two bytes that are
    /// not there would misreport the first design axis's tag.
    #[test]
    fn version_1_0_omits_the_elided_fallback_field() {
        let table = build_stat(
            0,
            &[(b"wght", 256, 0)],
            &[Value::Single(0, 0, 300, 600.0)],
            0,
        );
        let stat = read(&table).unwrap();
        assert_eq!(stat.elided_fallback_name_id, None);
        assert_eq!(
            stat.axes[0].tag,
            AxisTag::WEIGHT,
            "axes still read correctly"
        );
        assert_eq!(stat.values[0].name_id, 300);
    }

    /// A static font can carry `STAT` with no `fvar` — that is how a family of
    /// separate files declares itself one family.
    #[test]
    fn a_table_with_axes_but_no_values_is_valid() {
        let table = build_stat(1, &[(b"wght", 256, 0)], &[], 300);
        let stat = read(&table).unwrap();
        assert_eq!(stat.axes.len(), 1);
        assert!(stat.values.is_empty());
        assert!(!stat.is_empty());
    }

    // ── forward compatibility and malformed input ────────────────────────

    /// An axis-value format from a future spec revision is skipped, leaving the
    /// rest usable. This is a gap in this reader, not a broken font.
    #[test]
    fn an_unknown_axis_value_format_is_skipped_not_fatal() {
        let table = build_stat(
            1,
            &[(b"wght", 256, 0)],
            &[Value::UnknownFormat(9), Value::Single(0, 0, 300, 600.0)],
            300,
        );
        let stat = read(&table).expect("an unknown format must not fail the table");
        assert_eq!(stat.values.len(), 1);
        assert_eq!(stat.values[0].name_id, 300);
    }

    /// An axis index pointing past the declared axes names nothing.
    #[test]
    fn an_axis_value_referencing_a_missing_axis_is_skipped() {
        let table = build_stat(
            1,
            &[(b"wght", 256, 0)],
            &[Value::Single(7, 0, 300, 600.0)],
            300,
        );
        let stat = read(&table).unwrap();
        assert!(stat.values.is_empty());
    }

    #[test]
    fn a_table_shorter_than_its_header_is_an_error() {
        assert_eq!(
            read(&[0u8; 10]).unwrap_err(),
            MalformedTable::TooShort {
                table: "STAT",
                needed: 18,
                actual: 10
            }
        );
    }

    #[test]
    fn an_unknown_major_version_is_reported_as_such() {
        let mut table = weight_width_stat();
        table[0..2].copy_from_slice(&3u16.to_be_bytes());
        assert_eq!(
            read(&table).unwrap_err(),
            MalformedTable::UnsupportedVersion {
                table: "STAT",
                field: "majorVersion",
                value: 3
            }
        );
    }

    #[test]
    fn a_design_axis_array_running_past_the_table_is_an_error() {
        let mut table = weight_width_stat();
        table[6..8].copy_from_slice(&900u16.to_be_bytes()); // designAxisCount
        assert!(matches!(
            read(&table).unwrap_err(),
            MalformedTable::OutOfBounds { table: "STAT", .. }
        ));
    }

    #[test]
    fn an_undersized_design_axis_record_is_refused() {
        let mut table = weight_width_stat();
        table[4..6].copy_from_slice(&4u16.to_be_bytes()); // designAxisSize
        assert!(matches!(
            read(&table).unwrap_err(),
            MalformedTable::UnsupportedVersion {
                table: "STAT",
                field: "designAxisSize",
                ..
            }
        ));
    }
}
