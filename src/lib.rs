use ab_glyph::{Font, FontRef, PxScale, OutlineCurve, Point, ScaleFont};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, IncrementalDocument, Object, ObjectId, Stream};
use lopdf::dictionary;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

// ============================================================================
// 常量定义 - Constants
// ============================================================================

/// 默认字体大小（点数）
const DEFAULT_FONT_SIZE: f32 = 26.0;

/// 水平方向水印间距（固定点数）
/// 沿水印文字本身方向（行内）两个相邻水印起点之间的距离 = text_w + GRID_HORIZONTAL_GAP
const GRID_HORIZONTAL_GAP: f32 = 30.0;

/// 垂直方向水印间距倍数（相对于字体大小）
/// 实际行间距 = DEFAULT_FONT_SIZE * GRID_VERTICAL_MULTIPLIER = 26.0 * 6.0 = 156.0 点
/// 注意：这个值故意设置得比水平间距大，以避免垂直方向的水印过于密集
const GRID_VERTICAL_MULTIPLIER: f32 = 6.0;

/// 水印旋转角度（度数）
/// 仅控制水印自身的倾斜方向，与 PDF 页面 /Rotate 无关
const WATERMARK_ANGLE_DEG: f32 = 60.0;

/// 水印网格中心在页面X轴的偏移（用于视觉居中调整）
const CENTER_X_OFFSET: f32 = 0.0;

/// 水印网格中心在页面Y轴的偏移（用于视觉居中调整）
const CENTER_Y_OFFSET: f32 = 0.0;

/// 覆盖范围倍数（相对于页面对角线长度）
/// 较大的值能确保页面各个角落都被水印覆盖，但也会增加计算量
/// 建议范围：1.5 ~ 2.5
const COVERAGE_MULTIPLIER: f32 = 2.2;

/// 可见性裁剪边界（单位：点数）
/// 在 AABB 交集判断时给页面加一圈膨胀余量，避免边界处的水印被误删
const VISIBILITY_MARGIN: f32 = 200.0;

// ----------------------------------------------------------------------------
// 水印 XObject（Form XObject）自身的 BBox
// 必须与 run_watermark_process 中写入 "BBox" 的 4 个值完全一致
// 可见性裁剪时会把它作为"水印文本在局部坐标系的范围"参与旋转后 AABB 的计算
// ----------------------------------------------------------------------------

/// 水印 XObject 局部坐标系：左下角 x（略留负余量避免裁剪到字形左下伸出部分）
const XOBJ_BBOX_LLX: f32 = -10.0;
/// 水印 XObject 局部坐标系：左下角 y（留足负余量避免裁剪到下伸字形）
const XOBJ_BBOX_LLY: f32 = -50.0;
/// 水印 XObject 局部坐标系：右上角 x（需覆盖最长可能的文本宽度，一般留 2000 足够）
const XOBJ_BBOX_URX: f32 = 2000.0;
/// 水印 XObject 局部坐标系：右上角 y（留足上伸字形 + 字号的余量）
const XOBJ_BBOX_URY: f32 = 200.0;

/// 单个PDF允许的最大水印数量
/// 防止极端情况（极小的页面或间距）导致生成过多水印对象
const MAX_ALLOWED_WATERMARKS: usize = 1_000_000;

/// 网格间距最小值校验
/// 如果水平或垂直间距小于此值，拒绝生成以避免过度计算
const MIN_GRID_STEP_SIZE: f32 = 0.1;

// ============================================================================
// FFI 接口 - C语言互操作
// ============================================================================

/// 供C/其他语言调用的FFI接口
/// 
/// # Safety
/// 
/// 调用者必须确保所有传入的指针 (`input_path`, `output_path`, `font_path`, 
/// `user_name`, `date_str`) 都是有效的、指向以空字符结尾的 C 字符串（null-terminated）。
/// 此外，这些指针在函数调用期间必须保持有效且不被修改。
///
/// # 参数
/// - `input_path`: 输入PDF文件路径
/// - `output_path`: 输出PDF文件路径
/// - `font_path`: 字体文件路径
/// - `user_name`: 用户名
/// - `date_str`: 日期字符串
///
/// # 返回值
/// - `0`: 成功
/// - `-1`: 处理过程中发生错误
/// - `-2`: 空指针参数
/// - `-3`: UTF-8编码错误
#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_pdf_watermark(
    input_path: *const c_char,
    output_path: *const c_char,
    font_path: *const c_char,
    user_name: *const c_char,
    date_str: *const c_char,
) -> i32 {
    // 参数空指针检查
    if input_path.is_null()
        || output_path.is_null()
        || font_path.is_null()
        || user_name.is_null()
        || date_str.is_null()
    {
        eprintln!("ERROR: NULL pointer passed to add_pdf_watermark");
        return -2;
    }

    // 将 CStr 转为 &str 并校验 UTF-8
    let input = unsafe {
        match CStr::from_ptr(input_path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in input_path");
                return -3;
            }
        }
    };
    let output = unsafe {
        match CStr::from_ptr(output_path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in output_path");
                return -3;
            }
        }
    };
    let font_p = unsafe {
        match CStr::from_ptr(font_path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in font_path");
                return -3;
            }
        }
    };
    let name = unsafe {
        match CStr::from_ptr(user_name).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in user_name");
                return -3;
            }
        }
    };
    let date = unsafe {
        match CStr::from_ptr(date_str).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in date_str");
                return -3;
            }
        }
    };

    let text = format!("致{}-{}:高度保密", name, date);

    match run_watermark_process(&input, &output, &font_p, &text) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("ERROR: add_pdf_watermark failed: {:?}", e);
            -1
        }
    }
}

// ============================================================================
// 公共处理函数 - 供 main.rs 和 FFI 调用
// ============================================================================

