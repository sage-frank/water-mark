//! Emoji rendering pipeline.
//!
//! Three stages: cluster classification (UAX #29 + UTS #51) in [`cluster`],
//! host-OS color emoji typeface resolution in [`resolve`], and Skia
//! raster-backend rasterization with a per-render cache in [`raster`];
//! [`crate::render::shape`] drives Skia's HarfBuzz so multi-codepoint
//! sequences ligate. It lives outside this module because body text in a
//! joining script needs the same machinery (issue #131) — one shaper per
//! render, not two.
//!
//! Skia's PDF backend cannot emit colored glyph tables (COLR/CPAL, CBDT/CBLC,
//! sbix, SVG-in-OT) even when the resolved typeface carries them, but its
//! raster backend honours all four — hence rasterize-then-embed rather than
//! a text run.
//!
//! The rest of the renderer interacts with this module through typed ADTs;
//! no string-name allowlists, no font bundling.

pub mod cluster;
pub mod raster;
pub mod resolve;
