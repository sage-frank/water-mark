//! [`Dup`] — a property whose element the schema allows once but producers repeat.
//!
//! # Why the model carries every occurrence
//!
//! OOXML property bags are `xsd:sequence`s whose children are `maxOccurs="1"`,
//! so a repeated child is schema-invalid. Real producers emit them anyway (see
//! `crate::docx::parse::primitives::duplicates` for who and what). The parser
//! therefore accepts a `Vec<T>` for every duplicable child — and then has a
//! choice about *where* the extra occurrences stop.
//!
//! They stop here, at the point of use, not at the XML→domain seam. `Dup<T>`
//! carries the whole list through parse → resolve → layout, and [`Dup::get`]
//! applies the policy when a consumer actually asks for a value. Nothing is
//! discarded on the way in.
//!
//! # Parsing is lossless; resolution is deferred
//!
//! The transformations along the way are **structure-preserving**.
//! [`Dup::map`] takes `Dup<T>` to `Dup<U>` — every occurrence transformed, none
//! chosen — so `From<XxxXml> for Model` converts each occurrence into its
//! domain type and passes the whole list on. The parse layer therefore answers
//! "what did the document say?" and never "which one did it mean?".
//!
//! Only [`Dup::get`] and [`Dup::into_value`] resolve, and they live at the read
//! site. That separation is the point: a consumer that wants first-wins, or to
//! merge two occurrences, or to warn when they disagree, needs no change to the
//! parser — [`Dup::all`] is right there.
//!
//! The cost of that choice is real and is the reason this type is a newtype
//! rather than a bare `Vec`: a `Vec` is three words and a heap allocation that
//! now lives for the whole render instead of being freed during parse, and
//! every read is an indirection rather than a field access. `Dup` keeps the
//! *shape* of the decision in one place so both the policy and its price stay
//! measurable — see `benches/` for the numbers that justified it.
//!
//! # The policy
//!
//! **Last occurrence wins.** ECMA-376 does not decide this: §17.7.2 defines
//! last-wins for *toggle properties*, and a repeated `<w:tcMar>` is not a
//! toggle. The reasoning, and the Word reference render that would settle it,
//! are documented once in `crate::docx::parse::primitives::duplicates`.
//!
//! Because the occurrences survive, a consumer that wants a different rule can
//! have one without touching the parser: [`Dup::all`] hands back every
//! occurrence in document order.
//!
//! # Where a `Dup` goes, and why that is not a matter of taste
//!
//! On a **property-bag field** — a direct child of `w:pPr`, `w:rPr`, `w:tblPr`,
//! `w:trPr`, `w:tcPr`, `w:sectPr`. **Never inside a composite value type**
//! ([`crate::model::ParagraphBorders`], `Border`, `EdgeInsets`, `Transform2D`),
//! which stay plain and `Copy`.
//!
//! This was settled by two whole-tree attempts that collapsed before it was
//! clear, and re-deriving it is expensive enough to be worth stating. Widening
//! the *sides* inside `ParagraphBorders` strips `Copy` from a type layout reads
//! in 59 places, turns one bordered paragraph into five heap allocations, and
//! cascades through every reader: compile errors went 82 → 216 on one attempt
//! and 143 → 180 on the other, and neither converged. Widening the *owner* cost
//! 12 errors and was fixed in a single pass.
//!
//! The property-bag child is also the level §17.7.2 operates at, so `Dup` and
//! the style cascade line up instead of crossing. The price is that a repeated
//! `<w:top>` inside one `<w:pBdr>` still collapses at the seam — not maximally
//! lossless, and deliberate.
//!
//! The `Option<bool>` toggles are excluded for the opposite reason and should
//! stay that way: they come from `last_toggle`, where last-wins is §17.7.2's
//! own rule rather than this parser's choice, and converting them would erase
//! the distinction `crate::docx::parse::primitives::duplicates` exists to
//! record.
//!
//! # What the corpus proves about this, and what it does not
//!
//! Measured — do not re-measure. **0 of 20** committed documents repeat a
//! property-bag child, so "corpus renders byte-identical" proves no regression
//! and *nothing at all* about the tolerance itself. Against eight synthetic
//! documents built for it: **7 of 8** were fatal before this type existed
//! (`tcMar` alone had been fixed separately), **8 of 8** convert now, and each
//! renders identically to the same document carrying only the winning
//! occurrence. That validates internal consistency — not that Word picks last,
//! which still needs the reference render named in
//! `crate::docx::parse::primitives::duplicates`.
//!
//! Cost is within noise on all four benchmark documents by whole-process
//! timing. Treat that as **no measurable cost**, not as the speedup the
//! criterion reports showed, which has no mechanism anyone could name.

/// Every occurrence of a child element the schema allows at most once.
///
/// Empty means the element was absent — the same "inherit from the cascade"
/// signal `Option::None` carried before. One element is the ordinary case.
/// Two or more means the document is schema-invalid and [`Dup::get`] picks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dup<T>(Vec<T>);

