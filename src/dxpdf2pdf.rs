use std::env;
use std::io::{Read, Write, Cursor};
use std::time::{Duration, Instant};
use zip::write::ZipWriter;
use zip::ZipArchive;

/// 分阶段耗时汇总，用于最后打印占比。
#[derive(Default)]
struct StageTimes {
    read_file: Duration,
    unzip: Duration,
    patch_xml: Duration,
    rezip: Duration,
    parse: Duration,
    render: Duration,
    write_file: Duration,
}

fn pct(d: Duration, total: Duration) -> f64 {
    let t = total.as_secs_f64();
    if t <= 0.0 { 0.0 } else { d.as_secs_f64() / t * 100.0 }
}

/// 预处理 DOCX：补全缺失的 `ilvl` 属性。
///
/// `dxpdf` 的 serde schema 将 `@ilvl` 标记为必填字段，但许多非 Word
/// 生成的 DOCX 文件不含此属性。此函数处理两处：
///
/// 1. `word/numbering.xml` — `<w:lvl>` 元素需要 `w:ilvl` 属性
/// 2. `word/document.xml`、`word/header*.xml`、`word/footer*.xml` —
///    `<w:numPr>` 块需要 `<w:ilvl w:val="0"/>` 子元素
fn patch_docx(
    docx_bytes: &[u8],
    times: &mut StageTimes,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let t_unzip = Instant::now();
    let reader = Cursor::new(docx_bytes.to_vec());
    let mut archive = ZipArchive::new(reader)?;

    // 读取所有条目到内存
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        entries.push((name, data));
    }
    times.unzip = t_unzip.elapsed();

    // 修补需要处理的 XML 条目
    let t_patch = Instant::now();
    for (name, data) in entries.iter_mut() {
        if !name.ends_with(".xml") {
            continue;
        }
        let xml = String::from_utf8_lossy(data).into_owned();

        if name == "word/numbering.xml" {
            // 修补 <w:lvl> 标签：添加缺失的 w:ilvl 属性
            *data = patch_lvl_ilvl(&xml).into_bytes();
        } else if name.starts_with("word/")
            && (name.contains("document")
                || name.contains("header")
                || name.contains("footer"))
        {
            // 修补 <w:numPr> 块：添加缺失的 <w:ilvl w:val="0"/> 子元素
            *data = patch_numpr_ilvl(&xml).into_bytes();
        }

    }
    times.patch_xml = t_patch.elapsed();

    // 重新打包为 ZIP
    let t_rezip = Instant::now();
    let mut output = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut output);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for (name, data) in &entries {
            zip.start_file(name, opts)?;
            zip.write_all(data)?;
        }
        zip.finish()?;
    }
    times.rezip = t_rezip.elapsed();

    Ok(output.into_inner())
}

