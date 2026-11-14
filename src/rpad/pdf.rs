//! DOCX → PDF：dxpdf + Skia 进程内转换。
//!
//! 与 `src/dxpdfd.rs` 里的转换管线同源（那条管线已用真实模板验收过）：
//! 1. 预处理：补全 dxpdf serde schema 必填的 `w:ilvl`（很多非 Word 生成的 DOCX 不带）；
//! 2. `dxpdf::docx::parse` 解析；
//! 3. `dxpdf::render::render_with_font_mgr` 渲染（thread-local FontMgr，见下）；
//! 4. `lopdf` 回读校验页数 > 0（防「0 页却报成功」的坑）。
//!
//! dxpdf 的 `render` 没有 panic 保护，常驻进程里一个畸形 DOCX 不能把服务带崩，
//! 所以整体包了 `catch_unwind`。

use std::io::{Cursor, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use lopdf::Document;

/// 转换失败的阶段（日志里区分「文档本身的问题」还是「环境问题」）。
#[derive(Debug)]
pub enum ConvError {
    Preprocess(String),
    Parse(String),
    Render(String),
    Output(String),
    Panic(String),
}

impl ConvError {
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Preprocess(_) => "preprocess",
            Self::Parse(_) => "parse",
            Self::Render(_) => "render",
            Self::Output(_) => "output",
            Self::Panic(_) => "panic",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Preprocess(m)
            | Self::Parse(m)
            | Self::Render(m)
            | Self::Output(m)
            | Self::Panic(m) => m,
        }
    }
}

impl std::fmt::Display for ConvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.stage(), self.message())
    }
}

thread_local! {
    /// `skia_safe::FontMgr` 是 `!Send + !Sync`（skia-safe 只给 `Typeface` 标了
    /// unsafe_send_sync），进不了 `static`，只能每线程一个。复用比每请求重建强。
    static FONT_MGR: skia_safe::FontMgr = skia_safe::FontMgr::new();
}

fn with_font_mgr<T>(f: impl FnOnce(&skia_safe::FontMgr) -> T) -> T {
    FONT_MGR.with(f)
}

/// DOCX → PDF。返回 `(PDF 字节, 页数, 内部分段耗时)`。CPU 密集，调用方应放到
/// `spawn_blocking`。
///
/// 分段耗时是为了回答「6 秒到底花在哪」：预处理（zip 往返）／解析／渲染／校验。
pub fn docx_to_pdf(
    docx_bytes: &[u8],
    opts: &dxpdf::RenderOptions,
) -> Result<(Vec<u8>, usize, String), ConvError> {
    match catch_unwind(AssertUnwindSafe(move || {
        convert_inner(docx_bytes, opts)
    })) {
        Ok(r) => r,
        Err(p) => Err(ConvError::Panic(panic_message(&p))),
    }
}

fn convert_inner(
    docx_bytes: &[u8],
    opts: &dxpdf::RenderOptions,
) -> Result<(Vec<u8>, usize, String), ConvError> {
    let t = Instant::now();
    // 1) 补全 dxpdf serde schema 要求的 w:ilvl（模板无 numbering.xml 时是空操作）。
    //    文档本来就是合规的（绝大多数情况）时 `None`，直接用原字节，不重打包。
    let patched = patch_docx(docx_bytes).map_err(|e| ConvError::Preprocess(e.to_string()))?;
    let pre_ms = ms(t.elapsed());

    let t = Instant::now();
    // 2) 解析。render 按值消费 Document，所以每请求必须重新 parse。
    let document = dxpdf::docx::parse(patched.as_deref().unwrap_or(docx_bytes))
        .map_err(|e| ConvError::Parse(e.to_string()))?;
    let parse_ms = ms(t.elapsed());

    let t = Instant::now();
    // 3) 渲染
    let pdf = with_font_mgr(|mgr| dxpdf::render::render_with_font_mgr(document, mgr, opts))
        .map_err(|e| ConvError::Render(e.to_string()))?;
    let render_ms = ms(t.elapsed());

    let t = Instant::now();
    // 4) 回读校验：既确认是合法 PDF，又拿到真实页数
    let pages = probe_pdf(&pdf).map_err(ConvError::Output)?;
    let probe_ms = ms(t.elapsed());

    if pages == 0 {
        return Err(ConvError::Output("渲染结果页数为 0".to_string()));
    }
    let breakdown = format!(
        "预处理{pre_ms}ms / 解析{parse_ms}ms / 渲染{render_ms}ms / 校验{probe_ms}ms"
    );
    Ok((pdf, pages, breakdown))
}

