//! DOCX 合并：把用户上传文档的内容「原封不动」追加到内置模板末尾，并填充模板占位符。
//!
//! 全程在 OOXML（zip + XML 字符串）层面完成，**不启动 Word / LibreOffice**。
//! 旧实现走 Python + wincom + Word 的 copy 命令，易报错且慢，已废弃。
//!
//! # 业务规则
//!
//! 1. 模板里 `{{Fund_cnname}}` / `{{letters_date}}` 替换成调用方传入的真实值。
//! 2. 模板在前，用户文档的内容在后。
//! 3. 追加的内容**格式、图片原封不动**（连分节导致的横/纵向版式也保留）。
//! 4. 追加的每一页**页眉页脚和模板一致**。
//! 5. **页码连续**，不因追加而重排。
//!
//! # 三个必须绕开的坑（都是实测出来的）
//!
//! - **占位符被 Word 拆成多个 run**。模板里 `{{Fund_cnname}}` 实际是
//!   `<w:t>{{</w:t>` + `<w:t>Fund_cnname</w:t>` + `<w:t>}}</w:t>` 三个 run，
//!   直接对 XML 做字符串替换**一定找不到**。见 [`fill_placeholders_in_part`]。
//! - **模板最终 sectPr 带 `<w:pgNumType w:start="1"/>`**，会让追加内容所在节的
//!   页码从 1 重排。删掉它，OOXML 语义即「承接上一节」，页码才连续。
//! - **用户 `docDefaults` 的字号在模板里不存在**。用户正文绝大多数 run 不写显式
//!   `<w:sz>`，靠 `docDefaults` 取字号；模板的 `docDefaults` 没有 `w:sz`，
//!   直接搬过去字号就变了。需要把这些「本文件默认值」回填到每个 run 上。

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use zip::write::ZipWriter;
use zip::ZipArchive;

// ---------------------------------------------------------------- 部件名与常量

const DOC: &str = "word/document.xml";
const DOC_RELS: &str = "word/_rels/document.xml.rels";
const CONTENT_TYPES: &str = "[Content_Types].xml";
const STYLES: &str = "word/styles.xml";

const REL_IMAGE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const REL_HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

/// CT_RPr 的子元素顺序（ECMA-376 §17.3.2）。回填默认值时必须按这个顺序插，
/// 否则产出的 docx 在 Word 里会被判为需要修复。
///
/// dxpdf 自己的 serde schema 按字段名解析、不挑顺序，但合并产物是给人验收的，
/// 所以严格排。
const RPR_ORDER: &[&str] = &[
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
];

/// 未知元素一律排在已知元素之后（`rPrChange` 之类必须留在最后）。
fn rpr_rank(name: &str) -> usize {
    RPR_ORDER
        .iter()
        .position(|n| *n == name)
        .unwrap_or(RPR_ORDER.len() + 1)
}

/// 合并结果的自查数据，用于日志与验收。
#[derive(Default, Debug)]
pub struct Stats {
    /// 占位符替换次数。
    pub replaced: usize,
    /// 回填了默认字号等属性的 run 数。
    pub backfilled: usize,
    /// 从用户文档搬进模板包的媒体文件数。
    pub media: usize,
    /// 搬过来的脚注/尾注条数。
    pub notes: usize,
}

// ---------------------------------------------------------------- 入口

