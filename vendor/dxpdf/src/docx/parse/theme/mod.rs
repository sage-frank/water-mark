//! Parser for `word/theme/theme1.xml` (DrawingML theme).
//!
//! Hierarchical parsing per §20.1.6.9 `CT_OfficeStyleSheet`. Color transforms
//! (`satMod`, `shade`, `tint`, `alpha`, `lumMod`, `lumOff`) are accepted
//! syntactically but discarded — the resolved model stores only flat RGB.

mod script;

use crate::model::Dup;
use serde::Deserialize;

use crate::docx::error::Result;
use crate::docx::model::{
    EffectList, Outline, Theme, ThemeColorScheme, ThemeFontScheme, ThemeScriptFont,
};
use crate::docx::parse::drawing::schema::effect::EffectListXml;
use crate::docx::parse::drawing::schema::stroke::OutlineXml;
use crate::docx::parse::primitives::HexColor;
use crate::docx::parse::serde_xml::from_xml;

use self::script::parse_script_tag;

pub fn parse_theme(data: &[u8]) -> Result<Theme> {
    if data.is_empty() {
        return Ok(Theme::default());
    }
    from_xml::<ThemeXml>(data).map(Into::into)
}

#[derive(Deserialize, Default)]
struct ThemeXml {
    #[serde(rename = "themeElements", default)]
    theme_elements: Vec<ThemeElementsXml>,
}

#[derive(Deserialize, Default)]
struct ThemeElementsXml {
    #[serde(rename = "clrScheme", default)]
    clr_scheme: Vec<ClrSchemeXml>,
    #[serde(rename = "fontScheme", default)]
    font_scheme: Vec<FontSchemeXml>,
    #[serde(rename = "fmtScheme", default)]
    fmt_scheme: Vec<FmtSchemeXml>,
}

#[derive(Deserialize, Default)]
struct FmtSchemeXml {
    #[serde(rename = "fillStyleLst", default)]
    fill_style_lst: Vec<FillStyleLstXml>,
    #[serde(rename = "lnStyleLst", default)]
    ln_style_lst: Vec<LnStyleLstXml>,
    #[serde(rename = "effectStyleLst", default)]
    effect_style_lst: Vec<EffectStyleLstXml>,
}

/// §20.1.4.1.13 CT_FillStyleList — theme fill styles (spec requires ≥3). Each
/// child is one of the DrawingML fill choices, routed through the shared
/// `DrawingFillXml` parser.
#[derive(Deserialize, Default)]
struct FillStyleLstXml {
    #[serde(rename = "$value", default)]
    fills: Vec<crate::docx::parse::drawing::schema::fill::DrawingFillXml>,
}

/// §20.1.4.1.21 CT_LineStyleList — exactly 3 `<a:ln>` entries per spec.
#[derive(Deserialize, Default)]
struct LnStyleLstXml {
    #[serde(rename = "ln", default)]
    lines: Vec<OutlineXml>,
}

#[derive(Deserialize, Default)]
struct EffectStyleLstXml {
    #[serde(rename = "effectStyle", default)]
    effect_styles: Vec<EffectStyleXml>,
}

/// §20.1.4.1.11 CT_EffectStyleItem — an effect style entry. May include
/// `scene3d`/`sp3d` siblings (ignored — Tier-3 3D extrusion).
#[derive(Deserialize, Default)]
struct EffectStyleXml {
    #[serde(rename = "effectLst", default)]
    effect_lst: Vec<EffectListXml>,
}

#[derive(Deserialize, Default)]
struct ClrSchemeXml {
    #[serde(default)]
    dk1: Vec<ColorChoice>,
    #[serde(default)]
    lt1: Vec<ColorChoice>,
    #[serde(default)]
    dk2: Vec<ColorChoice>,
    #[serde(default)]
    lt2: Vec<ColorChoice>,
    #[serde(default)]
    accent1: Vec<ColorChoice>,
    #[serde(default)]
    accent2: Vec<ColorChoice>,
    #[serde(default)]
    accent3: Vec<ColorChoice>,
    #[serde(default)]
    accent4: Vec<ColorChoice>,
    #[serde(default)]
    accent5: Vec<ColorChoice>,
    #[serde(default)]
    accent6: Vec<ColorChoice>,
    #[serde(default)]
    hlink: Vec<ColorChoice>,
    #[serde(rename = "folHlink", default)]
    fol_hlink: Vec<ColorChoice>,
}

