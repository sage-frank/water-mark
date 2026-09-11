//! The OpenType `fvar` table — a variable font's axes and its named instances.
//!
//! A variable font is one face that can be *positioned* anywhere in a design
//! space. `fvar` describes that space: which axes exist (`wght`, `wdth`, `opsz`,
//! …), what each ranges over, and which points in it the designer considered
//! worth naming.
//!
//! Named instances are what make a variable font reachable from a DOCX. A
//! document asking for `"Source Sans 3 SemiBold"` on a host that installs Source
//! Sans as a single variable file has no static face to match — but it does have
//! an instance named `SemiBold` at `wght = 600`, and
//! [`Typeface::clone_with_arguments`] can produce exactly that face.
//!
//! [`Typeface::clone_with_arguments`]: skia_safe::Typeface::clone_with_arguments
//!
//! # Layout
//!
//! ```text
//! majorVersion u16  minorVersion u16  axesArrayOffset u16  (reserved) u16
//! axisCount u16  axisSize u16  instanceCount u16  instanceSize u16
//! ... at axesArrayOffset: [ axisTag Tag  minValue Fixed  defaultValue Fixed
//!                           maxValue Fixed  flags u16  axisNameID u16 ] × axisCount
//! ... then:          [ subfamilyNameID u16  flags u16  coordinates Fixed × axisCount
//!                      (postScriptNameID u16, optional) ] × instanceCount
//! ```
//!
//! `axisSize` and `instanceSize` are read from the header rather than assumed,
//! because the spec permits a font to make the records *larger* than the fields
//! defined so far. Striding by a hard-coded 20 would misread every such font.

use super::{MalformedTable, Reader};

const TABLE: &str = "fvar";
const HEADER_SIZE: usize = 16;
const MIN_AXIS_SIZE: usize = 20;
/// A record must at least hold `subfamilyNameID`, `flags`, and one coordinate
/// per axis. `postScriptNameID` beyond that is optional.
const INSTANCE_FIXED_SIZE: usize = 4;

/// The `HIDDEN_AXIS` flag: the axis exists but is not meant to be shown to a
/// user choosing a style. Recorded so alias generation can skip it — a hidden
/// optical-size axis should not produce a `"Family 14pt"` alias.
const AXIS_FLAG_HIDDEN: u16 = 0x0001;

/// A four-byte axis tag (`wght`, `wdth`, `slnt`, `opsz`, `ital`, or a custom
/// registered tag).
///
/// A newtype rather than `[u8; 4]` so an axis tag cannot be mixed up with the
/// table tags in [`super::table_tag`], which are the same four bytes with an
/// entirely different meaning.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AxisTag(pub [u8; 4]);

impl AxisTag {
    pub const WEIGHT: Self = Self(*b"wght");
    pub const WIDTH: Self = Self(*b"wdth");
    pub const SLANT: Self = Self(*b"slnt");
    pub const ITALIC: Self = Self(*b"ital");
    pub const OPTICAL_SIZE: Self = Self(*b"opsz");

    /// The tag as text. Tags are required to be printable ASCII, but a
    /// malformed font can carry anything, so non-ASCII bytes are escaped rather
    /// than producing invalid UTF-8 or a panic.
    pub fn as_str(&self) -> String {
        self.0
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    char::from(b)
                } else {
                    '?'
                }
            })
            .collect()
    }
}

impl std::fmt::Debug for AxisTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AxisTag({})", self.as_str())
    }
}

impl std::fmt::Display for AxisTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// One point on one axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VariationCoord {
    pub axis: AxisTag,
    pub value: f32,
}

/// One axis of a variable font's design space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub tag: AxisTag,
    pub min: f32,
    pub default: f32,
    pub max: f32,
    /// The axis is flagged `HIDDEN_AXIS` — present in the design space but not
    /// intended for a user-facing style menu.
    pub hidden: bool,
    /// Name id of the axis's display name, for callers that want to show it.
    pub name_id: u16,
}

impl Axis {
    /// Whether `value` sits inside the axis's declared range.
    ///
    /// A coordinate outside the range is not merely unusual — passing one to
    /// Skia yields an instance the font does not define, so selection clamps
    /// rather than trusting the request.
    pub fn contains(&self, value: f32) -> bool {
        value >= self.min && value <= self.max
    }

    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(self.min, self.max)
    }
}

/// A point in the design space the designer named.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedInstance {
    /// Name id of the instance's subfamily name (`"SemiBold"`, `"Condensed
    /// Light Italic"`). Resolving it to text needs the `name` table, which this
    /// module deliberately does not reach into — see [`read`].
    pub subfamily_name_id: u16,
    /// Name id of the instance's PostScript name, when the font supplies one.
    pub post_script_name_id: Option<u16>,
    /// One coordinate per axis, in axis order.
    pub coords: Vec<VariationCoord>,
}

