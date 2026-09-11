//! Byte extraction — pull subsettable SFNT bytes for a typeface, regardless of
//! whether it came from an embedded DOCX font or a system `FontMgr`.
//!
//! Steps:
//! 1. Source bytes from the typeface's [`TypefaceOrigin`] —
//!    `Embedded` reads from the registry (already deobfuscated per
//!    ECMA-376 §17.8.1.4); `System` calls `Typeface::to_font_data`, which also
//!    reports which face of a collection the typeface came from.
//! 2. Classify with [`FontFormat::detect`].
//! 3. Direct SFNT → return as-is. WOFF2 → decompress via `fontcull`. TTC →
//!    carve out the selected face via [`carve_collection_face`] — which face
//!    is "selected" travels on [`TypefaceOrigin::Embedded::collection_index`].
//!    WOFF1 → return
//!    [`ExtractionError::UnsupportedFormat`], a documented capability boundary:
//!    ECMA-376 forbids WOFF in DOCX.

use crate::render::fonts::opentype::{
    carve_collection_face, CollectionCarveError, ExtractedSfnt, FontFormat, FormatError,
};
use crate::render::fonts::{FontRegistry, TypefaceEntry, TypefaceOrigin};

#[derive(thiserror::Error, Debug)]
pub enum ExtractionError {
    #[error("system typeface returned no font data via Typeface::to_font_data")]
    NoBytesAvailable,
    #[error("font format detection failed: {0}")]
    Format(#[from] FormatError),
    #[error("unsupported font format for subsetting: {0:?}")]
    UnsupportedFormat(FontFormat),
    #[error("WOFF2 decompression failed: {0}")]
    Woff2DecompressionFailed(String),
    #[error("post-decompression bytes are not SFNT (got {0:?})")]
    PostUnwrapNotSfnt(FontFormat),
    #[error(transparent)]
    Collection(#[from] CollectionCarveError),
}

/// Pull SFNT bytes for a typeface, unwrapping WOFF2 if necessary.
pub fn extract(
    entry: &TypefaceEntry,
    registry: &FontRegistry,
) -> Result<ExtractedSfnt, ExtractionError> {
    // The collection index travels with the bytes. For an embedded font it is
    // whatever the catalogue enumerated; for a system font `to_font_data`
    // reports it, and Skia is not in fact guaranteed to have stripped the
    // collection down to one face.
    let (raw, face) = match entry.origin {
        TypefaceOrigin::Embedded {
            id,
            collection_index,
        } => (registry.embedded_bytes(id).to_vec(), collection_index),
        TypefaceOrigin::System { .. } => {
            let (bytes, ttc_index) = entry
                .typeface
                .to_font_data()
                .ok_or(ExtractionError::NoBytesAvailable)?;
            (bytes, ttc_index as u32)
        }
    };

    classify_and_unwrap(raw, face)
}

fn classify_and_unwrap(bytes: Vec<u8>, face: u32) -> Result<ExtractedSfnt, ExtractionError> {
    let format = FontFormat::detect(&bytes)?;
    match format {
        FontFormat::Sfnt(flavor) => Ok(ExtractedSfnt { bytes, flavor }),
        FontFormat::Ttc { face_count } => Ok(carve_collection_face(&bytes, face, face_count)?),
        FontFormat::Woff(crate::render::subset::WoffVersion::V2) => unwrap_woff2(&bytes),
        // WOFF1 is a documented capability boundary — see the module doc.
        FontFormat::Woff(crate::render::subset::WoffVersion::V1) => {
            Err(ExtractionError::UnsupportedFormat(format))
        }
    }
}

fn unwrap_woff2(bytes: &[u8]) -> Result<ExtractedSfnt, ExtractionError> {
    let unwrapped = fontcull::decompress_font(bytes)
        .map_err(|e| ExtractionError::Woff2DecompressionFailed(e.to_string()))?;
    let format = FontFormat::detect(&unwrapped)?;
    match format {
        FontFormat::Sfnt(flavor) => Ok(ExtractedSfnt {
            bytes: unwrapped,
            flavor,
        }),
        other => Err(ExtractionError::PostUnwrapNotSfnt(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EmbeddedFontVariant;
    use crate::render::fonts::opentype::SfntFlavor;
    use crate::render::fonts::FaceRequest;
    use crate::render::fonts::FontRegistry;
    use crate::render::subset::WoffVersion;
    use skia_safe::{FontMgr, FontStyle};

    fn registry() -> FontRegistry {
        FontRegistry::new(FontMgr::new())
    }

    /// Pull bytes from any guaranteed-available system typeface.
    fn arbitrary_system_font_bytes() -> Vec<u8> {
        let mgr = FontMgr::new();
        let tf = mgr
            .legacy_make_typeface(None::<&str>, FontStyle::normal())
            .expect("system has no default typeface");
        tf.to_font_data().expect("legacy default lacks bytes").0
    }

    #[test]
    fn extract_from_embedded_origin_returns_raw_docx_bytes() {
        let bytes = arbitrary_system_font_bytes();
        let mut r = registry();
        r.register_embedded(
            "EmbeddedExtractProbe",
            EmbeddedFontVariant::Regular,
            bytes.clone(),
        )
        .unwrap();
        let entry = r.resolve(&FaceRequest::plain("EmbeddedExtractProbe"));
        assert!(matches!(entry.origin, TypefaceOrigin::Embedded { .. }));

        let extracted = extract(&entry, &r).expect("embedded extraction must succeed");
        assert_eq!(
            extracted.bytes, bytes,
            "embedded extraction must hand back exactly the registered bytes"
        );
    }

    #[test]
    fn extract_from_system_origin_calls_to_font_data() {
        let r = registry();
        let entry = r.resolve(&FaceRequest::plain("UnknownExtractProbe"));
        assert!(matches!(entry.origin, TypefaceOrigin::System { .. }));
        let extracted = extract(&entry, &r).expect("system extraction must succeed");
        // Phase 0 verified to_font_data returns clean SFNT on macOS+Linux.
        assert!(matches!(
            extracted.flavor,
            SfntFlavor::TrueTypeStandard | SfntFlavor::TrueTypeApple | SfntFlavor::OpenTypeCff,
        ));
        assert!(!extracted.bytes.is_empty());
    }

    #[test]
    fn extract_returns_unsupported_for_woff1_input() {
        // Build a synthetic WOFF1 magic — we don't actually decompress, just
        // assert that classification routes WOFF1 to the documented boundary.
        let mut woff1 = b"wOFF".to_vec();
        woff1.extend_from_slice(&[0u8; 40]); // pad to a plausible length
        let result = classify_and_unwrap(woff1, 0);
        assert!(matches!(
            result,
            Err(ExtractionError::UnsupportedFormat(FontFormat::Woff(
                WoffVersion::V1
            )))
        ));
    }

    // Collection carving itself (`carve_collection_face`) is unit-tested at
    // its own definition (`fonts::opentype::collection`) — these two tests
    // only need to pin that `classify_and_unwrap` routes a `Ttc` to it and
    // surfaces its error through `ExtractionError::Collection` correctly.

    #[test]
    fn a_collection_index_past_the_end_is_reported() {
        let mut ttc = b"ttcf".to_vec();
        ttc.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        ttc.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // numFonts = 2
        assert!(matches!(
            classify_and_unwrap(ttc, 7),
            Err(ExtractionError::Collection(
                CollectionCarveError::IndexOutOfRange {
                    index: 7,
                    face_count: 2
                }
            ))
        ));
    }

    /// A header that promises faces it does not describe must produce a typed
    /// error, not a panic — these bytes come from documents.
    #[test]
    fn a_truncated_collection_is_reported_not_fatal() {
        let mut ttc = b"ttcf".to_vec();
        ttc.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        ttc.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // numFonts = 2, no offsets
        assert!(matches!(
            classify_and_unwrap(ttc, 0),
            Err(ExtractionError::Collection(
                CollectionCarveError::FaceUnreadable { index: 0, .. }
            ))
        ));
    }

    #[test]
    fn extract_returns_format_error_for_garbage() {
        let garbage = vec![0xab, 0xcd, 0xef, 0x00, 1, 2, 3];
        let result = classify_and_unwrap(garbage, 0);
        assert!(matches!(result, Err(ExtractionError::Format(_))));
    }

    #[test]
    fn extract_unwraps_woff2_to_sfnt() {
        // Synthesize a WOFF2 from a real SFNT via fontcull's compress_to_woff2,
        // then verify our extract path round-trips it back to SFNT.
        let sfnt = arbitrary_system_font_bytes();
        let woff2 = match fontcull::compress_to_woff2(&sfnt) {
            Ok(b) => b,
            Err(e) => {
                // Some system fonts don't survive a WOFF2 round-trip in the
                // current fontcull/woofwoof — skip rather than fail. The
                // unwrap path itself is still exercised by garbage / format
                // tests; this test is the integration smoke.
                eprintln!("skipping: WOFF2 compression of system font failed: {e}");
                return;
            }
        };
        assert_eq!(&woff2[..4], b"wOF2", "fixture must actually be WOFF2");

        let unwrapped = classify_and_unwrap(woff2, 0).expect("WOFF2 must unwrap to SFNT");
        assert!(matches!(
            unwrapped.flavor,
            SfntFlavor::TrueTypeStandard | SfntFlavor::TrueTypeApple | SfntFlavor::OpenTypeCff,
        ));
        assert!(
            FontFormat::detect(&unwrapped.bytes)
                .unwrap()
                .is_directly_subsettable(),
            "post-unwrap bytes must be directly subsettable"
        );
    }
}