/// A slot's single color-choice element. §20.1.2.3 defines several; we resolve
/// `srgbClr` and `sysClr` and ignore the rest (scRgbClr, hslClr, schemeClr,
/// prstClr) by leaving their fields absent.
#[derive(Deserialize, Default)]
struct ColorChoice {
    #[serde(rename = "srgbClr", default)]
    srgb: Vec<SrgbClr>,
    #[serde(rename = "sysClr", default)]
    sys: Vec<SysClr>,
}

#[derive(Deserialize)]
struct SrgbClr {
    #[serde(rename = "@val")]
    val: HexColor,
}

#[derive(Deserialize)]
struct SysClr {
    #[serde(rename = "@lastClr", default)]
    last_clr: Option<HexColor>,
}

impl ColorChoice {
    fn resolve(self) -> Option<u32> {
        if let Some(s) = Dup::from(self.srgb).into_value() {
            return s.val.rgb();
        }
        if let Some(s) = Dup::from(self.sys).into_value() {
            return s.last_clr.and_then(HexColor::rgb);
        }
        None
    }
}

#[derive(Deserialize, Default)]
struct FontSchemeXml {
    #[serde(rename = "majorFont", default)]
    major: Vec<FontCollectionXml>,
    #[serde(rename = "minorFont", default)]
    minor: Vec<FontCollectionXml>,
}

#[derive(Deserialize, Default)]
struct FontCollectionXml {
    #[serde(default)]
    latin: Vec<TypefaceXml>,
    #[serde(default)]
    ea: Vec<TypefaceXml>,
    #[serde(default)]
    cs: Vec<TypefaceXml>,
    #[serde(rename = "font", default)]
    fonts: Vec<ScriptFontXml>,
}

#[derive(Deserialize)]
struct TypefaceXml {
    #[serde(rename = "@typeface", default)]
    typeface: String,
}

#[derive(Deserialize)]
struct ScriptFontXml {
    #[serde(rename = "@script")]
    script: String,
    #[serde(rename = "@typeface", default)]
    typeface: String,
}

impl From<ThemeXml> for Theme {
    fn from(x: ThemeXml) -> Self {
        let mut theme = Theme::default();
        if let Some(elements) = Dup::from(x.theme_elements).into_value() {
            if let Some(cs) = Dup::from(elements.clr_scheme).into_value() {
                theme.color_scheme = cs.into();
            }
            if let Some(fs) = Dup::from(elements.font_scheme).into_value() {
                if let Some(major) = Dup::from(fs.major).into_value() {
                    theme.major_font = major.into();
                }
                if let Some(minor) = Dup::from(fs.minor).into_value() {
                    theme.minor_font = minor.into();
                }
            }
            if let Some(fmt) = Dup::from(elements.fmt_scheme).into_value() {
                if let Some(list) = Dup::from(fmt.fill_style_lst).into_value() {
                    theme.fill_styles = list.fills.into_iter().map(Into::into).collect();
                }
                if let Some(list) = Dup::from(fmt.ln_style_lst).into_value() {
                    theme.line_styles = list.lines.into_iter().map(Outline::from).collect();
                }
                if let Some(list) = Dup::from(fmt.effect_style_lst).into_value() {
                    theme.effect_styles = list
                        .effect_styles
                        .into_iter()
                        .map(|es| {
                            Dup::from(es.effect_lst)
                                .into_value()
                                .map(EffectList::from)
                                .unwrap_or_default()
                        })
                        .collect();
                }
            }
        }
        theme
    }
}

