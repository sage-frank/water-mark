//! The OpenType `OS/2` table — a face's own weight, width and style bits.
//!
//! This is the table that ends the guessing. `"Proxima Nova Semibold"` is
//! weight 600 because `usWeightClass` says 600, not because the string ends in
//! a word a lookup table associates with 600. Where the two disagree — and they
//! do, for fonts whose family name embeds a style word — the font wins.
//!
//! Only the identity fields are read. `OS/2` also carries vertical metrics,
//! Unicode range bitmaps and embedding permissions; layout takes its metrics
//! from Skia and subsetting from `fontcull`, so reading them here would be
//! duplicate sources of truth for values nothing in this module decides.

use skia_safe::font_style::{Slant, Weight, Width};
use skia_safe::FontStyle;

use super::{MalformedTable, Reader};

const TABLE: &str = "OS/2";

/// Offsets of the fields read here. `fsSelection` at 62 is the last of them, so
/// a table shorter than 64 bytes cannot answer the questions this module asks —
/// even though a complete version-0 table is 78.
const USWEIGHTCLASS: usize = 4;
const USWIDTHCLASS: usize = 6;
const FSSELECTION: usize = 62;
const MIN_SIZE: usize = FSSELECTION + 2;

/// `fsSelection` bits this module acts on (OpenType §OS/2).
const FS_SELECTION_ITALIC: u16 = 1 << 0;
const FS_SELECTION_BOLD: u16 = 1 << 5;
const FS_SELECTION_REGULAR: u16 = 1 << 6;
const FS_SELECTION_OBLIQUE: u16 = 1 << 9;

/// A face's intrinsic style, as the face itself declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Os2Metrics {
    /// `usWeightClass`, clamped to the 1–1000 the spec allows. Note this is a
    /// *finer* scale than the nine `Weight` constants: variable-font instances
    /// legitimately sit at 342 or 587.
    pub weight: i32,
    /// `usWidthClass` 1–9, mapped onto Skia's identically-scaled `Width`.
    pub width: Width,
    /// Italic or oblique per `fsSelection`. The two bits are distinct in the
    /// font and collapse here because Skia's `Slant` is what selection compares
    /// against, and `Slant::Oblique` is not something `FontMgr` matching
    /// distinguishes in practice.
    pub slant: Slant,
    /// `fsSelection` bit 5. Kept separately from `weight` because a face can
    /// set the bold bit at weight 600, and the bit is what the legacy
    /// four-slot family model keys on.
    pub bold_bit: bool,
    /// `fsSelection` bit 6 — the face declares itself the family's Regular.
    pub regular_bit: bool,
}

impl Os2Metrics {
    /// The Skia style this face *is*, as opposed to any style requested of it.
    pub fn font_style(&self) -> FontStyle {
        FontStyle::new(Weight::from(self.weight), self.width, self.slant)
    }
}

/// Weight values outside this range are a broken font, not a lighter or heavier
/// face. OpenType 1.8 widened the field to 1–1000 for variable fonts; older
/// specs said 100–900 in steps of 100, and plenty of shipping fonts write 0.
const MIN_WEIGHT: i32 = 1;
const MAX_WEIGHT: i32 = 1000;

/// Fallback when `usWeightClass` is outside 1–1000 — a zero or 65535 weight is
/// a producer bug, and treating it as Regular is what every other consumer does.
const FALLBACK_WEIGHT: i32 = 400;

pub fn read(bytes: &[u8]) -> Result<Os2Metrics, MalformedTable> {
    let r = Reader::new(bytes, TABLE);
    r.require(MIN_SIZE)?;

    let raw_weight = i32::from(r.u16_at(USWEIGHTCLASS, "usWeightClass")?);
    let weight = if (MIN_WEIGHT..=MAX_WEIGHT).contains(&raw_weight) {
        raw_weight
    } else {
        FALLBACK_WEIGHT
    };

    let width = width_from_class(r.u16_at(USWIDTHCLASS, "usWidthClass")?);

    let fs_selection = r.u16_at(FSSELECTION, "fsSelection")?;
    let slant = if fs_selection & FS_SELECTION_OBLIQUE != 0 {
        Slant::Oblique
    } else if fs_selection & FS_SELECTION_ITALIC != 0 {
        Slant::Italic
    } else {
        Slant::Upright
    };

    Ok(Os2Metrics {
        weight,
        width,
        slant,
        bold_bit: fs_selection & FS_SELECTION_BOLD != 0,
        regular_bit: fs_selection & FS_SELECTION_REGULAR != 0,
    })
}

