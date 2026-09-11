//! What a face *is*: the identity the resolver matches against and the
//! instructions for opening it again afterwards.
//!
//! One [`FaceRecord`] describes one selectable face. "Selectable" is the load
//! bearing word — a named instance of a variable font is a `FaceRecord` in its
//! own right, not an attribute of the variable face it lives in, because a
//! document can ask for `"Fixture Sans SemiBold"` and get exactly it. Modelling
//! instances as separate records is what keeps the resolver uniform: every step
//! matches a requested name against records and nothing has to special-case a
//! design space.
//!
//! # The two halves
//!
//! [`FaceIdentity`] is the *reopening* half — enough to obtain the same Skia
//! typeface again, and nothing more. It deliberately holds no names: a face is
//! reopened by index, never by re-matching a string, so a catalogue entry and
//! the typeface it yields cannot drift apart.
//!
//! [`FaceName`] is the *matching* half. Each carries a [`NameKind`] recording
//! **how the engine came to know that name**, because that is exactly what
//! decides which resolution step may use it. A PostScript name read from the
//! font is evidence; a name composed from a family and a style word is a guess,
//! and the eight-step chain runs the evidence first.

use skia_safe::font_style::{Slant, Weight, Width};
use skia_safe::FontStyle;

use super::opentype::fvar::VariationCoord;
use super::opentype::NameId;
use super::EmbeddedFontId;

/// How to obtain this face's Skia typeface again.
///
/// Both variants address a face *positionally*. That is the point: reopening by
/// `(family, style)` would run the host's own matching a second time and could
/// return a different face than the one the catalogue indexed, which is the
/// class of bug acceptance item 7 is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FaceIdentity {
    /// A face the host font manager exposes: `match_family(family)` gives the
    /// style set, `new_typeface(face_index)` gives this face.
    System {
        /// The family name the manager itself enumerated — not a name from the
        /// `name` table, which may differ.
        family: String,
        face_index: usize,
    },
    /// A face inside a font embedded in the DOCX. `collection_index` is 0 for an
    /// ordinary SFNT and the face's position for a TrueType Collection; OOXML
    /// has no way to express it, so it is discovered by reading the bytes.
    Embedded {
        font: EmbeddedFontId,
        collection_index: u32,
    },
}

impl FaceIdentity {
    pub fn is_embedded(&self) -> bool {
        matches!(self, Self::Embedded { .. })
    }
}

/// A point in a variable font's design space that the designer named, resolved
/// to text.
///
/// Distinct from [`super::opentype::fvar::NamedInstance`], which carries name
/// *ids*: `fvar` cannot resolve them without the `name` table, and joining the
/// two is the catalogue's job.
#[derive(Clone, Debug, PartialEq)]
pub struct VariationInstance {
    /// The instance's subfamily name, e.g. `"SemiBold"` or `"Condensed Light"`.
    pub subfamily: String,
    /// The instance's own PostScript name, when the font supplies one.
    pub post_script_name: Option<String>,
    /// The design-space location, one coordinate per axis.
    pub coords: Vec<VariationCoord>,
}

/// The style a face *has*, as opposed to any style asked of it.
///
/// Read from `OS/2` where the bytes are available, and from the host font
/// manager's own `FontStyle` where they are not. The distinction matters for
/// `weight`: this is the face's real `usWeightClass`, so 342 and 587 are
/// ordinary values here, not roundable approximations of 300 and 600.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntrinsicStyle {
    pub weight: i32,
    pub width: Width,
    pub slant: Slant,
    /// The face declares itself its family's Bold slot (`OS/2` `fsSelection`
    /// bit 5). Kept alongside `weight` rather than derived from it because the
    /// two genuinely disagree: a face at weight 600 may set the bit, and a face
    /// at 700 may not.
    pub bold_slot: bool,
    /// The face declares itself its family's Regular (`fsSelection` bit 6).
    pub regular_slot: bool,
}

