//! The OpenType `name` table — a face's own account of what it is called.
//!
//! Every identity the resolver can match on beyond "the family Skia reported"
//! comes from here: the full name a designer prints on a specimen, the
//! PostScript name a PDF embeds, the typographic family that groups eight
//! weights the legacy family had to split into four, and the localized
//! spellings a Russian or Japanese author's copy of Word will have written into
//! `w:rFonts`.
//!
//! # Layout
//!
//! A six-byte header, then `count` twelve-byte records, then a storage area the
//! records index into by byte offset:
//!
//! ```text
//! version u16   count u16   storageOffset u16
//! [ platformID u16  encodingID u16  languageID u16  nameID u16  length u16  stringOffset u16 ] × count
//! (version 1 only) langTagCount u16   [ length u16  langTagOffset u16 ] × langTagCount
//! ... storage area ...
//! ```
//!
//! # Why records are kept rather than reduced
//!
//! A face typically carries the *same* name several times over — once per
//! platform, and again per language. Collapsing to "the family name" at read
//! time would throw away exactly the records the localized-alias requirement
//! needs. So [`read`] keeps every decodable record and lets the catalogue decide
//! which ones to index; [`NameRecords::preferred`] exists for the one caller
//! that genuinely wants a single canonical answer.
//!
//! # Undecodable records are dropped, not fatal
//!
//! Shipping fonts routinely carry records in encodings nobody has needed since
//! Mac OS 9 — platform 1 with a Japanese or Cyrillic encoding id, platform 2
//! (ISO, deprecated by OpenType itself). Those are skipped individually. Only a
//! header that cannot be read at all makes [`read`] return `Err`, because that
//! means the offsets for *every* record are untrustworthy.

use std::collections::BTreeSet;

use super::{MalformedTable, Reader};

const TABLE: &str = "name";
const HEADER_SIZE: usize = 6;
const RECORD_SIZE: usize = 12;
const LANG_TAG_RECORD_SIZE: usize = 4;

/// Language ids at or above this index the format-1 `langTagRecord` array
/// instead of naming a platform-defined language (OpenType `name`, version 1).
const LANG_TAG_BASE: u16 = 0x8000;

/// The name ids that identify a face.
///
/// Modelled as an enum rather than passed around as `u16` so that a resolution
/// step naming `NameKind::PostScript` cannot silently be pointed at name id 6's
/// neighbour. [`Other`](Self::Other) keeps the ids this engine has no use for
/// (copyright, designer, sample text) addressable without enumerating them —
/// `fvar` needs it, since a named instance's `subfamilyNameID` may be any id
/// at or above 256.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameId {
    /// ID 1 — the *legacy* family, constrained to four style slots.
    Family,
    /// ID 2 — the legacy subfamily: Regular, Bold, Italic, Bold Italic.
    Subfamily,
    /// ID 3 — a unique identifier, not a display name. Indexed because some
    /// producers write it where a full name belongs.
    UniqueId,
    /// ID 4 — the full human-readable name, e.g. `"Source Sans 3 SemiBold"`.
    Full,
    /// ID 6 — the PostScript name, e.g. `"SourceSans3-Semibold"`.
    PostScript,
    /// ID 16 — the typographic (a.k.a. preferred) family that groups every
    /// weight, e.g. `"Source Sans 3"` for all nine.
    TypographicFamily,
    /// ID 17 — the typographic subfamily, e.g. `"SemiBold Italic"`.
    TypographicSubfamily,
    /// ID 18 — Macintosh-only compatible full name, present when ID 4 had to be
    /// something else for legacy menu grouping.
    CompatibleFull,
    /// ID 21 — WWS family: the family with weight, width and slant factored out.
    WwsFamily,
    /// ID 22 — WWS subfamily.
    WwsSubfamily,
    /// Any other id, kept raw.
    Other(u16),
}

impl NameId {
    pub fn from_raw(raw: u16) -> Self {
        match raw {
            1 => Self::Family,
            2 => Self::Subfamily,
            3 => Self::UniqueId,
            4 => Self::Full,
            6 => Self::PostScript,
            16 => Self::TypographicFamily,
            17 => Self::TypographicSubfamily,
            18 => Self::CompatibleFull,
            21 => Self::WwsFamily,
            22 => Self::WwsSubfamily,
            other => Self::Other(other),
        }
    }

