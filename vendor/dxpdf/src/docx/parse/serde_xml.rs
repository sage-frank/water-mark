//! Shared helpers for serde-driven OOXML parsers.

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde::de::{DeserializeOwned, Deserializer, EnumAccess, IgnoredAny, VariantAccess, Visitor};
use serde::Deserialize;

use crate::docx::error::Result;

/// Deserialize an OOXML part into a schema type, mapping quick-xml's error
/// into the crate's `ParseError`.
pub fn from_xml<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    Ok(quick_xml::de::from_reader(data)?)
}

/// The children of a property bag that its schema does not name.
///
/// A `#[serde(other)]` catch-all needs an enum to live in, and it is a *unit*
/// variant — it records that something unmodelled was there but not what. A
/// plain-struct property bag has neither: an element it does not name is
/// dropped by the deserializer with nothing left behind at all. That is why
/// `w:hMerge` and `w:bidiVisual` were invisible at runtime rather than merely
/// unimplemented, and it is a different thing from the catch-all *branches*
/// that AGENTS.md already requires to log.
///
/// This closes it without restructuring the bag. quick-xml routes an element
/// to a named field when the schema has one and to `$value` when it does not,
/// so a bag that adds
///
/// ```ignore
/// #[serde(rename = "$value", default)]
/// unknown: UnknownChildren,
/// ```
///
/// keeps every field it already had and collects exactly the leftovers.
/// [`UnknownChildren::warn_once`] then names them.
///
/// The names are captured rather than counted because a name is the whole
/// value of the record: "some table property was dropped" cannot be acted on,
/// and `w:hMerge` can. Element content is skipped via [`IgnoredAny`], so an
/// unknown child costs one allocation for its name however large its subtree.
///
/// # Non-contiguous repeats
///
/// quick-xml rejects a named field whose element repeats *non-contiguously*
/// ("duplicate field") — `<w:tblCellSpacing/><w:jc/><w:tblCellSpacing/>` is a
/// parse error. That is a pre-existing limitation of the named fields, not
/// something `$value` introduces: it reproduces identically on a bag with no
/// `$value` at all. Adding this field neither fixes nor worsens it.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnknownChildren(Vec<Box<str>>);

impl UnknownChildren {
    /// The element names collected, in document order, with repeats.
    #[cfg(test)]
    pub(crate) fn names(&self) -> Vec<&str> {
        self.0.iter().map(|n| &**n).collect()
    }

    /// Log each unmodelled child once per document, at `warn`.
    ///
    /// `parent` is the bag's own element name, so the message says where the
    /// child was found and not merely that it existed.
    ///
    /// Deduplication is per `(parent, child)` name pair and per *document*,
    /// reset by [`reset_unknown_child_log`] at the top of a parse. A document
    /// with 693 tables that all carry `w:hMerge` logs one line, and the next
    /// document logs it again — a process-lifetime set would silently make the
    /// second document look clean.
    /// The `log::warn!` call itself is the one line here no unit test covers.
    /// A capturing logger would need `log::set_logger`, and this crate's lib
    /// test binary already installs one in `render::layout::section`'s tests
    /// with `.unwrap()` — a second installation panics whichever loses the
    /// race. The emission is checked end-to-end instead, by running the binary
    /// over a fixture under `RUST_LOG=warn`; everything that *decides* what to
    /// emit is [`take_unreported`](Self::take_unreported), which is tested.
    pub(crate) fn warn_once(&self, parent: &'static str) {
        for name in self.take_unreported(parent) {
            log::warn!("[parse] <{parent}> child <w:{name}> is not modelled and was ignored");
        }
    }

    /// The names not yet reported under `parent` for this document, marking
    /// them reported as it goes.
    ///
    /// Split out from [`warn_once`](Self::warn_once) so "once" is a property
    /// of a value a test can read. Asserting on the dedup set instead would
    /// not have caught a `warn_once` that recorded every name and logged every
    /// name too — the set looks identical either way.
    fn take_unreported(&self, parent: &'static str) -> Vec<Box<str>> {
        if self.0.is_empty() {
            return Vec::new();
        }
        SEEN_UNKNOWN_CHILDREN.with(|seen| {
            let mut seen = seen.borrow_mut();
            self.0
                .iter()
                .filter(|name| seen.insert((parent, (*name).clone())))
                .cloned()
                .collect()
        })
    }
}

thread_local! {
    /// `(parent, child)` pairs already logged for the document being parsed.
    ///
    /// Thread-local rather than a `static`, so two documents parsed on two
    /// threads cannot deduplicate against each other's names.
    static SEEN_UNKNOWN_CHILDREN: RefCell<BTreeSet<(&'static str, Box<str>)>> =
        const { RefCell::new(BTreeSet::new()) };
}

/// Forget which unmodelled children have been logged.
///
/// Called at the start of every [`crate::docx::parse::parse`] so the dedup set
/// describes one document. Without it the set would be process-lifetime and a
/// second document's unmodelled properties would be silently suppressed.
pub(crate) fn reset_unknown_child_log() {
    SEEN_UNKNOWN_CHILDREN.with(|seen| seen.borrow_mut().clear());
}

impl<'de> Deserialize<'de> for UnknownChildren {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Vec::<UnknownElement>::deserialize(d).map(|v| Self(v.into_iter().map(|e| e.0).collect()))
    }
}

