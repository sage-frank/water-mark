use ab_glyph::{Font, FontRef, PxScale, OutlineCurve, Point, ScaleFont};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, IncrementalDocument, LoadOptions, Object, ObjectId, Stream};
use lopdf::dictionary;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;

// ============================================================================
// 常量
// ============================================================================

const DEFAULT_FONT_SIZE: f32 = 26.0;

/// 水平方向水印间距：相邻水印起点距离 = text_w + GRID_HORIZONTAL_GAP。
const GRID_HORIZONTAL_GAP: f32 = 30.0;

/// 垂直方向水印间距倍数。故意大于水平间距，避免垂直方向过密。
const GRID_VERTICAL_MULTIPLIER: f32 = 6.0;

/// 水印旋转角度（视觉上希望呈现的倾斜角）。
const WATERMARK_ANGLE_DEG: f32 = 60.0;

/// 水印网格中心 X/Y 偏移（视觉居中微调）。
const CENTER_X_OFFSET: f32 = 0.0;
const CENTER_Y_OFFSET: f32 = 0.0;

/// 网格覆盖半径 = 视觉对角线 × 此倍数。越大覆盖越充分但计算量越大。
const COVERAGE_MULTIPLIER: f32 = 2.2;

/// 可见性裁剪外扩余量：AABB 判定时给页面边界外推一圈，避免浮点误差误裁。
const VISIBILITY_MARGIN: f32 = 200.0;

// ---- 水印 Form XObject 自身的 BBox ----
// 与可见性裁剪共用一组数值，必须保持一致。
const XOBJ_BBOX_LLX: f32 = -10.0;
const XOBJ_BBOX_LLY: f32 = -50.0;
const XOBJ_BBOX_URX: f32 = 2000.0;
const XOBJ_BBOX_URY: f32 = 200.0;

/// 单个 PDF 允许的最大水印数（防止极小页面/极小间距导致对象数爆炸）。
const MAX_ALLOWED_WATERMARKS: usize = 1_000_000;

/// 网格间距下限；小于此值拒绝生成。
const MIN_GRID_STEP_SIZE: f32 = 0.1;

/// 网格内容流字节的预分配上限（每锚点约 80 B），超过则交给 Vec 自己 grow。
/// 内存不做约束，这里只是避免一次性 reserve 得过分离谱。
const GRID_BYTES_PREALLOC_CAP: usize = 1 << 20;

// ---- 可选阶段计时（仅设了 WATERMARK_PROFILE 环境变量时输出）----

/// 生产路径只有一次 OnceLock 读取；不设环境变量时完全不打印、不格式化。
fn profile_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WATERMARK_PROFILE").is_some())
}

#[inline]
fn profile_mark(label: &str, mark: &mut Instant) {
    let now = Instant::now();
    if profile_on() {
        eprintln!("PROFILE {:<26} {:>9.2?}", label, now.duration_since(*mark));
    }
    *mark = now;
}

// ============================================================================
// 进程级资产缓存
// ============================================================================

/// 一次「字体 → 字形轮廓 → PDF 路径 → Flate 压缩」的全部产物。
/// 同一个 (字体, 文本) 组合在整个进程生命周期内只算一次：PHP-FPM 常驻、
/// CLI 批处理、以及多 PDF 并行时都能直接命中缓存。
struct WatermarkAsset {
    /// 已经压缩好的水印 Form XObject（含 /Filter /FlateDecode）。
    xobject: Stream,
    /// 文本宽度，网格间距计算用。
    text_w: f32,
}

type AssetKey = (String, String);
static ASSET_CACHE: OnceLock<RwLock<HashMap<AssetKey, Arc<WatermarkAsset>>>> = OnceLock::new();
static FONT_CACHE: OnceLock<RwLock<HashMap<String, Arc<Vec<u8>>>>> = OnceLock::new();

/// 字体文件字节常驻内存（11 MB 级），避免每次调用重复读盘 + 解析 CFF 表。
fn get_font_bytes(font_path: &str) -> BoxResult<Arc<Vec<u8>>> {
    let cache = FONT_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(bytes) = cache
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(font_path)
    {
        return Ok(Arc::clone(bytes));
    }
    let bytes = Arc::new(std::fs::read(font_path)?);
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    Ok(Arc::clone(
        guard.entry(font_path.to_owned()).or_insert(bytes),
    ))
}

/// 取（必要时构建）水印资产。写锁内二次检查：并行批处理首轮 N 个线程同时
/// miss 时，仍只解析一次 11 MB 字体、只跑一次字形轮廓 + Flate 压缩。
fn get_watermark_asset(font_path: &str, text: &str) -> BoxResult<Arc<WatermarkAsset>> {
    let cache = ASSET_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    let key: AssetKey = (font_path.to_owned(), text.to_owned());
    if let Some(asset) = cache.read().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(Arc::clone(asset));
    }

    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    if let Some(asset) = guard.get(&key) {
        return Ok(Arc::clone(asset));
    }

    let font_data = get_font_bytes(font_path)?;
    let font = FontRef::try_from_slice(font_data.as_slice())?;
    let encoded = Content {
        operations: text_to_pdf_paths(&font, text, 0.0, 0.0, DEFAULT_FONT_SIZE),
    }
    .encode()
    .map_err(|e| format!("encode watermark content failed: {:?}", e))?;
    let text_w = measure_text_width(&font, text, DEFAULT_FONT_SIZE);

    let mut xstream = build_watermark_xobject(encoded);
    if let Err(e) = xstream.compress() {
        eprintln!("WARN: 水印 XObject 压缩失败（按未压缩写入）：{:?}", e);
    }
    let asset = Arc::new(WatermarkAsset {
        xobject: xstream,
        text_w,
    });
    guard.insert(key, Arc::clone(&asset));
    Ok(asset)
}

// ============================================================================
// FFI
// ============================================================================

/// # Safety
/// 5 个 `*const c_char` 必须指向合法的 NUL 结尾 C 字符串，且调用期间保持有效。
///
/// # 返回值
/// - `0` 成功
/// - `-1` 处理失败（stderr 有原因）
/// - `-2` 任一参数为 NULL
/// - `-3` 任一参数不是合法 UTF-8
#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_pdf_watermark(
    input_path: *const c_char,
    output_path: *const c_char,
    font_path: *const c_char,
    user_name: *const c_char,
    date_str: *const c_char,
) -> i32 {
    if input_path.is_null()
        || output_path.is_null()
        || font_path.is_null()
        || user_name.is_null()
        || date_str.is_null()
    {
        eprintln!("ERROR: NULL pointer passed to add_pdf_watermark");
        return -2;
    }
    macro_rules! cstr {
        ($p:expr, $name:literal) => {
            match unsafe { CStr::from_ptr($p).to_str() } {
                Ok(s) => s.to_string(),
                Err(_) => {
                    eprintln!("ERROR: Invalid UTF-8 in {}", $name);
                    return -3;
                }
            }
        };
    }
    let input = cstr!(input_path, "input_path");
    let output = cstr!(output_path, "output_path");
    let font_p = cstr!(font_path, "font_path");
    let name = cstr!(user_name, "user_name");
    let date = cstr!(date_str, "date_str");

    let text = format!("致{}-{}:高度保密", name, date);

    // extern "C" 边界不允许 unwind：不兜住的话，畸形 PDF 触发的任意 panic 都会
    // 直接 abort 宿主进程（PHP-FPM worker 整条挂掉）。这里统一转成返回码 -1。
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        run_watermark_process(&input, &output, &font_p, &text)
    }));
    match outcome {
        Ok(Ok(_)) => 0,
        Ok(Err(e)) => {
            eprintln!("ERROR: add_pdf_watermark failed: {:?}", e);
            -1
        }
        Err(_) => {
            eprintln!("ERROR: add_pdf_watermark panicked; host process kept alive");
            -1
        }
    }
}