    pub fn raw(self) -> u16 {
        match self {
            Self::Family => 1,
            Self::Subfamily => 2,
            Self::UniqueId => 3,
            Self::Full => 4,
            Self::PostScript => 6,
            Self::TypographicFamily => 16,
            Self::TypographicSubfamily => 17,
            Self::CompatibleFull => 18,
            Self::WwsFamily => 21,
            Self::WwsSubfamily => 22,
            Self::Other(raw) => raw,
        }
    }
}

/// `name` table platform id (OpenType §name).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlatformId {
    /// 0 — Unicode. Strings are UTF-16BE.
    Unicode,
    /// 1 — Macintosh. Encoding 0 is Mac Roman; other encodings are legacy
    /// codepages this reader declines.
    Macintosh,
    /// 2 — ISO. Deprecated by OpenType; declined.
    Iso,
    /// 3 — Windows. Strings are UTF-16BE.
    Windows,
    Other(u16),
}

impl PlatformId {
    fn from_raw(raw: u16) -> Self {
        match raw {
            0 => Self::Unicode,
            1 => Self::Macintosh,
            2 => Self::Iso,
            3 => Self::Windows,
            other => Self::Other(other),
        }
    }
}

/// Which language a record is written in, reduced to the distinction the
/// resolver actually acts on.
///
/// Face selection matches a requested name against *every* record regardless of
/// language — an author whose Word wrote a Cyrillic family name gets that name
/// matched. Language only decides which record is *canonical* when a single
/// answer is needed, and what a diagnostic prints. So this deliberately does not
/// model the full LCID space; it answers "is this the primary (English) record?"
/// and carries a BCP 47 tag when one is derivable.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Language {
    /// English — which OpenType treats as a face's primary language, and which
    /// every conforming font is required to provide for ids 1–6.
    English,
    /// Anything else. `tag` is a BCP 47 tag when this module's `LANGUAGE_TAGS` knows the
    /// platform language id, or the explicit tag from a version-1
    /// `langTagRecord`; `None` when the id is one this table does not carry.
    Localized { tag: Option<String> },
}

impl Language {
    pub fn is_english(&self) -> bool {
        matches!(self, Self::English)
    }
}

/// One decoded record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRecord {
    pub id: NameId,
    pub platform: PlatformId,
    pub language: Language,
    pub text: String,
}

/// Every decodable record in one face's `name` table, in table order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameRecords {
    records: Vec<NameRecord>,
}

impl NameRecords {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &NameRecord> {
        self.records.iter()
    }

    /// Every record carrying `id`, in table order.
    pub fn all(&self, id: NameId) -> impl Iterator<Item = &NameRecord> {
        self.records.iter().filter(move |r| r.id == id)
    }

    /// The canonical text for `id`: the English record if there is one, else the
    /// first record present.
    ///
    /// "English first" is not a preference for English — it is what OpenType
    /// defines as the primary record, and it is what `Typeface::family_name()`
    /// reports on both CoreText and fontconfig. Agreeing with the platform here
    /// is what keeps a face record's canonical family comparable
    /// with a Skia-reported family name.
    pub fn preferred(&self, id: NameId) -> Option<&str> {
        self.all(id)
            .find(|r| r.language.is_english())
            .or_else(|| self.all(id).next())
            .map(|r| r.text.as_str())
    }

    /// Distinct texts carried under `id`, deduplicated but order-independent.
    ///
    /// The same name is usually present two to four times over (Macintosh and
    /// Windows platforms, sometimes several English locales). Alias indexing
    /// wants each spelling once.
    pub fn distinct(&self, id: NameId) -> BTreeSet<&str> {
        self.all(id).map(|r| r.text.as_str()).collect()
    }
}