/// 执行水印处理的主函数
///
/// # 流程
/// 1. 加载PDF文档
/// 2. 读取并解析字体（只做一次）
/// 3. 预计算文本矢量路径（只做一次）
/// 4. 将文本作为XObject流对象嵌入PDF
/// 5. 遍历所有页面，生成水印网格（考虑页面旋转）
/// 6. 保存处理后的PDF
///
/// # 参数
/// - `input_path`: 输入PDF路径
/// - `output_path`: 输出PDF路径
/// - `font_path`: 字体文件路径
/// - `text`: 水印文本
///
/// # 返回
/// - `Ok(())`: 水印已写入并保存成功
/// - `Err`: 处理过程中的错误信息（含所有页面都注入失败的情况）
pub fn run_watermark_process(
    input_path: &str,
    output_path: &str,
    font_path: &str,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // 加载 PDF
    let mut doc = Document::load(input_path)?;

    // 加密 PDF 必须先解密再处理。
    // lopdf 不会自动解密：未解密时对象流（ObjStm）的字节是密文，解析不出里面的对象，
    // 于是页树无法解析、大量对象静默丢失，最终写出的是一份损坏的文件。
    if doc.is_encrypted() {
        doc.decrypt("").map_err(|e| {
            format!(
                "输入 PDF 已加密，且无法用空密码解密（可能需要用户密码）：{:?}",
                e
            )
        })?;
    }

    // 读取并解析字体（一次性）
    let font_data = std::fs::read(font_path)?;
    let font = FontRef::try_from_slice(&font_data)?;

    // 预计算文本矢量（只做一次）
    let watermark_ops = text_to_pdf_paths(&font, text, 0.0, 0.0, DEFAULT_FONT_SIZE);
    let watermark_content = Content {
        operations: watermark_ops,
    };
    let encoded = watermark_content
        .encode()
        .map_err(|e| format!("encode watermark content failed: {:?}", e))?;
    // 预计算文本宽度，避免重复计算
    let text_w = measure_text_width(&font, text, DEFAULT_FONT_SIZE);

    // =====================================================================
    // 数字签名：直接移除，让阅读器不再提示「签名无效」
    //
    // 加水印必然改写页面内容，而数字签名校验的是**签名时刻的原始文件字节**，
    // 因此签名无论如何都不可能再通过校验（Adobe 会提示「签名无效：文档自签名
    // 后已被更改或损坏」）。业务上不需要保留签名的法律效力，所以这里把签名域、
    // 签名值字典、目录里的 /Perms 以及页面上的签名控件一并删除，让阅读器认为
    // 这份 PDF 从未被签名过，从根上消除该告警。
    // =====================================================================
    let removed_signatures = strip_digital_signatures(&mut doc);
    if removed_signatures > 0 {
        eprintln!(
            "INFO: 检测到数字签名，已剥离 {} 处签名校验信息（签章图章按原样保留）",
            removed_signatures
        );
    }

    // =====================================================================
    // 保存策略：无签名时优先「增量更新」，否则「整篇重写」
    //
    // 增量更新只把新对象与新的交叉引用段追加在文件末尾，原始字节一字不动，
    // 输出体积更小；但移除签名必须把原始字节一并重写，两条路径因此互斥。
    //
    // 例外：xref_start == 0 表示原文件的交叉引用表已丢失（扫描重建的 PDF），
    // 此时没有可指向的 /Prev，增量保存会写出不完整的文件 → 退回整篇重写。
    // =====================================================================
    if removed_signatures == 0 && doc.xref_start != 0 {
        let prev_bytes = std::fs::read(input_path)?;
        let mut inc = IncrementalDocument::create_from(prev_bytes, doc);
        let (total_pages, watermarked_pages) = watermark_incremental(&mut inc, &encoded, text_w)?;
        ensure_pages_watermarked(total_pages, watermarked_pages)?;
        inc.save(output_path)?;
    } else {
        if removed_signatures == 0 {
            eprintln!("WARN: 输入 PDF 缺少交叉引用表（xref_start=0），退回整篇重写保存");
        }
        let (total_pages, watermarked_pages) =
            watermark_full_rewrite(&mut doc, &encoded, text_w)?;
        ensure_pages_watermarked(total_pages, watermarked_pages)?;
        doc.save(output_path)?;
    }

    // 验证文件确实保存
    if !Path::new(output_path).exists() {
        return Err("输出文件保存失败".into());
    }

    Ok(())
}

/// 整篇重写保存路径：把整份 PDF 重新序列化后落盘。
///
/// **会破坏原有数字签名**（签名覆盖的字节被改写），因此只用于无法增量更新的
/// 输入，例如交叉引用表缺失的扫描重建文件。正常输入请走 `watermark_incremental`。
///
/// # 返回
/// `(总页数, 成功注入水印的页数)`
fn watermark_full_rewrite(
    doc: &mut Document,
    encoded_watermark: &[u8],
    text_w: f32,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let xobject_id = doc.add_object(build_watermark_xobject(encoded_watermark.to_vec()));
    let xobject_name = "Watermark1";

    // 遍历页面并注入资源与内容
    let mut total_pages = 0usize;
    let mut watermarked_pages = 0usize;
    for (page_num, object_id) in doc.get_pages() {
        total_pages += 1;
        // 获取媒体框左下坐标 + 尺寸（4 个值），处理 llx/lly ≠ 0 的情况
        let (mb_llx, mb_lly, w, h) = page_size(doc, object_id).unwrap_or((0.0, 0.0, 595.0, 842.0));

        // 获取页面旋转角度（支持旋转PDF）
        let page_rotation = get_page_rotation(doc, object_id);

        // 添加XObject资源到页面
        if let Err(e) = add_xobject_to_page(doc, object_id, xobject_name, xobject_id) {
            eprintln!(
                "WARN: 第 {} 页结构非标准，无法注入资源。错误：{:?}",
                page_num, e
            );
            continue;
        }

        // 生成水印网格操作（传入 MediaBox 4 个完整值 + 页面旋转）
        let ops = match build_watermark_grid_ops_xobject_optimized(
            xobject_name,
            DEFAULT_FONT_SIZE,
            WATERMARK_ANGLE_DEG,
            mb_llx,
            mb_lly,
            w,
            h,
            text_w,
            page_rotation,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN: 生成水印网格失败，跳过第 {} 页：{:?}", page_num, e);
                continue;
            }
        };

        // 将水印内容添加到页面。
        // 注意：不能用 doc.add_to_page_content 直接追加——它会继承原内容
        // 遗留的图形状态（如 Spire.Doc / wkhtmltopdf 的 Y 轴翻转 CTM），
        // 导致水印文字头朝下，详见 append_watermark_isolated 的注释。
        if let Err(e) = append_watermark_isolated(doc, object_id, Content { operations: ops }) {
            eprintln!("WARN: 添加页面内容失败，跳过第 {} 页：{:?}", page_num, e);
            continue;
        }

        watermarked_pages += 1;
    }

    Ok((total_pages, watermarked_pages))
}

/// 保存前的完整性校验：一页都没解析出来、或一页都没注入成功，都不允许写出文件。
fn ensure_pages_watermarked(
    total_pages: usize,
    watermarked_pages: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // 一页都解析不出来，说明文档结构压根没读通（损坏、仍加密或页树异常）。
    // 这种情况下写出的只会是一份残缺文件，必须报错而不是假装成功。
    if total_pages == 0 {
        return Err(
            "未能从输入 PDF 中解析出任何页面，文件可能已损坏或仍处于加密状态，未生成输出文件"
                .into(),
        );
    }

    // 有页面但一页都没注入成功时，同样不要写出一个没有水印却自称成功的文件
    if watermarked_pages == 0 {
        return Err(format!(
            "全部 {} 页均无法注入水印资源，未生成输出文件",
            total_pages
        )
        .into());
    }

    Ok(())
}

/// 取出对象内部的字典（`Dictionary` 直接取，`Stream` 取它的 dict）。
fn object_dict(obj: &Object) -> Option<&Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

/// `object_dict` 的可变版本。
fn object_dict_mut(obj: &mut Object) -> Option<&mut Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&mut s.dict),
        _ => None,
    }
}

/// 判断对象是否为签名/时间戳**值**字典。
///
/// 常见形态：
/// - `/Type /Sig`：普通数字签名；
/// - `/Type /DocTimeStamp`：文档时间戳；
/// - 省略 `/Type`，但带 `/ByteRange` + `/Contents`：少数签名实现的写法。
fn is_signature_value(obj: &Object) -> bool {
    let dict = match object_dict(obj) {
        Some(d) => d,
        None => return false,
    };
    if let Ok(Object::Name(name)) = dict.get(b"Type") {
        if name.as_slice() == b"Sig" || name.as_slice() == b"DocTimeStamp" {
            return true;
        }
    }
    dict.has(b"ByteRange") && dict.has(b"Contents")
}