/// One element with no matching named field, reduced to its name.
struct UnknownElement(Box<str>);

impl<'de> Deserialize<'de> for UnknownElement {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct ElementVisitor;

        impl<'de> Visitor<'de> for ElementVisitor {
            type Value = UnknownElement;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an XML element")
            }

            fn visit_enum<A: EnumAccess<'de>>(
                self,
                data: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                // quick-xml presents a `$value` child as an enum keyed by the
                // element name. Taking the variant *identifier* is what turns
                // an unmodelled element into a reportable name; the payload is
                // skipped, so an unknown subtree of any size costs nothing.
                let (name, variant): (String, _) = data.variant()?;
                variant.newtype_variant::<IgnoredAny>()?;
                Ok(UnknownElement(name.into_boxed_str()))
            }
        }

        // An empty `variants` list is deliberate: every element reaching this
        // type is by definition one the bag's schema did not name.
        d.deserialize_enum("UnknownElement", &[], ElementVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Deserialize)]
    struct Bag {
        #[serde(rename = "known", default)]
        known: Vec<Known>,
        #[serde(rename = "$value", default)]
        unknown: UnknownChildren,
    }

    #[derive(Debug, Deserialize)]
    struct Known {
        #[serde(rename = "@val", default)]
        #[allow(dead_code)]
        val: Option<String>,
    }

    fn parse(xml: &str) -> Bag {
        quick_xml::de::from_str(xml).unwrap()
    }

    #[test]
    fn a_named_child_is_not_reported_as_unknown() {
        let b = parse(r#"<bag><known val="7"/></bag>"#);
        assert_eq!(b.known.len(), 1);
        assert!(
            b.unknown.names().is_empty(),
            "a child the schema names must never be reported"
        );
    }

    #[test]
    fn an_unnamed_child_is_captured_by_name() {
        let b = parse(r#"<bag><known val="7"/><hMerge val="restart"/></bag>"#);
        assert_eq!(b.known.len(), 1, "the named child still parses");
        assert_eq!(b.unknown.names(), ["hMerge"]);
    }

    #[test]
    fn several_unnamed_children_are_captured_in_document_order() {
        let b = parse(r#"<bag><hMerge/><bidiVisual/></bag>"#);
        assert_eq!(b.unknown.names(), ["hMerge", "bidiVisual"]);
    }

    /// An unmodelled element may carry attributes, children and text. All of
    /// it is skipped, and the *name* still arrives.
    #[test]
    fn an_unnamed_child_with_a_subtree_is_captured_by_name_alone() {
        let b = parse(r#"<bag><foo w="1"><bar/>text</foo></bag>"#);
        assert_eq!(b.unknown.names(), ["foo"]);
    }

    #[test]
    fn an_empty_bag_reports_nothing() {
        assert!(parse("<bag/>").unknown.names().is_empty());
    }

    /// Each unmodelled name is reported once per document, however many bags
    /// carry it — the whole point, for a document with 693 tables.
    #[test]
    fn a_name_is_reported_once_per_document() {
        reset_unknown_child_log();
        let u = UnknownChildren(vec!["hMerge".into(), "hMerge".into(), "bidiVisual".into()]);
        assert_eq!(
            u.take_unreported("w:tcPr"),
            vec!["hMerge".into(), "bidiVisual".into()] as Vec<Box<str>>,
            "a repeat within one bag is still one report"
        );
        assert!(
            u.take_unreported("w:tcPr").is_empty(),
            "the next table with the same unmodelled child reports nothing"
        );
    }

    /// The same child name under a different bag is a different fact, so it
    /// gets its own report.
    #[test]
    fn the_report_is_keyed_by_parent_as_well_as_child() {
        reset_unknown_child_log();
        let u = UnknownChildren(vec!["hMerge".into()]);
        assert_eq!(u.take_unreported("w:tcPr").len(), 1);
        assert_eq!(
            u.take_unreported("w:tblPr").len(),
            1,
            "same name, different parent — still worth saying"
        );
    }

    /// Without the reset a second document's unmodelled properties would be
    /// suppressed by the first document's log.
    #[test]
    fn the_next_document_reports_the_same_name_again() {
        reset_unknown_child_log();
        let u = UnknownChildren(vec!["hMerge".into()]);
        assert_eq!(u.take_unreported("w:tcPr").len(), 1);
        assert!(u.take_unreported("w:tcPr").is_empty());

        reset_unknown_child_log();
        assert_eq!(
            u.take_unreported("w:tcPr").len(),
            1,
            "a new document starts from a clean log"
        );
    }
}