/// 在 `<w:lvl>` 标签中添加缺失的 `w:ilvl` 属性。
///
/// `<w:lvl>` → `<w:lvl w:ilvl="0">`
/// 已经有 `w:ilvl` 属性的标签不变。
fn patch_lvl_ilvl(xml: &str) -> String {
    // 替换所有不带 w:ilvl 的 <w:lvl> 和 <w:lvl/> 标签
    let mut result = xml.replace("<w:lvl>", r#"<w:lvl w:ilvl="0">"#);
    result = result.replace("<w:lvl/>", r#"<w:lvl w:ilvl="0"/>"#);
    result
}

/// 在 `<w:numPr>` 中插入缺失的 `<w:ilvl w:val="0"/>`。
///
/// 对于每个 `<w:numPr>...</w:numPr>` 块，如果其中不包含 `<w:ilvl`，
/// 则在 `<w:numId` 之前插入 `<w:ilvl w:val="0"/>`。
fn patch_numpr_ilvl(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len());
    let mut remaining = xml;

    loop {
        if let Some(start) = remaining.find("<w:numPr>") {
            result.push_str(&remaining[..start + 9]); // 包含 "<w:numPr>"
            let rest = &remaining[start + 9..];

            if let Some(end) = rest.find("</w:numPr>") {
                let inner = &rest[..end];
                if !inner.contains("<w:ilvl") {
                    // 在 <w:numId 之前插入 <w:ilvl w:val="0"/>
                    if let Some(numid_pos) = inner.find("<w:numId") {
                        result.push_str(&inner[..numid_pos]);
                        result.push_str(r#"<w:ilvl w:val="0"/>"#);
                        result.push_str(&inner[numid_pos..]);
                    } else {
                        // 没有 numId，直接在前面插入
                        result.push_str(r#"<w:ilvl w:val="0"/>"#);
                        result.push_str(inner);
                    }
                } else {
                    result.push_str(inner);
                }
                result.push_str("</w:numPr>");
                remaining = &rest[end + 10..];
            } else {
                // 没有闭合标签，直接追加剩余内容
                result.push_str(rest);
                break;
            }
        } else {
            result.push_str(remaining);
            break;
        }
    }

    result
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 刻意不注册日志后端：dxpdf 内部的 log 输出会与下面 [1]~[6] 的计时段混在一起。
    // 需要排查 dxpdf 内部细节时，临时加回 env_logger 并设 RUST_LOG=debug。

    let total_start = Instant::now();
    let mut times = StageTimes::default();

    let args: Vec<String> = env::args().collect();
    let input_path = args.get(1).map(|s| s.as_str()).unwrap_or("in.docx");
    let output_path = args.get(2).map(|s| s.as_str()).unwrap_or("dxpdf2pdf-out.pdf");

    println!("正在转换 DOCX → PDF (dxpdf/Skia): {}", input_path);

    // ---------- [1] 读取输入 ----------
    let t = Instant::now();
    let docx_bytes = std::fs::read(input_path)?;
    times.read_file = t.elapsed();
    println!(
        "[1] 读取输入文件              {:>10.2?}  ({} 字节)",
        times.read_file,
        docx_bytes.len()
    );

    // ---------- [2] 预处理（解压 → 改 XML → 重新压缩）----------
    let t = Instant::now();
    let patched_docx = patch_docx(&docx_bytes, &mut times)?;
    println!("[2] patch_docx (预处理)       {:>10.2?}", t.elapsed());
    println!("      ├─ 解压/读取 zip 条目    : {:>10.2?}", times.unzip);
    println!("      ├─ 正则替换 XML          : {:>10.2?}", times.patch_xml);
    println!("      └─ 重新 Deflate 打包     : {:>10.2?}", times.rezip);
    println!(
        "      （{} 字节 → {} 字节）",
        docx_bytes.len(),
        patched_docx.len()
    );

    // ---------- [3] 解析（dxpdf 内部阶段：解压、各 XML、媒体、页眉页脚…）----------
    let t = Instant::now();
    let document = dxpdf::docx::parse(&patched_docx)?;
    times.parse = t.elapsed();
    println!("[3] dxpdf::docx::parse        {:>10.2?}", times.parse);

    // ---------- [4] 渲染（resolve → registry → layout → subset → paint）----------
    let t = Instant::now();
    let pdf_bytes = dxpdf::render::render(document, &dxpdf::RenderOptions::default())?;
    times.render = t.elapsed();
    println!("[4] dxpdf::render::render     {:>10.2?}", times.render);

    // ---------- [5] 写出 ----------
    let t = Instant::now();
    std::fs::write(output_path, &pdf_bytes)?;
    times.write_file = t.elapsed();
    println!(
        "[5] 写出 {}     {:>10.2?}  ({} 字节)",
        output_path,
        times.write_file,
        pdf_bytes.len()
    );

    // ---------- [6] 汇总 ----------
    let total = total_start.elapsed();
    println!("[6] 总耗时                    {:>10.2?}", total);
    println!("    ---- 占比 ----");
    let rows: [(&str, Duration); 7] = [
        ("读取输入", times.read_file),
        ("patch_docx 预处理", times.unzip + times.patch_xml + times.rezip),
        ("  ├ 解压 zip", times.unzip),
        ("  ├ 改 XML", times.patch_xml),
        ("  └ 重打包", times.rezip),
        ("parse 解析", times.parse),
        ("render 渲染", times.render),
    ];
    for (name, d) in rows {
        println!("      {:<22} {:>10.2?}  {:>5.1}%", name, d, pct(d, total));
    }
    println!(
        "      {:<22} {:>10.2?}  {:>5.1}%",
        "写文件",
        times.write_file,
        pct(times.write_file, total)
    );

    println!("转换成功！保存为 {}", output_path);
    Ok(())
}