impl From<ClrSchemeXml> for ThemeColorScheme {
    fn from(x: ClrSchemeXml) -> Self {
        let mut s = ThemeColorScheme::default();
        assign(&mut s.dark1, Dup::from(x.dk1).into_value());
        assign(&mut s.light1, Dup::from(x.lt1).into_value());
        assign(&mut s.dark2, Dup::from(x.dk2).into_value());
        assign(&mut s.light2, Dup::from(x.lt2).into_value());
        assign(&mut s.accent1, Dup::from(x.accent1).into_value());
        assign(&mut s.accent2, Dup::from(x.accent2).into_value());
        assign(&mut s.accent3, Dup::from(x.accent3).into_value());
        assign(&mut s.accent4, Dup::from(x.accent4).into_value());
        assign(&mut s.accent5, Dup::from(x.accent5).into_value());
        assign(&mut s.accent6, Dup::from(x.accent6).into_value());
        assign(&mut s.hyperlink, Dup::from(x.hlink).into_value());
        assign(
            &mut s.followed_hyperlink,
            Dup::from(x.fol_hlink).into_value(),
        );
        s
    }
}

fn assign(slot: &mut u32, choice: Option<ColorChoice>) {
    if let Some(rgb) = choice.and_then(ColorChoice::resolve) {
        *slot = rgb;
    }
}