/// 把用户 DOCX 的内容追加到模板末尾，产出一个新的 DOCX 字节流。
pub fn build_merged_docx(
    template_bytes: &[u8],
    user_bytes: &[u8],
    fund_cnname: &str,
    letters_date: &str,
) -> Result<(Vec<u8>, Stats), String> {
    let mut stats = Stats::default();
    let mut tpl = read_zip(template_bytes).map_err(|e| format!("解压内置模板失败: {e}"))?;
    let user = read_zip(user_bytes).map_err(|e| format!("解压上传的 DOCX 失败: {e}"))?;

    // ---- 1. 替换模板里的占位符（document.xml 和 header*.xml 里都有）--------
    let values: Vec<(&str, String)> = vec![
        ("{{Fund_cnname}}", fund_cnname.to_string()),
        ("{{letters_date}}", letters_date.to_string()),
    ];
    for (name, data) in tpl.iter_mut() {
        // 只看 `word/` 这一层：document.xml / header*.xml / footer*.xml / footnotes.xml…
        // （`word/theme/theme1.xml` 这种子目录里的部件不参与替换）
        if !is_word_part(name) {
            continue;
        }
        let xml = String::from_utf8_lossy(data).into_owned();
        if !xml.contains("{{") {
            continue;
        }
        let (new_xml, n) = fill_placeholders_in_part(&xml, &values);
        if n > 0 {
            stats.replaced += n;
            *data = new_xml.into_bytes();
        }
    }
    if stats.replaced == 0 {
        return Err("模板里没有找到任何 {{...}} 占位符，模板可能已损坏".to_string());
    }
    // 自检：正文可见文本里不允许残留 {{ —— 宁可直接失败，
    // 也不要产出一份带着占位符的正式文件。
    let mut all_text = String::new();
    for (name, data) in tpl.iter() {
        if !is_word_part(name) {
            continue;
        }
        let xml = String::from_utf8_lossy(data);
        let text = visible_text(&xml);
        if text.contains("{{") || text.contains("}}") {
            return Err(format!("部件 {name} 里仍有未替换的 {{{{...}}}} 占位符"));
        }
        all_text.push_str(&text);
    }
    // 两个值都必须真的落到文档里，否则说明占位符写法变了、替换静默失效。
    // 注意 `visible_text` 取的是 `<w:t>` 的**原始**内容（仍带实体编码），
    // 所以拿转义后的形式去比对。
    for (label, v) in [("Fund_cnname", fund_cnname), ("letters_date", letters_date)] {
        if !all_text.contains(&xml_escape(v)) {
            return Err(format!("模板里没有出现 {label} 的值，占位符可能已被改动"));
        }
    }

    // ---- 2. 组装新的 word/document.xml ------------------------------------
    let tpl_doc = part(&tpl, DOC)?.to_string();
    let user_doc = part(&user, DOC)?.to_string();
    let user_styles = part_opt(&user, STYLES).map(|s| s.to_string());

    // 回填清单：用户 docDefaults 里有、模板 docDefaults 里没有的 run 属性
    let tpl_styles = part_opt(&tpl, STYLES).map(|s| s.to_string());
    let inject = match (&user_styles, &tpl_styles) {
        (Some(u), Some(t)) => missing_defaults(u, t),
        _ => Vec::new(),
    };

    // 模板最终 sectPr 的页眉/页脚引用 —— 追加内容的每一个节都照抄这一组，
    // 保证「追加的每一页页眉页脚和模板一致」。
    let tpl_prs = find_sect_prs(&tpl_doc);
    let tpl_final = tpl_prs
        .last()
        .map(|&(s, e)| tpl_doc[s..e].to_string())
        .ok_or("模板 document.xml 里没有 sectPr")?;
    let tpl_refs = hf_refs(&tpl_final);
    if tpl_refs.is_empty() {
        return Err("模板最终 sectPr 里没有任何页眉/页脚引用".to_string());
    }
    // 页码连续：删掉模板最终节的 pgNumType(start=1)，让它承接上一节
    let tpl_final_cont = strip_pg_num_type(&tpl_final);

    let tpl_body = body_inner(&tpl_doc)?;
    let (tpl_head, tpl_tail_sect) = take_trailing_sect_pr(tpl_body);
    if tpl_tail_sect.is_none() {
        return Err("模板 <w:body> 末尾没有 sectPr".to_string());
    }

    // ---- 3. 取用户正文，并把它的节属性改造成「沿用模板页眉页脚」-----------
    let user_body = body_inner(&user_doc)?;
    let (mut user_content, user_final_sect) = take_trailing_sect_pr(user_body);

    // 用户正文里的内联 sectPr（实测：横版那一节的 `w:pgSz w:orient="landscape"`）：
    // 必须留着 —— 删掉的话横向页会变成纵向，内容重排，就不是「原封不动」了。
    //
    // 先摘掉它原有的页眉页脚引用 + pgNumType；模板的引用要等关系 ID 处理完
    // 再挂上去 —— 顺序反了的话，扫关系时会把模板的 rId15 当成上传文档的关系。
    user_content = sanitize_sect_prs(&user_content);

    // ---- 4. 回填默认值（字号等），然后改写关系 ID -------------------------
    let (user_content, backfilled) = backfill_run_defaults(&user_content, &inject);
    stats.backfilled = backfilled;

    let mut user_content = user_content;
    let rels_old = part(&user, DOC_RELS)?.to_string();
    let mut rels_new = part(&tpl, DOC_RELS)?.to_string();

    // 4a. 图片、超链接：搬到模板包里 + 在模板 rels 里建一条新关系
    let refs = scan_rel_refs(&user_content);
    let mut id_map: HashMap<String, String> = HashMap::new();
    for old_id in &refs {
        let Some((ty, target, mode)) = rel_lookup(&rels_old, old_id) else {
            return Err(format!("上传文档引用了不存在的关系 {old_id}"));
        };
        if ty == REL_IMAGE {
            let src = format!("word/{target}");
            let data = part_bytes(&user, &src)
                .map_err(|_| format!("上传文档缺少图片 {src}"))?
                .to_vec();
            let dest = unique_media_name(&tpl, &src)?;
            ensure_content_type(&mut tpl, &dest, &user)?;
            tpl.push((dest.clone(), data));
            stats.media += 1;
            let new_id = next_rel_id(&rels_new);
            rels_new = append_rel(&rels_new, &new_id, &ty, &dest["word/".len()..], false);
            id_map.insert(old_id.clone(), new_id);
        } else if ty == REL_HYPERLINK && mode.as_deref() == Some("External") {
            let new_id = next_rel_id(&rels_new);
            rels_new = append_rel(&rels_new, &new_id, &ty, &target, true);
            id_map.insert(old_id.clone(), new_id);
        } else {
            return Err(format!(
                "上传文档引用了暂不支持的关系类型 {ty}（{old_id} → {target}）"
            ));
        }
    }
    user_content = rewrite_rel_refs(&user_content, &id_map);

    // 4b. 脚注 / 尾注：把被正文引用的条目并进模板的 notes 部件
    for (notes_part, ref_tag, root_close) in [
        ("word/footnotes.xml", "w:footnoteReference", "</w:footnotes>"),
        ("word/endnotes.xml", "w:endnoteReference", "</w:endnotes>"),
    ] {
        let ids = scan_attr_values(&user_content, ref_tag, "w:id");
        if ids.is_empty() {
            continue;
        }
        let user_notes = part_opt(&user, notes_part).map(|s| s.to_string());
        let tpl_notes = part_opt(&tpl, notes_part).map(|s| s.to_string());
        let (Some(user_notes), Some(tpl_notes)) = (user_notes, tpl_notes) else {
            return Err(format!(
                "上传文档引用了 {notes_part}，但两边至少有一方没有这个部件"
            ));
        };
        let note_tag = notes_part
            .trim_start_matches("word/")
            .trim_end_matches(".xml")
            .trim_end_matches('s'); // footnotes → footnote
        let mut merged = tpl_notes;
        for old in ids {
            let (s, e) = find_note(&user_notes, note_tag, &old)
                .ok_or_else(|| format!("{notes_part} 里找不到被引用的条目 {old}"))?;
            let new_id = next_note_id(&merged, note_tag);
            let note = set_attr(&user_notes[s..e], "w:id", &new_id);
            let at = merged
                .rfind(root_close)
                .ok_or_else(|| format!("{notes_part} 缺少 {root_close}"))?;
            merged.insert_str(at, &note);
            user_content = replace_attr_value(&user_content, ref_tag, "w:id", &old, &new_id);
            stats.notes += 1;
        }
        set_part(&mut tpl, notes_part, merged.into_bytes());
    }

    // 关系都处理完了，现在把模板的页眉页脚引用挂到追加内容的每个 sectPr 上
    user_content = attach_hf_refs(&user_content, &tpl_refs);

    // ---- 5. 拼 body：模板 + 追加内容 -------------------------------------
    //
    // 节由「内联 sectPr」或「body 末尾 sectPr」终止，而**节的属性来自终止它的那个
    // sectPr**。
    //
    // 模板尾部那个节（最后一个内联 sectPr 之后的内容）通常只有空白段落 —— 本模板
    // 就只有一个空段落。这种节没有任何可见内容，**不能为它单开一节**：分节段落带
    // 的 `w:type` 缺省值是 nextPage，它会把这一节封成一整页空白，追加内容被推到
    // 下一页（实测：合并后第 2 页整页空白，正文从第 3 页才开始）。
    //
    // 所以先看模板尾部节有没有可见内容：有才保留它并补一个分节段落，没有就整个
    // 丢掉，让追加内容紧接封面。封面节由它自己的内联 sectPr 终止，不受影响。
    //
    // 拼出来的结构：
    //   节1 = 模板封面页      ← 模板内联 sectPr，保留 w:start="1"
    //   节2 = 追加内容前半段  ← 用户内联 sectPr，已挂模板页眉页脚引用、无 pgNumType
    //   节3 = 追加内容后半段  ← 用户最终 sectPr（同上）；用户没有则退回模板最终 sectPr
    //
    // （模板尾部节非空时，在它和追加内容之间插一个分节段落，即上面的节2 位置。）
    //
    // 页眉页脚：所有节都指向模板那套；页码依次承接：1 → 2 → 3,4,5…
    //
    // 末尾 sectPr 用**用户自己的**：这样追加内容最后一节保留它原本的页边距和纸张，
    // 否则会被换成模板正文的页面设置（实测左右页边距从 1800 变 1440、版心变宽、
    // 断行位置跟着变，就不是「原封不动」了）。只把页眉页脚引用换成模板的、并删掉
    // pgNumType 让页码接着往下走。
    let final_sect = match &user_final_sect {
        Some(s) => insert_hf_refs(&strip_pg_num_type(&strip_hf_refs(s)), &tpl_refs),
        None => tpl_final_cont.clone(),
    };

    let (tpl_core, tpl_trailing) = split_last_section(&tpl_head);
    let mut new_body = String::with_capacity(tpl_core.len() + user_content.len() + 512);
    new_body.push_str(tpl_core);
    if section_has_visible_content(tpl_trailing) {
        new_body.push_str(tpl_trailing);
        new_body.push_str(&sect_break_paragraph(&tpl_final_cont));
    }
    new_body.push_str(&user_content);
    new_body.push_str(&final_sect);

    let merged_doc = replace_body(&tpl_doc, &new_body)?;
    set_part(&mut tpl, DOC, merged_doc.into_bytes());
    set_part(&mut tpl, DOC_RELS, rels_new.into_bytes());

    let out = write_zip(&tpl)?;
    Ok((out, stats))
}

