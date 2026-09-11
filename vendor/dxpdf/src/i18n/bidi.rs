//! UAX #9: which direction does this text run?
//!
//! The odd one out in [`crate::i18n`]: it needs no ICU4X data at all, so none
//! of that module's blob machinery applies here — `unicode-bidi` carries its
//! own tables, which is why this phase adds no bytes to `icu_data.blob` or to
//! the Python wheel.
//!
//! The third module in a row, with [`crate::render::spacing`] (*where may space
//! go* — the UAX #29 grapheme cluster) and [`crate::i18n::segment`] (*where may
//! a line break* — the UAX #14 opportunity). All three exist for one reason:
//! the answer is needed in more than one place, and a second copy of the rule
//! drifts from the first. #130 found that had already happened once for line
//! breaking. Direction gets the same treatment before it can.
//!
//! # What runs where
//!
//! [`resolve_levels`] answers it for a whole paragraph at once, because that is
//! the only scope at which UAX #9 *can* answer it — rules W1–W7 and N0–N2
//! resolve a weak or neutral character from the strong characters around it,
//! and those may be arbitrarily far away. [`reorder`] then answers the
//! per-*line* question (rule L2), because a line is what gets painted and UAX #9
//! reorders per line, after the breaks are chosen. That ordering is why this
//! phase follows #130 rather than preceding it.
//!
//! Between the two sits an omission worth naming: **rule L1 is applied at
//! fragment granularity, not character granularity.** L1 resets segment
//! separators, paragraph separators, and any whitespace that trails them or
//! ends the line, back to the paragraph level. Tabs are handled structurally —
//! `layout::paragraph::line_emit` reorders within a tab-delimited segment, so a
//! class-S character is never inside a unit being reordered — but a space that
//! *ends a text fragment* keeps its neighbour's level rather than the
//! paragraph's. That space is past the line's visible end, and every width that
//! decides a position (`Fragment::trimmed_width`, `visible_line_width`) already
//! excludes it, so it moves nothing. Splitting a fragment to model it exactly
//! would cost a fragment per line for no pixel.
//!
//! # What this does not do
//!
//! Reordering is not legibility. A joining script — Arabic, Syriac, N'Ko — is
//! unreadable in isolated forms however correctly its runs are ordered, and
//! ordering them correctly is all this module does. [`crate::render::shape`]
//! owns the other half; the two are complementary, and [`BidiLevel::is_rtl`] is
//! the input the shaper needs to shape a run in the right direction.
//!
//! Hebrew needs nothing from the shaper — its final forms are separate
//! codepoints rather than positional variants — so Hebrew is the script this
//! module completes on its own, which is why rule L4 ([`mirror`]) is here and
//! not deferred: a Hebrew parenthetical with its brackets pointing outward is
//! the one visible defect that would otherwise remain in a script that is
//! otherwise correct.

use unicode_bidi::{BidiClass, BidiInfo, Level};

/// The paragraph embedding direction — §17.3.1.6 `w:bidi`.
///
/// UAX #9 rules P2/P3 would *derive* this from the first strong character, and
/// this engine never lets them: §17.3.1.6 states the paragraph's direction
/// outright, and a document that says so must win over a heuristic reading of
/// its own text. UAX #9 provides for exactly that in HL1 ("override P3").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BaseDirection {
    /// `w:bidi` absent or off.
    #[default]
    Ltr,
    /// `<w:bidi/>`.
    Rtl,
}

impl BaseDirection {
    /// The embedding level a paragraph in this direction starts at.
    pub fn level(self) -> BidiLevel {
        match self {
            Self::Ltr => BidiLevel::LTR,
            Self::Rtl => BidiLevel::RTL,
        }
    }

    /// §17.3.2.30 `w:rtl`, as the isolate that expresses it in an analysis
    /// string: `RLI` for an RTL run, `LRI` for one explicitly turned off.
    ///
    /// Isolates, not the older embeddings (`RLE`/`LRE`), because UAX #9 itself
    /// recommends them for new implementations — an embedding leaves its
    /// content able to affect how neutrals *outside* it resolve, which is not
    /// what "this run is right-to-left" means. Nor an override (`RLO`): that
    /// forces every character to the given direction, so Latin inside a
    /// `w:rtl` run would come out reversed.
    pub fn isolate(self) -> char {
        match self {
            Self::Ltr => unicode_bidi::format_chars::LRI,
            Self::Rtl => unicode_bidi::format_chars::RLI,
        }
    }
}