// ============================================================================
// 主流程
// ============================================================================

/// 加载策略：越靠前越快，但对 PDF 结构的前提越强；任一级不满足就降级。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadMode {
    /// 连对象流都不展开。仅当页树、页字典、资源字典都是普通对象时可用。
    Lite,
    /// 展开对象流，但丢弃结构树叶子（增量保存时原字节仍在，成品不受影响）。
    Filtered,
    /// 完整加载。整篇重写（剥签名 / xref 缺失）必须用这一级。
    Full,
}

/// lopdf 加载期对象过滤：丢弃 Tagged PDF 的 `/Type /StructElem`（无障碍结构树叶子）。
///
/// 水印链路只读页树 / 页字典 / 资源字典 / 签名字典，一个 StructElem 都不碰；
/// 但 1000 页样本里有 240,710 个，占全部对象的 96%。把它们塞进 `doc.objects`
/// 的 BTreeMap，会让解析、签名扫描、析构全部退化成 O(24 万)：实测分别
/// 502ms / 237ms / 457ms。
///
/// 只在「增量保存」路径使用：原文件字节原样写进输出，结构树在成品里完好无损。
/// 整篇重写必须重新完整加载（见 `run_watermark_process`）。
///
/// 注意：FilterFunc 是 `fn` 指针、不能捕获状态；lopdf 对普通对象会**忽略返回值**
/// 并继续使用被 `&mut` 借用的原对象，只有对象流成员才用返回值。因此本函数
/// 只能只读检查，保留的对象返回 clone，绝不能 mutate。
fn drop_struct_elem_filter(id: ObjectId, obj: &mut Object) -> Option<(ObjectId, Object)> {
    let dict = match obj {
        Object::Dictionary(d) => Some(&*d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    };
    if let Some(d) = dict
        && matches!(d.get(b"Type"), Ok(Object::Name(n)) if n.as_slice() == b"StructElem")
    {
        return None;
    }
    // 对象流里不允许放 Stream（PDF 1.7 §7.5.7），因此 Stream 一定是普通对象，
    // lopdf 会忽略本函数的返回值、继续用 &mut 指向的原对象。返回一个空值
    // 可避免把十几 MB 的流正文再 clone 一遍；非 Stream 才可能是对象流成员，
    // 必须返回货真价实的 clone。
    let kept = match obj {
        Object::Stream(_) => Object::Null,
        _ => obj.clone(),
    };
    Some((id, kept))
}

/// Lite 专用：对象流是普通间接对象，lopdf 在 filter 返回 None 时直接跳过它，
/// 连解压带 24 万个结构树叶子对象的解析都省掉了。
fn lite_filter(id: ObjectId, obj: &mut Object) -> Option<(ObjectId, Object)> {
    if let Object::Stream(s) = obj
        && matches!(s.dict.get(b"Type"), Ok(Object::Name(n)) if n.as_slice() == b"ObjStm")
    {
        return None;
    }
    drop_struct_elem_filter(id, obj)
}

/// 一次线性扫描找签名相关标记。命中任意一个就说明 Lite 路径不可信
/// （对象流没展开，可能漏掉藏在里面的签名 / 表单），应直接走常规路径。
/// 同时把 /AcroForm /Perms /DSS 纳入，是为了在加载**之前**就拦住带表单但
/// 无签名的文档，省掉一次注定失败的 Lite 加载（48MB 样本可省 ~76ms）。
fn raw_signature_hint(bytes: &[u8]) -> bool {
    // memchr 用 SIMD 找 '/'；PDF 里 '/' 密度不高，命中后只做 4 次 starts_with。
    for i in memchr::memchr_iter(b'/', bytes) {
        let tail = &bytes[i..];
        if tail.starts_with(b"/ByteRange")
            || tail.starts_with(b"/AcroForm")
            || tail.starts_with(b"/Perms")
            || tail.starts_with(b"/DSS")
        {
            return true;
        }
    }
    false
}

/// 加载 + 解密。
fn load_document(prev_bytes: &[u8], mode: LoadMode) -> BoxResult<Document> {
    let mut doc = match mode {
        LoadMode::Lite => {
            Document::load_mem_with_options(prev_bytes, LoadOptions::with_filter(lite_filter))?
        }
        LoadMode::Filtered => Document::load_mem_with_options(
            prev_bytes,
            LoadOptions::with_filter(drop_struct_elem_filter),
        )?,
        LoadMode::Full => Document::load_mem(prev_bytes)?,
    };
    // 加密 PDF 必须先解密：lopdf 不会自动解密，未解密时 ObjStm 字节是密文，
    // 解析不出对象 → 页树残缺 → 输出文件损坏。
    if doc.is_encrypted() {
        doc.decrypt("").map_err(|e| {
            format!(
                "输入 PDF 已加密，且无法用空密码解密（可能需要用户密码）：{:?}",
                e
            )
        })?;
    }
    Ok(doc)
}

/// 从根 /Pages 节点读规范强制存在的 /Count（总页数）。
fn declared_page_count(doc: &Document) -> Option<u32> {
    let catalog = doc.catalog().ok()?;
    let pages_id = match catalog.get(b"Pages").ok()? {
        Object::Reference(id) => *id,
        _ => return None,
    };
    let node = match doc.get_object(pages_id).ok()? {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &s.dict,
        _ => return None,
    };
    match node.get(b"Count").ok()? {
        Object::Integer(n) => u32::try_from(*n).ok().filter(|&v| v > 0),
        _ => None,
    }
}

/// 极速路径：用不展开对象流的 Document 做一次增量更新。
/// 成功即写盘返回；任何前提不满足都返回 Err，由调用方降级到常规路径，
/// 且**在写文件之前**就失败，不会留下半成品输出。
fn try_lite_incremental(
    prev_bytes: &[u8],
    doc: Document,
    asset: &WatermarkAsset,
    output_path: &str,
    out_capacity: usize,
    mark: &mut Instant,
) -> BoxResult<()> {
    // 对象流没展开，签名可能藏在里面。只有目录里完全没有签名入口、
    // 且原字节里也没有签名值字典特征时，才敢走这条路径。
    match doc.catalog() {
        Ok(cat) if !(cat.has(b"AcroForm") || cat.has(b"Perms") || cat.has(b"DSS")) => {}
        _ => return Err("lite: 目录含签名入口或不可读".into()),
    }
    // 调用方已扫过原字节里的 /ByteRange（避免对 20MB 文件重复全扫描）。
    if doc.xref_start == 0 {
        return Err("lite: 缺少交叉引用表".into());
    }

    // 页树完整性自校验：根 /Pages 节点的 /Count 是 PDF 规范强制写的总页数，
    // 拿它和实际遍历到的页数比对。若页字典藏在被跳过展开的对象流里，
    // 遍历数必然少于 /Count → 立即回退，避免静默漏页。
    // 比另起一次 load_metadata_mem 便宜得多（1000 页样本省 152ms）。
    let expected_pages = declared_page_count(&doc)
        .ok_or_else(|| -> Box<dyn Error> { "lite: 读不到页树 /Count".into() })?;
    let pages = doc.get_pages();
    if pages.is_empty() || pages.len() != expected_pages as usize {
        return Err(format!(
            "lite: 解析到 {} 页，页树声明 {} 页",
            pages.len(),
            expected_pages
        )
        .into());
    }

    let mut inc = IncrementalDocument::create_from(prev_bytes.to_vec(), doc);
    let (total, ok) = {
        let mut target = SaveTarget::Inc(&mut inc);
        watermark_pages(&mut target, asset)?
    };
    // Lite 对每页都要求成功：少一页就说明资源/内容对象被对象流吃掉了。
    if total != expected_pages as usize || ok != total {
        return Err(format!("lite: 仅 {}/{} 页注入成功", ok, total).into());
    }

    let mut out = Vec::with_capacity(out_capacity);
    inc.save_to(&mut out)?;
    profile_mark("lite save_to", mark);
    std::fs::write(output_path, out)?;
    profile_mark("lite fs::write", mark);
    drop(inc);
    profile_mark("lite drop", mark);
    Ok(())
}

/// 加载 PDF → 预生成水印对象 → 按需剥离签名 → 按签名/xref 状态选保存路径 → 写盘。
/// 返回 `Err` 时输出文件不会被创建（见 `ensure_pages_watermarked`）。
pub fn run_watermark_process(
    input_path: &str,
    output_path: &str,
    font_path: &str,
    text: &str,
) -> Result<(), Box<dyn Error>> {
    // 只读一次盘：load_mem 直接在这块内存上解析，增量保存还能原样复用它，
    // 省掉旧实现 `load(path)` + `read(path)` 的第二次整文件 I/O 与拷贝。
    let mut mark = Instant::now();
    let prev_bytes = std::fs::read(input_path)?;
    profile_mark("read", &mut mark);
    // 内存不做约束：一次性预留输出缓冲，避免 Vec 反复 grow + 拷贝。
    let out_capacity = prev_bytes.len().saturating_mul(4).max(1 << 20);

    // 字体解析 / 字形轮廓 / Flate 压缩走进程级缓存，命中时这里几乎零成本。
    let asset = get_watermark_asset(font_path, text)?;
    profile_mark("asset", &mut mark);

    // ---- 极速路径：页树/资源都在普通对象里时，连对象流都不展开 ----
    // 页树自身的 /Count 用来校验没有静默漏页，不需要额外解析一遍文件。
    let sig_hint = raw_signature_hint(&prev_bytes);
    profile_mark("sig_hint_scan", &mut mark);
    if !sig_hint {
        if let Ok(lite_doc) = load_document(&prev_bytes, LoadMode::Lite) {
            profile_mark("load_mem(lite)", &mut mark);
            if profile_on() {
                eprintln!("PROFILE objects_in_doc(lite)    {:>9}", lite_doc.objects.len());
            }
            match try_lite_incremental(
                &prev_bytes,
                lite_doc,
                &asset,
                output_path,
                out_capacity,
                &mut mark,
            ) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if profile_on() {
                        eprintln!("PROFILE lite fallback: {}", e);
                    }
                }
            }
        } else if profile_on() {
            eprintln!("PROFILE lite load failed, fallback");
        }
    }

    // ---- 常规路径：展开对象流但丢弃结构树叶子 ----
    // 增量保存时原字节原样写出，被丢弃的 StructElem 仍完整存在于成品里。
    let mut doc = load_document(&prev_bytes, LoadMode::Filtered)?;
    profile_mark("load_mem(filtered)", &mut mark);
    if profile_on() {
        eprintln!("PROFILE objects_in_doc          {:>9}", doc.objects.len());
    }

    // 加水印必然改字节，签名校验永远走不通；剥离校验链路让 Adobe 不再弹
    // "签名无效"，同时把 /AP 里的签章图片降级为图章注释保留显示。
    let mut removed_signatures = strip_digital_signatures(&mut doc);
    profile_mark("strip_signatures", &mut mark);

    // 无签名且 xref 完整 → 增量更新（保住其余字节，输出更小）；
    // 否则整篇重写。整篇重写会按 doc.objects 重新生成文件，被过滤掉的
    // StructElem 必须回来，否则结构树引用全部悬空 → 重新完整加载一次。
    // 有签名的文件通常只有几 MB，这次二次加载成本远小于收益。
    let mut use_incremental = removed_signatures == 0 && doc.xref_start != 0;
    if !use_incremental {
        doc = load_document(&prev_bytes, LoadMode::Full)?;
        profile_mark("load_mem(full_reload)", &mut mark);
        if profile_on() {
            eprintln!("PROFILE objects_in_doc(full)    {:>9}", doc.objects.len());
        }
        removed_signatures = strip_digital_signatures(&mut doc);
        profile_mark("strip_signatures(full)", &mut mark);
        use_incremental = removed_signatures == 0 && doc.xref_start != 0;
    }
    if removed_signatures > 0 {
        eprintln!(
            "INFO: 检测到数字签名，已剥离 {} 处签名校验信息（签章图章按原样保留）",
            removed_signatures
        );
    }

    if use_incremental {
        let mut inc = IncrementalDocument::create_from(prev_bytes, doc);
        {
            let mut target = SaveTarget::Inc(&mut inc);
            let (total, ok) = watermark_pages(&mut target, &asset)?;
            ensure_pages_watermarked(total, ok)?;
        }
        profile_mark("watermark_pages(inc)", &mut mark);
        // save_to 写进预分配 Vec，再一次 fs::write 落盘：
        // 绕过 BufWriter 的二次缓冲拷贝，且只在成功后创建输出文件。
        let mut out = Vec::with_capacity(out_capacity);
        inc.save_to(&mut out)?;
        profile_mark("save_to(inc)", &mut mark);
        std::fs::write(output_path, out)?;
        profile_mark("fs::write", &mut mark);
        drop(inc);
        profile_mark("drop(inc)", &mut mark);
    } else {
        if removed_signatures == 0 {
            eprintln!("WARN: 输入 PDF 缺少交叉引用表（xref_start=0），退回整篇重写保存");
        }
        {
            let mut target = SaveTarget::Full(&mut doc);
            let (total, ok) = watermark_pages(&mut target, &asset)?;
            ensure_pages_watermarked(total, ok)?;
        }
        profile_mark("watermark_pages(full)", &mut mark);
        let mut out = Vec::with_capacity(out_capacity);
        doc.save_to(&mut out)?;
        profile_mark("save_to(full)", &mut mark);
        std::fs::write(output_path, out)?;
        profile_mark("fs::write", &mut mark);
        drop(doc);
        profile_mark("drop(doc)", &mut mark);
    }
    Ok(())
}