impl From<FontCollectionXml> for ThemeFontScheme {
    fn from(x: FontCollectionXml) -> Self {
        Self {
            latin: Dup::from(x.latin)
                .into_value()
                .map(|t| t.typeface)
                .unwrap_or_default(),
            east_asian: Dup::from(x.ea)
                .into_value()
                .map(|t| t.typeface)
                .unwrap_or_default(),
            complex_script: Dup::from(x.cs)
                .into_value()
                .map(|t| t.typeface)
                .unwrap_or_default(),
            script_fonts: x
                .fonts
                .into_iter()
                .map(|f| ThemeScriptFont {
                    script: parse_script_tag(&f.script),
                    typeface: f.typeface,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::model::ScriptTag;

    const MINIMAL_THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<a:theme xmlns:a="urn:a" name="Office">
  <a:themeElements>
    <a:clrScheme name="Office">
      <a:dk1><a:srgbClr val="000000"/></a:dk1>
      <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
      <a:accent1><a:srgbClr val="4F81BD"/></a:accent1>
      <a:hlink><a:srgbClr val="0000FF"/></a:hlink>
      <a:folHlink><a:srgbClr val="800080"/></a:folHlink>
    </a:clrScheme>
    <a:fontScheme name="Office">
      <a:majorFont>
        <a:latin typeface="Calibri Light"/>
        <a:ea typeface=""/>
        <a:cs typeface=""/>
        <a:font script="Hans" typeface="Noto Sans SC"/>
      </a:majorFont>
      <a:minorFont>
        <a:latin typeface="Calibri"/>
        <a:ea typeface=""/>
        <a:cs typeface=""/>
      </a:minorFont>
    </a:fontScheme>
    <a:fmtScheme name="Office">
      <a:fillStyleLst/>
    </a:fmtScheme>
  </a:themeElements>
  <a:objectDefaults/>
  <a:extraClrSchemeLst/>
</a:theme>"#;

    #[test]
    fn end_to_end_theme_parse() {
        let theme = parse_theme(MINIMAL_THEME.as_bytes()).unwrap();

        assert_eq!(theme.color_scheme.dark1, 0x000000);
        assert_eq!(theme.color_scheme.light1, 0xFFFFFF);
        assert_eq!(theme.color_scheme.accent1, 0x4F81BD);
        assert_eq!(theme.color_scheme.hyperlink, 0x0000FF);
        assert_eq!(theme.color_scheme.followed_hyperlink, 0x800080);

        assert_eq!(theme.major_font.latin, "Calibri Light");
        assert_eq!(theme.minor_font.latin, "Calibri");
        assert_eq!(theme.major_font.script_fonts.len(), 1);
        assert_eq!(theme.major_font.script_fonts[0].script, ScriptTag::Hans);
        assert_eq!(theme.major_font.script_fonts[0].typeface, "Noto Sans SC");
    }

    #[test]
    fn empty_input_returns_default_theme() {
        let theme = parse_theme(b"").unwrap();
        assert_eq!(theme.color_scheme.dark1, 0);
        assert!(theme.major_font.latin.is_empty());
    }

    #[test]
    fn missing_theme_elements_keeps_defaults() {
        let xml = r#"<a:theme xmlns:a="urn:a"><a:objectDefaults/></a:theme>"#;
        let theme = parse_theme(xml.as_bytes()).unwrap();
        assert_eq!(theme.color_scheme.dark1, 0);
        assert!(theme.minor_font.latin.is_empty());
    }

    #[test]
    fn srgb_transforms_are_tolerated() {
        let xml = r#"<a:theme xmlns:a="urn:a"><a:themeElements><a:clrScheme>
            <a:accent1>
                <a:srgbClr val="DEADBE">
                    <a:shade val="75000"/>
                    <a:satMod val="200000"/>
                </a:srgbClr>
            </a:accent1>
        </a:clrScheme></a:themeElements></a:theme>"#;
        let theme = parse_theme(xml.as_bytes()).unwrap();
        assert_eq!(theme.color_scheme.accent1, 0xDEADBE);
    }

    #[test]
    fn fill_style_lst_parsed_into_fill_styles() {
        let xml = r#"<a:theme xmlns:a="urn:a"><a:themeElements><a:fmtScheme>
            <a:fillStyleLst>
                <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
                <a:gradFill><a:gsLst>
                    <a:gs pos="0"><a:schemeClr val="phClr"/></a:gs>
                    <a:gs pos="100000"><a:schemeClr val="phClr"/></a:gs>
                </a:gsLst><a:lin ang="5400000"/></a:gradFill>
                <a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>
            </a:fillStyleLst>
        </a:fmtScheme></a:themeElements></a:theme>"#;
        let theme = parse_theme(xml.as_bytes()).unwrap();
        assert_eq!(theme.fill_styles.len(), 3);
        assert!(matches!(
            &theme.fill_styles[0],
            crate::docx::model::DrawingFill::Solid(_)
        ));
        assert!(matches!(
            &theme.fill_styles[1],
            crate::docx::model::DrawingFill::Gradient(_)
        ));
        assert!(matches!(
            &theme.fill_styles[2],
            crate::docx::model::DrawingFill::Solid(_)
        ));
    }

    #[test]
    fn sys_clr_uses_last_clr() {
        let xml = r#"<a:theme xmlns:a="urn:a"><a:themeElements><a:clrScheme>
            <a:lt1><a:sysClr val="window" lastClr="ABCDEF"/></a:lt1>
        </a:clrScheme></a:themeElements></a:theme>"#;
        let theme = parse_theme(xml.as_bytes()).unwrap();
        assert_eq!(theme.color_scheme.light1, 0xABCDEF);
    }

    #[test]
    fn unknown_script_preserved_as_other() {
        let xml = r#"<a:theme xmlns:a="urn:a"><a:themeElements><a:fontScheme>
            <a:majorFont>
                <a:latin typeface="L"/>
                <a:ea typeface=""/>
                <a:cs typeface=""/>
                <a:font script="Xxxx" typeface="FallbackFace"/>
            </a:majorFont>
            <a:minorFont>
                <a:latin typeface=""/>
                <a:ea typeface=""/>
                <a:cs typeface=""/>
            </a:minorFont>
        </a:fontScheme></a:themeElements></a:theme>"#;
        let theme = parse_theme(xml.as_bytes()).unwrap();
        assert_eq!(theme.major_font.script_fonts.len(), 1);
        match &theme.major_font.script_fonts[0].script {
            ScriptTag::Other(s) => assert_eq!(&**s, "Xxxx"),
            other => panic!("expected Other, got {other:?}"),
        }
    }
}
