//! Edge insets (§17.4.68 tcMar, §17.4.42 tblCellMar) — four-sided twips
//! padding shared by table-cell margins and table default cell margins.
//!
//! Each side is `<w:top w:w="N" w:type="dxa"/>` etc. Only `dxa` (twips) is
//! meaningful for cell padding; other `@type` values are ignored here.

use crate::model::Dup;
use serde::Deserialize;

use crate::docx::model::dimension::{Dimension, Twips};
use crate::docx::model::geometry::{EdgeInsets, PartialEdgeInsets};
use crate::docx::parse::primitives::units::deserialize_optional_nonnegative_dimension;

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct EdgeInsetsTwipsXml {
    #[serde(default)]
    top: Vec<SideXml>,
    #[serde(default)]
    bottom: Vec<SideXml>,
    #[serde(default, alias = "start")]
    left: Vec<SideXml>,
    #[serde(default, alias = "end")]
    right: Vec<SideXml>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct SideXml {
    #[serde(
        rename = "@w",
        default,
        deserialize_with = "deserialize_optional_nonnegative_dimension"
    )]
    w: Option<Dimension<Twips>>,
}

/// Conversion used for the table-level default (`<w:tblCellMar>`). Per
/// OOXML §17.4.42 a table default has no further inheritance, so missing
/// sides collapse to zero.
impl From<EdgeInsetsTwipsXml> for EdgeInsets<Twips> {
    fn from(x: EdgeInsetsTwipsXml) -> Self {
        Self::new(
            Dup::from(x.top)
                .into_value()
                .and_then(|s| s.w)
                .unwrap_or_default(),
            Dup::from(x.right)
                .into_value()
                .and_then(|s| s.w)
                .unwrap_or_default(),
            Dup::from(x.bottom)
                .into_value()
                .and_then(|s| s.w)
                .unwrap_or_default(),
            Dup::from(x.left)
                .into_value()
                .and_then(|s| s.w)
                .unwrap_or_default(),
        )
    }
}