// ---------------------------------------------------------------- zip 读写

type Entries = Vec<(String, Vec<u8>)>;

fn read_zip(bytes: &[u8]) -> Result<Entries, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(|e| format!("不是合法 zip: {e}"))?;
    let mut out = Entries::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().to_string();
        let mut data = Vec::new();
        f.read_to_end(&mut data).map_err(|e| e.to_string())?;
        out.push((name, data));
    }
    Ok(out)
}

fn write_zip(entries: &Entries) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            zip.start_file(name.as_str(), opts).map_err(|e| e.to_string())?;
            zip.write_all(data).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    Ok(out.into_inner())
}

fn part<'a>(entries: &'a Entries, name: &str) -> Result<&'a str, String> {
    part_opt(entries, name).ok_or_else(|| format!("DOCX 里缺少部件 {name}"))
}

/// 取二进制部件（图片等）。不能用 [`part`] —— 它不是合法 UTF-8。
fn part_bytes<'a>(entries: &'a Entries, name: &str) -> Result<&'a [u8], String> {
    entries
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| d.as_slice())
        .ok_or_else(|| format!("DOCX 里缺少部件 {name}"))
}

fn part_opt<'a>(entries: &'a Entries, name: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| std::str::from_utf8(d).unwrap_or(""))
        .filter(|s| !s.is_empty())
}

fn set_part(entries: &mut Entries, name: &str, data: Vec<u8>) {
    if let Some(e) = entries.iter_mut().find(|(n, _)| n == name) {
        e.1 = data;
    } else {
        entries.push((name.to_string(), data));
    }
}

/// 是不是 `word/` 这一层下的 XML 部件。
///
/// 注意别写成 `name.contains('/')` —— `word/document.xml` 自己就带 `/`，
/// 那样会把所有部件都判掉（占位符一个都替换不到）。
fn is_word_part(name: &str) -> bool {
    match name.strip_prefix("word/") {
        Some(rest) => !rest.is_empty() && !rest.contains('/') && rest.ends_with(".xml"),
        None => false,
    }
}

// ---------------------------------------------------------------- XML 小工具

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// `<w:rFonts w:ascii="x"/>` → `rFonts`
fn elem_name(open_tag: &str) -> String {
    let t = open_tag.strip_prefix("<w:").unwrap_or(open_tag);
    let end = t
        .find(|c: char| matches!(c, ' ' | '/' | '>' | '\t' | '\r' | '\n'))
        .unwrap_or(t.len());
    t[..end].to_string()
}

/// `<w:xxx` 后面必须紧跟空白/`/`/`>`，否则就是 `w:sz` 匹配到 `w:szCs` 这类误命中。
fn tag_boundary_ok(s: &[u8], pos_after_name: usize) -> bool {
    matches!(
        s.get(pos_after_name),
        Some(b' ') | Some(b'/') | Some(b'>') | Some(b'\t') | Some(b'\r') | Some(b'\n') | None
    )
}

/// 在 `xml` 里找下一个 `<{tag}` 的起点（带名字边界检查）。
fn find_tag(xml: &str, from: usize, tag: &str) -> Option<usize> {
    let b = xml.as_bytes();
    let mut i = from;
    while let Some(rel) = xml[i..].find(tag) {
        let s = i + rel;
        if tag_boundary_ok(b, s + tag.len()) {
            return Some(s);
        }
        i = s + tag.len();
    }
    None
}