impl NamedInstance {
    /// Whether this instance sits at the design space's default location — the
    /// face you get with no variation applied.
    ///
    /// Worth knowing because such an instance needs no `clone_with_arguments`
    /// call, and because it is the one variable selection whose *embedded* PDF
    /// bytes are already correct.
    pub fn is_default(&self, axes: &[Axis]) -> bool {
        self.coords.iter().all(|c| {
            axes.iter()
                .find(|a| a.tag == c.axis)
                .is_some_and(|a| a.default == c.value)
        })
    }
}

/// A variable font's design space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Variations {
    pub axes: Vec<Axis>,
    pub instances: Vec<NamedInstance>,
}

impl Variations {
    pub fn is_empty(&self) -> bool {
        self.axes.is_empty()
    }

    pub fn axis(&self, tag: AxisTag) -> Option<&Axis> {
        self.axes.iter().find(|a| a.tag == tag)
    }
}

/// Decode an `fvar` table.
///
/// Returns axes and instances as *name ids*, not names. Resolving those ids
/// needs the `name` table, and reading it here would mean either taking two
/// tables as arguments — coupling two independent readers — or reaching for a
/// typeface, which this module has no business doing. The catalogue owns both
/// and joins them; see the `catalog` module.
pub fn read(bytes: &[u8]) -> Result<Variations, MalformedTable> {
    let mut r = Reader::new(bytes, TABLE);
    r.require(HEADER_SIZE)?;

    let major = r.u16("majorVersion")?;
    let _minor = r.u16("minorVersion")?;
    if major != 1 {
        return Err(MalformedTable::UnsupportedVersion {
            table: TABLE,
            field: "majorVersion",
            value: u32::from(major),
        });
    }

    let axes_offset = r.u16("axesArrayOffset")? as usize;
    let _reserved = r.u16("reserved")?;
    let axis_count = r.u16("axisCount")? as usize;
    let axis_size = r.u16("axisSize")? as usize;
    let instance_count = r.u16("instanceCount")? as usize;
    let instance_size = r.u16("instanceSize")? as usize;

    // A record smaller than the fields the spec defines cannot be strided
    // through — the second record would start inside the first.
    if axis_count > 0 && axis_size < MIN_AXIS_SIZE {
        return Err(MalformedTable::UnsupportedVersion {
            table: TABLE,
            field: "axisSize",
            value: axis_size as u32,
        });
    }

    // Bound the axis array *before* deriving anything from `axis_count`. A
    // corrupt count would otherwise inflate `min_instance_size` below and
    // surface as a complaint about `instanceSize`, which is the one field that
    // is not wrong — the reader would be blaming the wrong header field for a
    // fault it can name precisely.
    let instances_offset = array_end(axes_offset, axis_count, axis_size, r.len(), "axis array")?;

    let min_instance_size = INSTANCE_FIXED_SIZE + axis_count * 4;
    if instance_count > 0 && instance_size < min_instance_size {
        return Err(MalformedTable::UnsupportedVersion {
            table: TABLE,
            field: "instanceSize",
            value: instance_size as u32,
        });
    }
    array_end(
        instances_offset,
        instance_count,
        instance_size,
        r.len(),
        "instance array",
    )?;

    let mut axes = Vec::with_capacity(axis_count);
    for i in 0..axis_count {
        let rec = axes_offset + i * axis_size;
        axes.push(Axis {
            tag: AxisTag(r.tag_at(rec, "axisTag")?),
            min: r.fixed_at(rec + 4, "minValue")?,
            default: r.fixed_at(rec + 8, "defaultValue")?,
            max: r.fixed_at(rec + 12, "maxValue")?,
            hidden: r.u16_at(rec + 16, "axis flags")? & AXIS_FLAG_HIDDEN != 0,
            name_id: r.u16_at(rec + 18, "axisNameID")?,
        });
    }

    let mut instances = Vec::with_capacity(instance_count);
    for i in 0..instance_count {
        let rec = instances_offset + i * instance_size;
        let subfamily_name_id = r.u16_at(rec, "subfamilyNameID")?;
        let _flags = r.u16_at(rec + 2, "instance flags")?;
        let mut coords = Vec::with_capacity(axis_count);
        for (a, axis) in axes.iter().enumerate() {
            coords.push(VariationCoord {
                axis: axis.tag,
                value: r.fixed_at(rec + INSTANCE_FIXED_SIZE + a * 4, "coordinate")?,
            });
        }
        // `postScriptNameID` is present only when the record is wide enough for
        // it — the spec makes it optional by making the record two bytes longer.
        let post_script_name_id = if instance_size >= min_instance_size + 2 {
            r.u16_at(rec + min_instance_size, "postScriptNameID").ok()
        } else {
            None
        };
        instances.push(NamedInstance {
            subfamily_name_id,
            post_script_name_id,
            coords,
        });
    }

    Ok(Variations { axes, instances })
}