/// `usWidthClass` 1–9 onto Skia's `Width`, which uses the same 1–9 scale.
/// Out-of-range values fall back to normal rather than being clamped to an
/// extreme: a font writing 0 means "unset", not "ultra-condensed".
fn width_from_class(class: u16) -> Width {
    match class {
        1 => Width::ULTRA_CONDENSED,
        2 => Width::EXTRA_CONDENSED,
        3 => Width::CONDENSED,
        4 => Width::SEMI_CONDENSED,
        5 => Width::NORMAL,
        6 => Width::SEMI_EXPANDED,
        7 => Width::EXPANDED,
        8 => Width::EXTRA_EXPANDED,
        9 => Width::ULTRA_EXPANDED,
        _ => Width::NORMAL,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build an `OS/2` table carrying the identity fields under test. The rest
    /// is zero — this reader touches nothing else, and a test that filled in
    /// vertical metrics would be asserting something it does not read.
    pub(crate) fn build_os2(weight: u16, width_class: u16, fs_selection: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 78];
        bytes[USWEIGHTCLASS..USWEIGHTCLASS + 2].copy_from_slice(&weight.to_be_bytes());
        bytes[USWIDTHCLASS..USWIDTHCLASS + 2].copy_from_slice(&width_class.to_be_bytes());
        bytes[FSSELECTION..FSSELECTION + 2].copy_from_slice(&fs_selection.to_be_bytes());
        bytes
    }

    #[test]
    fn reads_weight_width_and_slant() {
        let m = read(&build_os2(600, 3, FS_SELECTION_ITALIC)).unwrap();
        assert_eq!(m.weight, 600);
        assert_eq!(m.width, Width::CONDENSED);
        assert_eq!(m.slant, Slant::Italic);
        assert!(!m.bold_bit);
        assert!(!m.regular_bit);
    }

    /// The point of the whole module: a face's declared weight is read, not
    /// inferred from nine canonical steps. Variable instances land between them.
    #[test]
    fn an_off_step_weight_survives_intact() {
        assert_eq!(read(&build_os2(342, 5, 0)).unwrap().weight, 342);
        assert_eq!(read(&build_os2(587, 5, 0)).unwrap().weight, 587);
        assert_eq!(read(&build_os2(1000, 5, 0)).unwrap().weight, 1000);
        assert_eq!(read(&build_os2(1, 5, 0)).unwrap().weight, 1);
    }

    /// A zero or absurd `usWeightClass` is a producer bug. Treating it as
    /// Regular keeps a broken font usable; propagating it would make every
    /// weight comparison against that face nonsense.
    #[test]
    fn an_out_of_range_weight_falls_back_to_regular() {
        assert_eq!(read(&build_os2(0, 5, 0)).unwrap().weight, 400);
        assert_eq!(read(&build_os2(65535, 5, 0)).unwrap().weight, 400);
        assert_eq!(read(&build_os2(1001, 5, 0)).unwrap().weight, 400);
    }

    #[test]
    fn width_class_maps_across_the_whole_scale() {
        let width = |class| read(&build_os2(400, class, 0)).unwrap().width;
        assert_eq!(width(1), Width::ULTRA_CONDENSED);
        assert_eq!(width(5), Width::NORMAL);
        assert_eq!(width(9), Width::ULTRA_EXPANDED);
    }

    /// An unset width class means "unset", not "narrowest".
    #[test]
    fn an_out_of_range_width_class_is_normal() {
        assert_eq!(read(&build_os2(400, 0, 0)).unwrap().width, Width::NORMAL);
        assert_eq!(read(&build_os2(400, 10, 0)).unwrap().width, Width::NORMAL);
    }

    #[test]
    fn oblique_outranks_italic_when_both_bits_are_set() {
        let both = FS_SELECTION_ITALIC | FS_SELECTION_OBLIQUE;
        assert_eq!(
            read(&build_os2(400, 5, both)).unwrap().slant,
            Slant::Oblique
        );
        assert_eq!(
            read(&build_os2(400, 5, FS_SELECTION_OBLIQUE))
                .unwrap()
                .slant,
            Slant::Oblique
        );
    }

    /// The bold bit is independent of the weight class, and both are kept: a
    /// face at 600 that sets the bold bit is the family's Bold slot even though
    /// 600 is Semibold on the numeric scale.
    #[test]
    fn the_bold_bit_is_kept_separately_from_the_weight() {
        let m = read(&build_os2(600, 5, FS_SELECTION_BOLD)).unwrap();
        assert_eq!(m.weight, 600);
        assert!(m.bold_bit);

        let m = read(&build_os2(700, 5, 0)).unwrap();
        assert_eq!(m.weight, 700);
        assert!(!m.bold_bit, "weight 700 alone does not set the bit");
    }

    #[test]
    fn the_regular_bit_is_reported() {
        assert!(
            read(&build_os2(400, 5, FS_SELECTION_REGULAR))
                .unwrap()
                .regular_bit
        );
    }

    #[test]
    fn font_style_reflects_the_face_not_a_request() {
        let m = read(&build_os2(342, 3, FS_SELECTION_ITALIC)).unwrap();
        let style = m.font_style();
        assert_eq!(*style.weight(), 342);
        assert_eq!(style.width(), Width::CONDENSED);
        assert_eq!(style.slant(), Slant::Italic);
    }

    /// The fields end at byte 64; anything shorter cannot be read even though
    /// a complete version-0 table is 78 bytes.
    #[test]
    fn a_table_too_short_for_fs_selection_is_an_error() {
        assert_eq!(
            read(&[0u8; 63]).unwrap_err(),
            MalformedTable::TooShort {
                table: "OS/2",
                needed: 64,
                actual: 63
            }
        );
        assert!(read(&[0u8; 64]).is_ok(), "64 bytes is exactly enough");
    }

    #[test]
    fn an_empty_table_is_an_error_not_a_panic() {
        assert!(read(&[]).is_err());
    }
}
