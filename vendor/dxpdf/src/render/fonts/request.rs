//! What a document asked for, and what that means once a face has been found.
//!
//! # The tri-state
//!
//! ECMA-376 §17.3.2.1 (`w:b`) and §17.3.2.16 (`w:i`) are toggle properties, and
//! §17.7.2 gives them three states, not two: absent from the whole cascade,
//! explicitly on, explicitly off. The model preserves all three as
//! `Option<bool>` — and then the render side collapsed them to `bool` at a
//! single line in `fragment::font_props_from_run`, two layers above font
//! resolution.
//!
//! That collapse is why resolution used to need a special case. With a `bool`,
//! "not bold" arrives as a *request for weight 400*, indistinguishable from a
//! document that genuinely wants Regular. So a face name carrying its own weight
//! — `"Calibri Light"` at 342 — was overruled by a default that meant nothing,
//! and the old `merged_alias_weight` had to carry a comment explaining that the
//! requested weight "is not really a weight" and must be ignored when it is
//! `NORMAL`.
//!
//! [`Toggle`] removes the need for that reasoning. [`Absent`](Toggle::Absent)
//! asks for no weight at all, so the face's own weight stands; only
//! [`On`](Toggle::On) asks for one.
//!
//! # Absent and Off select the same face
//!
//! Deliberately, and it is worth being explicit about why the third state still
//! earns its place. The two differ *during* the §17.7.2 cascade, where an
//! explicit `w:val="0"` must override an inherited `w:b` — the model already
//! handles that, and it is what produces `Off` here rather than `Absent`. At
//! face selection they agree: neither asks for extra weight, so both leave the
//! matched face's intrinsic weight alone.
//!
//! The one behaviour that would separate them is *synthetic* emboldening — Word
//! thickens a face when `w:b` is set and no bolder face exists. [`Synthesis`]
//! is where that now lives (issue #115), and it settles what this section used
//! to only speculate about: gated on [`Toggle::On`] specifically, `Off` and
//! `Absent` both decline to synthesise, for the same reason they already agree
//! at selection — neither is asking for anything. Not a special case written
//! for `Off`; [`Synthesis::compute`] never asks [`Toggle::is_on`], only
//! `== Toggle::On`, which already excludes both. Modelling the third state
//! still earns its place for the reason above — the §17.7.2 cascade itself
//! needs it — even though, here too, it produces the same answer as `Absent`,
//! not because one collapsed into the other but because a fully resolved
//! "not bold" is not bold in Word regardless of how it got there.

use skia_safe::font_style::{Slant, Weight, Width};
use skia_safe::FontStyle;

use super::face::IntrinsicStyle;

/// A §17.7.2 toggle property as it reaches face selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Toggle {
    /// No `w:b` / `w:i` anywhere in the cascade. The matched face's own style
    /// stands.
    #[default]
    Absent,
    /// `<w:b w:val="0"/>` — the run explicitly declines the toggle.
    Off,
    /// `<w:b/>` or `<w:b w:val="1"/>`.
    On,
}

impl Toggle {
    /// Build from the model's `Option<bool>`, which is the shape the §17.7.2
    /// cascade produces.
    pub fn from_option(value: Option<bool>) -> Self {
        match value {
            None => Self::Absent,
            Some(false) => Self::Off,
            Some(true) => Self::On,
        }
    }

    /// Whether the toggle is asking for something, as opposed to leaving the
    /// face alone. `Off` is not asking — see the module doc.
    pub fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// A request for a face, as the document expressed it.
///
/// `name` is the `w:rFonts` value after theme resolution (§17.3.2.26) — it may
/// name a family, a face, a PostScript name, or something that is none of those.
/// Deciding which is the resolver's job, not the caller's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FaceRequest<'a> {
    pub name: &'a str,
    pub bold: Toggle,
    pub italic: Toggle,
}

impl<'a> FaceRequest<'a> {
    pub fn new(name: &'a str, bold: Toggle, italic: Toggle) -> Self {
        Self { name, bold, italic }
    }

    /// A request that names a family and asks for nothing else.
    pub fn plain(name: &'a str) -> Self {
        Self::new(name, Toggle::Absent, Toggle::Absent)
    }
}