/// Decode a `name` table.
///
/// Returns `Err` only when the header itself is unreadable — at that point no
/// record offset can be trusted. Individual records in encodings this reader
/// declines, or whose string offsets fall outside the table, are skipped; see
/// the module doc for why that is the common case rather than an anomaly.
pub fn read(bytes: &[u8]) -> Result<NameRecords, MalformedTable> {
    let mut r = Reader::new(bytes, TABLE);
    r.require(HEADER_SIZE)?;

    let version = r.u16("version")?;
    let count = r.u16("count")? as usize;
    let storage_offset = r.u16("storageOffset")? as usize;

    let records_start = HEADER_SIZE;
    // Saturating, not wrapping: `count` is a `u16` straight off a font that may
    // be hostile, and a wrapped end offset would pass the bounds test below.
    let records_end = records_start.saturating_add(count.saturating_mul(RECORD_SIZE));
    if records_end > bytes.len() {
        return Err(MalformedTable::OutOfBounds {
            table: TABLE,
            field: "nameRecord array",
            offset: records_start,
            len: bytes.len(),
        });
    }

    // Version 1 adds a language-tag array between the records and the storage
    // area. It is read up front because a record's languageID may point into
    // it, and a version this reader does not know means every languageID above
    // `LANG_TAG_BASE` is unresolvable — worth reporting rather than silently
    // mislabelling every localized record as `tag: None`.
    let lang_tags = match version {
        0 => Vec::new(),
        1 => read_lang_tags(&r, records_end, storage_offset)?,
        other => {
            return Err(MalformedTable::UnsupportedVersion {
                table: TABLE,
                field: "version",
                value: u32::from(other),
            })
        }
    };

    let mut records = Vec::with_capacity(count);
    for i in 0..count {
        let rec = records_start + i * RECORD_SIZE;
        // Every field is inside `records_end`, checked above.
        let platform = PlatformId::from_raw(r.u16_at(rec, "platformID")?);
        let encoding = r.u16_at(rec + 2, "encodingID")?;
        let language_id = r.u16_at(rec + 4, "languageID")?;
        let id = NameId::from_raw(r.u16_at(rec + 6, "nameID")?);
        let length = r.u16_at(rec + 8, "length")? as usize;
        let offset = r.u16_at(rec + 10, "stringOffset")? as usize;

        let Some(start) = storage_offset.checked_add(offset) else {
            continue;
        };
        let Ok(raw) = r.slice_at(start, length, "string") else {
            // A record pointing outside the table is that record's problem.
            continue;
        };
        let Some(text) = decode(platform, encoding, raw) else {
            continue;
        };
        let text = text.trim().to_owned();
        if text.is_empty() {
            continue;
        }

        records.push(NameRecord {
            id,
            platform,
            language: language(platform, language_id, &lang_tags),
            text,
        });
    }

    Ok(NameRecords { records })
}

/// Read the version-1 `langTagRecord` array into BCP 47 strings, indexed by
/// `languageID - LANG_TAG_BASE`.
fn read_lang_tags(
    r: &Reader<'_>,
    records_end: usize,
    storage_offset: usize,
) -> Result<Vec<Option<String>>, MalformedTable> {
    let lang_tag_count = r.u16_at(records_end, "langTagCount")? as usize;
    let array_start = records_end + 2;
    let mut tags = Vec::with_capacity(lang_tag_count);
    for i in 0..lang_tag_count {
        let rec = array_start + i * LANG_TAG_RECORD_SIZE;
        let length = r.u16_at(rec, "langTag length")? as usize;
        let offset = r.u16_at(rec + 2, "langTagOffset")? as usize;
        let tag = storage_offset
            .checked_add(offset)
            .and_then(|start| r.slice_at(start, length, "langTag").ok())
            // Language tags are UTF-16BE like every other version-1 string.
            .and_then(decode_utf16be);
        tags.push(tag);
    }
    Ok(tags)
}