/// 找下一个子元素起点。
///
/// 不能用 [`find_tag`] + `"<w:"` —— 那个函数要求标签名后面紧跟空白/`/`/`>`，
/// 而 `"<w:"` 后面永远是名字的首字母，检查必然失败，结果一个子元素都找不到。
fn find_child_start(xml: &str, from: usize) -> Option<usize> {
    xml[from..].find("<w:").map(|k| from + k)
}

/// 元素/属性值的起始标签结束位置（`>` 的下标）。
fn tag_end(xml: &str, start: usize) -> Option<usize> {
    xml[start..].find('>').map(|k| start + k)
}

// ---------------------------------------------------------------- `<w:t>` 扫描

/// 一个 `<w:t>` 元素在 XML 里的位置与内容。
struct TEl {
    /// 元素起点（`<` 的下标）
    start: usize,
    /// 元素终点（`</w:t>` 的 `>` 之后）
    end: usize,
    /// 原始起始标签，如 `<w:t xml:space="preserve">` —— 重建时原样保留属性
    open_tag: String,
    /// 原始文本（**仍是实体编码的**，重建时不能再转义一次）
    text: String,
}

fn scan_t(xml: &str) -> Vec<TEl> {
    let b = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, i, "<w:t") {
        let gt = match tag_end(xml, s) {
            Some(g) => g,
            None => break,
        };
        if b[gt - 1] == b'/' {
            // <w:t/>
            out.push(TEl {
                start: s,
                end: gt + 1,
                open_tag: xml[s..gt + 1].to_string(),
                text: String::new(),
            });
            i = gt + 1;
            continue;
        }
        let close = match xml[gt..].find("</w:t>") {
            Some(k) => gt + k,
            None => break,
        };
        out.push(TEl {
            start: s,
            end: close + 6,
            open_tag: xml[s..gt + 1].to_string(),
            text: xml[gt + 1..close].to_string(),
        });
        i = close + 6;
    }
    out
}

/// 一个部件里所有可见文本（把各 `<w:t>` 拼起来），用于自检。
fn visible_text(xml: &str) -> String {
    scan_t(xml).into_iter().map(|e| e.text).collect()
}

/// 用新的文本重建一个 `<w:t>` 元素。`text` 必须是**已经转义好**的原始 XML 文本。
fn make_t(open_tag: &str, text: &str) -> String {
    if text.is_empty() {
        return "<w:t/>".to_string();
    }
    // 去掉原有的 xml:space，再按新文本是否需要保留空白补回来
    let mut tag = open_tag.to_string();
    if let Some(p) = tag.find("xml:space") {
        if let Some(eq) = tag[p..].find('=') {
            let q = p + eq + 1;
            if tag.as_bytes().get(q) == Some(&b'"') {
                let mut e = q + 1;
                while e < tag.len() && tag.as_bytes()[e] != b'"' {
                    e += 1;
                }
                e = (e + 1).min(tag.len());
                let mut s = p;
                while s > 0 && tag.as_bytes()[s - 1].is_ascii_whitespace() {
                    s -= 1;
                }
                tag.replace_range(s..e, "");
            }
        }
    }
    let need_space = text.starts_with(|c: char| c.is_whitespace())
        || text.ends_with(|c: char| c.is_whitespace());

    let mut out = String::with_capacity(tag.len() + text.len() + 16);
    if need_space && tag.ends_with('>') {
        out.push_str(&tag[..tag.len() - 1]);
        out.push_str(" xml:space=\"preserve\">");
    } else {
        out.push_str(&tag);
    }
    out.push_str(text);
    out.push_str("</w:t>");
    out
}

// ---------------------------------------------------------------- 占位符替换

/// 在一份 XML 部件里替换占位符，返回 `(新 XML, 替换次数)`。
///
/// 做法：按 `</w:p>` 切段（跨段拼接没有语义，会误匹配）→ 段内把所有 `<w:t>`
/// 的文本拼成一个字符串并记录每个 run 覆盖的区间 → 在拼接串上定位占位符 →
/// 把替换值写进**命中区间覆盖到的第一个 run**，同区间内其余 run 清空。
///
/// 之所以敢把值全塞进第一个 run：模板里 `{{` / `Fund_cnname` / `}}` 三个 run
/// 的 `<w:rPr>` 完全相同（同一个 `w:sz`），格式不会因此改变。
fn fill_placeholders_in_part(xml: &str, values: &[(&str, String)]) -> (String, usize) {
    let els = scan_t(xml);
    if els.is_empty() {
        return (xml.to_string(), 0);
    }

    // 切段：两个 <w:t> 之间若跨过 </w:p> 就断开
    let mut segs: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (idx, e) in els.iter().enumerate() {
        if let Some(&prev) = cur.last() {
            if xml[els[prev].end..e.start].contains("</w:p>") {
                segs.push(std::mem::take(&mut cur));
            }
        }
        cur.push(idx);
    }
    if !cur.is_empty() {
        segs.push(cur);
    }

    // run 下标 → 该 run 内要做的原位替换：(原文本起, 原文本止, 插入文本)
    let mut ops: Vec<Vec<(usize, usize, String)>> = vec![Vec::new(); els.len()];
    let mut count = 0usize;

    for seg in &segs {
        let mut joined = String::new();
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(seg.len());
        for &ri in seg {
            let s = joined.len();
            joined.push_str(&els[ri].text);
            spans.push((s, joined.len()));
        }

        let mut hits: Vec<(usize, usize, String)> = Vec::new();
        for (ph, val) in values {
            let esc = xml_escape(val);
            let mut from = 0usize;
            while let Some(rel) = joined[from..].find(ph) {
                let hs = from + rel;
                hits.push((hs, hs + ph.len(), esc.clone()));
                from = hs + ph.len();
            }
        }
        hits.sort_by_key(|h| h.0);

        for (hs, he, esc) in hits {
            count += 1;
            let mut first = true;
            for (k, &ri) in seg.iter().enumerate() {
                let (rs, re) = spans[k];
                let os = rs.max(hs);
                let oe = re.min(he);
                if os >= oe {
                    continue; // 这个 run 完全不在命中区间内
                }
                let ins = if first {
                    first = false;
                    esc.clone()
                } else {
                    String::new()
                };
                ops[ri].push((os - rs, oe - rs, ins));
            }
        }
    }

    if count == 0 {
        return (xml.to_string(), 0);
    }

    // 从后往前重建，避免前面的改动让后面的偏移失效
    let mut out = xml.to_string();
    for (ri, el) in els.iter().enumerate().rev() {
        if ops[ri].is_empty() {
            continue;
        }
        let mut list = std::mem::take(&mut ops[ri]);
        list.sort_by_key(|o| o.0);
        let mut text = String::new();
        let mut pos = 0usize;
        for (s, e, ins) in list {
            if s > pos {
                text.push_str(&el.text[pos..s]);
            }
            text.push_str(&ins);
            pos = e;
        }
        if pos < el.text.len() {
            text.push_str(&el.text[pos..]);
        }
        out.replace_range(el.start..el.end, &make_t(&el.open_tag, &text));
    }
    (out, count)
}