impl<T> Default for Dup<T> {
    /// Absent, and allocation-free — `Vec::new` does not touch the heap, so an
    /// unset property costs the same three words it would as an `Option`.
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Dup<T> {
    /// The effective value: the last occurrence, or `None` when absent.
    pub fn get(&self) -> Option<&T> {
        self.0.last()
    }

    /// The effective value, mutably — the same occurrence [`Dup::get`] returns.
    ///
    /// This is what the style cascade merges into. §17.3.1.12 and §17.3.1.33
    /// combine *sub-fields* of `<w:ind>` and `<w:spacing>` across levels rather
    /// than replacing the element wholesale, and the level being combined is
    /// the effective one. The occurrences that lost are left exactly as the
    /// document wrote them, so [`Dup::all`] stays a record of the XML while the
    /// last element becomes the resolved value.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.0.last_mut()
    }

    /// The effective value, by value. Discards the occurrences that lost.
    pub fn into_value(self) -> Option<T> {
        self.0.into_iter().next_back()
    }

    /// The effective value, cloned — the common shape at a read site that
    /// needs ownership but only has a borrow.
    pub fn cloned(&self) -> Option<T>
    where
        T: Clone,
    {
        self.0.last().cloned()
    }

    /// Every occurrence, in document order. This is what the type exists for:
    /// a consumer that wants first-wins, or to merge, or to warn when two
    /// occurrences disagree, can do it without changing the parser.
    pub fn all(&self) -> &[T] {
        &self.0
    }

    /// True when the element was absent.
    pub fn is_absent(&self) -> bool {
        self.0.is_empty()
    }

    /// True when the document repeated a child the schema allows once.
    pub fn is_duplicated(&self) -> bool {
        self.0.len() > 1
    }

    /// Transform **every** occurrence, preserving all of them.
    ///
    /// This is the functor operation, not a resolution: `Dup<T>` in, `Dup<U>`
    /// out, same length, same order. It is what keeps the XML→domain seam
    /// lossless — `From<TcPrXml>` maps each occurrence into its model type and
    /// hands the whole list on, and no occurrence is chosen until a consumer
    /// calls [`Dup::get`].
    ///
    /// A `map` that collapsed to `Option` here would put the decision back at
    /// the seam, which is the thing this type exists to avoid.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Dup<U> {
        Dup(self.0.into_iter().map(f).collect())
    }

    /// Transform every occurrence, dropping the ones that yield `None`.
    ///
    /// The `Dup` analogue of `Option::and_then`: an occurrence the domain
    /// cannot represent disappears, the rest survive.
    pub fn filter_map<U>(self, f: impl FnMut(T) -> Option<U>) -> Dup<U> {
        Dup(self.0.into_iter().filter_map(f).collect())
    }

    /// Borrow every occurrence for transformation without consuming.
    pub fn map_ref<U>(&self, f: impl FnMut(&T) -> U) -> Dup<U> {
        Dup(self.0.iter().map(f).collect())
    }
}

impl<T> From<Vec<T>> for Dup<T> {
    fn from(v: Vec<T>) -> Self {
        Self(v)
    }
}

impl<T> From<Option<T>> for Dup<T> {
    fn from(v: Option<T>) -> Self {
        Self(v.into_iter().collect())
    }
}

impl<T> FromIterator<T> for Dup<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_reads_as_none() {
        let d: Dup<u8> = Dup::default();
        assert!(d.is_absent());
        assert_eq!(d.get(), None);
        assert_eq!(d.clone().into_value(), None);
        assert!(!d.is_duplicated());
    }

    #[test]
    fn a_single_occurrence_is_itself() {
        let d = Dup::from(vec![7]);
        assert_eq!(d.get(), Some(&7));
        assert!(!d.is_duplicated());
    }

    #[test]
    fn the_last_occurrence_wins() {
        let d = Dup::from(vec![1, 2, 3]);
        assert_eq!(d.get(), Some(&3));
        assert_eq!(d.clone().into_value(), Some(3));
        // Order matters: this is the whole policy, so pin both directions.
        assert_eq!(Dup::from(vec![3, 2, 1]).get(), Some(&1));
    }

    #[test]
    fn the_losing_occurrences_survive_into_the_model() {
        // The point of the type: a downstream consumer can still see them.
        let d = Dup::from(vec![1, 2, 3]);
        assert!(d.is_duplicated());
        assert_eq!(d.all(), &[1, 2, 3]);
        assert_eq!(d.all().first(), Some(&1), "first-wins is still reachable");
    }

    #[test]
    fn map_preserves_every_occurrence() {
        let d = Dup::from(vec![1, 2, 3]).map(|n| n * 10);
        assert_eq!(d.all(), &[10, 20, 30], "map is a functor, not a resolution");
        assert_eq!(
            d.get(),
            Some(&30),
            "and the policy still applies at the read"
        );
    }

    #[test]
    fn filter_map_drops_only_what_it_is_told_to() {
        let d = Dup::from(vec![1, 2, 3]).filter_map(|n| (n % 2 == 1).then_some(n));
        assert_eq!(d.all(), &[1, 3]);
    }

    #[test]
    fn mapping_an_absent_property_stays_absent() {
        assert!(Dup::<u8>::default().map(|n| n + 1).is_absent());
    }

    #[test]
    fn it_round_trips_an_option() {
        assert_eq!(Dup::from(Some(5)).get(), Some(&5));
        assert_eq!(Dup::from(None::<u8>).get(), None);
    }
}