/// Conversion used for per-cell overrides (`<w:tcMar>`, §17.4.68). Each
/// child element (`<w:top>` §17.4.75, `<w:start>` §17.4.34, `<w:bottom>`
/// §17.4.5, `<w:end>` §17.4.11) is an *exception* that overrides the
/// corresponding side of the parent `<w:tblCellMar>` (§17.4.42).
///
/// Per the spec, cascade is per-side: a side whose element is structurally
/// absent inherits from the parent; a side whose element is present is an
/// explicit override, including `<w:top w:w="0" w:type="dxa"/>` which is
/// `CT_TblWidth` (§17.4.87) for "0 twips" — an explicit zero, not a
/// placeholder for "no override". The layout layer resolves the partial
/// override against the table-level default via
/// [`PartialEdgeInsets::resolve_against`].
impl From<EdgeInsetsTwipsXml> for PartialEdgeInsets<Twips> {
    fn from(x: EdgeInsetsTwipsXml) -> Self {
        Self::new(
            Dup::from(x.top).into_value().and_then(|s| s.w),
            Dup::from(x.right).into_value().and_then(|s| s.w),
            Dup::from(x.bottom).into_value().and_then(|s| s.w),
            Dup::from(x.left).into_value().and_then(|s| s.w),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> EdgeInsets<Twips> {
        let x: EdgeInsetsTwipsXml = quick_xml::de::from_str(xml).unwrap();
        x.into()
    }

    #[test]
    fn all_four_sides_captured() {
        let e = parse(
            r#"<tcMar>
                <top w="100"/>
                <bottom w="200"/>
                <left w="50"/>
                <right w="75"/>
            </tcMar>"#,
        );
        assert_eq!(e.top.raw(), 100);
        assert_eq!(e.bottom.raw(), 200);
        assert_eq!(e.left.raw(), 50);
        assert_eq!(e.right.raw(), 75);
    }

    #[test]
    fn start_and_end_alias_left_right() {
        let e = parse(
            r#"<tcMar>
                <start w="80"/>
                <end w="120"/>
            </tcMar>"#,
        );
        assert_eq!(e.left.raw(), 80);
        assert_eq!(e.right.raw(), 120);
    }

    #[test]
    fn missing_sides_default_to_zero() {
        let e = parse(r#"<tcMar><top w="100"/></tcMar>"#);
        assert_eq!(e.top.raw(), 100);
        assert_eq!(e.right.raw(), 0);
        assert_eq!(e.bottom.raw(), 0);
        assert_eq!(e.left.raw(), 0);
    }

    /// Per OOXML §17.4.68, an absent child element in `<w:tcMar>` means
    /// "inherit from `<w:tblCellMar>` for that side". Only the sides
    /// structurally present in the XML are an override; the rest stay `None`
    /// so the layout cascade can fall back to the table-level default.
    #[test]
    fn partial_tcmar_preserves_absent_sides_as_none() {
        let xml = r#"<tcMar>
            <top w="40"/>
        </tcMar>"#;
        let parsed: EdgeInsetsTwipsXml = quick_xml::de::from_str(xml).unwrap();
        let p: PartialEdgeInsets<Twips> = parsed.into();
        assert_eq!(p.top.map(|d| d.raw()), Some(40));
        assert!(p.bottom.is_none(), "absent <w:bottom> must stay None");
        assert!(p.left.is_none(), "absent <w:start>/<w:left> must stay None");
        assert!(p.right.is_none(), "absent <w:end>/<w:right> must stay None");

        let default = EdgeInsets::<Twips>::new(
            Dimension::new(57),
            Dimension::new(108),
            Dimension::new(57),
            Dimension::new(103),
        );
        let resolved = p.resolve_against(default);
        assert_eq!(resolved.top.raw(), 40, "explicit override wins");
        assert_eq!(resolved.bottom.raw(), 57, "absent side inherits");
        assert_eq!(resolved.left.raw(), 103, "absent side inherits");
        assert_eq!(resolved.right.raw(), 108, "absent side inherits");
    }

    /// Per OOXML §17.4.87 (`CT_TblWidth`), `@type="dxa" @w="0"` is
    /// "0 twentieths of a point", i.e. an explicit zero. Combined with
    /// §17.4.68's "exception" semantics, a `<w:top w:w="0" w:type="dxa"/>`
    /// inside `<w:tcMar>` is an *explicit override* to zero, structurally
    /// distinct from an absent child element. This test pins the spec-
    /// faithful interpretation: the parser must report `Some(0)`, not `None`.
    ///
    /// (Word/LibreOffice may render such cells with the table-default
    /// padding regardless — that is a rendering quirk in those products
    /// and not something the parser may silently impose; emulating it
    /// belongs in the layout layer behind an explicit, named code path.)
    #[test]
    fn partial_tcmar_w_zero_is_explicit_zero_override() {
        let xml = r#"<tcMar>
            <top w="0"/>
            <bottom w="0"/>
        </tcMar>"#;
        let parsed: EdgeInsetsTwipsXml = quick_xml::de::from_str(xml).unwrap();
        let p: PartialEdgeInsets<Twips> = parsed.into();
        assert_eq!(
            p.top.map(|d| d.raw()),
            Some(0),
            "w=0 must be preserved as explicit Some(0), not silently dropped"
        );
        assert_eq!(p.bottom.map(|d| d.raw()), Some(0));
        assert!(p.left.is_none());
        assert!(p.right.is_none());

        let default = EdgeInsets::<Twips>::new(
            Dimension::new(57),
            Dimension::new(108),
            Dimension::new(57),
            Dimension::new(103),
        );
        let resolved = p.resolve_against(default);
        assert_eq!(resolved.top.raw(), 0, "explicit zero overrides default");
        assert_eq!(resolved.bottom.raw(), 0, "explicit zero overrides default");
        assert_eq!(resolved.left.raw(), 103, "absent side inherits default");
        assert_eq!(resolved.right.raw(), 108, "absent side inherits default");
    }
}