// ---------------------------------------------------------------- 节属性

/// 找出所有 `<w:sectPr>...</w:sectPr>` 的 `(起, 止)`。
fn find_sect_prs(xml: &str) -> Vec<(usize, usize)> {
    let b = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, i, "<w:sectPr") {
        let gt = match tag_end(xml, s) {
            Some(g) => g,
            None => break,
        };
        if b[gt - 1] == b'/' {
            out.push((s, gt + 1));
            i = gt + 1;
            continue;
        }
        let close = match xml[gt..].find("</w:sectPr>") {
            Some(k) => gt + k,
            None => break,
        };
        out.push((s, close + "</w:sectPr>".len()));
        i = close + "</w:sectPr>".len();
    }
    out
}

/// 抽出 sectPr 里的页眉/页脚引用元素（保持文档顺序）。
fn hf_refs(sect_pr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        let next = ["<w:headerReference", "<w:footerReference"]
            .iter()
            .filter_map(|t| find_tag(sect_pr, i, t))
            .min();
        let Some(s) = next else { break };
        let Some(gt) = tag_end(sect_pr, s) else { break };
        out.push(sect_pr[s..gt + 1].to_string());
        i = gt + 1;
    }
    out
}

/// 删掉 sectPr 里的所有页眉/页脚引用。
fn strip_hf_refs(sect_pr: &str) -> String {
    let mut out = String::with_capacity(sect_pr.len());
    let mut i = 0usize;
    loop {
        let next = ["<w:headerReference", "<w:footerReference"]
            .iter()
            .filter_map(|t| find_tag(sect_pr, i, t))
            .min();
        match next {
            Some(s) => {
                let Some(gt) = tag_end(sect_pr, s) else { break };
                out.push_str(&sect_pr[i..s]);
                i = gt + 1;
            }
            None => {
                out.push_str(&sect_pr[i..]);
                return out;
            }
        }
    }
    out.push_str(&sect_pr[i..]);
    out
}

/// 在 sectPr 起始标签之后插入一组页眉/页脚引用（CT_SectPr 要求它们排在最前）。
fn insert_hf_refs(sect_pr: &str, refs: &[String]) -> String {
    let Some(gt) = tag_end(sect_pr, 0) else {
        return sect_pr.to_string();
    };
    let mut out = String::with_capacity(sect_pr.len() + 256);
    out.push_str(&sect_pr[..gt + 1]);
    for r in refs {
        out.push_str(r);
    }
    out.push_str(&sect_pr[gt + 1..]);
    out
}

/// 删掉 `<w:pgNumType/>`。
///
/// 模板两个 sectPr 都写了 `w:start="1"`，即第 2 节自己从 1 重新起算。
/// 删掉之后按 OOXML 语义就是「承接上一节」，页码才会连续。
fn strip_pg_num_type(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut i = 0usize;
    loop {
        let Some(s) = find_tag(xml, i, "<w:pgNumType") else {
            out.push_str(&xml[i..]);
            return out;
        };
        let Some(gt) = tag_end(xml, s) else {
            out.push_str(&xml[i..]);
            return out;
        };
        out.push_str(&xml[i..s]);
        if xml.as_bytes()[gt - 1] == b'/' {
            i = gt + 1;
        } else if let Some(k) = xml[gt..].find("</w:pgNumType>") {
            i = gt + k + "</w:pgNumType>".len();
        } else {
            i = gt + 1;
        }
    }
}

/// 摘掉内容里所有 sectPr 的页眉/页脚引用，并去掉 pgNumType。
///
/// 用户自己的页眉（`header1.xml`）不跟着搬，所以这些引用必须去掉 ——
/// 否则后面扫关系 ID 时会被当成「上传文档引用的关系」而找不到。
fn sanitize_sect_prs(xml: &str) -> String {
    let prs = find_sect_prs(xml);
    if prs.is_empty() {
        return xml.to_string();
    }
    let mut out = xml.to_string();
    for (s, e) in prs.into_iter().rev() {
        let sect = strip_pg_num_type(&strip_hf_refs(&out[s..e]));
        out.replace_range(s..e, &sect);
    }
    out
}

/// 给内容里每个 sectPr 挂上 `refs`（模板正文那套页眉页脚）。
///
/// 显式指向、而不是靠「无引用则继承上一节」的隐含语义，结果确定、可核对 ——
/// 这就是「追加的每一页页眉页脚和模板一致」的落点。
fn attach_hf_refs(xml: &str, refs: &[String]) -> String {
    let prs = find_sect_prs(xml);
    if prs.is_empty() {
        return xml.to_string();
    }
    let mut out = xml.to_string();
    for (s, e) in prs.into_iter().rev() {
        let sect = insert_hf_refs(&out[s..e], refs);
        out.replace_range(s..e, &sect);
    }
    out
}

// ---------------------------------------------------------------- body

fn body_inner(doc_xml: &str) -> Result<&str, String> {
    let s = doc_xml
        .find("<w:body")
        .ok_or("document.xml 里没有 <w:body>")?;
    let gt = tag_end(doc_xml, s).ok_or("w:body 起始标签不完整")?;
    let e = doc_xml[gt..]
        .find("</w:body>")
        .ok_or("document.xml 里没有 </w:body>")?
        + gt;
    Ok(&doc_xml[gt + 1..e])
}