impl IntrinsicStyle {
    /// The style a bare family name implies when nothing is known about a
    /// specific face: upright Regular at normal width.
    ///
    /// This is what keeps a plain `"Calibri"` request behaving exactly as it
    /// did before face metadata existed — the tri-state `Absent` resolves
    /// against *this*, not against whatever the first indexed face happens to
    /// be.
    pub const NEUTRAL: Self = Self {
        weight: 400,
        width: Width::NORMAL,
        slant: Slant::Upright,
        bold_slot: false,
        regular_slot: true,
    };

    pub fn from_font_style(style: FontStyle) -> Self {
        Self {
            weight: *style.weight(),
            width: style.width(),
            slant: style.slant(),
            // A `FontStyle` carries no `fsSelection`, so the slot flags are
            // inferred from the only evidence there is. `>= BOLD` rather than
            // `>= SEMI_BOLD` deliberately: this is "does the face claim the
            // bold slot", and the manager reports 700 for a family's Bold.
            bold_slot: *style.weight() >= *Weight::BOLD,
            regular_slot: *style.weight() == *Weight::NORMAL && style.slant() == Slant::Upright,
        }
    }

    pub fn font_style(&self) -> FontStyle {
        FontStyle::new(Weight::from(self.weight), self.width, self.slant)
    }

    pub fn is_italic(&self) -> bool {
        matches!(self.slant, Slant::Italic | Slant::Oblique)
    }

    /// Whether the face already claims to be bold — checked against *both*
    /// signals `OS/2` can carry, not just the legacy slot bit: a family with
    /// weights past Bold (ExtraBold 800, Black 900) may leave `fsSelection`'s
    /// bold bit set only on the literal Bold face, since the bit names one
    /// slot in the legacy four-style model, not "at least this heavy". A
    /// `w:b` request against such a family ranks its heaviest available face
    /// as the closest match to "at least Bold" (see
    /// [`EffectiveStyle::resolve`](super::request::EffectiveStyle::resolve))
    /// — `bold_slot` alone would then read Black as un-bold and thicken it
    /// further.
    pub fn is_already_bold(&self) -> bool {
        self.bold_slot || self.weight >= *Weight::BOLD
    }
}

/// How the engine came to know a face by a given name.
///
/// This is the field the eight-step resolution chain keys on. Steps 4 and 5 may
/// only match names the *font* asserts ([`Table`](Self::Table)); step 6 may
/// match names the engine composed. Collapsing the two into a flat alias list —
/// which is what the previous `FaceAliasIndex` did — is precisely what let a
/// composed guess outrank a name the font stated outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameKind {
    /// Read from the font's own `name` table at this id. Evidence.
    Table(NameId),
    /// The family name the host font manager reports for the face.
    ManagerFamily,
    /// The manager's family joined to the style name it reports, e.g.
    /// `"Inter Light"`. Composed, not asserted — the manager's style name is
    /// itself often derived.
    ManagerFace,
    /// A named instance's subfamily joined to its family, e.g.
    /// `"Fixture Sans SemiBold"` from `fvar`.
    Instance,
    /// Composed from the family and one or more `STAT` axis-value names, e.g.
    /// `"Fixture Sans Condensed SemiBold"`.
    ComposedStyle,
}

impl NameKind {
    /// Whether the font itself asserts this name, as opposed to the engine
    /// having assembled it. Only asserted names are eligible for the metadata
    /// steps of the resolution chain.
    pub fn is_asserted(self) -> bool {
        matches!(self, Self::Table(_))
    }

    /// Whether this is one of the three *primary* identity names — full,
    /// compatible-full, or PostScript — that resolution step 4 matches before
    /// any other alias.
    ///
    /// These three are singled out by the spec's own usage: they name exactly
    /// one face each, whereas a family or typographic-family name is shared by
    /// every face in the family and cannot select between them.
    pub fn is_primary_identity(self) -> bool {
        matches!(
            self,
            Self::Table(NameId::Full)
                | Self::Table(NameId::CompatibleFull)
                | Self::Table(NameId::PostScript)
        )
    }
}