fn ms(d: std::time::Duration) -> u64 {
    d.as_millis() as u64
}

fn probe_pdf(bytes: &[u8]) -> Result<usize, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("输出缺少 %PDF- 魔数".to_string());
    }
    let doc = Document::load_mem(bytes).map_err(|e| e.to_string())?;
    Ok(doc.get_pages().len())
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "(非文本 panic)".to_string()
    }
}

// ============================================================================
// DOCX 预处理：补全 dxpdf serde schema 要求的 w:ilvl
// ============================================================================
//
// 以下三个函数从 `src/dxpdfd.rs` 直接拷贝过来（本 bin 不依赖 water_mark 库，
// 与 dxpdfd 的做法一致）。
//
// `dxpdf` 的 serde schema 把 `@ilvl` 当必填字段，但很多非 Word 生成的 DOCX
// 不带这个属性，解析会直接失败。要补两处：
//
// 1. `word/numbering.xml` — `<w:lvl>` 元素需要 `w:ilvl` 属性
// 2. `word/document.xml`、`word/header*.xml`、`word/footer*.xml` —
//    `<w:numPr>` 块需要 `<w:ilvl w:val="0"/>` 子元素

/// 返回 `None` 表示这份文档不需要修补（调用方直接用原字节，省掉一整次 zip 往返）。
fn patch_docx(docx_bytes: &[u8]) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    // 直接借入 `bytes`：zip 只需要 Read + Seek，不必先把整份文档拷一份。
    let mut archive = zip::ZipArchive::new(Cursor::new(docx_bytes))?;

    // 读取所有条目到内存
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        entries.push((name, data));
    }

    // 修补需要处理的 XML 条目。
    //
    // 先做一次**字节级**的 `contains` 预判：绝大多数部件里根本没有 `<w:lvl` /
    // `<w:numPr>`，直接跳过，省掉 `from_utf8_lossy(...).into_owned()` 加
    // `replace` 的两次整部件分配（document.xml 动辄几百 KB，逐个白重写一遍不划算）。
    let mut changed = false;
    for (name, data) in entries.iter_mut() {
        if !name.ends_with(".xml") {
            continue;
        }
        let patched: Option<String> = if name == "word/numbering.xml" {
            if !contains_bytes(data, b"<w:lvl") {
                None
            } else {
                Some(patch_lvl_ilvl(&String::from_utf8_lossy(data)))
            }
        } else if name.starts_with("word/")
            && (name.contains("document") || name.contains("header") || name.contains("footer"))
            && contains_bytes(data, b"<w:numPr>")
        {
            Some(patch_numpr_ilvl(&String::from_utf8_lossy(data)))
        } else {
            None
        };
        if let Some(xml) = patched {
            let new = xml.into_bytes();
            // 内容没变就别算「改过」——否则每份文档都要白重压一次 zip。
            if new != *data {
                *data = new;
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(None);
    }

    // 重新打包为 ZIP
    let mut output = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut output);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for (name, data) in &entries {
            zip.start_file(name.as_str(), opts)?;
            zip.write_all(data)?;
        }
        zip.finish()?;
    }
    Ok(Some(output.into_inner()))
}

/// 字节级的子串查找（memchr 级别就够，不需要引入依赖）。
fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// 在 `<w:lvl>` 标签中添加缺失的 `w:ilvl` 属性。
fn patch_lvl_ilvl(xml: &str) -> String {
    let mut result = xml.replace("<w:lvl>", r#"<w:lvl w:ilvl="0">"#);
    result = result.replace("<w:lvl/>", r#"<w:lvl w:ilvl="0"/>"#);
    result
}

/// 在 `<w:numPr>` 中插入缺失的 `<w:ilvl w:val="0"/>`。
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
                        result.push_str(r#"<w:ilvl w:val="0"/>"#);
                        result.push_str(inner);
                    }
                } else {
                    result.push_str(inner);
                }
                result.push_str("</w:numPr>");
                remaining = &rest[end + 10..];
            } else {
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