/// Classify a record's language id.
///
/// Windows ids are LCIDs whose low ten bits are the primary language; English is
/// primary language `0x09`, so every English sublocale (US, UK, Australian…)
/// classifies as [`Language::English`] without enumerating them. Macintosh ids
/// are a flat list in which 0 is English.
fn language(platform: PlatformId, id: u16, lang_tags: &[Option<String>]) -> Language {
    if id >= LANG_TAG_BASE {
        let tag = lang_tags
            .get(usize::from(id - LANG_TAG_BASE))
            .cloned()
            .flatten();
        // An explicit tag beats the platform table, but "en-…" is still English.
        return match tag {
            Some(t) if t.split(['-', '_']).next() == Some("en") => Language::English,
            other => Language::Localized { tag: other },
        };
    }

    match platform {
        PlatformId::Windows if id & 0x03FF == 0x09 => Language::English,
        PlatformId::Macintosh if id == 0 => Language::English,
        // Platform 0 (Unicode) has no language ids; the field is zero or a
        // Unicode-platform-specific value, and there is nothing to localize by.
        PlatformId::Unicode => Language::English,
        _ => Language::Localized {
            tag: lookup_language_tag(platform, id).map(str::to_owned),
        },
    }
}

/// BCP 47 primary subtags for the platform language ids that appear in real
/// fonts.
///
/// Deliberately a subset, not the full LCID registry. The tag is used for
/// diagnostics and to leave a hook for a future locale-aware step — resolution
/// itself matches localized names regardless of language, so an unknown id costs
/// a `None` here and nothing else. Extend by adding rows.
const LANGUAGE_TAGS: &[(PlatformKind, u16, &str)] = &[
    // Windows LCIDs, keyed on the primary-language bits.
    (PlatformKind::Windows, 0x01, "ar"),
    (PlatformKind::Windows, 0x02, "bg"),
    (PlatformKind::Windows, 0x03, "ca"),
    (PlatformKind::Windows, 0x04, "zh"),
    (PlatformKind::Windows, 0x05, "cs"),
    (PlatformKind::Windows, 0x06, "da"),
    (PlatformKind::Windows, 0x07, "de"),
    (PlatformKind::Windows, 0x08, "el"),
    (PlatformKind::Windows, 0x0A, "es"),
    (PlatformKind::Windows, 0x0B, "fi"),
    (PlatformKind::Windows, 0x0C, "fr"),
    (PlatformKind::Windows, 0x0D, "he"),
    (PlatformKind::Windows, 0x0E, "hu"),
    (PlatformKind::Windows, 0x0F, "is"),
    (PlatformKind::Windows, 0x10, "it"),
    (PlatformKind::Windows, 0x11, "ja"),
    (PlatformKind::Windows, 0x12, "ko"),
    (PlatformKind::Windows, 0x13, "nl"),
    (PlatformKind::Windows, 0x14, "no"),
    (PlatformKind::Windows, 0x15, "pl"),
    (PlatformKind::Windows, 0x16, "pt"),
    (PlatformKind::Windows, 0x18, "ro"),
    (PlatformKind::Windows, 0x19, "ru"),
    (PlatformKind::Windows, 0x1A, "hr"),
    (PlatformKind::Windows, 0x1B, "sk"),
    (PlatformKind::Windows, 0x1D, "sv"),
    (PlatformKind::Windows, 0x1E, "th"),
    (PlatformKind::Windows, 0x1F, "tr"),
    (PlatformKind::Windows, 0x22, "uk"),
    (PlatformKind::Windows, 0x25, "et"),
    (PlatformKind::Windows, 0x26, "lv"),
    (PlatformKind::Windows, 0x27, "lt"),
    (PlatformKind::Windows, 0x29, "fa"),
    (PlatformKind::Windows, 0x2A, "vi"),
    (PlatformKind::Windows, 0x2D, "eu"),
    (PlatformKind::Windows, 0x39, "hi"),
    // Macintosh language ids.
    (PlatformKind::Macintosh, 1, "fr"),
    (PlatformKind::Macintosh, 2, "de"),
    (PlatformKind::Macintosh, 3, "it"),
    (PlatformKind::Macintosh, 4, "nl"),
    (PlatformKind::Macintosh, 5, "sv"),
    (PlatformKind::Macintosh, 6, "es"),
    (PlatformKind::Macintosh, 7, "da"),
    (PlatformKind::Macintosh, 8, "pt"),
    (PlatformKind::Macintosh, 9, "no"),
    (PlatformKind::Macintosh, 10, "he"),
    (PlatformKind::Macintosh, 11, "ja"),
    (PlatformKind::Macintosh, 12, "ar"),
    (PlatformKind::Macintosh, 13, "fi"),
    (PlatformKind::Macintosh, 14, "el"),
    (PlatformKind::Macintosh, 15, "is"),
    (PlatformKind::Macintosh, 17, "tr"),
    (PlatformKind::Macintosh, 18, "hr"),
    (PlatformKind::Macintosh, 19, "zh"),
    (PlatformKind::Macintosh, 23, "hi"),
    (PlatformKind::Macintosh, 24, "ur"),
    (PlatformKind::Macintosh, 25, "th"),
    (PlatformKind::Macintosh, 26, "ko"),
    (PlatformKind::Macintosh, 27, "lt"),
    (PlatformKind::Macintosh, 28, "pl"),
    (PlatformKind::Macintosh, 29, "hu"),
    (PlatformKind::Macintosh, 30, "et"),
    (PlatformKind::Macintosh, 31, "lv"),
    (PlatformKind::Macintosh, 32, "se"),
    (PlatformKind::Macintosh, 33, "fo"),
    (PlatformKind::Macintosh, 34, "fa"),
    (PlatformKind::Macintosh, 35, "ru"),
    (PlatformKind::Macintosh, 39, "vi"),
    (PlatformKind::Macintosh, 44, "uk"),
];