/// 一页都没解析出来、或一页都没注入成功，都不允许写出文件。
fn ensure_pages_watermarked(
    total_pages: usize,
    watermarked_pages: usize,
) -> Result<(), Box<dyn Error>> {
    if total_pages == 0 {
        return Err(
            "未能从输入 PDF 中解析出任何页面，文件可能已损坏或仍处于加密状态，未生成输出文件"
                .into(),
        );
    }
    if watermarked_pages == 0 {
        return Err(format!(
            "全部 {} 页均无法注入水印资源，未生成输出文件",
            total_pages
        )
        .into());
    }
    // 部分页失败也不允许返回成功：过去会静默产出「只有一部分页有水印」的文件。
    if watermarked_pages != total_pages {
        return Err(format!(
            "仅 {}/{} 页成功注入水印，疑似页结构异常，未生成输出文件",
            watermarked_pages, total_pages
        )
        .into());
    }
    Ok(())
}// ============================================================================
// 保存路径抽象：Full / Inc 共用一套 per-page 流程
// ============================================================================

/// 封装「整篇重写」与「增量更新」两条保存路径共用的高层操作。
/// - Full：所有对象本就在 `Document` 里，`ensure_in_new_doc` 是 no-op。
/// - Inc：改写落在 `IncrementalDocument::new_document`，
///   读取上一修订走 `prev_doc`（保留原始字节）。
enum SaveTarget<'a> {
    Full(&'a mut Document),
    Inc(&'a mut IncrementalDocument),
}

type BoxResult<T> = Result<T, Box<dyn Error>>;

impl<'a> SaveTarget<'a> {
    fn prev_doc(&self) -> &Document {
        match self {
            Self::Full(d) => d,
            Self::Inc(i) => i.get_prev_documents(),
        }
    }

    fn new_doc_mut(&mut self) -> &mut Document {
        match self {
            Self::Full(d) => d,
            Self::Inc(i) => &mut i.new_document,
        }
    }

    /// 在「上一修订」中只读查找对象。Full 路径下 prev_doc 就是 doc；Inc 路径下
    /// 包含本次未改动的全部原对象（用于追溯原 /Contents 指向的内容流）。
    fn lookup_prev(&self, id: ObjectId) -> Option<&Object> {
        self.prev_doc().get_object(id).ok()
    }

    /// 把对象确保在「本次修订」中可写。
    /// Full 路径无需操作（对象本来就在 doc 里）；Inc 路径从 prev 复制。
    fn ensure_in_new_doc(&mut self, id: ObjectId) -> BoxResult<()> {
        if let Self::Inc(i) = self {
            i.opt_clone_object_to_new_document(id)?;
        }
        Ok(())
    }

    fn add_object(&mut self, obj: impl Into<Object>) -> ObjectId {
        match self {
            Self::Full(d) => d.add_object(obj),
            Self::Inc(i) => i.new_document.add_object(obj),
        }
    }

    /// 在 `owner[key]` 写入任意 Object 值。owner 被 ensure 后才能写入。
    fn set_dict_entry(&mut self, owner_id: ObjectId, key: &[u8], value: Object) -> BoxResult<()> {
        self.ensure_in_new_doc(owner_id)?;
        let new_doc = self.new_doc_mut();
        let owner = new_doc.get_object_mut(owner_id)?;
        let dict = match owner {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => {
                return Err(format!("对象 {} 不是字典或流", owner_id.0).into());
            }
        };
        dict.set(key, value);
        Ok(())
    }

    /// 取出 `owner[key]` 指向的字典 id。缺失按规范补空字典；类型异常则报错。
    fn ensure_sub_dict(&mut self, owner_id: ObjectId, key: &[u8]) -> BoxResult<ObjectId> {
        enum Slot {
            Ref(ObjectId),
            Inline(Dictionary),
            Missing,
        }
        let slot = {
            let new_doc = self.new_doc_mut();
            let owner = new_doc.get_object(owner_id)?;
            let owner_dict = match owner {
                Object::Dictionary(d) => d,
                Object::Stream(s) => &s.dict,
                _ => return Err(format!("对象 {} 不是字典或流", owner_id.0).into()),
            };
            match owner_dict.get(key) {
                Ok(Object::Reference(id)) => Slot::Ref(*id),
                Ok(Object::Dictionary(d)) => Slot::Inline(d.clone()),
                Ok(other) => {
                    return Err(format!(
                        "对象 {} 的 {} 类型异常：{}",
                        owner_id.0,
                        String::from_utf8_lossy(key),
                        other.enum_variant()
                    )
                    .into());
                }
                Err(_) => Slot::Missing,
            }
        };
        match slot {
            Slot::Ref(id) => {
                self.ensure_in_new_doc(id)?;
                Ok(id)
            }
            Slot::Inline(d) => {
                let id = self.add_object(d);
                self.set_dict_entry(owner_id, key, Object::Reference(id))?;
                Ok(id)
            }
            Slot::Missing => {
                let id = self.add_object(dictionary! {});
                self.set_dict_entry(owner_id, key, Object::Reference(id))?;
                Ok(id)
            }
        }
    }

    /// 取页面的 /Resources 在新文档里的对象 id。
    /// 缺失时按 PDF 规范继承父 Pages 节点的 /Resources（避免整页排版错乱）。
    fn ensure_page_resources(&mut self, page_id: ObjectId) -> BoxResult<ObjectId> {
        self.ensure_in_new_doc(page_id)?;
        enum Slot {
            Ref(ObjectId),
            Inline(Dictionary),
            Missing,
        }
        let slot = {
            let new_doc = self.new_doc_mut();
            let page = new_doc.get_object(page_id)?;
            let page_dict = match page {
                Object::Dictionary(d) => d,
                Object::Stream(s) => &s.dict,
                _ => return Err(format!("页面 {} 不是字典或流", page_id.0).into()),
            };
            match page_dict.get(b"Resources") {
                Ok(Object::Reference(id)) => Slot::Ref(*id),
                Ok(Object::Dictionary(d)) => Slot::Inline(d.clone()),
                Ok(other) => {
                    return Err(format!(
                        "页面 {} 的 /Resources 类型异常：{}",
                        page_id.0,
                        other.enum_variant()
                    )
                    .into());
                }
                Err(_) => Slot::Missing,
            }
        };
        match slot {
            Slot::Ref(id) => {
                // /Resources 常被多页共享：复制进本次修订后统一修改。
                // 本工具每页都加水印，正好希望共享该资源的页面都看到新版本。
                self.ensure_in_new_doc(id)?;
                Ok(id)
            }
            Slot::Inline(d) => {
                let id = self.add_object(d);
                self.set_dict_entry(page_id, b"Resources", Object::Reference(id))?;
                Ok(id)
            }
            Slot::Missing => {
                let inherited = find_inherited_resources(self.prev_doc(), page_id)
                    .unwrap_or_default();
                let id = self.add_object(inherited);
                self.set_dict_entry(page_id, b"Resources", Object::Reference(id))?;
                Ok(id)
            }
        }
    }

    /// 追加水印到页面 /Contents，同时插入一对 q/Q 隔离原内容遗留的图形状态。
    /// 原内容流对象本身不被修改。
    ///
    /// 隔离原因：Spire.Doc / wkhtmltopdf 等工具产出的 PDF，页面内容流以**未配对**
    /// 的 `cm` 开头（如 Y 轴翻转）且全程不恢复。若直接在水印前不隔离，水印会
    /// 继承这个翻转后的坐标系（字形上下镜像）。统一在原内容前后各插一个 q/Q
    /// 小内容流，把遗留的 CTM / 裁剪路径等图形状态封装起来。
    ///
    /// 性能：q / Q / 网格流都是全文档共享的间接对象，由 `watermark_pages` 一次
    /// 性创建。每页只改自己的 /Contents 数组去引用这三个 id，不再逐页建流、
    /// 编码 ops、跑 Flate —— 500 页同尺寸 PDF 从 ~1500 个新对象降到 3 个。
    fn set_page_contents(
        &mut self,
        page_id: ObjectId,
        open_id: ObjectId,
        close_id: ObjectId,
        watermark_id: ObjectId,
    ) -> BoxResult<()> {
        self.ensure_in_new_doc(page_id)?;
        // 1. 收集原 /Contents 的全部流引用（保留 prev 字节）。
        // /Contents 引用可能在 prev 里（Inc 路径），所以走 prev_doc().get_object()。
        let mut original_refs: Vec<Object> = Vec::new();
        {
            let new_doc = self.new_doc_mut();
            let page = new_doc.get_object(page_id)?;
            let page_dict = match page {
                Object::Dictionary(d) => d,
                Object::Stream(s) => &s.dict,
                other => {
                    return Err(format!(
                        "page object is not a Dictionary or Stream: {}",
                        other.enum_variant()
                    )
                    .into());
                }
            };
            // 把 /Contents 的形态先克隆出来，避免借用 new_doc 又想用 lookup_prev(&self)
            let contents_kind: Option<Result<Object, ()>> = match page_dict.get(b"Contents") {
                Ok(c) => Some(Ok(c.clone())),
                Err(_) => None,
            };
            match contents_kind {
                Some(Ok(Object::Reference(id))) => match self.lookup_prev(id) {
                    Some(Object::Stream(_)) => original_refs.push(Object::Reference(id)),
                    Some(Object::Array(arr)) => original_refs.extend(arr.clone()),
                    Some(_) => original_refs.push(Object::Reference(id)),
                    None => {
                        return Err(format!(
                            "页面 {} 的 /Contents 对象 {:?} 在 prev 中未找到",
                            page_id.0, id
                        )
                        .into());
                    }
                },
                Some(Ok(Object::Array(arr))) => original_refs = arr,
                _ => {}
            }
        }
        // 2. 重组 /Contents：q + 原内容 + Q + 水印（全是共享对象的引用）
        let mut contents: Vec<Object> = Vec::with_capacity(original_refs.len() + 3);
        contents.push(Object::Reference(open_id));
        contents.extend(original_refs);
        contents.push(Object::Reference(close_id));
        contents.push(Object::Reference(watermark_id));
        self.set_dict_entry(page_id, b"Contents", Object::Array(contents))?;
        Ok(())
    }
}

// ============================================================================
// 数字签名剥离
// ============================================================================

/// 取出对象内部的字典（Dictionary 直取，Stream 取它的 dict）。
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

/// 判断对象是否为签名/时间戳值字典：
/// - `/Type /Sig`：普通数字签名
/// - `/Type /DocTimeStamp`：文档时间戳
/// - 无 /Type 但同时有 /ByteRange + /Contents：少数签名实现的写法
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
/// 步骤：
/// 1. `/AcroForm` 字段树（含嵌套 /Kids）中 `/FT /Sig` 的字段及 `/V` 指向签名值的字段；
/// 2. 各页 /Annots 中的签名控件：`/Subtype /Widget` → `/Subtype /Stamp`，
///    清掉 `/FT /V /T /Parent`；/AP 保留 → 签章视觉效果由此获得；
/// 3. 目录 /Perms（认证签名 DocMDP 引用）；
/// 4. 签名值字典本体（`/Type /Sig`、`/Type /DocTimeStamp`）。
///
/// 返回被剥离的签名对象数量（多控件签名域字典 + 签名值字典；图章注释本身不计入）。
fn strip_digital_signatures(doc: &mut Document) -> usize {
    // 目录里既没有 /AcroForm、也没有 /Perms /DSS 时，阅读器根本没有可校验的
    // 签名链路（孤立 /Sig 字典不会被验证，自然也不会弹「签名无效」）。
    // 大文档里 doc.objects 可能有几十万对象，这一步能省掉整轮全量扫描。
    // catalog 解析不出来时保守地照常全扫。
    if doc
        .catalog()
        .map(|c| !(c.has(b"AcroForm") || c.has(b"Perms") || c.has(b"DSS")))
        .unwrap_or(false)
    {
        return 0;
    }

    // 单次全量遍历：同时收签名值字典与「可能是签名域」的候选。第二遍只扫这个
    // 小候选集，不再对 doc.objects（大文档里几万个对象）做第二次全量扫描。
    let mut sig_value_ids: HashSet<ObjectId> = HashSet::new();
    let mut candidates: Vec<(ObjectId, bool, Option<ObjectId>)> = Vec::new();
    for (&id, obj) in doc.objects.iter() {
        let dict = match object_dict(obj) {
            Some(d) => d,
            None => continue,
        };
        if is_signature_value(obj) {
            sig_value_ids.insert(id);
        }
        let ft_is_sig = matches!(dict.get(b"FT"), Ok(Object::Name(n)) if n.as_slice() == b"Sig");
        let v_ref = match dict.get(b"V") {
            Ok(Object::Reference(v)) => Some(*v),
            _ => None,
        };
        if ft_is_sig || v_ref.is_some() {
            candidates.push((id, ft_is_sig, v_ref));
        }
    }

    let mut sig_field_ids: HashSet<ObjectId> = HashSet::new();
    for (id, ft_is_sig, v_ref) in candidates {
        if ft_is_sig || v_ref.is_some_and(|v| sig_value_ids.contains(&v)) {
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

    // 从 AcroForm 字段树中摘除签名域
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
            // /SigFlags 3 = SignaturesExist | AppendOnly。字段已清空，若仍声明
            // 「本文档含签名」，Adobe 会继续走签名校验路径并可能弹告警。
            if let Ok(acro) = doc.get_dictionary_mut(acro_id) {
                acro.remove(b"SigFlags");
            }
        }
        Some(Object::Dictionary(mut acro)) => {
            if let Some(Object::Array(fields)) = acro.get(b"Fields").ok().cloned() {
                let kept = prune_signature_fields(doc, &fields, &sig_field_ids);
                acro.set(b"Fields", Object::Array(kept));
            }
            acro.remove(b"SigFlags");
            if let Ok(catalog) = doc.catalog_mut() {
                catalog.set(b"AcroForm", Object::Dictionary(acro));
            }
        }
        _ => {}
    }

    // 把签名控件降级为装饰性图章：
    // /AP（外观流）画的是签章图片——这正是签章效果的载体，必须保留。
    // 只剥掉字段语义，让 Adobe 把它当普通图章注释渲染。
    for &widget_id in &widget_ids {
        if let Ok(obj) = doc.get_object_mut(widget_id) {
            if let Some(d) = object_dict_mut(obj) {
                d.set(b"Subtype", Object::Name(b"Stamp".to_vec()));
                d.remove(b"Parent");
                d.remove(b"FT");
                d.remove(b"V");
                d.remove(b"T");
            }
        }
    }

    // 认证签名的权限声明也要清掉
    if let Ok(catalog) = doc.catalog_mut() {
        catalog.remove(b"Perms");
    }

    // 抹掉签名值字典与「多控件」签名域字典（就地清空，不删对象）：
    // 删除对象会在交叉引用表里留下没有条目的「洞」（trailer 的 /Size 仍按
    // 最大对象号计算，条目却缺了），部分阅读器会重建 xref 甚至报错。
    // 把对象原地清空成空字典——对象号照样出现在交叉引用表里，任何残留引用都
    // 只指向一个无害的空字典，且不再携带任何签名语义。
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

/// 递归裁剪字段数组：丢弃整棵签名域子树；普通字段递归处理其 /Kids。
/// 只有真的从 /Kids 中摘掉了签名域才回写，避免无谓改动无关字段。
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
}// ============================================================================
// 水印 Form XObject + 文本 → 路径转换
// ============================================================================

/// 构造水印 Form XObject 字典（BBox / Resources / Matrix），流内容由调用方压入。
fn build_watermark_xobject(encoded: Vec<u8>) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
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
                        "ca" => 0.1f32,
                        "CA" => 0.1f32,
                    }
                }
            },
        },
        encoded,
    )
}

