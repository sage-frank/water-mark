//! 临时基准：定位 dxpdf paint 阶段（Skia PDF 后端）的每字形成本。
//!
//! 用法：
//!   cargo run --release --bin bench_glyph -- glyph   <font> [n]           # 单字形 path 生成成本
//!   cargo run --release --bin bench_glyph -- pdf     <font> [n] [size]    # PDF 画布，逐字符 draw_str
//!   cargo run --release --bin bench_glyph -- pdfone  <font> [n] [size]    # PDF 画布，一次 draw_str 画 n 字
//!   cargo run --release --bin bench_glyph -- pdfrep  <font> [n] [size]    # PDF 画布，同一字符重复 n 次
//!   cargo run --release --bin bench_glyph -- raster  <font> [n] [size]    # 光栅画布，逐字符 draw_str

use std::time::{Duration, Instant};

use skia_safe::{Data, Font, FontHinting, FontMgr, Paint, Point, Typeface};

fn load(path: &str) -> Option<(Vec<u8>, Typeface)> {
    let bytes = std::fs::read(path).ok()?;
    let mgr = FontMgr::new();
    let tf = mgr.new_from_data(&Data::new_copy(&bytes), 0)?;
    Some((bytes, tf))
}

fn make_font(tf: &Typeface, size: f32) -> Font {
    let mut font = Font::from_typeface(tf.clone(), size);
    font.set_subpixel(true);
    font.set_linear_metrics(true);
    font.set_hinting(FontHinting::None);
    font
}

/// `n` 个该字体覆盖的、互不相同的字符：优先常用汉字，无则退回 Latin。
fn pick_chars(tf: &Typeface, n: usize) -> Vec<char> {
    let mut out = Vec::with_capacity(n);
    for cp in 0x4E00u32..0x9FA5 {
        if let Some(c) = char::from_u32(cp) {
            if tf.unichar_to_glyph(c as i32) != 0 {
                out.push(c);
                if out.len() >= n {
                    return out;
                }
            }
        }
    }
    for c in ('a'..='z').chain('A'..='Z').chain('0'..='9') {
        if tf.unichar_to_glyph(c as i32) != 0 {
            out.push(c);
        }
    }
    out
}

// ── 每个字形首次 path 生成成本 ────────────────────────────────────────────────

fn bench_glyph(path: &str, max_glyphs: u16) {
    let Some((bytes, tf)) = load(path) else {
        println!("跳过 {path}: 无法构建 typeface");
        return;
    };
    let count = tf.count_glyphs();
    let upem = tf.units_per_em().unwrap_or(0);
    println!(
        "\n=== glyph: {} ({:.2} MB) family={:?} glyphs={} upem={} ===",
        path,
        bytes.len() as f64 / 1e6,
        tf.family_name(),
        count,
        upem
    );

    for size in [12.0f32, upem.max(1) as f32] {
        let font = make_font(&tf, size);
        let n = (count as u32).min(max_glyphs as u32) as u16;
        let mut ok = 0u64;
        let t = Instant::now();
        for gid in 1..n {
            if font.get_path(gid).is_some() {
                ok += 1;
            }
        }
        let el = t.elapsed();
        println!(
            "  size={size:>6}  字形 {ok}/{n}  总 {:>8.3?}  平均 {:>10.3?}/字形",
            el,
            el / (ok.max(1) as u32)
        );
    }
}

// ── 画布变体 ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Variant {
    /// 每个字符一次 draw_str（等价 dxpdf 每个 blob 一次，全是新字形）
    Distinct,
    /// 同一字符重复 N 次（无新字形，只剩每次 draw 的固定开销）
    Repeat,
    /// N 个字符拼成一个字符串，仅一次 draw_str（一次 emit_text 含多个字形）
    OneCall,
}

impl Variant {
    fn tag(self) -> &'static str {
        match self {
            Variant::Distinct => "distinct(逐字)",
            Variant::Repeat => "repeat(同字)",
            Variant::OneCall => "onecall(一次多字)",
        }
    }
}

/// 在给定画布上画完所有字符；y 每 48 行回绕以保持在页面内。
fn draw_all(
    chars: &[char],
    font: &Font,
    paint: &Paint,
    canvas: &skia_safe::Canvas,
    variant: Variant,
) -> Duration {
    let mut draw = Duration::ZERO;
    match variant {
        Variant::Distinct | Variant::Repeat => {
            for (i, ch) in chars.iter().enumerate() {
                let s = ch.to_string();
                let y = 20.0 + (i % 48) as f32 * 15.0;
                let t = Instant::now();
                canvas.draw_str(&s, Point::new(40.0, y), font, paint);
                draw += t.elapsed();
            }
        }
        Variant::OneCall => {
            let s: String = chars.iter().collect();
            let t = Instant::now();
            canvas.draw_str(&s, Point::new(40.0, 40.0), font, paint);
            draw = t.elapsed();
        }
    }
    draw
}