/// Terminates the isolate [`BaseDirection::isolate`] opens.
pub const POP_ISOLATE: char = unicode_bidi::format_chars::PDI;

/// A UAX #9 resolved embedding level.
///
/// Even is left-to-right, odd is right-to-left; the number is also the nesting
/// depth, which is what makes rule L2 a sort rather than a reversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct BidiLevel(u8);

impl BidiLevel {
    /// The level everything sits at in a document with no bidirectional text.
    pub const LTR: Self = Self(0);

    /// The level a `w:bidi` paragraph starts at.
    pub const RTL: Self = Self(1);

    /// True when text at this level is painted right-to-left.
    pub fn is_rtl(self) -> bool {
        self.0 % 2 == 1
    }

    /// True when text at this level is painted left-to-right.
    pub fn is_ltr(self) -> bool {
        !self.is_rtl()
    }

    /// A level of a stated number, for tests that need to hand-build one.
    ///
    /// Test-only so that production code cannot invent a level: every level a
    /// fragment carries comes from [`resolve_levels`] or is
    /// [`BidiLevel::LTR`], which is what keeps `unicode-bidi` behind this
    /// module rather than spread across the layout tree.
    #[cfg(test)]
    pub(crate) const fn from_number(n: u8) -> Self {
        Self(n)
    }
}

impl From<Level> for BidiLevel {
    fn from(l: Level) -> Self {
        Self(l.number())
    }
}

impl From<BidiLevel> for Level {
    fn from(l: BidiLevel) -> Self {
        // `Level::new` rejects a number past the maximum nesting depth, which
        // no `BidiLevel` can hold: every one is either a constant here or came
        // from `BidiInfo`, which enforces the same bound.
        Level::new(l.0).unwrap_or(unicode_bidi::LTR_LEVEL)
    }
}

/// True when `text` holds anything that could resolve to a right-to-left level.
///
/// The fast path, and the reason a document with no bidirectional text pays
/// nothing for this module: a caller that gets `false` for every one of a
/// paragraph's fragments can skip building an analysis string at all.
///
/// The strong RTL classes (`R`, `AL`) and the explicit RTL formatting controls
/// are the whole of it. `AN` — Arabic-Indic digits — is deliberately *not*
/// included: in a left-to-right paragraph rule I1 puts it at level 2, and since
/// 2 is even and rule L2 reverses only from the highest level down to the
/// lowest *odd* one, a run of them reorders to itself. Its own digits are
/// written left to right as well.
///
/// This is a scan, not a table: `unicode_bidi::bidi_class` reads the crate's own
/// data, so no list of codepoint ranges is maintained here to fall behind a
/// Unicode revision.
pub fn needs_analysis(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            unicode_bidi::bidi_class(c),
            BidiClass::R | BidiClass::AL | BidiClass::RLE | BidiClass::RLO | BidiClass::RLI
        )
    })
}

/// Resolve one embedding level per **byte** of `text`, as UAX #9 rules P–I.
///
/// Byte-indexed rather than character-indexed because that is what the caller
/// has: a fragment knows its byte range in the paragraph, and a multi-byte
/// character simply repeats its level across its bytes. `text` may contain
/// several bidi paragraphs (rule P1 splits at a class-`B` character, which is
/// what a `<w:br/>` contributes); each is resolved against `base`, so a forced
/// line break does not silently re-derive a direction the document stated.
pub fn resolve_levels(text: &str, base: BaseDirection) -> Vec<BidiLevel> {
    if text.is_empty() {
        return Vec::new();
    }
    BidiInfo::new(text, Some(base.level().into()))
        .levels
        .into_iter()
        .map(BidiLevel::from)
        .collect()
}

/// Rule L2: the visual order of items carrying `levels`, as indices into it.
///
/// `reorder(&levels)[0]` is the item to paint leftmost. Works on any sequence
/// whose elements each have one level — this engine passes fragments, so a
/// fragment is the unit that moves, which is why
/// [`crate::render::layout::fragment`] splits a fragment whenever a level
/// boundary falls inside it.
pub fn reorder(levels: &[BidiLevel]) -> Vec<usize> {
    let levels: Vec<Level> = levels.iter().copied().map(Level::from).collect();
    BidiInfo::reorder_visual(&levels)
}