/// The two platforms whose language ids [`LANGUAGE_TAGS`] covers. A separate
/// enum from [`PlatformId`] so the table cannot accidentally hold a row for a
/// platform that has no language ids at all.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlatformKind {
    Macintosh,
    Windows,
}

fn lookup_language_tag(platform: PlatformId, id: u16) -> Option<&'static str> {
    let (kind, key) = match platform {
        PlatformId::Windows => (PlatformKind::Windows, id & 0x03FF),
        PlatformId::Macintosh => (PlatformKind::Macintosh, id),
        _ => return None,
    };
    LANGUAGE_TAGS
        .iter()
        .find(|(k, i, _)| *k == kind && *i == key)
        .map(|(_, _, tag)| *tag)
}

/// Decode one record's bytes, or `None` for an encoding this reader declines.
fn decode(platform: PlatformId, encoding: u16, raw: &[u8]) -> Option<String> {
    match platform {
        // Unicode and Windows platforms are UTF-16BE across every encoding id
        // that appears in practice, including Windows encoding 0 ("symbol"),
        // whose *strings* are still UTF-16 even though its cmap is not.
        PlatformId::Unicode | PlatformId::Windows => decode_utf16be(raw),
        // Macintosh encoding 0 is Mac Roman. The other 32 are legacy codepages
        // (Japanese, Traditional Chinese, Korean, Cyrillic…) that no font ships
        // as its *only* record for a name id, so declining them loses nothing
        // the Windows records do not already carry.
        PlatformId::Macintosh if encoding == 0 => Some(decode_mac_roman(raw)),
        PlatformId::Macintosh | PlatformId::Iso | PlatformId::Other(_) => None,
    }
}

fn decode_utf16be(raw: &[u8]) -> Option<String> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

fn decode_mac_roman(raw: &[u8]) -> String {
    raw.iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                MAC_ROMAN_HIGH[usize::from(b - 0x80)]
            }
        })
        .collect()
}