/// 剥离文档中的「签名校验链路」，让 Adobe 不再弹「签名无效」；同时把签章控件
/// 降级为装饰性图章注释——/AP（外观流）里的签章图片原样保留在页面上。
///
/// 加水印必然改写页面内容，而签名校验的是**签名时刻的原始文件字节**，校验永远
/// 走不通（Adobe 会提示「签名无效：文档自签名后已被更改或损坏」）。业务上不要求
/// 保留签名的法律效力，因此：
///
/// 1. `/AcroForm` 字段树（含嵌套 `/Kids`）中 `/FT /Sig` 的字段，以及 `/V`
///    指向签名值字典的字段——从 `/Fields` 里摘掉，字典本体就地清空；
/// 2. 各页 `/Annots` 中的签名控件——`/Subtype /Widget` 改为 `/Subtype /Stamp`，
///    并删除 `/Parent /FT /V /T`，把它从「签名表单域」变成「图章注释」；
///    /AP（签章图片所在的外观流）保留，签章视觉效果由此获得；
/// 3. 目录 `/Perms`（认证签名的 DocMDP 引用，留着会让 Adobe 再报「认证失效」）；
/// 4. 签名值字典本体（`/Type /Sig`、`/Type /DocTimeStamp`）——没有它 Adobe
///    就找不到校验目标，自然不再提示「签名无效」。
///
/// 返回被剥离的签名对象数量（签名域 + 签名值字典；图章注释本身不计入）。
fn strip_digital_signatures(doc: &mut Document) -> usize {
    use std::collections::HashSet;

    // ---- 1. 签名值字典 ----
    let mut sig_value_ids: HashSet<ObjectId> = HashSet::new();
    for (&id, obj) in doc.objects.iter() {
        if is_signature_value(obj) {
            sig_value_ids.insert(id);
        }
    }

    // ---- 2. 签名域（/FT /Sig，或 /V 指向签名值字典）----
    let mut sig_field_ids: HashSet<ObjectId> = HashSet::new();
    for (&id, obj) in doc.objects.iter() {
        let dict = match object_dict(obj) {
            Some(d) => d,
            None => continue,
        };
        let ft_is_sig = matches!(dict.get(b"FT"), Ok(Object::Name(n)) if n.as_slice() == b"Sig");
        let v_is_sig = matches!(
            dict.get(b"V"),
            Ok(Object::Reference(v)) if sig_value_ids.contains(v)
        );
        if ft_is_sig || v_is_sig {
            sig_field_ids.insert(id);
        }
    }

    if sig_value_ids.is_empty() && sig_field_ids.is_empty() {
        return 0;
    }

    // 签名域派生的「可见控件」：含 /Kids 时收集 /Kids 里的全部控件；
    // 无 /Kids 时该字段字典本身就是控件（PDF 允许字段直接挂在 /Annots 上）。
    let mut widget_ids: HashSet<ObjectId> = HashSet::new();
    for &field_id in &sig_field_ids {
        let kids_opt = doc
            .get_object(field_id)
            .ok()
            .and_then(object_dict)
            .and_then(|d| d.get(b"Kids").ok())
            .cloned();
        match kids_opt {
            Some(Object::Array(kids)) => {
                for kid in &kids {
                    if let Object::Reference(kid_id) = kid {
                        widget_ids.insert(*kid_id);
                    }
                }
            }
            _ => {
                widget_ids.insert(field_id);
            }
        }
    }
    // ---- 3. 从 AcroForm 字段树中摘除签名域 ----
    let acro_form = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .cloned();
    match acro_form {
        Some(Object::Reference(acro_id)) => {
            let fields = doc
                .get_object(acro_id)
                .ok()
                .and_then(object_dict)
                .and_then(|d| d.get(b"Fields").ok())
                .cloned();
            if let Some(Object::Array(fields)) = fields {
                let kept = prune_signature_fields(doc, &fields, &sig_field_ids);
                if let Ok(acro) = doc.get_dictionary_mut(acro_id) {
                    acro.set(b"Fields", Object::Array(kept));
                }
            }
        }
        // 少见形态：AcroForm 内联在目录里
        Some(Object::Dictionary(acro)) => {
            if let Some(Object::Array(fields)) = acro.get(b"Fields").ok().cloned() {
                let kept = prune_signature_fields(doc, &fields, &sig_field_ids);
                let mut acro = acro;
                acro.set(b"Fields", Object::Array(kept));
                if let Ok(catalog) = doc.catalog_mut() {
                    catalog.set(b"AcroForm", Object::Dictionary(acro));
                }
            }
        }
        _ => {}
    }
    // ---- 4. 把签名控件「降级」为装饰性图章 ----
    //
    // 控件的 /AP（外观流）里画的是签章图片——这正是「签章效果」的载体，必须保留。
    // 我们只剥掉它的字段语义：把 /Subtype 从 Widget 改成 Stamp，让 Adobe 把
    // 它当作普通图章注释渲染；并清掉 /FT /V /T /Parent 这些指向签名校验链路
    // 的指针，免得读起来仍然像「未完成的签名域」。
    for &widget_id in &widget_ids {
        if let Ok(obj) = doc.get_object_mut(widget_id) {
            if let Some(d) = object_dict_mut(obj) {
                d.set(b"Subtype", Object::Name(b"Stamp".to_vec()));
                d.remove(b"Parent");
                // 极少数实现会把 /FT /V /T 直接挂在控件上（无 /Kids 的字段），
                // 顺手也清掉，避免成为孤儿键。
                d.remove(b"FT");
                d.remove(b"V");
                d.remove(b"T");
            }
        }
    }

    // ---- 5. 认证签名的权限声明也要清掉 ----
    if let Ok(catalog) = doc.catalog_mut() {
        catalog.remove(b"Perms");
    }

    // ---- 6. 抹掉签名值字典与「多控件」签名域字典（就地清空，不删对象） ----
    //
    // 不直接 `doc.objects.remove()`：删掉对象会在交叉引用表里留下没有条目的
    // 「洞」（trailer 的 /Size 仍按最大对象号计算，条目却缺了），部分阅读器
    // 会因此重建 xref 甚至报错。这里把对象原地清空成空字典——对象号照样出现在
    // 交叉引用表里，任何残留引用都只会指向一个无害的空字典，且不再携带任何
    // 签名语义（没有 /Type /Sig、/ByteRange、/Contents、/V）。
    //
    // 「单控件」签名域（无 /Kids，字段字典本身就是控件）不动——它已经降级为
    // 图章注释，/AP 里的签章图片还要靠它展示。这种字段同时出现在 sig_field_ids
    // 和 widget_ids 里，要靠 widget_ids.contains 跳过。
    let blank = Object::Dictionary(Dictionary::new());
    let mut removed = 0usize;
    for id in sig_field_ids.iter() {
        if widget_ids.contains(id) {
            continue;
        }
        if let Some(obj) = doc.objects.get_mut(id) {
            *obj = blank.clone();
            removed += 1;
        }
    }
    for id in sig_value_ids.iter() {
        if let Some(obj) = doc.objects.get_mut(id) {
            *obj = blank.clone();
            removed += 1;
        }
    }
    removed
}
/// 递归裁剪字段数组：丢弃整棵签名域子树；普通字段递归处理其 `/Kids`。
///
/// 只有真的从 `/Kids` 中摘掉了签名域才回写，避免无谓改动无关字段。
fn prune_signature_fields(
    doc: &mut Document,
    entries: &[Object],
    sig_fields: &std::collections::HashSet<ObjectId>,
) -> Vec<Object> {
    let mut kept: Vec<Object> = Vec::with_capacity(entries.len());
    for entry in entries {
        let field_id = match entry {
            Object::Reference(id) => *id,
            // 内联字段字典：不可能出现在收集到的签名域集合里，原样保留
            _ => {
                kept.push(entry.clone());
                continue;
            }
        };
        if sig_fields.contains(&field_id) {
            continue;
        }

        let kids = doc
            .get_object(field_id)
            .ok()
            .and_then(object_dict)
            .and_then(|d| d.get(b"Kids").ok())
            .cloned();
        if let Some(Object::Array(kids)) = kids {
            let new_kids = prune_signature_fields(doc, &kids, sig_fields);
            if new_kids.len() != kids.len() {
                if let Ok(obj) = doc.get_object_mut(field_id) {
                    if let Some(d) = object_dict_mut(obj) {
                        if new_kids.is_empty() {
                            d.remove(b"Kids");
                        } else {
                            d.set(b"Kids", Object::Array(new_kids));
                        }
                    }
                }
            }
        }
        kept.push(entry.clone());
    }
    kept
}