/// 把文本转为 PDF 路径操作序列：每个 glyph outline 转 m/l/c/h，
/// 二次贝塞尔按规范转三次，结尾统一 f 填充、Q 恢复图形状态。
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
            let mut last_point: Option<Point> = None;
            for curve in outline.curves {
                let p0 = match curve {
                    OutlineCurve::Line(p0, _) => p0,
                    OutlineCurve::Quad(p0, _, _) => p0,
                    OutlineCurve::Cubic(p0, _, _, _) => p0,
                };

                // 浮点容差 0.001 点：fontdue 给出的 outline 端点精度即此量级，
                // 超过此阈值视为新子轮廓（点 → 移动）。
                let is_new_contour = match last_point {
                    None => true,
                    Some(lp) => ((p0.x - lp.x).abs() > 0.001) || ((p0.y - lp.y).abs() > 0.001),
                };

                if is_new_contour {
                    if last_point.is_some() {
                        ops.push(Operation::new("h", vec![]));
                    }
                    ops.push(Operation::new(
                        "m",
                        vec![
                            (x_cursor + p0.x * h_factor).into(),
                            (y_start + p0.y * v_factor).into(),
                        ],
                    ));
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
                        // PDF 只支持三次贝塞尔：二次 → 三次控制点公式
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
                ops.push(Operation::new("h", vec![]));
            }
        }
        x_cursor += scaled_font.h_advance(glyph_id);
    }
    // 单次 f 填充覆盖全部子轮廓（每个子轮廓已用 h 闭合）
    ops.push(Operation::new("f", vec![]));
    ops.push(Operation::new("Q", vec![]));

    ops
}