fn bench_pdf(path: &str, nchars: usize, size: f32, variant: Variant, path_effect: bool) {
    let Some((bytes, tf)) = load(path) else {
        println!("跳过 {path}: 无法构建 typeface");
        return;
    };
    let mut chars = pick_chars(&tf, nchars);
    if chars.is_empty() {
        println!("跳过 {path}: 没有可用字符");
        return;
    }
    if matches!(variant, Variant::Repeat) {
        chars = vec![chars[0]; nchars];
    }

    println!(
        "\n=== pdf[{}] size={size}{}: {} ({:.2} MB) family={:?} upem={:?} 字符数 {} ===",
        variant.tag(),
        if path_effect { " +非dashPathEffect" } else { "" },
        path,
        bytes.len() as f64 / 1e6,
        tf.family_name(),
        tf.units_per_em(),
        chars.len()
    );

    let font = make_font(&tf, size);
    let mut paint = Paint::default();
    if path_effect {
        // 非 dash 的 PathEffect：会让 SkPDFStrike::Make 里的 scale_paint() 返回 false，
        // 从而把 path strike 的字号从 fontsUnitsPerEM 退回实际字号。
        paint.set_path_effect(skia_safe::PathEffect::discrete(3.0, 1.0, None));
    }
    let total = chars.len();

    let mut stream: Vec<u8> = Vec::new();
    let mut doc = skia_safe::pdf::new_document(&mut stream, None);
    let mut draw = Duration::ZERO;

    let mut page = doc.begin_page((612.0, 792.0), None);
    draw += draw_all(&chars, &font, &paint, page.canvas(), variant);
    doc = page.end_page();

    let t = Instant::now();
    doc.close();
    let close = t.elapsed();

    if let Ok(out) = std::env::var("BENCH_PDF_OUT") {
        let _ = std::fs::write(&out, &stream);
        println!("  已写出 {out}");
    }

    println!(
        "  draw 总 {:>10.3?}  平均 {:>12.3?}/字符  |  close() {:>10.3?}  |  pdf {} 字节",
        draw,
        draw / total as u32,
        close,
        stream.len()
    );
}

fn bench_raster(path: &str, nchars: usize, size: f32) {
    let Some((bytes, tf)) = load(path) else {
        println!("跳过 {path}: 无法构建 typeface");
        return;
    };
    let chars = pick_chars(&tf, nchars);
    println!(
        "\n=== raster size={size}: {} ({:.2} MB) family={:?} upem={:?} 字符数 {} ===",
        path,
        bytes.len() as f64 / 1e6,
        tf.family_name(),
        tf.units_per_em(),
        chars.len()
    );

    let font = make_font(&tf, size);
    let paint = Paint::default();

    let mut surface = skia_safe::surfaces::raster_n32_premul((612, 792)).expect("raster surface");
    let canvas = surface.canvas();
    let draw = draw_all(&chars, &font, &paint, canvas, Variant::Distinct);
    println!(
        "  draw 总 {:>10.3?}  平均 {:>12.3?}/字符",
        draw,
        draw / chars.len().max(1) as u32
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("pdf");
    let path = args.get(2).cloned();
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(400);
    let size: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(12.0);

    match (mode, path.as_deref()) {
        ("glyph", Some(p)) => bench_glyph(p, n as u16),
        ("pdf", Some(p)) => bench_pdf(p, n, size, Variant::Distinct, false),
        ("pdfpe", Some(p)) => bench_pdf(p, n, size, Variant::Distinct, true),
        ("pdfrep", Some(p)) => bench_pdf(p, n, size, Variant::Repeat, false),
        ("pdfone", Some(p)) => bench_pdf(p, n, size, Variant::OneCall, false),
        ("raster", Some(p)) => bench_raster(p, n, size),
        _ => {
            let defs = [
                "STSongStd-Light-Acro/STSongStd-Light-Acro.otf",
                "C:/Windows/Fonts/simsun.ttc",
                "C:/Windows/Fonts/msyh.ttc",
            ];
            for d in defs {
                bench_pdf(d, n, size, Variant::Distinct, false);
                bench_pdf(d, n, size, Variant::Distinct, true);
            }
        }
    }
}