/// 构造水印 Form XObject（两条保存路径共用）。
///
/// BBox 必须与 XOBJ_BBOX_* 常量一致——可见性裁剪是按这 4 个值算的。
fn build_watermark_xobject(encoded: Vec<u8>) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            // 使用顶部定义的 XOBJ_BBOX_* 常量（必须与可见性裁剪使用的数值完全一致）
            "BBox" => vec![
                XOBJ_BBOX_LLX.into(),
                XOBJ_BBOX_LLY.into(),
                XOBJ_BBOX_URX.into(),
                XOBJ_BBOX_URY.into(),
            ],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => dictionary! {
                "ExtGState" => dictionary! {
                    "GS1" => dictionary! {
                        "Type" => "ExtGState",
                        "ca" => 0.1f32, // fill alpha
                        "CA" => 0.1f32, // stroke alpha
                    }
                }
            },
        },
        encoded,
    )
}

// ============================================================================
// 内部算法逻辑 (私有函数)
// ============================================================================

/// 将文本转换为PDF路径操作序列
///
/// # 功能
/// - 遍历文本中的每个字符
/// - 从字体中提取字形轮廓
/// - 将轮廓曲线转换为PDF图形操作指令
///
/// # 参数
/// - `font`: 字体引用
/// - `text`: 要转换的文本
/// - `x_start`: 水平起始位置
/// - `y_start`: 垂直起始位置
/// - `size`: 字体大小（点数）
///
/// # 返回
/// PDF操作向量（包括移动、线段、贝塞尔曲线等）
fn text_to_pdf_paths(
    font: &FontRef,
    text: &str,
    x_start: f32,
    y_start: f32,
    size: f32,
) -> Vec<Operation> {
    let scale = PxScale::from(size);
    let scaled_font = font.as_scaled(scale);
    let h_factor = scaled_font.h_scale_factor();
    let v_factor = scaled_font.v_scale_factor();

    let mut ops = vec![
        Operation::new("q", vec![]),
        Operation::new("gs", vec!["GS1".into()]),
        Operation::new("rg", vec![0.1.into(), 0.1.into(), 0.1.into()]),
    ];

    let mut x_cursor = x_start;
    for c in text.chars() {
        let glyph_id = font.glyph_id(c);
        if let Some(outline) = font.outline(glyph_id) {
            // 使用 Option<Point> 替代 NaN 作为轮廓分界的标记
            let mut last_point: Option<Point> = None;
            for curve in outline.curves {
                let p0 = match curve {
                    OutlineCurve::Line(p0, _) => p0,
                    OutlineCurve::Quad(p0, _, _) => p0,
                    OutlineCurve::Cubic(p0, _, _, _) => p0,
                };

                // 判断是否为新轮廓（新的子轮廓起点）
                let is_new_contour = match last_point {
                    None => true,
                    Some(lp) => ((p0.x - lp.x).abs() > 0.001) || ((p0.y - lp.y).abs() > 0.001),
                };

                if is_new_contour {
                    if last_point.is_some() {
                        ops.push(Operation::new("h", vec![])); // 闭合上一个轮廓
                    }
                    ops.push(Operation::new(
                        "m",
                        vec![
                            (x_cursor + p0.x * h_factor).into(),
                            (y_start + p0.y * v_factor).into(),
                        ],
                    )); // 移动到新起点
                }

                match curve {
                    OutlineCurve::Line(_, p1) => {
                        ops.push(Operation::new(
                            "l",
                            vec![
                                (x_cursor + p1.x * h_factor).into(),
                                (y_start + p1.y * v_factor).into(),
                            ],
                        ));
                        last_point = Some(p1);
                    }
                    OutlineCurve::Quad(_, p1, p2) => {
                        // 将二次贝塞尔转换为三次贝塞尔（PDF只支持三次）
                        let q1_x = p0.x + (2.0 / 3.0) * (p1.x - p0.x);
                        let q1_y = p0.y + (2.0 / 3.0) * (p1.y - p0.y);
                        let q2_x = p2.x + (2.0 / 3.0) * (p1.x - p2.x);
                        let q2_y = p2.y + (2.0 / 3.0) * (p1.y - p2.y);
                        ops.push(Operation::new(
                            "c",
                            vec![
                                (x_cursor + q1_x * h_factor).into(),
                                (y_start + q1_y * v_factor).into(),
                                (x_cursor + q2_x * h_factor).into(),
                                (y_start + q2_y * v_factor).into(),
                                (x_cursor + p2.x * h_factor).into(),
                                (y_start + p2.y * v_factor).into(),
                            ],
                        ));
                        last_point = Some(p2);
                    }
                    OutlineCurve::Cubic(_, p1, p2, p3) => {
                        ops.push(Operation::new(
                            "c",
                            vec![
                                (x_cursor + p1.x * h_factor).into(),
                                (y_start + p1.y * v_factor).into(),
                                (x_cursor + p2.x * h_factor).into(),
                                (y_start + p2.y * v_factor).into(),
                                (x_cursor + p3.x * h_factor).into(),
                                (y_start + p3.y * v_factor).into(),
                            ],
                        ));
                        last_point = Some(p3);
                    }
                }
            }
            if last_point.is_some() {
                ops.push(Operation::new("h", vec![])); // 闭合最后一个轮廓
            }
        }
        x_cursor += scaled_font.h_advance(glyph_id);
    }
    ops.push(Operation::new("f", vec![])); // 填充路径
    ops.push(Operation::new("Q", vec![])); // 恢复图形状态

    ops
}

/// 计算文本宽度
///
/// # 参数
/// - `font`: 字体引用
/// - `text`: 文本内容
/// - `size`: 字体大小
///
/// # 返回
/// 文本总宽度（点数）
fn measure_text_width(font: &FontRef, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    for c in text.chars() {
        w += scaled.h_advance(font.glyph_id(c));
    }
    w
}

/// 从PDF页面对象中提取媒体框：左下角 (llx, lly) + 尺寸 (w, h)
///
/// # 为什么必须返回 llx/lly 而不只返回 w/h
/// - 多数 PDF 规范允许 MediaBox 左下不是 (0,0)，例如扫描件常用 (20,30,595,842)。
/// - 若把 (llx,lly) 当 (0,0) 来布网格，整页水印会偏移出页外或裁掉左下角落。
/// - 本函数就是「展开 4 个坐标」的唯一真源，后续所有位置计算以它为准。
fn page_size(doc: &Document, page_id: ObjectId) -> Option<(f32, f32, f32, f32)> {
    let page_obj = doc.get_object(page_id).ok()?;
    let dict = match page_obj {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &s.dict,
        _ => return None,
    };
    if let Ok(Object::Array(arr)) = dict.get(b"MediaBox") && arr.len() >= 4 {
        let llx = obj_to_f32(&arr[0]);
        let lly = obj_to_f32(&arr[1]);
        let urx = obj_to_f32(&arr[2]);
        let ury = obj_to_f32(&arr[3]);
        return Some((llx, lly, urx - llx, ury - lly));
    }
    None
}