/// One name a face answers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceName {
    pub kind: NameKind,
    pub text: String,
    /// The name came from a record in a language other than the font's primary
    /// (English) one. Matched just the same — an author's Word wrote whichever
    /// spelling its UI showed — but not used as a canonical name.
    pub localized: bool,
}

impl FaceName {
    pub fn asserted(id: NameId, text: impl Into<String>) -> Self {
        Self {
            kind: NameKind::Table(id),
            text: text.into(),
            localized: false,
        }
    }

    pub fn composed(kind: NameKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            localized: false,
        }
    }
}

/// One selectable face, with everything needed to match it and reopen it.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceRecord {
    pub identity: FaceIdentity,
    /// The family this face belongs to, preferring the font's own English
    /// `name` record and falling back to what the manager reported.
    pub canonical_family: String,
    /// The typographic family (`name` id 16) when the font declares one — the
    /// family that spans every weight rather than the four-slot legacy family.
    pub typographic_family: Option<String>,
    pub intrinsic: IntrinsicStyle,
    pub names: Vec<FaceName>,
    /// Set when this record is a named instance of a variable face rather than
    /// that face at its default design-space location.
    pub instance: Option<VariationInstance>,
}

impl FaceRecord {
    /// Every name of a kind the predicate accepts.
    pub fn names_matching(
        &self,
        accept: impl Fn(NameKind) -> bool + Copy,
    ) -> impl Iterator<Item = &FaceName> {
        self.names.iter().filter(move |n| accept(n.kind))
    }

    /// Whether the face carries `text` under a name kind the predicate accepts.
    /// Comparison is the catalogue's folded form — case- and
    /// whitespace-insensitive, punctuation significant.
    pub fn answers_to(&self, folded: &str, accept: impl Fn(NameKind) -> bool + Copy) -> bool {
        self.names_matching(accept)
            .any(|n| fold_name(&n.text) == folded)
    }

    /// Whether this face is a variable instance whose coordinates would have to
    /// be baked into the embedded font bytes for a PDF to render it correctly.
    ///
    /// The default location needs no baking: it is what the unmodified bytes
    /// already draw. Everything else does — see
    /// `SubsetOutcome::VariableInstanceNotBaked`.
    pub fn needs_instancing(&self) -> bool {
        self.instance.is_some()
    }
}