/// The style to select, once the request's name has been matched against
/// something with a known intrinsic style.
///
/// Not a `FontStyle` because the difference between "the request pinned this
/// weight" and "this is just the base the toggles were applied to" matters when
/// ranking candidate faces — see [`EffectiveStyle::weight_is_requested`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveStyle {
    pub weight: i32,
    pub width: Width,
    pub slant: Slant,
    /// `w:b` was explicitly on, so `weight` is a floor the request set rather
    /// than the base face's own weight.
    weight_requested: bool,
    /// `w:i` was explicitly on.
    slant_requested: bool,
}

impl EffectiveStyle {
    /// Apply a request's toggles to the style of the thing its name matched.
    ///
    /// `base` is [`IntrinsicStyle::NEUTRAL`] when the name matched a *family*,
    /// which carries no style of its own, and the matched face's own intrinsic
    /// style when it matched a face. That split is the whole behavioural change:
    /// a bare `"Calibri"` still resolves to Regular exactly as before, while
    /// `"Calibri Light"` now starts from 342 instead of being flattened to 400.
    pub fn resolve(request: &FaceRequest<'_>, base: IntrinsicStyle) -> Self {
        // `On` raises a lighter face but never lowers a heavier one: `<w:b/>`
        // on an ExtraBold face wants at least Bold, and flattening 800 to 700
        // would be a downgrade the document did not ask for.
        let weight = if request.bold.is_on() {
            base.weight.max(*Weight::BOLD)
        } else {
            base.weight
        };
        let slant = if request.italic.is_on() {
            Slant::Italic
        } else {
            base.slant
        };
        Self {
            weight,
            width: base.width,
            slant,
            weight_requested: request.bold.is_on(),
            slant_requested: request.italic.is_on(),
        }
    }

    /// Whether the document explicitly asked to be bold, so a candidate face
    /// lighter than [`weight`](Self::weight) is a genuine failure to honour the
    /// request rather than merely a different face.
    pub fn weight_is_requested(&self) -> bool {
        self.weight_requested
    }

    /// Whether the document explicitly asked to be italic.
    pub fn slant_is_requested(&self) -> bool {
        self.slant_requested
    }

    pub fn is_italic(&self) -> bool {
        matches!(self.slant, Slant::Italic | Slant::Oblique)
    }

    /// The Skia style to hand a font manager that matches by style rather than
    /// by index.
    pub fn font_style(&self) -> FontStyle {
        FontStyle::new(Weight::from(self.weight), self.width, self.slant)
    }
}

/// What resolution could not satisfy with a real face — issue #115.
///
/// The input `set_embolden`/`set_skew_x` need, since re-deriving it at paint
/// time would mean guessing at something resolution already knows for
/// certain. Carried on [`super::TypefaceEntry`] alongside the resolved
/// typeface itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Synthesis {
    /// `w:b` was `On` and the selected face is not already bold.
    pub embolden: bool,
    /// `w:i` was `On` and the selected face is not already italic or oblique.
    pub oblique: bool,
}