fn replace_body(doc_xml: &str, new_body: &str) -> Result<String, String> {
    let s = doc_xml
        .find("<w:body")
        .ok_or("document.xml 里没有 <w:body>")?;
    let gt = tag_end(doc_xml, s).ok_or("w:body 起始标签不完整")?;
    let e = doc_xml[gt..]
        .find("</w:body>")
        .ok_or("document.xml 里没有 </w:body>")?
        + gt;
    let mut out = String::with_capacity(doc_xml.len() + new_body.len());
    out.push_str(&doc_xml[..gt + 1]);
    out.push_str(new_body);
    out.push_str(&doc_xml[e..]);
    Ok(out)
}

/// 以「最后一个内联 sectPr 所在段落的结尾」为界，把正文切成两半。
///
/// 返回 `(前半, 最后一个节的正文)`。因为节的属性来自终止它的 sectPr，所以最后一个
/// 内联 sectPr 之后的内容就是模板的「尾部节」。
fn split_last_section(head: &str) -> (&str, &str) {
    let prs = find_sect_prs(head);
    let Some(&(_, end)) = prs.last() else {
        return (head, "");
    };
    match head[end..].find("</w:p>") {
        Some(k) => head.split_at(end + k + "</w:p>".len()),
        None => (head, ""),
    }
}

/// 这一节的正文里有没有「看得见的东西」。
///
/// 只含空白段落的节不该独占一页，见 `build_merged_docx` 第 5 步的说明。
fn section_has_visible_content(xml: &str) -> bool {
    if !visible_text(xml).trim().is_empty() {
        return true;
    }
    // 没有文本但仍有实体的：表格、图片、换行/分页。
    // `find_tag` 的边界检查会跳过 `<w:tblPr` 这类前缀相同的标签。
    [
        "<w:tbl",
        "<w:drawing",
        "<w:pict",
        "<w:object",
        "<w:br",
        "<w:pageBreakBefore",
    ]
    .iter()
    .any(|t| find_tag(xml, 0, t).is_some())
}

/// 造一个「分节段落」：段落本身没有内容，只负责用 `w:sectPr` 终止上一节。
///
/// 段落标记用 1/20 磅的行高（`w:line="1"` + `w:lineRule="exact"`）压到看不见，
/// 免得在模板正文末尾凭空多出一行空白。
///
/// 注意 CT_PPr 的顺序要求 `w:rPr` 在 `w:sectPr` 之前。
fn sect_break_paragraph(sect_pr: &str) -> String {
    format!(
        "<w:p><w:pPr>\
           <w:spacing w:before=\"0\" w:after=\"0\" w:line=\"1\" w:lineRule=\"exact\"/>\
           <w:rPr><w:sz w:val=\"2\"/><w:szCs w:val=\"2\"/></w:rPr>\
           {sect_pr}\
         </w:pPr></w:p>"
    )
}

/// 把 `<w:body>` 末尾那个 body 级 sectPr 摘出来。
///
/// 返回 `(去掉它的正文, 那个 sectPr)`。两边都要留着：用户的那份要挂上模板的页眉
/// 页脚引用后当整份文档的末尾 sectPr（保住它自己的页边距），模板的那份提供页眉
/// 页脚引用、并在模板尾部节非空时充当分节段落。
fn take_trailing_sect_pr(inner: &str) -> (String, Option<String>) {
    let prs = find_sect_prs(inner);
    if let Some(&(s, e)) = prs.last() {
        if inner[e..].trim().is_empty() {
            return (inner[..s].to_string(), Some(inner[s..e].to_string()));
        }
    }
    (inner.to_string(), None)
}

// ---------------------------------------------------------------- 关系（rels）

fn rel_attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let p = tag.find(&pat)? + pat.len();
    let e = tag[p..].find('"')? + p;
    Some(tag[p..e].to_string())
}

/// 查一个关系：`(Type, Target, TargetMode)`。
fn rel_lookup(rels: &str, id: &str) -> Option<(String, String, Option<String>)> {
    let mut i = 0usize;
    while let Some(s) = rels[i..].find("<Relationship") {
        let s = i + s;
        let gt = tag_end(rels, s)?;
        let tag = &rels[s..gt + 1];
        if rel_attr(tag, "Id").as_deref() == Some(id) {
            return Some((
                rel_attr(tag, "Type").unwrap_or_default(),
                rel_attr(tag, "Target").unwrap_or_default(),
                rel_attr(tag, "TargetMode"),
            ));
        }
        i = gt + 1;
    }
    None
}

fn used_rel_ids(rels: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = rels[i..].find("<Relationship") {
        let s = i + s;
        let Some(gt) = tag_end(rels, s) else { break };
        if let Some(id) = rel_attr(&rels[s..gt + 1], "Id") {
            out.push(id);
        }
        i = gt + 1;
    }
    out
}

/// 下一个可用的 `rIdN`（取现有最大值 +1，并避让非数字格式的既有 ID）。
fn next_rel_id(rels: &str) -> String {
    let used = used_rel_ids(rels);
    let mut max = 0u32;
    for id in &used {
        if let Some(n) = id.strip_prefix("rId").and_then(|x| x.parse::<u32>().ok()) {
            max = max.max(n);
        }
    }
    loop {
        max += 1;
        let cand = format!("rId{max}");
        if !used.contains(&cand) {
            return cand;
        }
    }
}

fn append_rel(rels: &str, id: &str, ty: &str, target: &str, external: bool) -> String {
    let mode = if external { " TargetMode=\"External\"" } else { "" };
    let new = format!(
        "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{}\"{mode}/>",
        xml_escape(target)
    );
    match rels.rfind("</Relationships>") {
        Some(at) => format!("{}{}{}", &rels[..at], new, &rels[at..]),
        None => format!("{rels}{new}"),
    }
}

/// 扫出正文里所有 `r:embed` / `r:link` / `r:id` 引用的 ID（去重）。
///
/// 只认带 `r:` 前缀的，所以 `w:id="1"`（脚注引用）不会被误收。
fn scan_rel_refs(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pat in ["r:embed=\"", "r:link=\"", "r:id=\""] {
        let mut i = 0usize;
        while let Some(rel) = xml[i..].find(pat) {
            let s = i + rel + pat.len();
            let Some(k) = xml[s..].find('"') else { break };
            let v = xml[s..s + k].to_string();
            if !out.contains(&v) {
                out.push(v);
            }
            i = s + k + 1;
        }
    }
    out
}