/// 文本宽度（点数）。
fn measure_text_width(font: &FontRef, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    for c in text.chars() {
        w += scaled.h_advance(font.glyph_id(c));
    }
    w
}

// ============================================================================
// 页面几何
// ============================================================================

/// 从 MediaBox 提取左下角 + 尺寸 `(llx, lly, w, h)`。
/// 必须返回 4 个值而不是只返回 (w, h)：扫描件 MediaBox 常以 (20, 30, 595, 842)
/// 起始，把 llx/lly 当 0 用会让水印整体偏移出页外。
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

fn obj_to_f32(o: &Object) -> f32 {
    match o {
        Object::Real(r) => *r,
        Object::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

/// 沿 /Parent 链查 Rotate（PDF 允许页面省略此键而由父 Pages 节点提供）。
/// 限制深度 10 防 /Parent 成环导致的死循环。
///
/// `memo` 按节点 id 缓存「该节点沿祖先链解出的 Rotate」：同一 Pages 父节点下
/// 几百页只解一次链。顺带兼容 Real 型 `/Rotate 90.0`（旧实现只认 Integer，
/// 会静默退化成 0，导致 90/270 页水印方向错）。
fn get_page_rotation(
    doc: &Document,
    page_id: ObjectId,
    memo: &mut HashMap<ObjectId, f32>,
) -> f32 {
    const MAX_PARENT_DEPTH: usize = 10;
    let mut chain: Vec<ObjectId> = Vec::with_capacity(12);
    let mut current_id = Some(page_id);
    let mut result = 0.0f32;
    let mut depth = 0usize;

    while let Some(id) = current_id {
        if depth > MAX_PARENT_DEPTH {
            break;
        }
        if let Some(&cached) = memo.get(&id) {
            result = cached;
            break;
        }
        chain.push(id);
        let obj = match doc.get_object(id) {
            Ok(o) => o,
            Err(_) => break,
        };
        let dict = match obj {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            _ => break,
        };
        let own = match dict.get(b"Rotate") {
            Ok(Object::Integer(r)) => Some(*r as f32),
            Ok(Object::Real(r)) => Some(*r),
            Ok(Object::Reference(ref_id)) => match doc.get_object(*ref_id) {
                Ok(Object::Integer(r)) => Some(*r as f32),
                Ok(Object::Real(r)) => Some(*r),
                _ => None,
            },
            _ => None,
        };
        if let Some(r) = own {
            result = r;
            break;
        }
        current_id = match dict.get(b"Parent") {
            Ok(Object::Reference(p)) => Some(*p),
            _ => None,
        };
        depth += 1;
    }

    for id in chain {
        memo.insert(id, result);
    }
    result
}

/// 沿 /Parent 链向上查找继承的 /Resources，返回其副本。
/// Word/WPS 产出的 PDF 常见页面省略 /Resources 而由父 Pages 节点提供。
/// 链断掉或超出 32 层返回 None。
fn find_inherited_resources(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut current = page_id;
    for _ in 0..32 {
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

// ============================================================================
// 每页水印注入（两条保存路径共用）
// ============================================================================

/// 网格内容流的分组键：决定一份网格字节能否被多页共享。
/// 只有页面几何与归一化 /Rotate 影响锚点坐标；字体、角度、间距全文档恒定。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GridKey {
    llx: u32,
    lly: u32,
    w: u32,
    h: u32,
    rotation: u32,
}

impl GridKey {
    #[inline]
    fn new(llx: f32, lly: f32, w: f32, h: f32, rotation: f32) -> Self {
        // -0.0 与 0.0 几何等价，统一成 0.0 提高分组命中率。
        let bits = |v: f32| if v == 0.0 { 0.0f32 } else { v }.to_bits();
        let r = ((rotation as i32) % 360 + 360) % 360;
        Self {
            llx: bits(llx),
            lly: bits(lly),
            w: bits(w),
            h: bits(h),
            rotation: r as u32,
        }
    }

    #[inline]
    fn values(&self) -> (f32, f32, f32, f32, f32) {
        (
            f32::from_bits(self.llx),
            f32::from_bits(self.lly),
            f32::from_bits(self.w),
            f32::from_bits(self.h),
            self.rotation as f32,
        )
    }
}

/// 把水印 XObject 登记到页面的资源字典里。
/// `registered` 记录已写过的 /Resources 对象：共享资源字典的几百页只需写一次。
fn add_xobject_to_page(
    target: &mut SaveTarget,
    page_id: ObjectId,
    x_name: &str,
    x_id: ObjectId,
    registered: &mut HashSet<ObjectId>,
) -> BoxResult<()> {
    let resources_id = target.ensure_page_resources(page_id)?;
    if registered.contains(&resources_id) {
        return Ok(());
    }
    let xobjects_id = target.ensure_sub_dict(resources_id, b"XObject")?;
    target.set_dict_entry(xobjects_id, x_name.as_bytes(), Object::Reference(x_id))?;
    // 成功后才记入，避免失败时把同组的后续页全部跳过。
    registered.insert(resources_id);
    Ok(())
}

/// 遍历所有页面，写入水印 XObject + 网格内容。
///
/// 三阶段设计（核心 CPU 优化）：
/// - A 只读扫描页树，按 (MediaBox, Rotate) 把页面分组；
/// - B **每个分组只生成一次**网格内容流（原来每页各生成一次）；
/// - C 每页只改自己的 /Contents 数组，引用共享流的 id。
///
/// 500 页同尺寸 PDF：网格计算 / ops 分配 / 浮点格式化 / Flate 压缩各从 468 次
/// 降到 1 次，新增对象从 ~1500 个降到 4 个。
///
/// 返回 `(总页数, 成功注入水印的页数)`。
fn watermark_pages(
    target: &mut SaveTarget,
    asset: &WatermarkAsset,
) -> BoxResult<(usize, usize)> {
    let xobject_id = target.add_object(asset.xobject.clone());
    let xobject_name = "Watermark1";

    // q / Q 隔离流全文档共享（每页只是引用它们）。
    let open_id = target.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let close_id = target.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));

    // 页面列表与几何都取自 prev（那里保留着原始页树）
    let mut mark = Instant::now();
    let pages: Vec<(u32, ObjectId)> = target.prev_doc().get_pages().into_iter().collect();
    profile_mark("  get_pages", &mut mark);

    // ---- 阶段 A：只读计算几何并分组 ----
    let mut keys: Vec<GridKey> = Vec::with_capacity(pages.len());
    let mut unique: HashSet<GridKey> = HashSet::new();
    let mut rot_memo: HashMap<ObjectId, f32> = HashMap::new();
    {
        let prev = target.prev_doc();
        for &(_, page_id) in &pages {
            let (llx, lly, w, h) =
                page_size(prev, page_id).unwrap_or((0.0, 0.0, 595.0, 842.0));
            let rotation = get_page_rotation(prev, page_id, &mut rot_memo);
            let key = GridKey::new(llx, lly, w, h, rotation);
            keys.push(key);
            unique.insert(key);
        }
    }
    profile_mark("  stage_A_geometry", &mut mark);

    // ---- 阶段 B：每分组生成一次网格流，压缩后登记为共享对象 ----
    let mut grid_ids: HashMap<GridKey, ObjectId> = HashMap::with_capacity(unique.len());
    for key in unique {
        let (llx, lly, w, h, rotation) = key.values();
        let bytes = match build_watermark_grid_bytes(
            xobject_name,
            DEFAULT_FONT_SIZE,
            WATERMARK_ANGLE_DEG,
            llx,
            lly,
            w,
            h,
            asset.text_w,
            rotation,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN: 生成水印网格失败（几何 {:?}）：{:?}", key.values(), e);
                continue;
            }
        };
        let mut stream = Stream::new(Dictionary::new(), bytes);
        if let Err(e) = stream.compress() {
            eprintln!("WARN: 水印内容流压缩失败（按未压缩写入）：{:?}", e);
        }
        grid_ids.insert(key, target.add_object(stream));
    }
    profile_mark("  stage_B_grid", &mut mark);

    // ---- 阶段 C：逐页只做资源注册去重 + /Contents 重组 ----
    let mut registered: HashSet<ObjectId> = HashSet::new();
    let mut total_pages = 0usize;
    let mut watermarked_pages = 0usize;
    for ((page_num, page_id), key) in pages.into_iter().zip(keys) {
        total_pages += 1;

        let Some(&wm_id) = grid_ids.get(&key) else {
            eprintln!("WARN: 第 {} 页的水印网格不可用，跳过", page_num);
            continue;
        };
        if let Err(e) = add_xobject_to_page(
            target,
            page_id,
            xobject_name,
            xobject_id,
            &mut registered,
        ) {
            eprintln!(
                "WARN: 第 {} 页结构非标准，无法注入资源。错误：{:?}",
                page_num, e
            );
            continue;
        }
        if let Err(e) = target.set_page_contents(page_id, open_id, close_id, wm_id) {
            eprintln!("WARN: 添加页面内容失败，跳过第 {} 页：{:?}", page_num, e);
            continue;
        }
        watermarked_pages += 1;
    }
    profile_mark("  stage_C_pages", &mut mark);
    Ok((total_pages, watermarked_pages))
}