/// End offset of a `count × size` record array starting at `start`, or an
/// out-of-bounds error naming the array. Saturating arithmetic throughout: a
/// count read from a hostile font can be 65535, and `65535 * size` must not wrap
/// into a range that looks valid.
fn array_end(
    start: usize,
    count: usize,
    size: usize,
    table_len: usize,
    field: &'static str,
) -> Result<usize, MalformedTable> {
    let end = count
        .checked_mul(size)
        .and_then(|span| start.checked_add(span));
    match end {
        Some(end) if end <= table_len => Ok(end),
        _ => Err(MalformedTable::OutOfBounds {
            table: TABLE,
            field,
            offset: start,
            len: table_len,
        }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn fixed(v: f32) -> [u8; 4] {
        ((v * 65536.0) as i32).to_be_bytes()
    }

    /// Build an `fvar` table. `axes` are `(tag, min, default, max, hidden)`;
    /// `instances` are `(subfamilyNameID, coords, postScriptNameID)`.
    pub(crate) fn build_fvar(
        axes: &[(&[u8; 4], f32, f32, f32, bool)],
        instances: &[(u16, &[f32], Option<u16>)],
    ) -> Vec<u8> {
        let axis_size = MIN_AXIS_SIZE;
        let with_ps = instances.iter().any(|(_, _, ps)| ps.is_some());
        let instance_size = INSTANCE_FIXED_SIZE + axes.len() * 4 + usize::from(with_ps) * 2;

        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        out.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
        out.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes()); // axesArrayOffset
        out.extend_from_slice(&2u16.to_be_bytes()); // reserved
        out.extend_from_slice(&(axes.len() as u16).to_be_bytes());
        out.extend_from_slice(&(axis_size as u16).to_be_bytes());
        out.extend_from_slice(&(instances.len() as u16).to_be_bytes());
        out.extend_from_slice(&(instance_size as u16).to_be_bytes());

        for (i, (tag, min, def, max, hidden)) in axes.iter().enumerate() {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&fixed(*min));
            out.extend_from_slice(&fixed(*def));
            out.extend_from_slice(&fixed(*max));
            out.extend_from_slice(&u16::from(*hidden).to_be_bytes());
            out.extend_from_slice(&(256 + i as u16).to_be_bytes()); // axisNameID
        }
        for (name_id, coords, ps) in instances {
            out.extend_from_slice(&name_id.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes()); // flags
            for c in *coords {
                out.extend_from_slice(&fixed(*c));
            }
            if with_ps {
                out.extend_from_slice(&ps.unwrap_or(0).to_be_bytes());
            }
        }
        out
    }

    /// A two-axis fixture in the shape real variable fonts take.
    pub(crate) fn weight_width_fvar() -> Vec<u8> {
        build_fvar(
            &[
                (b"wght", 100.0, 400.0, 900.0, false),
                (b"wdth", 75.0, 100.0, 125.0, false),
            ],
            &[
                (300, &[400.0, 100.0], Some(400)),
                (301, &[600.0, 100.0], Some(401)),
                (302, &[700.0, 75.0], Some(402)),
            ],
        )
    }

    #[test]
    fn reads_axes_with_their_ranges() {
        let v = read(&weight_width_fvar()).unwrap();
        assert_eq!(v.axes.len(), 2);

        let wght = v.axis(AxisTag::WEIGHT).expect("wght axis");
        assert_eq!(wght.min, 100.0);
        assert_eq!(wght.default, 400.0);
        assert_eq!(wght.max, 900.0);
        assert!(!wght.hidden);

        let wdth = v.axis(AxisTag::WIDTH).expect("wdth axis");
        assert_eq!((wdth.min, wdth.default, wdth.max), (75.0, 100.0, 125.0));
    }

    #[test]
    fn reads_named_instances_with_coordinates() {
        let v = read(&weight_width_fvar()).unwrap();
        assert_eq!(v.instances.len(), 3);

        let semibold = &v.instances[1];
        assert_eq!(semibold.subfamily_name_id, 301);
        assert_eq!(semibold.post_script_name_id, Some(401));
        assert_eq!(
            semibold.coords,
            vec![
                VariationCoord {
                    axis: AxisTag::WEIGHT,
                    value: 600.0
                },
                VariationCoord {
                    axis: AxisTag::WIDTH,
                    value: 100.0
                },
            ]
        );
    }

    /// The default instance is the one whose embedded bytes are already right,
    /// so selection needs to be able to spot it.
    #[test]
    fn the_default_location_is_recognised() {
        let v = read(&weight_width_fvar()).unwrap();
        assert!(v.instances[0].is_default(&v.axes), "400/100 is the default");
        assert!(!v.instances[1].is_default(&v.axes), "600 is not");
        assert!(!v.instances[2].is_default(&v.axes));
    }

    /// A record wider than the fields the spec defines is legal and must be
    /// strided by its declared size, not by a hard-coded 20.
    #[test]
    fn oversized_axis_records_are_strided_by_their_declared_size() {
        let mut table = build_fvar(&[(b"wght", 100.0, 400.0, 900.0, false)], &[]);
        // Widen axisSize to 24 and pad the single record to match.
        table[10..12].copy_from_slice(&24u16.to_be_bytes());
        table.extend_from_slice(&[0u8; 4]);
        // Add a second axis after the padding so a wrong stride is detectable.
        table[8..10].copy_from_slice(&2u16.to_be_bytes());
        table.extend_from_slice(b"wdth");
        table.extend_from_slice(&fixed(75.0));
        table.extend_from_slice(&fixed(100.0));
        table.extend_from_slice(&fixed(125.0));
        table.extend_from_slice(&0u16.to_be_bytes());
        table.extend_from_slice(&257u16.to_be_bytes());
        table.extend_from_slice(&[0u8; 4]);

        let v = read(&table).expect("an oversized axis record is legal");
        assert_eq!(v.axes.len(), 2);
        assert_eq!(v.axes[1].tag, AxisTag::WIDTH, "strided by axisSize, not 20");
    }

    #[test]
    fn a_hidden_axis_is_flagged() {
        let table = build_fvar(&[(b"opsz", 8.0, 14.0, 144.0, true)], &[]);
        let v = read(&table).unwrap();
        assert!(v.axes[0].hidden);
        assert_eq!(v.axes[0].tag, AxisTag::OPTICAL_SIZE);
    }

    #[test]
    fn instances_without_a_postscript_name_id_report_none() {
        let table = build_fvar(
            &[(b"wght", 100.0, 400.0, 900.0, false)],
            &[(300, &[400.0], None)],
        );
        let v = read(&table).unwrap();
        assert_eq!(v.instances[0].post_script_name_id, None);
    }

    #[test]
    fn axis_ranges_clamp_out_of_range_requests() {
        let v = read(&weight_width_fvar()).unwrap();
        let wght = v.axis(AxisTag::WEIGHT).unwrap();
        assert!(wght.contains(600.0));
        assert!(!wght.contains(1000.0));
        assert_eq!(wght.clamp(1000.0), 900.0);
        assert_eq!(wght.clamp(50.0), 100.0);
        assert_eq!(wght.clamp(600.0), 600.0);
    }

    #[test]
    fn axis_tags_render_as_text() {
        assert_eq!(AxisTag::WEIGHT.as_str(), "wght");
        assert_eq!(
            AxisTag(*b"\x00\x01wg").as_str(),
            "??wg",
            "non-printable escaped"
        );
    }

    // ── malformed input ──────────────────────────────────────────────────

    #[test]
    fn a_table_shorter_than_its_header_is_an_error() {
        assert_eq!(
            read(&[0u8; 8]).unwrap_err(),
            MalformedTable::TooShort {
                table: "fvar",
                needed: 16,
                actual: 8
            }
        );
    }

    #[test]
    fn an_unknown_major_version_is_reported_as_such() {
        let mut table = weight_width_fvar();
        table[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert_eq!(
            read(&table).unwrap_err(),
            MalformedTable::UnsupportedVersion {
                table: "fvar",
                field: "majorVersion",
                value: 2
            }
        );
    }

    /// An `axisSize` below the defined field width would make records overlap.
    /// That is a broken font, and striding it anyway would read garbage tags.
    #[test]
    fn an_undersized_axis_record_is_refused() {
        let mut table = weight_width_fvar();
        table[10..12].copy_from_slice(&16u16.to_be_bytes());
        assert!(matches!(
            read(&table).unwrap_err(),
            MalformedTable::UnsupportedVersion {
                table: "fvar",
                field: "axisSize",
                ..
            }
        ));
    }

    #[test]
    fn an_undersized_instance_record_is_refused() {
        let mut table = weight_width_fvar();
        table[14..16].copy_from_slice(&4u16.to_be_bytes());
        assert!(matches!(
            read(&table).unwrap_err(),
            MalformedTable::UnsupportedVersion {
                table: "fvar",
                field: "instanceSize",
                ..
            }
        ));
    }

    #[test]
    fn an_axis_array_running_past_the_table_is_an_error() {
        let mut table = weight_width_fvar();
        table[8..10].copy_from_slice(&500u16.to_be_bytes()); // axisCount
        assert!(matches!(
            read(&table).unwrap_err(),
            MalformedTable::OutOfBounds { table: "fvar", .. }
        ));
    }
}