/// Mac OS Roman, code points 0x80–0xFF. Below 0x80 the encoding is ASCII.
#[rustfmt::skip]
const MAC_ROMAN_HIGH: [char; 128] = [
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è',
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü',
    '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø',
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø',
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{00A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ',
    '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ',
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a `name` table from records, so tests state exactly the bytes they
    /// mean instead of depending on whatever a host font happens to carry.
    ///
    /// `records` are `(platform, encoding, language, nameId, text)`; `text` is
    /// encoded according to the platform the same way [`decode`] reads it back.
    pub(crate) fn build_name_table(
        version: u16,
        records: &[(u16, u16, u16, u16, &str)],
        lang_tags: &[&str],
    ) -> Vec<u8> {
        let mut storage = Vec::new();
        let mut encoded = Vec::new();
        for &(platform, _encoding, _lang, _id, text) in records {
            let bytes = if platform == 1 {
                text.chars().map(|c| c as u8).collect::<Vec<u8>>()
            } else {
                text.encode_utf16()
                    .flat_map(|u| u.to_be_bytes())
                    .collect::<Vec<u8>>()
            };
            encoded.push((storage.len(), bytes.len()));
            storage.extend_from_slice(&bytes);
        }
        let mut tag_spans = Vec::new();
        for tag in lang_tags {
            let bytes: Vec<u8> = tag.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
            tag_spans.push((storage.len(), bytes.len()));
            storage.extend_from_slice(&bytes);
        }

        let lang_tag_area = if version == 1 {
            2 + lang_tags.len() * LANG_TAG_RECORD_SIZE
        } else {
            0
        };
        let storage_offset = HEADER_SIZE + records.len() * RECORD_SIZE + lang_tag_area;

        let mut out = Vec::new();
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        out.extend_from_slice(&(storage_offset as u16).to_be_bytes());
        for (i, &(platform, encoding, lang, id, _)) in records.iter().enumerate() {
            let (offset, len) = encoded[i];
            for v in [platform, encoding, lang, id, len as u16, offset as u16] {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        if version == 1 {
            out.extend_from_slice(&(lang_tags.len() as u16).to_be_bytes());
            for &(offset, len) in &tag_spans {
                out.extend_from_slice(&(len as u16).to_be_bytes());
                out.extend_from_slice(&(offset as u16).to_be_bytes());
            }
        }
        out.extend_from_slice(&storage);
        out
    }

    const WIN: u16 = 3;
    const MAC: u16 = 1;
    const EN_US: u16 = 0x0409;
    const RU_RU: u16 = 0x0419;

    #[test]
    fn reads_every_identifying_name_id() {
        let table = build_name_table(
            0,
            &[
                (WIN, 1, EN_US, 1, "Source Sans 3 SemiBold"),
                (WIN, 1, EN_US, 2, "Regular"),
                (WIN, 1, EN_US, 3, "3.052;ADBE;SourceSans3-Semibold"),
                (WIN, 1, EN_US, 4, "Source Sans 3 SemiBold"),
                (WIN, 1, EN_US, 6, "SourceSans3-Semibold"),
                (WIN, 1, EN_US, 16, "Source Sans 3"),
                (WIN, 1, EN_US, 17, "SemiBold"),
                (WIN, 1, EN_US, 18, "Source Sans 3 SemiBold"),
                (WIN, 1, EN_US, 21, "Source Sans 3"),
                (WIN, 1, EN_US, 22, "SemiBold"),
            ],
            &[],
        );
        let names = read(&table).expect("well-formed table");

        assert_eq!(
            names.preferred(NameId::Family),
            Some("Source Sans 3 SemiBold")
        );
        assert_eq!(names.preferred(NameId::Subfamily), Some("Regular"));
        assert!(names.preferred(NameId::UniqueId).is_some());
        assert_eq!(
            names.preferred(NameId::Full),
            Some("Source Sans 3 SemiBold")
        );
        assert_eq!(
            names.preferred(NameId::PostScript),
            Some("SourceSans3-Semibold")
        );
        assert_eq!(
            names.preferred(NameId::TypographicFamily),
            Some("Source Sans 3")
        );
        assert_eq!(
            names.preferred(NameId::TypographicSubfamily),
            Some("SemiBold")
        );
        assert_eq!(
            names.preferred(NameId::CompatibleFull),
            Some("Source Sans 3 SemiBold")
        );
        assert_eq!(names.preferred(NameId::WwsFamily), Some("Source Sans 3"));
        assert_eq!(names.preferred(NameId::WwsSubfamily), Some("SemiBold"));
    }

    /// Name ids this engine has no use for stay addressable, because `fvar`
    /// instance names live at ids ≥ 256.
    #[test]
    fn unknown_name_ids_round_trip_as_other() {
        assert_eq!(NameId::from_raw(257), NameId::Other(257));
        assert_eq!(NameId::Other(257).raw(), 257);
        assert_eq!(NameId::from_raw(6), NameId::PostScript);
        assert_eq!(NameId::PostScript.raw(), 6);

        let table = build_name_table(0, &[(WIN, 1, EN_US, 257, "Semibold Display")], &[]);
        let names = read(&table).unwrap();
        assert_eq!(
            names.preferred(NameId::Other(257)),
            Some("Semibold Display")
        );
    }

    #[test]
    fn decodes_utf16be_beyond_the_bmp() {
        // U+1D400 MATHEMATICAL BOLD CAPITAL A — a surrogate pair, so a decoder
        // treating UTF-16 as UCS-2 would produce two replacement chars.
        let table = build_name_table(0, &[(WIN, 1, EN_US, 1, "A\u{1D400}Z")], &[]);
        let names = read(&table).unwrap();
        assert_eq!(names.preferred(NameId::Family), Some("A\u{1D400}Z"));
    }

    #[test]
    fn decodes_mac_roman_high_bytes() {
        // Raw Mac Roman: 0xC9 is '…', 0x8E is 'é'.
        let mut table = build_name_table(0, &[(MAC, 0, 0, 1, "AB")], &[]);
        // Rewrite the two stored bytes to the high values under test.
        let n = table.len();
        table[n - 2] = 0xC9;
        table[n - 1] = 0x8E;
        let names = read(&table).unwrap();
        assert_eq!(names.preferred(NameId::Family), Some("…é"));
    }

    /// Platform 2 (ISO) is deprecated and platform 1 with a non-Roman encoding
    /// is a legacy codepage. Both are skipped — but only *those* records; the
    /// rest of the table still reads.
    #[test]
    fn undecodable_records_are_skipped_not_fatal() {
        let table = build_name_table(
            0,
            &[
                (2, 0, 0, 1, "ISO record"),
                (MAC, 25, 11, 1, "Shift-JIS record"),
                (WIN, 1, EN_US, 1, "Good Family"),
            ],
            &[],
        );
        let names = read(&table).expect("a bad record must not fail the table");
        assert_eq!(names.len(), 1);
        assert_eq!(names.preferred(NameId::Family), Some("Good Family"));
    }

    #[test]
    fn localized_records_are_kept_and_tagged() {
        let table = build_name_table(
            0,
            &[(WIN, 1, EN_US, 1, "PT Sans"), (WIN, 1, RU_RU, 1, "ПТ Санс")],
            &[],
        );
        let names = read(&table).unwrap();

        assert_eq!(names.len(), 2);
        assert_eq!(
            names.preferred(NameId::Family),
            Some("PT Sans"),
            "the English record is canonical"
        );
        let localized = names
            .all(NameId::Family)
            .find(|r| !r.language.is_english())
            .expect("the Russian record must survive");
        assert_eq!(localized.text, "ПТ Санс");
        assert_eq!(
            localized.language,
            Language::Localized {
                tag: Some("ru".into())
            }
        );
    }

    /// Every English sublocale is primary, not just en-US — otherwise a font
    /// shipped with only en-GB records would have no canonical name at all.
    #[test]
    fn every_english_sublocale_counts_as_primary() {
        for lcid in [0x0409u16, 0x0809, 0x0C09, 0x1009, 0x2809] {
            let table = build_name_table(0, &[(WIN, 1, lcid, 1, "Family")], &[]);
            let names = read(&table).unwrap();
            assert!(
                names
                    .all(NameId::Family)
                    .next()
                    .unwrap()
                    .language
                    .is_english(),
                "LCID {lcid:#06x} has primary language English"
            );
        }
        // And a non-English one does not.
        let table = build_name_table(0, &[(WIN, 1, RU_RU, 1, "Family")], &[]);
        let names = read(&table).unwrap();
        assert!(!names
            .all(NameId::Family)
            .next()
            .unwrap()
            .language
            .is_english());
    }

    #[test]
    fn version_1_language_tags_resolve_through_the_lang_tag_array() {
        let table = build_name_table(
            1,
            &[
                (WIN, 1, EN_US, 1, "Fixture Sans"),
                (WIN, 1, LANG_TAG_BASE, 1, "Fixture Sans CY"),
                (WIN, 1, LANG_TAG_BASE + 1, 1, "Fixture Sans GB"),
            ],
            &["cy-GB", "en-GB"],
        );
        let names = read(&table).unwrap();

        let cy = names
            .all(NameId::Family)
            .find(|r| r.text == "Fixture Sans CY")
            .unwrap();
        assert_eq!(
            cy.language,
            Language::Localized {
                tag: Some("cy-GB".into())
            }
        );

        let gb = names
            .all(NameId::Family)
            .find(|r| r.text == "Fixture Sans GB")
            .unwrap();
        assert!(
            gb.language.is_english(),
            "an explicit en-* tag is still the primary language"
        );
    }

    #[test]
    fn distinct_deduplicates_repeated_spellings() {
        let table = build_name_table(
            0,
            &[
                (WIN, 1, EN_US, 4, "Fixture Sans Bold"),
                (MAC, 0, 0, 4, "Fixture Sans Bold"),
                (WIN, 1, RU_RU, 4, "Фикстур Санс"),
            ],
            &[],
        );
        let names = read(&table).unwrap();
        let distinct = names.distinct(NameId::Full);
        assert_eq!(distinct.len(), 2);
        assert!(distinct.contains("Fixture Sans Bold"));
        assert!(distinct.contains("Фикстур Санс"));
    }

    #[test]
    fn empty_and_whitespace_only_records_are_dropped() {
        let table = build_name_table(
            0,
            &[
                (WIN, 1, EN_US, 1, "   "),
                (WIN, 1, EN_US, 4, "  Padded Name  "),
            ],
            &[],
        );
        let names = read(&table).unwrap();
        assert_eq!(names.preferred(NameId::Family), None);
        assert_eq!(
            names.preferred(NameId::Full),
            Some("Padded Name"),
            "surrounding whitespace is trimmed"
        );
    }

    // ── malformed input ──────────────────────────────────────────────────

    #[test]
    fn a_table_shorter_than_its_header_is_an_error() {
        assert_eq!(
            read(&[0, 0, 0]).unwrap_err(),
            MalformedTable::TooShort {
                table: "name",
                needed: 6,
                actual: 3
            }
        );
    }

    #[test]
    fn a_record_array_running_past_the_table_is_an_error() {
        // count = 100 records, but the table holds only the header.
        let bytes = [0, 0, 0, 100, 0, 6];
        assert!(matches!(
            read(&bytes).unwrap_err(),
            MalformedTable::OutOfBounds { table: "name", .. }
        ));
    }

    #[test]
    fn an_unknown_table_version_is_reported_as_such() {
        // Version 2 does not exist; the distinction from OutOfBounds matters
        // because it means this reader is behind, not that the font is broken.
        let mut bytes = build_name_table(0, &[(WIN, 1, EN_US, 1, "X")], &[]);
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert_eq!(
            read(&bytes).unwrap_err(),
            MalformedTable::UnsupportedVersion {
                table: "name",
                field: "version",
                value: 2
            }
        );
    }

    #[test]
    fn a_string_offset_outside_the_table_drops_only_that_record() {
        let mut table = build_name_table(
            0,
            &[(WIN, 1, EN_US, 1, "Good"), (WIN, 1, EN_US, 4, "AlsoGood")],
            &[],
        );
        // Point the first record's string 60000 bytes past the storage area.
        let first_offset_field = HEADER_SIZE + 10;
        table[first_offset_field..first_offset_field + 2].copy_from_slice(&60000u16.to_be_bytes());

        let names = read(&table).expect("one bad offset must not fail the table");
        assert_eq!(names.preferred(NameId::Family), None);
        assert_eq!(names.preferred(NameId::Full), Some("AlsoGood"));
    }

    #[test]
    fn an_odd_length_utf16_string_is_dropped() {
        let mut table = build_name_table(0, &[(WIN, 1, EN_US, 1, "AB")], &[]);
        // Claim 3 bytes for a 4-byte UTF-16 string.
        let len_field = HEADER_SIZE + 8;
        table[len_field..len_field + 2].copy_from_slice(&3u16.to_be_bytes());
        let names = read(&table).unwrap();
        assert!(names.is_empty());
    }
}