/// 将PDF对象转换为f32
fn obj_to_f32(o: &Object) -> f32 {
    match o {
        Object::Real(r) => *r,
        Object::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

/// 从PDF页面对象中提取页面旋转角度
///
/// # 说明
/// - 搜索页面及其父页面的 Rotate 属性
/// - 限制搜索深度为10级以防止无限循环
/// - 返回值为 0, 90, 180, 270（PDF标准值）
/// - 现在被 run_watermark_process 调用以支持旋转 PDF
fn get_page_rotation(doc: &Document, page_id: ObjectId) -> f32 {
    let mut current_id = Some(page_id);
    let mut depth = 0usize;
    const MAX_PARENT_DEPTH: usize = 10;

    while let Some(id) = current_id {
        if depth > MAX_PARENT_DEPTH {
            break;
        }
        if let Ok(obj) = doc.get_object(id) {
            let dict = match obj {
                Object::Dictionary(d) => d,
                Object::Stream(s) => &s.dict,
                _ => break,
            };
            if let Ok(rotate_obj) = dict.get(b"Rotate") {
                let r = match rotate_obj {
                    Object::Integer(r) => Some(*r),
                    Object::Reference(ref_id) => {
                        if let Ok(Object::Integer(r)) = doc.get_object(*ref_id) {
                            Some(*r)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(val) = r {
                    return val as f32;
                }
            }
            current_id = if let Ok(Object::Reference(p)) = dict.get(b"Parent") {
                Some(*p)
            } else {
                None
            };
        } else {
            break;
        }
        depth += 1;
    }
    0.0
}

/// 取出 `owner[key]` 所指向的字典对象的 ObjectId；键缺失时新建一个空字典。
///
/// # 说明
/// PDF 中同一个键有两种等价写法，都必须支持：
/// - 间接引用：`/XObject 27 0 R`
/// - 内联字典：`/XObject << /Im1 30 0 R >>`
///
/// 内联字典会被提升为间接对象（PDF 规范允许），这样上层只需围绕 ObjectId 操作，
/// 不必为两种写法各写一套可选/可变借用逻辑。
fn ensure_sub_dict(
    doc: &mut Document,
    owner: &mut Dictionary,
    key: &[u8],
) -> Result<ObjectId, lopdf::Error> {
    // 先 clone 成自有值，避免 owner.get() 的不可变借用与 owner.set() 的可变借用冲突
    match owner.get(key).ok().cloned() {
        Some(Object::Reference(id)) => Ok(id),
        Some(Object::Dictionary(d)) => {
            let id = doc.add_object(d);
            owner.set(key.to_vec(), Object::Reference(id));
            Ok(id)
        }
        None => {
            let id = doc.add_object(dictionary! {});
            owner.set(key.to_vec(), Object::Reference(id));
            Ok(id)
        }
        // 既不是引用也不是字典（例如 /Resources 写成了整数），属于结构异常
        Some(other) => Err(lopdf::Error::ObjectType {
            expected: "reference or dictionary",
            found: other.enum_variant(),
        }),
    }
}

/// 沿 /Parent 链向上查找继承的 /Resources，返回其副本。
///
/// PDF 规范允许页面省略 /Resources 而由父 Pages 节点提供，Word/WPS 产出的 PDF 常见。
/// 返回副本而非引用，是为了让调用方为本页建独立对象，不污染共用同一资源的兄弟页面。
/// 链断掉或成环时返回 None。
fn find_inherited_resources(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut current = page_id;
    for _ in 0..32 {
        // 防御 /Parent 成环
        let parent = doc
            .get_dictionary(current)
            .ok()?
            .get(b"Parent")
            .and_then(Object::as_reference)
            .ok()?;
        let parent_dict = doc.get_dictionary(parent).ok()?;
        if let Ok(res) = parent_dict.get(b"Resources") {
            if let Ok((_, dict)) = doc.dereference(res) {
                if let Ok(d) = dict.as_dict() {
                    return Some(d.clone());
                }
            }
        }
        current = parent;
    }
    None
}

/// 将XObject资源添加到PDF页面
///
/// # 说明
/// 创建或更新页面的Resources > XObject字典，
/// 使其能引用水印XObject对象。
///
/// /Resources 与 /XObject 各自都可能是间接引用、内联字典或缺失，
/// 这里统一处理；页面自身没有 /Resources 时先按规范继承父节点的。
fn add_xobject_to_page(
    doc: &mut Document,
    page_id: ObjectId,
    x_name: &str,
    x_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 取页面对象的自有副本（页面可能是 Dictionary 或 Stream），
    //    这样后续在副本上修改的同时还能继续借用 doc
    let mut page_obj = doc.get_object(page_id)?.clone();
    let page_dict = match &mut page_obj {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &mut s.dict,
        _ => return Err("page object is not a Dictionary or Stream".into()),
    };

    // 2. 页面自身没有 /Resources 时，按规范继承父 Pages 节点的资源
    if !page_dict.has(b"Resources") {
        if let Some(inherited) = find_inherited_resources(doc, page_id) {
            let inherited_id = doc.add_object(inherited);
            page_dict.set(b"Resources", Object::Reference(inherited_id));
        }
    }
    let resources_id = ensure_sub_dict(doc, page_dict, b"Resources")?;
    // 写回页面对象（包含上面可能的继承 / 提升改动）
    doc.set_object(page_id, page_obj);

    // 3. 处理 /Resources 下的 /XObject
    let mut resources = doc.get_dictionary(resources_id)?.clone();
    let xobjects_id = ensure_sub_dict(doc, &mut resources, b"XObject")?;
    doc.set_object(resources_id, Object::Dictionary(resources));

    // 4. 登记水印XObject的名称 -> 对象引用
    let mut xobjects = doc.get_dictionary(xobjects_id)?.clone();
    xobjects.set(x_name.as_bytes().to_vec(), Object::Reference(x_id));
    doc.set_object(xobjects_id, Object::Dictionary(xobjects));

    Ok(())
}

/// 将水印内容追加到页面，并隔离原页面内容遗留的图形状态。
///
/// # 背景
/// Spire.Doc、wkhtmltopdf 等工具生成的 PDF，页面内容流以一个**未配对**
/// 的 `cm` 开头（如 `1 0 0 -1 0 842 cm`，即 Y 轴翻转），且全程不恢复。
/// PDF 中同一页的多个内容流在语义上是首尾拼接的，直接把水印操作追加在
/// 后面会让水印继承这个翻转后的坐标系：字形上下镜像（头朝下）、倾斜
/// 方向与预期相反。
///
/// # 解决方案
/// 与 PyMuPDF 等工具一致：在原内容流前后各插入一个只含 `q` / `Q` 的
/// 小内容流，把原内容遗留的 CTM、裁剪路径等图形状态封装起来，让水印
/// 始终绘制在干净的默认页面坐标系（原点左下、Y 轴向上）中。
///
/// # 说明
/// - 原页面渲染效果不变（只是在整体外层包了一对 save/restore）；
/// - 原内容中不配对的 `q` 会多弹一层 `Q`，属于无害的尾部冗余；
/// - 只新增对象并重写本页 `/Contents`，不修改（可能被多页共享的）
///   原内容流对象本身。
fn append_watermark_isolated(
    doc: &mut Document,
    page_id: ObjectId,
    watermark_ops: Content<Vec<Operation>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 收集原 /Contents 的全部流引用。
    //    /Contents 可能是：流引用 / 指向数组的引用 / 内联数组，也可能缺失。
    let mut original_refs: Vec<Object> = Vec::new();
    {
        let page_obj = doc.get_object(page_id)?;
        let page_dict = match page_obj {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            other => {
                return Err(format!(
                    "page object is not a Dictionary or Stream: {}",
                    other.enum_variant()
                )
                .into())
            }
        };
        match page_dict.get(b"Contents") {
            Ok(Object::Reference(id)) => match doc.get_object(*id)? {
                // 常见：引用直接指向内容流
                Object::Stream(_) => original_refs.push(Object::Reference(*id)),
                // 少见：引用指向数组，展开其元素
                Object::Array(arr) => original_refs.extend(arr.clone()),
                _ => original_refs.push(Object::Reference(*id)),
            },
            Ok(Object::Array(arr)) => original_refs = arr.clone(),
            // 无 /Contents 的空白页，无需隔离
            _ => {}
        }
    }

    // 2. 构造隔离用的 q/Q 小流与水印流
    let open_id = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let close_id = doc.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));
    let wm_data = watermark_ops
        .encode()
        .map_err(|e| format!("encode watermark page content failed: {:?}", e))?;
    let wm_id = doc.add_object(Stream::new(Dictionary::new(), wm_data));

    // 3. 重组 /Contents：q + 原内容 + Q + 水印
    let mut contents: Vec<Object> = Vec::with_capacity(original_refs.len() + 3);
    contents.push(Object::Reference(open_id));
    contents.extend(original_refs);
    contents.push(Object::Reference(close_id));
    contents.push(Object::Reference(wm_id));

    // 4. 写回页面对象（页面可能是 Dictionary 或 Stream）
    let mut page_obj = doc.get_object(page_id)?.clone();
    let page_dict = match &mut page_obj {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &mut s.dict,
        _ => unreachable!("第 1 步已校验过对象类型"),
    };
    page_dict.set(b"Contents", Object::Array(contents));
    doc.set_object(page_id, page_obj);

    Ok(())
}

// ============================================================================
// 增量更新保存路径 —— 保住数字签名
//
// 与上面的整篇重写路径相比，这里所有改动（新增的水印对象、页面对象的新版本、
// 资源字典的新版本）都写进「本次修订」，由 lopdf 追加到文件末尾，上一修订的
// 原始字节一字不动。数字签名覆盖的正是那段原始字节，因此签名校验依旧通过。
// ============================================================================

/// 在「本次修订」与「上一修订」中查找对象（只读）。
///
/// 增量保存时新写的对象在 `new_document`，上一修订的对象在 `prev_documents`，
/// 二者共同构成同一文档空间，查找时必须都看。
fn lookup_object_any(inc: &IncrementalDocument, id: ObjectId) -> Option<&Object> {
    inc.new_document
        .get_object(id)
        .ok()
        .or_else(|| inc.get_prev_documents().get_object(id).ok())
}

/// 取出 `owner[key]` 指向的字典在本次修订中的对象 id（内联字典会被提升为间接对象）。
fn ensure_sub_dict_incremental(
    inc: &mut IncrementalDocument,
    owner_id: ObjectId,
    key: &[u8],
) -> Result<ObjectId, Box<dyn std::error::Error>> {
    /// 子字典在父对象上的三种形态
    enum Slot {
        Ref(ObjectId),
        Inline(Dictionary),
        Missing,
    }

    let slot = {
        let owner = inc.new_document.get_object(owner_id)?;
        let owner_dict = match owner {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            other => {
                return Err(format!(
                    "object is not a Dictionary or Stream: {}",
                    other.enum_variant()
                )
                .into())
            }
        };
        match owner_dict.get(key) {
            Ok(Object::Reference(id)) => Slot::Ref(*id),
            Ok(Object::Dictionary(d)) => Slot::Inline(d.clone()),
            // 值类型异常（例如写成了整数）：退回空字典，保证水印能画上
            Ok(_) => Slot::Inline(Dictionary::new()),
            Err(_) => Slot::Missing,
        }
    };

    match slot {
        // 复制进本次修订后再改，上一修订里的那个对象保持原样
        Slot::Ref(id) => {
            inc.opt_clone_object_to_new_document(id)?;
            Ok(id)
        }
        Slot::Inline(dict) => {
            let id = inc.new_document.add_object(dict);
            set_dict_entry_ref(inc, owner_id, key, id)?;
            Ok(id)
        }
        Slot::Missing => {
            let id = inc.new_document.add_object(dictionary! {});
            set_dict_entry_ref(inc, owner_id, key, id)?;
            Ok(id)
        }
    }
}

/// 把 `owner[key]` 改成对 `target_id` 的间接引用
fn set_dict_entry_ref(
    inc: &mut IncrementalDocument,
    owner_id: ObjectId,
    key: &[u8],
    target_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let owner = inc.new_document.get_object_mut(owner_id)?;
    let owner_dict = match owner {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &mut s.dict,
        _ => return Err("object is not a Dictionary or Stream".into()),
    };
    owner_dict.set(key.to_vec(), Object::Reference(target_id));
    Ok(())
}

/// 把页面的 /Resources 指向本次修订中的资源对象
fn set_page_resources(
    inc: &mut IncrementalDocument,
    page_id: ObjectId,
    resources_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let page = inc.new_document.get_object_mut(page_id)?;
    let page_dict = match page {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &mut s.dict,
        _ => return Err("page object is not a Dictionary or Stream".into()),
    };
    page_dict.set(b"Resources", Object::Reference(resources_id));
    Ok(())
}

/// 取页面 /Resources 在本次修订中对应的对象 id（必要时复制、提升或按规范继承）。
///
/// 对应整篇重写路径里的 `add_xobject_to_page` 第 2 步，区别是所有改动都写进
/// 「本次修订」，上一修订的原始字节不受影响。
fn ensure_page_resources_incremental(
    inc: &mut IncrementalDocument,
    page_id: ObjectId,
) -> Result<ObjectId, Box<dyn std::error::Error>> {
    /// /Resources 在页面上的三种形态
    enum Slot {
        Ref(ObjectId),
        Inline(Dictionary),
        Missing,
    }

    // 页面对象先复制进本次修订
    inc.opt_clone_object_to_new_document(page_id)?;

    let slot = {
        let page = inc.new_document.get_object(page_id)?;
        let page_dict = match page {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            other => {
                return Err(format!(
                    "page object is not a Dictionary or Stream: {}",
                    other.enum_variant()
                )
                .into())
            }
        };
        match page_dict.get(b"Resources") {
            Ok(Object::Reference(id)) => Slot::Ref(*id),
            Ok(Object::Dictionary(d)) => Slot::Inline(d.clone()),
            // 结构异常：退回空字典
            Ok(_) => Slot::Inline(Dictionary::new()),
            Err(_) => Slot::Missing,
        }
    };

    let resources_id = match slot {
        // 资源字典常被多页共享：复制进本次修订后统一修改，
        // 之后引用它的页面看到的都是新版本（本工具给每页都加水印，正合需要）
        Slot::Ref(id) => {
            inc.opt_clone_object_to_new_document(id)?;
            id
        }
        Slot::Inline(dict) => {
            let id = inc.new_document.add_object(dict);
            set_page_resources(inc, page_id, id)?;
            id
        }
        // 页面自身没有 /Resources：按规范继承父 Pages 节点的资源。
        // 否则页面级资源一旦缺少字体等条目，整页排版都会错乱。
        Slot::Missing => {
            let inherited =
                find_inherited_resources(inc.get_prev_documents(), page_id).unwrap_or_default();
            let id = inc.new_document.add_object(inherited);
            set_page_resources(inc, page_id, id)?;
            id
        }
    };

    Ok(resources_id)
}

/// 增量更新版：把水印 XObject 登记到页面的资源字典里
fn add_xobject_to_page_incremental(
    inc: &mut IncrementalDocument,
    page_id: ObjectId,
    x_name: &str,
    x_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let resources_id = ensure_page_resources_incremental(inc, page_id)?;
    let xobjects_id = ensure_sub_dict_incremental(inc, resources_id, b"XObject")?;

    let xobjects = inc.new_document.get_dictionary_mut(xobjects_id)?;
    xobjects.set(x_name.as_bytes().to_vec(), Object::Reference(x_id));
    Ok(())
}

/// 增量更新版：把水印内容追加到页面 /Contents。
///
/// q/Q 隔离逻辑与 `append_watermark_isolated` 完全一致（原因见那里的注释），
/// 区别只是改写落在「本次修订」里，原内容流对象本身不被触碰。
fn append_watermark_isolated_incremental(
    inc: &mut IncrementalDocument,
    page_id: ObjectId,
    watermark_ops: Content<Vec<Operation>>,
) -> Result<(), Box<dyn std::error::Error>> {
    inc.opt_clone_object_to_new_document(page_id)?;

    // 1. 收集原 /Contents 的流引用（这些对象留在上一修订里，字节不变）
    let mut original_refs: Vec<Object> = Vec::new();
    {
        let page = inc.new_document.get_object(page_id)?;
        let page_dict = match page {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            other => {
                return Err(format!(
                    "page object is not a Dictionary or Stream: {}",
                    other.enum_variant()
                )
                .into())
            }
        };
        match page_dict.get(b"Contents") {
            Ok(Object::Reference(id)) => match lookup_object_any(inc, *id) {
                // 常见：引用直接指向内容流
                Some(Object::Stream(_)) => original_refs.push(Object::Reference(*id)),
                // 少见：引用指向数组，展开其元素
                Some(Object::Array(arr)) => original_refs.extend(arr.clone()),
                _ => original_refs.push(Object::Reference(*id)),
            },
            Ok(Object::Array(arr)) => original_refs = arr.clone(),
            // 无 /Contents 的空白页，无需隔离
            _ => {}
        }
    }

    // 2. 新增隔离用的 q/Q 小流与水印流
    let open_id = inc.new_document.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let close_id = inc.new_document.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));
    let wm_data = watermark_ops
        .encode()
        .map_err(|e| format!("encode watermark page content failed: {:?}", e))?;
    // 网格指令是本次新增数据量的大头，压一压避免输出文件膨胀
    let mut wm_stream = Stream::new(Dictionary::new(), wm_data);
    if let Err(e) = wm_stream.compress() {
        eprintln!("WARN: 水印内容流压缩失败（按未压缩写入）：{:?}", e);
    }
    let wm_id = inc.new_document.add_object(wm_stream);

    // 3. 重组 /Contents：q + 原内容 + Q + 水印
    let mut contents: Vec<Object> = Vec::with_capacity(original_refs.len() + 3);
    contents.push(Object::Reference(open_id));
    contents.extend(original_refs);
    contents.push(Object::Reference(close_id));
    contents.push(Object::Reference(wm_id));

    let page = inc.new_document.get_object_mut(page_id)?;
    let page_dict = match page {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &mut s.dict,
        _ => return Err("page object is not a Dictionary or Stream".into()),
    };
    page_dict.set(b"Contents", Object::Array(contents));

    Ok(())
}

/// 增量更新主流程：把水印写进「本次修订」。
///
/// # 返回
/// `(总页数, 成功注入水印的页数)`
fn watermark_incremental(
    inc: &mut IncrementalDocument,
    encoded_watermark: &[u8],
    text_w: f32,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let xobject_name = "Watermark1";

    // 水印 Form XObject 作为本次修订的新对象写入
    let mut watermark_stream = build_watermark_xobject(encoded_watermark.to_vec());
    if let Err(e) = watermark_stream.compress() {
        eprintln!("WARN: 水印 XObject 压缩失败（按未压缩写入）：{:?}", e);
    }
    let xobject_id = inc.new_document.add_object(watermark_stream);

    // 页面列表与页面几何都取自上一修订（那里保留着原始页树）
    let pages: Vec<(u32, ObjectId)> = inc.get_prev_documents().get_pages().into_iter().collect();

    let mut total_pages = 0usize;
    let mut watermarked_pages = 0usize;
    for (page_num, page_id) in pages {
        total_pages += 1;
        // 获取媒体框左下坐标 + 尺寸（4 个值），处理 llx/lly ≠ 0 的情况
        let (mb_llx, mb_lly, w, h) = page_size(inc.get_prev_documents(), page_id)
            .unwrap_or((0.0, 0.0, 595.0, 842.0));

        // 获取页面旋转角度（支持旋转PDF）
        let page_rotation = get_page_rotation(inc.get_prev_documents(), page_id);

        // 添加XObject资源到页面
        if let Err(e) = add_xobject_to_page_incremental(inc, page_id, xobject_name, xobject_id) {
            eprintln!(
                "WARN: 第 {} 页结构非标准，无法注入资源。错误：{:?}",
                page_num, e
            );
            continue;
        }

        // 生成水印网格操作（传入 MediaBox 4 个完整值 + 页面旋转）
        let ops = match build_watermark_grid_ops_xobject_optimized(
            xobject_name,
            DEFAULT_FONT_SIZE,
            WATERMARK_ANGLE_DEG,
            mb_llx,
            mb_lly,
            w,
            h,
            text_w,
            page_rotation,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN: 生成水印网格失败，跳过第 {} 页：{:?}", page_num, e);
                continue;
            }
        };

        if let Err(e) =
            append_watermark_isolated_incremental(inc, page_id, Content { operations: ops })
        {
            eprintln!("WARN: 添加页面内容失败，跳过第 {} 页：{:?}", page_num, e);
            continue;
        }

        watermarked_pages += 1;
    }

    Ok((total_pages, watermarked_pages))
}