/// 按映射改写正文里的关系 ID。模式里带引号，天然避免 `rId1` 误伤 `rId10`。
fn rewrite_rel_refs(xml: &str, map: &HashMap<String, String>) -> String {
    let mut out = xml.to_string();
    for (old, new) in map {
        for p in ["r:embed", "r:link", "r:id"] {
            out = out.replace(&format!("{p}=\"{old}\""), &format!("{p}=\"{new}\""));
        }
    }
    out
}

// ---------------------------------------------------------------- 媒体与内容类型

/// 给搬进来的媒体文件找一个模板包里没被占用的路径。
fn unique_media_name(tpl: &Entries, src: &str) -> Result<String, String> {
    let ext = src.rsplit_once('.').map(|(_, e)| e).unwrap_or("bin");
    let dir = src.rsplit_once('/').map(|(d, _)| d).unwrap_or("word/media");
    if !tpl.iter().any(|(n, _)| n == src) {
        return Ok(src.to_string());
    }
    for i in 1..1000 {
        let cand = format!("{dir}/appended{i}.{ext}");
        if !tpl.iter().any(|(n, _)| n == &cand) {
            return Ok(cand);
        }
    }
    Err(format!("模板包里的 {dir} 目录没有可用的文件名"))
}

/// 模板只声明了 png，搬进来的 jpeg 必须补一条 `Default`，
/// 否则产出的 docx 不合法（Word 会报需要修复）。
fn ensure_content_type(tpl: &mut Entries, dest: &str, user: &Entries) -> Result<(), String> {
    let ext = dest.rsplit_once('.').map(|(_, e)| e).unwrap_or("").to_string();
    if ext.is_empty() {
        return Ok(());
    }
    let need = {
        let ct = part(tpl, CONTENT_TYPES)?;
        !ct.contains(&format!("Extension=\"{ext}\""))
    };
    if !need {
        return Ok(());
    }
    // 从上传文档的 [Content_Types].xml 里抄同后缀的声明
    let decl = part_opt(user, CONTENT_TYPES).and_then(|ct| {
        let pat = format!("Extension=\"{ext}\"");
        let p = ct.find(&pat)?;
        let s = ct[..p].rfind('<')?;
        let e = ct[p..].find("/>")? + p + 2;
        Some(ct[s..e].to_string())
    });
    let decl = decl.ok_or_else(|| format!("无法确定 .{ext} 的 Content-Type"))?;

    let cur = part(tpl, CONTENT_TYPES)?.to_string();
    let at = cur
        .rfind("</Types>")
        .ok_or("[Content_Types].xml 缺少 </Types>")?;
    let mut new = String::with_capacity(cur.len() + decl.len());
    new.push_str(&cur[..at]);
    new.push_str(&decl);
    new.push_str(&cur[at..]);
    set_part(tpl, CONTENT_TYPES, new.into_bytes());
    Ok(())
}

// ---------------------------------------------------------------- docDefaults 回填

/// 从 styles.xml 的 `docDefaults/rPrDefault/w:rPr` 里取出子元素原文（保持顺序）。
fn doc_default_rpr(styles: &str) -> Vec<String> {
    let Some(d) = styles.find("<w:docDefaults>") else {
        return Vec::new();
    };
    let Some(r) = styles[d..].find("<w:rPrDefault>") else {
        return Vec::new();
    };
    let r = d + r;
    let Some(p) = styles[r..].find("<w:rPr>") else {
        return Vec::new();
    };
    let p = r + p + "<w:rPr>".len();
    let Some(e) = styles[p..].find("</w:rPr>") else {
        return Vec::new();
    };
    split_elements(&styles[p..p + e])
}

/// 把一段 XML 拆成顶层子元素原文。
fn split_elements(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_child_start(inner, i) {
        let Some(gt) = tag_end(inner, s) else { break };
        if inner.as_bytes()[gt - 1] == b'/' {
            out.push(inner[s..gt + 1].to_string());
            i = gt + 1;
            continue;
        }
        let name = elem_name(&inner[s..gt + 1]);
        let close = format!("</w:{name}>");
        match inner[gt..].find(&close) {
            Some(k) => {
                out.push(inner[s..gt + k + close.len()].to_string());
                i = gt + k + close.len();
            }
            None => {
                out.push(inner[s..gt + 1].to_string());
                i = gt + 1;
            }
        }
    }
    out
}

/// 用户 `docDefaults` 里有、模板 `docDefaults` 里没有的 run 属性 —— 就是需要
/// 逐个 run 回填的那几个。实测这里会是 `kern` / `sz` / `szCs`。
fn missing_defaults(user_styles: &str, tpl_styles: &str) -> Vec<String> {
    let tpl_names: Vec<String> = doc_default_rpr(tpl_styles)
        .iter()
        .map(|e| elem_name(e))
        .collect();
    doc_default_rpr(user_styles)
        .into_iter()
        .filter(|e| !tpl_names.contains(&elem_name(e)))
        .collect()
}

fn has_child(inner: &str, name: &str) -> bool {
    find_tag(inner, 0, &format!("<w:{name}")).is_some()
}

/// 把 `items` 按 CT_RPr 顺序插进 `inner`。
fn insert_ordered(inner: &str, items: &[String]) -> String {
    // 现有顶层子元素的起点
    let mut kids: Vec<(String, usize)> = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_child_start(inner, i) {
        let Some(gt) = tag_end(inner, s) else { break };
        let name = elem_name(&inner[s..gt + 1]);
        kids.push((name.clone(), s));
        if inner.as_bytes()[gt - 1] == b'/' {
            i = gt + 1;
        } else {
            let close = format!("</w:{name}>");
            i = match inner[gt..].find(&close) {
                Some(k) => gt + k + close.len(),
                None => gt + 1,
            };
        }
    }

    let mut ins: Vec<(usize, usize, String)> = Vec::new();
    for it in items {
        let r = rpr_rank(&elem_name(it));
        // 插在第一个「顺序排在它后面」的现有元素之前；没有就追加到末尾
        let at = kids
            .iter()
            .find(|(n, _)| rpr_rank(n) > r)
            .map(|(_, p)| *p)
            .unwrap_or(inner.len());
        ins.push((at, r, it.clone()));
    }
    ins.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut out = String::with_capacity(inner.len() + 64);
    let mut pos = 0usize;
    for (at, _, text) in ins {
        if at > pos {
            out.push_str(&inner[pos..at]);
            pos = at;
        }
        out.push_str(&text);
    }
    if pos < inner.len() {
        out.push_str(&inner[pos..]);
    }
    out
}