/// Rule L4: the mirrored form of `c`, when it has one.
///
/// Applied to text at an odd level, where `(` opens a parenthesis on the right
/// and must therefore be painted as `)`. A codepoint swap, not a glyph
/// substitution — so unlike joining it works in this engine's cmap-only paint
/// path, and unlike joining it is complete here rather than approximate.
pub fn mirror(c: char) -> Option<char> {
    unicode_bidi_mirroring::get_mirrored(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Level per *char*, for tests — the byte-indexed vector repeats a
    /// multi-byte character's level across its bytes, which says nothing extra.
    fn char_levels(text: &str, base: BaseDirection) -> Vec<u8> {
        let levels = resolve_levels(text, base);
        text.char_indices().map(|(i, _)| levels[i].0).collect()
    }

    // ── needs_analysis: the fast path ──────────────────────────────────────

    #[test]
    fn ordinary_latin_text_needs_no_analysis() {
        for text in [
            "Nicht gefunden",
            "S.I.G.M.A. Technik Service GmbH",
            "Türöffner-Gerät — 10:30–12:00",
            "日本語の文章",
            "ภาษาไทย",
            "",
        ] {
            assert!(
                !needs_analysis(text),
                "{text:?} has no right-to-left character"
            );
        }
    }

    #[test]
    fn hebrew_and_arabic_need_analysis() {
        for text in ["שלום", "مرحبا", "page שלום here"] {
            assert!(needs_analysis(text), "{text:?} is bidirectional");
        }
    }

    /// Arabic-Indic digits resolve to level 2 — even, so rule L2 leaves them
    /// alone. Treating them as a reason to analyse would cost every document
    /// containing one an analysis that changes nothing.
    #[test]
    fn arabic_indic_digits_alone_need_no_analysis() {
        assert!(!needs_analysis("\u{0661}\u{0662}\u{0663}"));
        assert_eq!(
            char_levels("a \u{0661}\u{0662}", BaseDirection::Ltr),
            [0, 0, 2, 2]
        );
        assert_eq!(
            reorder(&resolve_levels("a \u{0661}\u{0662}", BaseDirection::Ltr)),
            (0..6).collect::<Vec<_>>(),
            "an even level is not reversed",
        );
    }

    // ── resolve_levels ────────────────────────────────────────────────────

    #[test]
    fn plain_latin_is_all_level_zero() {
        assert_eq!(char_levels("abc", BaseDirection::Ltr), [0, 0, 0]);
    }

    #[test]
    fn hebrew_in_a_left_to_right_paragraph_is_level_one() {
        // The Hebrew word rises to level 1; the Latin around it stays at 0.
        assert_eq!(
            char_levels("a שלום b", BaseDirection::Ltr),
            [0, 0, 1, 1, 1, 1, 0, 0]
        );
    }

    #[test]
    fn latin_in_a_right_to_left_paragraph_is_level_two() {
        // Base 1, so Latin rises to the next even level rather than dropping
        // to 0 — which is what keeps it inside the RTL flow.
        assert_eq!(
            char_levels("שלום ab שלום", BaseDirection::Rtl),
            [1, 1, 1, 1, 1, 2, 2, 1, 1, 1, 1, 1]
        );
    }

    /// The base direction is the document's to state, not ours to derive.
    /// The same text, read two ways, is the whole of §17.3.1.6.
    #[test]
    fn the_base_direction_decides_and_is_never_sniffed() {
        assert_eq!(char_levels("שלום", BaseDirection::Ltr), [1, 1, 1, 1]);
        assert_eq!(char_levels("שלום", BaseDirection::Rtl), [1, 1, 1, 1]);
        // A neutral is where the two part company: at the end of an LTR
        // paragraph it falls back to 0, in an RTL one to 1.
        assert_eq!(char_levels("שלום.", BaseDirection::Ltr), [1, 1, 1, 1, 0]);
        assert_eq!(char_levels("שלום.", BaseDirection::Rtl), [1, 1, 1, 1, 1]);
    }

    /// §17.3.2.30 `w:rtl` on a run of neutrals, expressed as the isolate the
    /// analysis string carries. Without it the digits are level 2 (LTR-ish);
    /// inside an RLI they resolve against an RTL context.
    #[test]
    fn an_rtl_isolate_changes_how_neutrals_resolve() {
        let bare = char_levels("a (1) b", BaseDirection::Ltr);
        assert_eq!(bare, [0, 0, 0, 0, 0, 0, 0], "no strong RTL anywhere");

        let wrapped = format!("a {}(1){} b", BaseDirection::Rtl.isolate(), POP_ISOLATE);
        let levels = char_levels(&wrapped, BaseDirection::Ltr);
        // Index 2 is the RLI itself; 3..=5 are `(`, `1`, `)`.
        assert!(
            levels[3] > 0 && levels[5] > 0,
            "the isolate's contents resolve right-to-left: {levels:?}",
        );
        assert_eq!(levels[0], 0, "and the text outside it does not");
    }

    /// A `<w:br/>` contributes a class-B character, which rule P1 makes a
    /// paragraph boundary. Both halves must still take the document's stated
    /// direction rather than re-deriving one each.
    #[test]
    fn a_forced_break_starts_a_paragraph_at_the_stated_direction() {
        assert_eq!(
            char_levels("ab\nשלום", BaseDirection::Rtl),
            [2, 2, 1, 1, 1, 1, 1]
        );
    }

    #[test]
    fn empty_text_resolves_to_no_levels() {
        assert!(resolve_levels("", BaseDirection::Rtl).is_empty());
    }

    // ── reorder (rule L2) ─────────────────────────────────────────────────

    #[test]
    fn a_left_to_right_line_keeps_its_order() {
        let levels = [BidiLevel::from_number(0); 4];
        assert_eq!(reorder(&levels), [0, 1, 2, 3]);
    }

    #[test]
    fn a_right_to_left_line_is_reversed() {
        let levels = [BidiLevel::from_number(1); 4];
        assert_eq!(reorder(&levels), [3, 2, 1, 0]);
    }

    /// The case that makes L2 a sort and not a reversal: an embedded LTR run
    /// inside RTL text keeps its own internal order while its position flips.
    #[test]
    fn an_embedded_run_flips_position_but_not_internal_order() {
        // Levels for [rtl, rtl, ltr, ltr, rtl] — e.g. Hebrew, "ab", Hebrew.
        let levels = [
            BidiLevel::from_number(1),
            BidiLevel::from_number(1),
            BidiLevel::from_number(2),
            BidiLevel::from_number(2),
            BidiLevel::from_number(1),
        ];
        assert_eq!(
            reorder(&levels),
            [4, 2, 3, 1, 0],
            "the level-2 pair stays in order; everything else reverses",
        );
    }

    #[test]
    fn reordering_is_always_a_permutation() {
        for levels in [
            vec![
                BidiLevel::from_number(0),
                BidiLevel::from_number(1),
                BidiLevel::from_number(0),
            ],
            vec![
                BidiLevel::from_number(1),
                BidiLevel::from_number(2),
                BidiLevel::from_number(3),
                BidiLevel::from_number(1),
            ],
            vec![BidiLevel::from_number(2)],
            vec![],
        ] {
            let order = reorder(&levels);
            assert_eq!(order.len(), levels.len());
            let mut seen = order.clone();
            seen.sort_unstable();
            assert_eq!(seen, (0..levels.len()).collect::<Vec<_>>());
        }
    }

    // ── mirror (rule L4) ──────────────────────────────────────────────────

    #[test]
    fn paired_punctuation_mirrors() {
        for (from, to) in [
            ('(', ')'),
            (')', '('),
            ('[', ']'),
            ('{', '}'),
            ('<', '>'),
            ('\u{00AB}', '\u{00BB}'),
        ] {
            assert_eq!(mirror(from), Some(to), "{from:?} mirrors to {to:?}");
        }
    }

    #[test]
    fn unpaired_characters_do_not_mirror() {
        for c in ['a', 'א', '.', ' ', '"', '-'] {
            assert_eq!(mirror(c), None, "{c:?} has no mirror");
        }
    }

    // ── the levels a caller will actually see ─────────────────────────────

    /// End to end on the sentence the fixtures use: an Arabic phrase with a
    /// Western number in it, which is the case every bidi bug report opens
    /// with. `12` must come out at an even level so its digits keep their
    /// order while the words around them reverse.
    #[test]
    fn arabic_with_western_digits_puts_the_number_at_an_even_level() {
        let text = "صفحة 12 من";
        let levels = char_levels(text, BaseDirection::Rtl);
        let digits: Vec<u8> = text
            .char_indices()
            .zip(&levels)
            .filter(|((_, c), _)| c.is_ascii_digit())
            .map(|(_, l)| *l)
            .collect();
        assert_eq!(digits, [2, 2], "digits ride at an even level");
        assert!(
            levels.iter().filter(|l| **l == 1).count() >= 6,
            "and the Arabic around them is odd: {levels:?}",
        );
    }
}
