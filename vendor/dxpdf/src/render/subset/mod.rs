//! Font subsetting — collect codepoint usage, subset typefaces, replace.
//!
//! The pass runs between layout and paint. Driven by the `subset-fonts` Cargo
//! feature (default-on).
//!
//! Design invariants:
//! - **Single source of truth** for typeface bytes ([`crate::render::fonts::FontRegistry`])
//!   and for usage tracking ([`collect::CodepointUsage`]). Each piece of state
//!   lives in exactly one place.
//! - **Codepoint-driven subsetting.** `fontcull` walks the font's own `cmap` to
//!   derive the glyph closure and keeps `GSUB` substitutions reachable from it,
//!   so ligatures and contextual alternates survive and paint's
//!   `text_to_glyphs`/`cmap` lookups stay valid — no re-shaping needed.
//! - **Shapeability is validated against the original, not in isolation.** A
//!   structurally valid subset can still ship a broken `cmap`; [`apply()`]
//!   rejects a subset that *loses* coverage the original typeface had, and
//!   keeps the original bytes instead. Comparing against the original is what
//!   separates a destroyed cmap from a glyph the font never carried — without
//!   it, one soft hyphen was enough to embed a whole 606 KB face.
//! - **Spec touchpoints.** ECMA-376 §17.8 (DOCX font embedding,
//!   deobfuscation) is enforced upstream by the parser. ISO 32000-1 §9.6.4
//!   subset prefixes (`AAAAAA+`-style) are emitted by Skia's PDF backend at
//!   write time — we only feed it smaller bytes.
//! - **Order is load-bearing.** This pass runs after layout, not before:
//!   layout measures against the *full* typeface, and usage is derived from
//!   what layout produced, so subsetting first would be circular.

// No `#[cfg(feature = "subset-fonts")]` below, and none inside these modules:
// `render::mod` gates `pub mod subset` on that feature, so everything here
// compiles only when it is on and any inner gate is a tautology. The `not(...)`
// arms that used to pair with them were unreachable code carrying justifications
// that did not hold — one claimed Rust "still needs an implementation" for a
// function that does not exist without the feature.
pub mod apply;
pub mod collect;
pub mod extract;
pub mod name_splice;

/// Font-format detection, and collection-face carving, live with the other
/// OpenType readers in [`crate::render::fonts::opentype`], not here: the face
/// catalogue has to spot a TrueType Collection to enumerate its faces, and
/// `FontRegistry` has to carve a face into a standalone SFNT before handing it
/// to a platform that declines a non-zero collection index — both whether or
/// not the `subset-fonts` feature — which gates this whole module — is on.
/// Re-exported so subsetting's own callers keep their existing paths.
pub use crate::render::fonts::opentype::{
    CollectionCarveError, ExtractedSfnt, FontFormat, FormatError, SfntFlavor, WoffVersion,
};
pub use apply::{apply, SubsetOutcome, SubsetReport};
pub use collect::{collect, Codepoint, CodepointUsage};
pub use extract::{extract, ExtractionError};
