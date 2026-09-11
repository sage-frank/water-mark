//! One policy for a child element that repeats when the schema says it cannot.
//!
//! # The problem
//!
//! Every OOXML property bag — `w:pPr`, `w:rPr`, `w:tblPr`, `w:trPr`, `w:tcPr`,
//! `w:sectPr` and their DrawingML/VML counterparts — is an `xsd:sequence` whose
//! children carry `maxOccurs="1"`. A producer that emits the same child twice
//! has written a schema-invalid document. Real producers do it anyway:
//! LibreOffice/AOO emit redundant toggles like `<w:b/><w:b/>`, and duplicated
//! `<w:tcMar>` inside one `<w:tcPr>` is what motivated PR #146.
//!
//! Modelled as `Option<T>`, serde rejects the second occurrence as a duplicate
//! field. [`crate::docx::parse::parse`] returns a `Result`, so that rejection is
//! **fatal** — one redundant element fails the whole conversion rather than
//! degrading it, on a document Word opens without complaint.
//!
//! # The policy, and why it is a choice and not a citation
//!
//! Every duplicable child element is typed `Vec<T>`, which quick-xml + serde
//! accumulate natively, and carried into the model as [`crate::model::Dup`],
//! which resolves **the last occurrence wins** at the point of use rather than
//! here. Nothing is discarded at the seam.
//!
//! ECMA-376 does not decide this. §17.7.2 defines last-wins for *toggle
//! properties* resolving through the style cascade — that is the rule
//! [`super::last_toggle`] implements, and it does not reach a repeated
//! `<w:tcMar>`. For a non-toggle the spec says only that the document is
//! invalid; it prescribes no recovery, so a converter has to pick one.
//!
//! Last-wins is picked for three reasons, none of which is the spec:
//!
//! 1. It matches the toggle rule already in force, so one sentence describes
//!    the whole parser rather than two rules split by element kind.
//! 2. It matches the direct-formatting intuition the rest of OOXML runs on —
//!    a later assertion overrides an earlier one.
//! 3. It is what Word appears to do. *Appears* is the honest word: this is
//!    inferred from documents Word round-trips, not from a reference render of
//!    a document with **conflicting** duplicates.
//!
//! **What would settle it:** a Word reference render of a cell whose `w:tcPr`
//! holds two `<w:tcMar>` with different values. If Word takes the first, or
//! merges them field-by-field, [`crate::model::Dup::get`] is what changes — one
//! place, and every read follows. Until then last-wins is an assumption held in
//! one function on purpose, and [`crate::model::Dup::all`] keeps the evidence.
//!
//! # What this deliberately does not do
//!
//! It does not warn. A redundant duplicate is the common case and the log would
//! be noise on ordinary documents; a *conflicting* duplicate is the interesting
//! one, and distinguishing them needs `PartialEq` on every schema type for no
//! behavioural gain. It also does not validate: a child that must satisfy a
//! constraint (a non-negative measurement, say) is checked **after** collapsing,
//! so a discarded occurrence cannot fail a document whose surviving value is
//! fine.