/// 给每个缺少这些属性的 run 补上默认值，返回 `(新 XML, 补过的 run 数)`。
///
/// 不补的话，用户正文里那些不写 `<w:sz>` 的 run 会改用**模板的**默认字号渲染，
/// 字号就变了 —— 这正是「格式原封不动」最容易被破坏的地方。
fn backfill_run_defaults(xml: &str, inject: &[String]) -> (String, usize) {
    if inject.is_empty() {
        return (xml.to_string(), 0);
    }
    let b = xml.as_bytes();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut count = 0usize;
    let mut i = 0usize;

    while let Some(s) = find_tag(xml, i, "<w:r") {
        let Some(gt) = tag_end(xml, s) else { break };
        if b[gt - 1] == b'/' {
            i = gt + 1;
            continue;
        }
        let Some(close) = xml[gt..].find("</w:r>") else {
            break;
        };
        let run_end = gt + close + 6;

        // run 内第一个 <w:rPr>（真实 rPr；w:rPrChange 在它内部，不会被先撞上）
        let rpr = find_tag(&xml[gt + 1..run_end], 0, "<w:rPr").map(|p| gt + 1 + p);
        let mut handled = false;
        if let Some(rp) = rpr {
            if xml.as_bytes().get(rp + 6) == Some(&b'>') {
                if let Some(rpgt) = tag_end(xml, rp) {
                    if let Some(k) = xml[rpgt..run_end].find("</w:rPr>") {
                        let inner = &xml[rpgt + 1..rpgt + k];
                        let missing: Vec<String> = inject
                            .iter()
                            .filter(|e| !has_child(inner, &elem_name(e)))
                            .cloned()
                            .collect();
                        if !missing.is_empty() {
                            let new_inner = insert_ordered(inner, &missing);
                            edits.push((rpgt + 1, rpgt + k, new_inner));
                            count += 1;
                        }
                        handled = true;
                    }
                }
            }
        }
        if !handled {
            // 整个 run 连 <w:rPr> 都没有：新建一个
            let mut sorted = inject.to_vec();
            sorted.sort_by_key(|e| rpr_rank(&elem_name(e)));
            let mut rpr_xml = String::from("<w:rPr>");
            for e in &sorted {
                rpr_xml.push_str(e);
            }
            rpr_xml.push_str("</w:rPr>");
            edits.push((gt + 1, gt + 1, rpr_xml));
            count += 1;
        }
        i = run_end;
    }

    if edits.is_empty() {
        return (xml.to_string(), 0);
    }
    let mut out = xml.to_string();
    for (s, e, text) in edits.into_iter().rev() {
        out.replace_range(s..e, &text);
    }
    (out, count)
}

// ---------------------------------------------------------------- 脚注 / 尾注

/// 扫出某个引用元素上指定属性的所有取值（如所有 `<w:footnoteReference w:id="N">` 的 N）。
fn scan_attr_values(xml: &str, tag: &str, attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, i, tag) {
        let Some(gt) = tag_end(xml, s) else { break };
        if let Some(v) = rel_attr(&xml[s..gt + 1], attr) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
        i = gt + 1;
    }
    out
}

/// 找一条 `<w:footnote ... w:id="N">...</w:footnote>`。
fn find_note(notes: &str, note_tag: &str, id: &str) -> Option<(usize, usize)> {
    let open = format!("<w:{note_tag}");
    let mut i = 0usize;
    while let Some(s) = find_tag(notes, i, &open) {
        let gt = tag_end(notes, s)?;
        if rel_attr(&notes[s..gt + 1], "w:id").as_deref() == Some(id) {
            if notes.as_bytes()[gt - 1] == b'/' {
                return Some((s, gt + 1));
            }
            let close = format!("</w:{note_tag}>");
            let e = notes[gt..].find(&close)? + gt + close.len();
            return Some((s, e));
        }
        i = gt + 1;
    }
    None
}

fn next_note_id(notes: &str, note_tag: &str) -> String {
    let open = format!("<w:{note_tag}");
    let mut max = 0i64;
    let mut i = 0usize;
    while let Some(s) = find_tag(notes, i, &open) {
        let Some(gt) = tag_end(notes, s) else { break };
        if let Some(v) = rel_attr(&notes[s..gt + 1], "w:id") {
            if let Ok(n) = v.parse::<i64>() {
                max = max.max(n);
            }
        }
        i = gt + 1;
    }
    (max + 1).to_string()
}

/// 改写元素起始标签上的某个属性值。
fn set_attr(elem: &str, attr: &str, value: &str) -> String {
    let pat = format!("{attr}=\"");
    let Some(p) = elem.find(&pat) else {
        return elem.to_string();
    };
    let vs = p + pat.len();
    let Some(k) = elem[vs..].find('"') else {
        return elem.to_string();
    };
    format!("{}{}{}", &elem[..vs], value, &elem[vs + k..])
}

/// 把正文里 `<tag attr="old">` 的属性值换成 `new`。
fn replace_attr_value(xml: &str, tag: &str, attr: &str, old: &str, new: &str) -> String {
    let mut out = xml.to_string();
    let mut i = 0usize;
    while let Some(s) = find_tag(&out, i, tag) {
        let Some(gt) = tag_end(&out, s) else { break };
        let seg = &out[s..gt + 1];
        if rel_attr(seg, attr).as_deref() == Some(old) {
            let new_seg = set_attr(seg, attr, new);
            out.replace_range(s..gt + 1, &new_seg);
            i = s + new_seg.len();
        } else {
            i = gt + 1;
        }
    }
    out
}