/// 生成水印网格PDF操作指令（矩阵法版本，对任意 /Rotate + 任意 MediaBox 都正确）
///
/// # 核心思想（解决老三的回忆 PDF 的第 3+ 页的 Bug）
/// 老三的回忆 PDF：第 3 页起 MediaBox = (0,0,434,283) 即宽高=横屏 434×283，
/// 同时 /Rotate=270°。之前的代码对 90/270 直接交换宽高导致又交换了一次，
/// 锚点全部错位造成重叠，并且水印倾斜方向完全反导致倒立。
///
/// 这里不再用「if 90/270 交换宽高」的硬编码，而是严格按 PDF 规范用仿射矩阵，
/// 任何 /Rotate × 任何 MediaBox 都能得到正确结果。
///
/// # 推导
/// 1. **阅读器施加的显示变换（P → V，将 PDF 内部坐标变成用户看到的视觉坐标）**
///    根据 PDF 1.7 规范 §7.2.3.3，/Rotate=R° 会先把页面绕 MediaBox 中心逆时针
///    旋转 R°，然后把显示矩形对其到视口。把这个过程写成 P→V 的仿射：
///      V = T_center · Rot(R) · T(-center_P) · P
///    其中 center_P = (llx + w/2, lly + h/2)
///
/// 2. **我们需要反函数 V→P（在视觉坐标系布好点后，映射回 PDF 内部坐标写 cm）**：
///      P = T(center_P) · Rot(-R) · T(-center_V) · V
///    注意：因为 /Rotate=90/270 会交换宽高，视觉中心 center_V 的尺寸是
///      vis_w = R=90|270 ? h : w
///      vis_h = R=90|270 ? w : h
///    所以 center_V = (vis_w/2, vis_h/2)。
///
/// 3. **对水印自身角度 θ 的补偿**
///    阅读器显示时会把整页（含水印）再转 R°，所以我们把水印先转 -R°再转 θ°，
///    最终用户看到的总倾斜就是：(θ − R) + R = θ，正好等于我们想要的水印角度。
///    因此：水印最终 cm 的旋转部分 = Rot(θ − R) = Rot(-R) · Rot(θ)
///
/// # 参数
/// - `x_name`: XObject 资源名
/// - `size`: 字体大小（用于计算垂直间距）
/// - `angle`:  **视觉上**希望的水印倾斜角度（度），例如 60°
/// - `mb_llx`: MediaBox 左下角 x（PDF 内部坐标，不必为 0）
/// - `mb_lly`: MediaBox 左下角 y（PDF 内部坐标，不必为 0）
/// - `w`    : MediaBox 宽度
/// - `h`    : MediaBox 高度
/// - `text_w`: 预计算文本宽度
/// - `page_rotation`: /Rotate 值（0/90/180/270）
fn build_watermark_grid_ops_xobject_optimized(
    x_name: &str,
    size: f32,
    angle: f32,
    mb_llx: f32,
    mb_lly: f32,
    w: f32,
    h: f32,
    text_w: f32,
    page_rotation: f32,
) -> Result<Vec<Operation>, Box<dyn std::error::Error>> {
    let step_inner = text_w + GRID_HORIZONTAL_GAP;
    let step_outer = size * GRID_VERTICAL_MULTIPLIER;

    if !(step_inner > MIN_GRID_STEP_SIZE && step_outer > MIN_GRID_STEP_SIZE) {
        return Err(format!(
            "Grid step too small: inner={}, outer={}",
            step_inner, step_outer
        )
        .into());
    }

    // =====================================================================
    // 1. 归一化 Rotate 角度到 0/90/180/270（PDF 规范的标准值）
    // =====================================================================
    let r = ((page_rotation as i32) % 360 + 360) % 360;
    let rad_r = (r as f32).to_radians();
    let (cr, sr) = (rad_r.cos(), rad_r.sin());
    let rot_neg_r = |x: f32, y: f32| -> (f32, f32) {
        // 2D 旋转 -R°（顺时针 R°）：x' =  x*cosR + y*sinR ; y' = -x*sinR + y*cosR
        (x * cr + y * sr, -x * sr + y * cr)
    };

    // =====================================================================
    // 2. 视觉系的宽/高（90/270 时交换），以及视觉中心
    // =====================================================================
    let (vis_w, vis_h) = match r {
        90 | 270 => (h, w),
        _        => (w, h),
    };
    let cx_v = vis_w / 2.0 + CENTER_X_OFFSET;
    let cy_v = vis_h / 2.0 - CENTER_Y_OFFSET;

    // PDF 内部 MediaBox 中心
    let cx_p = mb_llx + w / 2.0;
    let cy_p = mb_lly + h / 2.0;

    // =====================================================================
    // 3. V → P 仿射映射（严格按 PDF 规范推导）
    //    P = T(cx_p, cy_p) · Rot(-R) · T(-cx_v, -cy_v) · V
    // =====================================================================
    let v_to_p = |vx: f32, vy: f32| -> (f32, f32) {
        // 步骤 A：V 减去视觉中心
        let (ax, ay) = (vx - cx_v, vy - cy_v);
        // 步骤 B：旋转 -R°
        let (bx, by) = rot_neg_r(ax, ay);
        // 步骤 C：加回 PDF 内部 MediaBox 中心
        (bx + cx_p, by + cy_p)
    };

    // =====================================================================
    // 4. 水印最终的旋转部分：Rot(angle + R)
    //    推导：
    //    阅读器显示时，对整页（含水印）还会再施加一次「顺时针 R°」= 数学逆时针 −R°
    //    如果我们在 P 系写水印时旋转 φ，则用户视觉上看到的总旋转 = φ + (−R°)
    //    我们希望它等于我们要的视觉水印角 angle（如 60°）
    //        φ + (−R°) = angle   →   φ = angle + R°
    //    例子（老三回忆 R=270°，angle=60°）：
    //        φ = 60 + 270 = 330°，再加上阅读器的 −270° → 330−270 = 60° ✅
    //    若写成 angle−R（错误），对 R=270° 得 −210°≡150°，(150−270)=−120°≠60°，差 180°，字正好上下倒立
    // =====================================================================
    let rad_wm = (angle + r as f32).to_radians();
    let (cw, sw) = (rad_wm.cos(), rad_wm.sin());

    // =====================================================================
    // 5. 在视觉坐标系 (V) 中按斜向网格布锚点（中心对称，足够的覆盖余量）
    // =====================================================================
    let diag = (vis_w.powi(2) + vis_h.powi(2)).sqrt() * COVERAGE_MULTIPLIER;

    let v_start = -diag - 400.0;
    let v_end   =  diag + 400.0;
    let total_v_span = v_end - v_start;
    let v_count = ((total_v_span / step_outer).ceil() as isize).max(0) as usize;

    let u_start = -diag - 400.0;
    let u_end   =  diag + 400.0;
    let total_u_span = u_end - u_start;
    let u_count = ((total_u_span / step_inner).ceil() as isize).max(0) as usize;

    let estimated = v_count.saturating_mul(u_count);
    if estimated > MAX_ALLOWED_WATERMARKS {
        return Err(format!("Too many watermarks to render: {}", estimated).into());
    }

    // =====================================================================
    // 6. XObject 4 个局部角点 → 配合 AABB 可见性裁剪
    // =====================================================================
    let corners_local = [
        (XOBJ_BBOX_LLX, XOBJ_BBOX_LLY),
        (XOBJ_BBOX_URX, XOBJ_BBOX_LLY),
        (XOBJ_BBOX_URX, XOBJ_BBOX_URY),
        (XOBJ_BBOX_LLX, XOBJ_BBOX_URY),
    ];

    // 页面 P 系 AABB（完全按 MediaBox 的 4 个值，外加膨胀余量）
    let page_min_x = mb_llx - VISIBILITY_MARGIN;
    let page_min_y = mb_lly - VISIBILITY_MARGIN;
    let page_max_x = mb_llx + w + VISIBILITY_MARGIN;
    let page_max_y = mb_lly + h + VISIBILITY_MARGIN;

    // 生成网格时，锚点用纯视觉坐标系的「角度=angle」旋转进行分布（和视觉上最终显示一致）
    let rad_grid = angle.to_radians();
    let (cg, sg) = (rad_grid.cos(), rad_grid.sin());

    let mut ops: Vec<Operation> = Vec::with_capacity(estimated.min(4096) * 4);

    for vi in 0..=v_count {
        let v = v_start + (vi as f32) * step_outer;
        for ui in 0..=u_count {
            let u = u_start + (ui as f32) * step_inner;

            // 6a. 锚点在 V 系中的坐标（按 angle 斜向分布，保证视觉密度均匀）
            let vx = cx_v + u * cg - v * sg;
            let vy = cy_v + u * sg + v * cg;

            // 6b. V → P 映射：得到在 PDF 内部坐标系中要写入的实际锚点
            let (px, py) = v_to_p(vx, vy);

            // 6c. 把 XObject 4 角点先按水印旋转角 (angle - R) 旋转，再平移到 (px,py)，
            //     求 P 系下该水印的 AABB
            let mut x_min =  f32::INFINITY;
            let mut y_min =  f32::INFINITY;
            let mut x_max = -f32::INFINITY;
            let mut y_max = -f32::INFINITY;
            for &(qx, qy) in &corners_local {
                let tx = px + qx * cw - qy * sw;
                let ty = py + qx * sw + qy * cw;
                if tx < x_min { x_min = tx; }
                if tx > x_max { x_max = tx; }
                if ty < y_min { y_min = ty; }
                if ty > y_max { y_max = ty; }
            }

            // 6d. AABB 交集判断：不与页面膨胀矩形相交的直接跳过
            let overlaps =
                x_min <= page_max_x &&
                x_max >= page_min_x &&
                y_min <= page_max_y &&
                y_max >= page_min_y;
            if !overlaps {
                continue;
            }

            // 6e. 写 PDF 指令
            //     cm 的 a b c d 用最终水印旋转角 (angle - R)
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new(
                "cm",
                vec![
                    cw.into(),
                    sw.into(),
                    (-sw).into(),
                    cw.into(),
                    px.into(),
                    py.into(),
                ],
            ));
            ops.push(Operation::new("Do", vec![x_name.into()]));
            ops.push(Operation::new("Q", vec![]));
        }
    }

    Ok(ops)
}