impl Synthesis {
    /// `selected` is the face resolution actually chose — its *own* intrinsic
    /// style, not [`EffectiveStyle`] (which forces weight/slant toward what
    /// was asked, for ranking; comparing against that would make every `On`
    /// request read as already satisfied).
    ///
    /// Gated on the raw [`Toggle::On`], not
    /// [`EffectiveStyle::weight_is_requested`]/[`EffectiveStyle::slant_is_requested`]:
    /// those answer "should ranking's floor rise", which `Off` and `Absent`
    /// answer identically (see this module's doc) — the same reason a fully
    /// resolved §17.7.2 cascade renders them the same here too, now checked
    /// directly against the tri-state rather than through a value that
    /// already discarded it.
    ///
    /// The [`IntrinsicStyle::is_italic`] check (not `slant == Slant::Italic`)
    /// is deliberate: a face whose `OS/2` already declares `OBLIQUE` must not
    /// be slanted again. [`IntrinsicStyle::is_already_bold`] is the weight
    /// analogue — see its own doc for why the legacy bold-slot bit alone is
    /// not enough.
    pub fn compute(request: &FaceRequest<'_>, selected: &IntrinsicStyle) -> Self {
        Self {
            embolden: request.bold == Toggle::On && !selected.is_already_bold(),
            oblique: request.italic == Toggle::On && !selected.is_italic(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fonts::FaceRequest;
    use crate::render::fonts::Toggle;

    fn at(weight: i32, slant: Slant) -> IntrinsicStyle {
        IntrinsicStyle {
            weight,
            slant,
            ..IntrinsicStyle::NEUTRAL
        }
    }

    // ── the tri-state itself ─────────────────────────────────────────────

    #[test]
    fn the_cascade_s_option_bool_maps_onto_all_three_states() {
        assert_eq!(Toggle::from_option(None), Toggle::Absent);
        assert_eq!(Toggle::from_option(Some(false)), Toggle::Off);
        assert_eq!(Toggle::from_option(Some(true)), Toggle::On);
    }

    /// `Off` declines the toggle; it does not request the absence of weight.
    #[test]
    fn only_on_is_asking_for_something() {
        assert!(Toggle::On.is_on());
        assert!(!Toggle::Off.is_on());
        assert!(!Toggle::Absent.is_on());
    }

    #[test]
    fn absent_is_the_default() {
        assert_eq!(Toggle::default(), Toggle::Absent);
    }

    // ── the behavioural change ───────────────────────────────────────────

    /// The regression this whole tri-state exists to prevent: a face that
    /// carries its own weight must keep it when nothing asked otherwise.
    /// Under the old `bool` this arrived as "weight 400" and flattened 342.
    #[test]
    fn an_unrequested_weight_leaves_a_light_face_light() {
        let base = at(342, Slant::Upright);
        for toggle in [Toggle::Absent, Toggle::Off] {
            let e = EffectiveStyle::resolve(
                &FaceRequest::new("Calibri Light", toggle, Toggle::Absent),
                base,
            );
            assert_eq!(e.weight, 342, "{toggle:?} must not thicken the face");
            assert!(!e.weight_is_requested());
        }
    }

    /// And the guard on the other side: a bare family name still resolves to
    /// Regular, which is what keeps every existing document paginating the same.
    #[test]
    fn a_family_name_with_no_toggles_still_resolves_to_regular() {
        let e = EffectiveStyle::resolve(&FaceRequest::plain("Calibri"), IntrinsicStyle::NEUTRAL);
        assert_eq!(e.weight, 400);
        assert_eq!(e.width, Width::NORMAL);
        assert_eq!(e.slant, Slant::Upright);
        assert_eq!(e.font_style(), FontStyle::normal());
    }

    #[test]
    fn an_explicit_bold_raises_a_lighter_face() {
        let e = EffectiveStyle::resolve(
            &FaceRequest::new("Calibri Light", Toggle::On, Toggle::Absent),
            at(342, Slant::Upright),
        );
        assert_eq!(e.weight, *Weight::BOLD);
        assert!(e.weight_is_requested());
    }

    /// …but never lowers a heavier one. `<w:b/>` on an ExtraBold face means
    /// "at least bold", not "exactly bold".
    #[test]
    fn an_explicit_bold_never_lowers_a_heavier_face() {
        let e = EffectiveStyle::resolve(
            &FaceRequest::new("Inter ExtraBold", Toggle::On, Toggle::Absent),
            at(*Weight::EXTRA_BOLD, Slant::Upright),
        );
        assert_eq!(e.weight, *Weight::EXTRA_BOLD);
    }

    // ── slant ────────────────────────────────────────────────────────────

    #[test]
    fn an_explicit_italic_overrides_an_upright_face() {
        let e = EffectiveStyle::resolve(
            &FaceRequest::new("Inter", Toggle::Absent, Toggle::On),
            at(400, Slant::Upright),
        );
        assert_eq!(e.slant, Slant::Italic);
        assert!(e.is_italic());
        assert!(e.slant_is_requested());
    }

    /// An unrequested slant keeps whatever the matched face has — including
    /// oblique, which must not be silently normalised to italic.
    #[test]
    fn an_unrequested_slant_preserves_the_face_s_own() {
        for toggle in [Toggle::Absent, Toggle::Off] {
            let e = EffectiveStyle::resolve(
                &FaceRequest::new("Inter Oblique", Toggle::Absent, toggle),
                at(400, Slant::Oblique),
            );
            assert_eq!(e.slant, Slant::Oblique, "{toggle:?}");
            assert!(e.is_italic());
            assert!(!e.slant_is_requested());
        }
    }

    #[test]
    fn width_is_carried_through_untouched() {
        let base = IntrinsicStyle {
            width: Width::CONDENSED,
            ..at(600, Slant::Upright)
        };
        let e = EffectiveStyle::resolve(
            &FaceRequest::new("X Condensed SemiBold", Toggle::On, Toggle::On),
            base,
        );
        assert_eq!(
            e.width,
            Width::CONDENSED,
            "no OOXML toggle addresses width, so the face's own must survive"
        );
        assert_eq!(*e.font_style().weight(), *Weight::BOLD);
        assert_eq!(e.font_style().slant(), Slant::Italic);
    }

    #[test]
    fn both_toggles_compose() {
        let e = EffectiveStyle::resolve(
            &FaceRequest::new("Inter", Toggle::On, Toggle::On),
            IntrinsicStyle::NEUTRAL,
        );
        assert_eq!(e.font_style(), FontStyle::bold_italic());
    }

    // ── Synthesis (issue #115) ──────────────────────────────────────────

    #[test]
    fn bold_requested_against_a_light_face_embolds() {
        let s = Synthesis::compute(
            &FaceRequest::new("Dx", Toggle::On, Toggle::Absent),
            &at(400, Slant::Upright),
        );
        assert!(s.embolden);
        assert!(!s.oblique);
    }

    /// Never synthesise over a real face: the bold-slot bit alone is enough
    /// to suppress it, regardless of weight.
    #[test]
    fn bold_requested_against_a_bold_slot_face_does_not_embold() {
        let bold_slot_face = IntrinsicStyle {
            weight: 600,
            bold_slot: true,
            ..IntrinsicStyle::NEUTRAL
        };
        let s = Synthesis::compute(
            &FaceRequest::new("Dx SemiBold", Toggle::On, Toggle::Absent),
            &bold_slot_face,
        );
        assert!(!s.embolden);
    }

    /// The `is_already_bold` refinement, pinned at this layer too: a family
    /// with nothing but Regular and Black ranks Black closest to "at least
    /// Bold" (`EffectiveStyle::resolve`'s own floor-raising), and Black not
    /// setting the legacy bit must not thicken it further.
    #[test]
    fn bold_requested_against_an_unmarked_black_face_does_not_embold() {
        let black_no_bit = IntrinsicStyle {
            weight: *Weight::BLACK,
            bold_slot: false,
            ..IntrinsicStyle::NEUTRAL
        };
        let s = Synthesis::compute(
            &FaceRequest::new("Dx Black", Toggle::On, Toggle::Absent),
            &black_no_bit,
        );
        assert!(!s.embolden);
    }

    /// The acceptance criterion this issue was written to settle: a fully
    /// §17.7.2-cascaded "not bold" is not bold whether it got there via an
    /// explicit `w:val="0"` or plain absence, so both read identically here —
    /// checked explicitly against the raw tri-state, not assumed.
    #[test]
    fn off_and_absent_never_embold_even_against_an_already_bold_face() {
        let already_bold = IntrinsicStyle {
            weight: 600,
            bold_slot: true,
            ..IntrinsicStyle::NEUTRAL
        };
        for toggle in [Toggle::Off, Toggle::Absent] {
            let s = Synthesis::compute(
                &FaceRequest::new("Dx SemiBold", toggle, Toggle::Absent),
                &already_bold,
            );
            assert!(!s.embolden, "{toggle:?} must not synthesise");
        }
    }

    #[test]
    fn italic_requested_against_an_upright_face_obliques() {
        let s = Synthesis::compute(
            &FaceRequest::new("Dx", Toggle::Absent, Toggle::On),
            &at(400, Slant::Upright),
        );
        assert!(s.oblique);
        assert!(!s.embolden);
    }

    /// The other acceptance criterion this issue names explicitly: a face
    /// whose `OS/2` already declares OBLIQUE (not ITALIC) must not be slanted
    /// again — `is_italic` treating the two alike is what this test pins.
    #[test]
    fn italic_requested_against_an_oblique_face_does_not_double_slant() {
        let s = Synthesis::compute(
            &FaceRequest::new("Dx Oblique", Toggle::Absent, Toggle::On),
            &at(400, Slant::Oblique),
        );
        assert!(!s.oblique);
    }

    #[test]
    fn off_and_absent_never_oblique_even_against_an_already_italic_face() {
        let already_italic = at(400, Slant::Italic);
        for toggle in [Toggle::Off, Toggle::Absent] {
            let s = Synthesis::compute(
                &FaceRequest::new("Dx Italic", Toggle::Absent, toggle),
                &already_italic,
            );
            assert!(!s.oblique, "{toggle:?} must not synthesise");
        }
    }

    #[test]
    fn both_toggles_synthesise_together_against_a_fully_plain_face() {
        let s = Synthesis::compute(
            &FaceRequest::new("Dx", Toggle::On, Toggle::On),
            &IntrinsicStyle::NEUTRAL,
        );
        assert!(s.embolden);
        assert!(s.oblique);
    }
}