// ============================================================================
// 水印网格指令生成
// ============================================================================

/// 生成水印网格的内容流字节（矩阵法版本，对任意 /Rotate × 任意 MediaBox 都正确）。
/// 直接写 `Vec<u8>`：不再构造 `Vec<Operation>`（每锚点 4 个 String + 4 个
/// `Vec<Object>` 堆分配）再交给 `Content::encode` 逐操作数做浮点格式化。
///
/// # 核心思路
/// 老三的回忆 PDF 第 3 页起 `MediaBox=(0,0,434,283)`（横屏 434×283）+ `/Rotate=270`。
/// 旧实现用「if 90/270 交换宽高」硬编码，导致 R=90/270 时又被交换一次、锚点错位重叠。
/// 这里严格按 PDF 1.7 §7.2.3.3 的仿射变换推导，任意 /Rotate × 任意 MediaBox 都对。
///
/// # 推导
/// 1. 阅读器施加的显示变换 P→V：`V = T(center) · Rot(R) · T(-center_P) · P`，
///    `center_P = (llx + w/2, lly + h/2)`。
/// 2. 反函数 V→P：`P = T(center_P) · Rot(-R) · T(-center_V) · V`。
///    /Rotate=90/270 会交换宽高 → 视觉尺寸 `vis_w / vis_h` 需相应交换。
/// 3. 水印最终 cm 的旋转部分：
///    阅读器对整页（含水印）再施加「逆时针 -R°」（即顺时针 R°）；
///    若我们在 P 系写水印旋转 φ，则视觉总旋转 = φ + (-R°)；
///    要它等于视觉目标角 angle → `φ = angle + R°`。
///    验证：R=270°、angle=60° → φ=330° → 视觉 330+(-270)=60° ✓
fn build_watermark_grid_bytes(
    x_name: &str,
    size: f32,
    angle: f32,
    mb_llx: f32,
    mb_lly: f32,
    w: f32,
    h: f32,
    text_w: f32,
    page_rotation: f32,
) -> BoxResult<Vec<u8>> {
    // 几何值必须是有限正数：obj_to_f32 遇到异常对象返回 0.0，旧实现会继续
    // 算出 NaN / 除零坐标却仍返回「成功」，最终静默生成无水印的页面。
    if !(mb_llx.is_finite()
        && mb_lly.is_finite()
        && w.is_finite()
        && h.is_finite()
        && w > 0.0
        && h > 0.0
        && text_w.is_finite()
        && text_w >= 0.0
        && page_rotation.is_finite())
    {
        return Err(format!(
            "页面几何无效: llx={} lly={} w={} h={} text_w={} rotate={}",
            mb_llx, mb_lly, w, h, text_w, page_rotation
        )
        .into());
    }

    let step_inner = text_w + GRID_HORIZONTAL_GAP;
    let step_outer = size * GRID_VERTICAL_MULTIPLIER;

    if !(step_inner > MIN_GRID_STEP_SIZE && step_outer > MIN_GRID_STEP_SIZE) {
        return Err(format!(
            "Grid step too small: inner={}, outer={}",
            step_inner, step_outer
        )
        .into());
    }

    // 1. 归一化 Rotate 到 0/90/180/270
    let r = ((page_rotation as i32) % 360 + 360) % 360;
    let rad_r = (r as f32).to_radians();
    let (cr, sr) = (rad_r.cos(), rad_r.sin());
    let rot_neg_r = |x: f32, y: f32| -> (f32, f32) {
        // 2D 旋转 -R°（顺时针 R°）：x' = x·cosR + y·sinR ; y' = -x·sinR + y·cosR
        (x * cr + y * sr, -x * sr + y * cr)
    };

    // 2. 视觉系宽/高（90/270 时交换）+ 视觉中心 + PDF 内部中心
    let (vis_w, vis_h) = match r {
        90 | 270 => (h, w),
        _ => (w, h),
    };
    let cx_v = vis_w / 2.0 + CENTER_X_OFFSET;
    let cy_v = vis_h / 2.0 - CENTER_Y_OFFSET;
    let cx_p = mb_llx + w / 2.0;
    let cy_p = mb_lly + h / 2.0;

    // 3. V→P 仿射
    let v_to_p = |vx: f32, vy: f32| -> (f32, f32) {
        let (ax, ay) = (vx - cx_v, vy - cy_v);
        let (bx, by) = rot_neg_r(ax, ay);
        (bx + cx_p, by + cy_p)
    };

    // 4. 水印最终旋转 φ = angle + R
    let rad_wm = (angle + r as f32).to_radians();
    let (cw, sw) = (rad_wm.cos(), rad_wm.sin());

    // 5. 视觉系斜向网格覆盖范围
    let diag = (vis_w.powi(2) + vis_h.powi(2)).sqrt() * COVERAGE_MULTIPLIER;
    let v_start = -diag - 400.0;
    let v_end = diag + 400.0;
    let v_count = ((v_end - v_start) / step_outer).ceil().max(0.0) as usize;
    let u_start = -diag - 400.0;
    let u_end = diag + 400.0;
    let u_count = ((u_end - u_start) / step_inner).ceil().max(0.0) as usize;

    let estimated = v_count.saturating_mul(u_count);
    if estimated > MAX_ALLOWED_WATERMARKS {
        return Err(format!("Too many watermarks to render: {}", estimated).into());
    }

    // 6. 一次性算出水印 4 角点的旋转结果 + AABB 边界。
    //    关键优化：cw/sw 是当页常量，4 个角点的旋转结果与 min/max 都只需算一次，
    //    内层循环里只剩 4 次加法（之前每锚点要 8 次乘法 + 8 次加法）。
    let corners_local = [
        (XOBJ_BBOX_LLX, XOBJ_BBOX_LLY),
        (XOBJ_BBOX_URX, XOBJ_BBOX_LLY),
        (XOBJ_BBOX_URX, XOBJ_BBOX_URY),
        (XOBJ_BBOX_LLX, XOBJ_BBOX_URY),
    ];
    let (mut min_dx, mut max_dx, mut min_dy, mut max_dy) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for &(qx, qy) in &corners_local {
        let dx = qx * cw - qy * sw;
        let dy = qx * sw + qy * cw;
        if dx < min_dx {
            min_dx = dx;
        }
        if dx > max_dx {
            max_dx = dx;
        }
        if dy < min_dy {
            min_dy = dy;
        }
        if dy > max_dy {
            max_dy = dy;
        }
    }

    // 7. 页面 P 系 AABB（MediaBox 外加膨胀余量）
    let page_min_x = mb_llx - VISIBILITY_MARGIN;
    let page_min_y = mb_lly - VISIBILITY_MARGIN;
    let page_max_x = mb_llx + w + VISIBILITY_MARGIN;
    let page_max_y = mb_lly + h + VISIBILITY_MARGIN;

    // 8. 锚点在 V 系的斜向分布：按视觉目标角 angle 旋转布点，密度均匀
    let rad_grid = angle.to_radians();
    let (cg, sg) = (rad_grid.cos(), rad_grid.sin());

    // 每锚点约 80 B；一次预留到位，避免循环里反复 grow + 拷贝。
    let prealloc = estimated
        .saturating_mul(80)
        .min(GRID_BYTES_PREALLOC_CAP)
        .max(256);
    let mut buf: Vec<u8> = Vec::with_capacity(prealloc);

    for vi in 0..=v_count {
        let v = v_start + (vi as f32) * step_outer;
        for ui in 0..=u_count {
            let u = u_start + (ui as f32) * step_inner;

            let vx = cx_v + u * cg - v * sg;
            let vy = cy_v + u * sg + v * cg;
            let (px, py) = v_to_p(vx, vy);

            // AABB 判定：4 个旋转后的角点平移到 (px,py) 后的边界
            let x_min = px + min_dx;
            let x_max = px + max_dx;
            let y_min = py + min_dy;
            let y_max = py + max_dy;
            if x_min > page_max_x
                || x_max < page_min_x
                || y_min > page_max_y
                || y_max < page_min_y
            {
                continue;
            }

            // cm 的 a b c d = 最终水印旋转 (angle + R) 的矩阵元素。
            // 手工拼 q / cm / Do / Q 四行，零额外堆分配。
            buf.extend_from_slice(b"q\n");
            push_pdf_number(&mut buf, cw);
            buf.push(b' ');
            push_pdf_number(&mut buf, sw);
            buf.push(b' ');
            push_pdf_number(&mut buf, -sw);
            buf.push(b' ');
            push_pdf_number(&mut buf, cw);
            buf.push(b' ');
            push_pdf_number(&mut buf, px);
            buf.push(b' ');
            push_pdf_number(&mut buf, py);
            buf.extend_from_slice(b" cm\n/");
            buf.extend_from_slice(x_name.as_bytes());
            buf.extend_from_slice(b" Do\nQ\n");
        }
    }

    Ok(buf)
}

/// 把一个有限 f32 写成 PDF 定点小数。
/// Rust 的 f32 Display 不会产生科学计数法（PDF 不接受 `1e-7` 这类写法）；
/// NaN / Infinity 已在 `build_watermark_grid_bytes` 入口拦截。
#[inline]
fn push_pdf_number(buf: &mut Vec<u8>, v: f32) {
    use std::io::Write;
    let _ = write!(buf, "{}", v);
}