/// Fold a face name to its comparison form.
///
/// Runs of whitespace collapse to one space and case is discarded, because
/// producers are inconsistent about both — `"Proxima   Nova SEMIBOLD"` and
/// `"Proxima Nova Semibold"` are the same request. Punctuation is *kept*:
/// `"Arial-BoldMT"` and `"ArialBoldMT"` are different PostScript names, and
/// folding the hyphen away would merge two distinct faces.
pub fn fold_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, word) in name.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.extend(word.chars().flat_map(char::to_lowercase));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn face(family: &str, weight: i32, names: Vec<FaceName>) -> FaceRecord {
        FaceRecord {
            identity: FaceIdentity::System {
                family: family.to_owned(),
                face_index: 0,
            },
            canonical_family: family.to_owned(),
            typographic_family: None,
            intrinsic: IntrinsicStyle {
                weight,
                ..IntrinsicStyle::NEUTRAL
            },
            names,
            instance: None,
        }
    }

    // ── name folding ─────────────────────────────────────────────────────

    #[test]
    fn folding_ignores_case_and_repeated_whitespace() {
        assert_eq!(
            fold_name("  Proxima   Nova SEMIBOLD "),
            fold_name("proxima nova semibold")
        );
        assert_eq!(fold_name("A\tB\nC"), "a b c");
    }

    /// Punctuation distinguishes real PostScript names, so folding must not
    /// discard it.
    #[test]
    fn folding_preserves_punctuation() {
        assert_ne!(fold_name("Arial-BoldMT"), fold_name("Arial BoldMT"));
        assert_ne!(fold_name("A-B"), fold_name("AB"));
    }

    /// Lowercasing has to be Unicode-aware: a localized Cyrillic or Greek name
    /// is exactly the case this exists for.
    #[test]
    fn folding_lowercases_beyond_ascii() {
        assert_eq!(fold_name("ПТ Санс"), fold_name("пт санс"));
        assert_eq!(fold_name("ΑΒΓ"), "αβγ");
    }

    #[test]
    fn folding_an_empty_or_blank_name_yields_empty() {
        assert_eq!(fold_name(""), "");
        assert_eq!(fold_name("   \t "), "");
    }

    // ── NameKind ─────────────────────────────────────────────────────────

    /// The distinction the whole chain rests on: only the font's own records
    /// count as evidence.
    #[test]
    fn only_table_names_are_asserted_by_the_font() {
        assert!(NameKind::Table(NameId::PostScript).is_asserted());
        assert!(NameKind::Table(NameId::Family).is_asserted());
        assert!(!NameKind::ManagerFamily.is_asserted());
        assert!(!NameKind::ManagerFace.is_asserted());
        assert!(!NameKind::Instance.is_asserted());
        assert!(!NameKind::ComposedStyle.is_asserted());
    }

    /// Step 4 matches only names that identify exactly one face. A family name
    /// is shared across the family and cannot select within it.
    #[test]
    fn only_full_compatible_full_and_postscript_are_primary_identities() {
        assert!(NameKind::Table(NameId::Full).is_primary_identity());
        assert!(NameKind::Table(NameId::CompatibleFull).is_primary_identity());
        assert!(NameKind::Table(NameId::PostScript).is_primary_identity());

        assert!(!NameKind::Table(NameId::Family).is_primary_identity());
        assert!(!NameKind::Table(NameId::TypographicFamily).is_primary_identity());
        assert!(!NameKind::Table(NameId::WwsFamily).is_primary_identity());
        assert!(!NameKind::Table(NameId::Subfamily).is_primary_identity());
        assert!(!NameKind::ManagerFace.is_primary_identity());
    }

    // ── IntrinsicStyle ───────────────────────────────────────────────────

    /// A bare family name implies Regular, and that is what keeps every
    /// existing document resolving as it did.
    #[test]
    fn the_neutral_style_is_upright_regular() {
        let n = IntrinsicStyle::NEUTRAL;
        assert_eq!(n.weight, 400);
        assert_eq!(n.width, Width::NORMAL);
        assert_eq!(n.slant, Slant::Upright);
        assert!(!n.bold_slot);
        assert!(n.regular_slot);
        assert!(!n.is_italic());
    }

    #[test]
    fn a_font_style_round_trips_through_intrinsic() {
        let style = FontStyle::new(Weight::from(342), Width::CONDENSED, Slant::Italic);
        let i = IntrinsicStyle::from_font_style(style);
        assert_eq!(i.weight, 342);
        assert_eq!(i.width, Width::CONDENSED);
        assert_eq!(i.slant, Slant::Italic);
        assert!(i.is_italic());
        assert_eq!(i.font_style(), style);
    }

    /// Derived from a `FontStyle` there is no `fsSelection`, so the slot flags
    /// are inferred — and inferred at 700, not at the 600 the *embedded-variant*
    /// bucketing uses. The two answer different questions.
    #[test]
    fn slot_flags_inferred_from_a_font_style_use_the_bold_threshold() {
        let at = |w: i32| {
            IntrinsicStyle::from_font_style(FontStyle::new(
                Weight::from(w),
                Width::NORMAL,
                Slant::Upright,
            ))
        };
        assert!(!at(600).bold_slot, "Semibold does not claim the Bold slot");
        assert!(at(700).bold_slot);
        assert!(at(900).bold_slot);
        assert!(at(400).regular_slot);
        assert!(!at(500).regular_slot);
    }

    #[test]
    fn oblique_counts_as_italic() {
        let oblique = IntrinsicStyle {
            slant: Slant::Oblique,
            ..IntrinsicStyle::NEUTRAL
        };
        assert!(oblique.is_italic());
    }

    /// The legacy slot bit alone: a face can claim it well under weight 700.
    #[test]
    fn the_bold_slot_bit_alone_counts_as_already_bold() {
        let semibold_bold_slot = IntrinsicStyle {
            weight: 600,
            bold_slot: true,
            ..IntrinsicStyle::NEUTRAL
        };
        assert!(semibold_bold_slot.is_already_bold());
    }

    /// Weight alone, with no bit: issue #115's motivating case — a family
    /// with weights past Bold (ExtraBold, Black) may leave the bit set only
    /// on the literal Bold face, since the bit names one slot in the legacy
    /// four-style model, not "at least this heavy".
    #[test]
    fn a_weight_past_bold_alone_counts_as_already_bold() {
        let black_no_bit = IntrinsicStyle {
            weight: *Weight::BLACK,
            bold_slot: false,
            ..IntrinsicStyle::NEUTRAL
        };
        assert!(black_no_bit.is_already_bold());
    }

    #[test]
    fn neither_signal_present_is_not_already_bold() {
        let semibold = IntrinsicStyle {
            weight: 600,
            bold_slot: false,
            ..IntrinsicStyle::NEUTRAL
        };
        assert!(!semibold.is_already_bold());
    }

    // ── FaceRecord matching ──────────────────────────────────────────────

    #[test]
    fn a_face_answers_to_its_names_under_the_accepted_kinds() {
        let f = face(
            "Proxima Nova",
            600,
            vec![
                FaceName::asserted(NameId::PostScript, "ProximaNova-Semibold"),
                FaceName::composed(NameKind::ManagerFace, "Proxima Nova Semibold"),
            ],
        );

        assert!(f.answers_to(&fold_name("proximanova-semibold"), NameKind::is_asserted));
        assert!(
            !f.answers_to(&fold_name("Proxima Nova Semibold"), NameKind::is_asserted),
            "a composed name is not evidence and must not match an evidence-only step"
        );
        assert!(f.answers_to(&fold_name("Proxima Nova Semibold"), |_| true));
    }

    #[test]
    fn names_matching_filters_by_kind() {
        let f = face(
            "Inter",
            700,
            vec![
                FaceName::asserted(NameId::Full, "Inter Bold"),
                FaceName::asserted(NameId::Family, "Inter"),
                FaceName::composed(NameKind::ComposedStyle, "Inter Bold Condensed"),
            ],
        );
        let primary: Vec<&str> = f
            .names_matching(NameKind::is_primary_identity)
            .map(|n| n.text.as_str())
            .collect();
        assert_eq!(primary, vec!["Inter Bold"]);
    }

    /// Only a non-default instance needs its coordinates baked; the default
    /// location is what the unmodified bytes already draw.
    #[test]
    fn only_an_instance_record_needs_instancing() {
        let mut f = face("Fixture Sans", 400, vec![]);
        assert!(!f.needs_instancing());

        f.instance = Some(VariationInstance {
            subfamily: "SemiBold".into(),
            post_script_name: None,
            coords: vec![VariationCoord {
                axis: super::super::opentype::AxisTag::WEIGHT,
                value: 600.0,
            }],
        });
        assert!(f.needs_instancing());
    }

    #[test]
    fn embedded_identity_is_distinguishable() {
        let embedded = FaceIdentity::Embedded {
            font: EmbeddedFontId::from_raw(3),
            collection_index: 1,
        };
        assert!(embedded.is_embedded());
        assert!(!FaceIdentity::System {
            family: "X".into(),
            face_index: 0
        }
        .is_embedded());
    }
}
