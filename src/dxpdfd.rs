//! `dxpdfd` —— dxpdf + Skia 的 DOCX→PDF **常驻服务**。
//!
//! 两步式接口：上传 DOCX → 拿到下载地址 → 再按地址取 PDF。
//!
//! ```text
//! POST /convert          multipart/form-data，字段 file=<docx>（可选 image_dpi）
//!   → 200 {"id":"...","url":"http://host/download/<id>","filename":...,"pages":N,...}
//! GET  /download/<id>    → 200 application/pdf
//! GET  /healthz          → 200 {"status":"ok",...}
//! ```
//!
//! 转换管线：`docx_preprocess::patch_docx` → `dxpdf::docx::parse`
//! → `dxpdf::render::render_with_font_mgr`。结果只落在进程内存里（带 TTL），不写磁盘。
//!
//! 本 bin **自成一体**：只依赖 `Cargo.toml` 里的第三方 crate（axum / tokio / dxpdf /
//! skia-safe / lopdf / zip / serde_json），**不依赖本仓库的 `water_mark` 库**，
//! 与 `src/dxpdf2pdf.rs` 等其它 bin 没有任何代码耦合 —— 改这里不会影响别的 bin。
//! 其中的 DOCX 预处理（补全 `w:ilvl`）是从 `src/dxpdf2pdf.rs` 直接拷贝过来的。
//!
//! 配置全部走环境变量，见 [`Config::from_env`]。

use std::collections::HashMap;
use std::env;
use std::io::{Cursor, Read, Write};
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Semaphore;
use zip::write::ZipWriter;
use zip::ZipArchive;

// ============================================================================
// 配置
// ============================================================================

struct Config {
    addr: SocketAddr,
    max_body_bytes: usize,
    concurrency: usize,
    default_image_dpi: f32,
    ttl: Duration,
    max_results: usize,
    max_result_bytes: usize,
    /// 对外播报的地址前缀。留空则按请求的 `Host` 头推断（`http://<host>`）。
    /// 走了反向代理/HTTPS 时必须显式设置，否则返回的下载地址是错的。
    public_base: Option<String>,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let addr_raw = env::var("DXPDFD_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let addr: SocketAddr = addr_raw
            .parse()
            .map_err(|e| format!("DXPDFD_ADDR 不是合法的监听地址 {addr_raw:?}: {e}"))?;

        // 默认并发 = CPU 核数：render 是同步 CPU 密集型，开超过核数只会互相抢。
        let concurrency = env_usize("DXPDFD_CONCURRENCY")
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));

        Ok(Self {
            addr,
            max_body_bytes: env_usize("DXPDFD_MAX_BODY_MB").unwrap_or(64) * 1024 * 1024,
            concurrency,
            default_image_dpi: env_f32("DXPDFD_IMAGE_DPI").unwrap_or(dxpdf::DEFAULT_IMAGE_DPI),
            ttl: Duration::from_secs(env_usize("DXPDFD_TTL_SECS").unwrap_or(1800) as u64),
            max_results: env_usize("DXPDFD_MAX_RESULTS").unwrap_or(64),
            max_result_bytes: env_usize("DXPDFD_MAX_RESULT_MB").unwrap_or(256) * 1024 * 1024,
            public_base: env::var("DXPDFD_PUBLIC_BASE")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        })
    }
}

/// 读一个正整数环境变量。`0` 一律当作没设 —— 否则
/// `DXPDFD_CONCURRENCY=0` 会让每个请求都 503、`DXPDFD_MAX_BODY_MB=0`
/// 会让每个请求都 413，都是纯粹的配置陷阱。
fn env_usize(key: &str) -> Option<usize> {
    env::var(key).ok()?.trim().parse().ok().filter(|v| *v > 0)
}

fn env_f32(key: &str) -> Option<f32> {
    env::var(key).ok()?.trim().parse().ok()
}

// ============================================================================
// 状态
// ============================================================================

/// 一份转换好的结果，等待被 `/download/<id>` 取走。
struct Stored {
    /// 用 `Bytes` 而不是 `Vec<u8>`：下载时 clone 只加一次引用计数，不复制几 MB 的 PDF。
    bytes: Bytes,
    filename: String,
    pages: usize,
    created: Instant,
}

#[derive(Default)]
struct Store {
    map: HashMap<String, Stored>,
    /// 当前持有的总字节数，用于淘汰判断，避免每次都遍历求和。
    bytes: usize,
}

#[derive(Default)]
struct Counters {
    ok: AtomicU64,
    /// 所有返回 4xx/5xx 的请求（含 multipart 阶段就失败的）。
    failed: AtomicU64,
    /// 因并发已满被 503 挡掉的请求。这是背压，不是故障，所以单独计。
    rejected: AtomicU64,
    inflight: AtomicU64,
}

struct AppState {
    cfg: Config,
    /// `RenderOptions` 是 `Copy`（只有一个 f32），可以自由复制分发给各请求。
    opts: dxpdf::RenderOptions,
    /// 并发闸门。拿不到许可就立刻 503，不做无界排队。
    permits: Semaphore,
    store: Mutex<Store>,
    counters: Counters,
    started: Instant,
}

// ============================================================================
// DOCX 预处理：补全 dxpdf serde schema 要求的 w:ilvl
// ============================================================================
//
// 以下三个函数从 `src/dxpdf2pdf.rs` 直接拷贝过来（本 bin 不依赖 water_mark 库）。
//
// `dxpdf` 的 serde schema 把 `@ilvl` 当必填字段，但很多非 Word 生成的 DOCX
// 不带这个属性，解析会直接失败。要补两处：
//
// 1. `word/numbering.xml` — `<w:lvl>` 元素需要 `w:ilvl` 属性
// 2. `word/document.xml`、`word/header*.xml`、`word/footer*.xml` —
//    `<w:numPr>` 块需要 `<w:ilvl w:val="0"/>` 子元素

/// 预处理三个阶段的耗时（服务里用不上，保留结构便于排查慢请求）。
#[derive(Default, Debug, Clone, Copy)]
struct PreprocessTimes {
    unzip: Duration,
    patch_xml: Duration,
    rezip: Duration,
}

/// 预处理 DOCX：解压 → 补全 `ilvl` → 重新打包。
fn patch_docx(docx_bytes: &[u8]) -> Result<(Vec<u8>, PreprocessTimes), Box<dyn std::error::Error>> {
    let mut times = PreprocessTimes::default();

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
            && (name.contains("document") || name.contains("header") || name.contains("footer"))
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

    Ok((output.into_inner(), times))
}

/// 在 `<w:lvl>` 标签中添加缺失的 `w:ilvl` 属性。
///
/// `<w:lvl>` → `<w:lvl w:ilvl="0">`
/// 已经有 `w:ilvl` 属性的标签不变。
fn patch_lvl_ilvl(xml: &str) -> String {
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

// ============================================================================
// 转换（在 spawn_blocking 线程上跑）
// ============================================================================

thread_local! {
    /// `skia_safe::FontMgr` 是 `RCHandle<SkFontMgr>` 包着一个 `NonNull`，而 skia-safe 的
    /// `unsafe_send_sync!` 只给了 `Typeface`、没给 `FontMgr` —— 所以它是 `!Send + !Sync`，
    /// 进不了 `static`/`OnceLock`，也不能跨线程共享，只能每线程一个。
    ///
    /// 复用它的收益有限（dxpdf 自测 catalog 构建仅 0.05ms；真正贵的 Tier-2 深索引是
    /// per-`FaceCatalog` 的 `OnceCell`，每请求必重建且没有注入的缝），但总比每次重建
    /// SkFontMgr 强 —— 这正是 `render_with_font_mgr` 存在的理由。
    static FONT_MGR: skia_safe::FontMgr = skia_safe::FontMgr::new();
}

fn with_font_mgr<T>(f: impl FnOnce(&skia_safe::FontMgr) -> T) -> T {
    FONT_MGR.with(f)
}

enum ConvError {
    /// DOCX 预处理失败（不是合法 zip、条目损坏等）。
    Preprocess(String),
    /// dxpdf 解析 DOCX 失败。
    Parse(String),
    /// dxpdf 渲染失败（如 `NoFontsAvailable`）。
    Render(String),
    /// 渲染产物的回读校验没通过。
    Output(String),
    /// catch_unwind 兜下来的 panic。
    Panic(String),
}

impl ConvError {
    fn stage(&self) -> &'static str {
        match self {
            ConvError::Preprocess(_) => "preprocess",
            ConvError::Parse(_) => "parse",
            ConvError::Render(_) => "render",
            ConvError::Output(_) => "output",
            ConvError::Panic(_) => "panic",
        }
    }

    fn message(&self) -> &str {
        match self {
            ConvError::Preprocess(m)
            | ConvError::Parse(m)
            | ConvError::Render(m)
            | ConvError::Output(m)
            | ConvError::Panic(m) => m,
        }
    }

    /// 文档本身有问题 → 422；服务端/环境问题 → 500。
    fn status(&self) -> StatusCode {
        match self {
            ConvError::Preprocess(_) | ConvError::Parse(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ConvError::Render(_) | ConvError::Output(_) | ConvError::Panic(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

/// dxpdf 的 `render` 没有任何 panic 保护（只有它的 C ABI `capi.rs` 里包了
/// `catch_unwind`），`FontMgr::new()` 内部也会 unwrap。常驻进程里一个畸形 DOCX
/// 不能把整个服务带下去，所以这里必须兜住。
fn convert_guarded(docx: Bytes, opts: dxpdf::RenderOptions) -> Result<(Bytes, usize), ConvError> {
    match catch_unwind(AssertUnwindSafe(move || convert_inner(docx, opts))) {
        Ok(r) => r,
        Err(p) => Err(ConvError::Panic(panic_message(&p))),
    }
}

fn convert_inner(docx: Bytes, opts: dxpdf::RenderOptions) -> Result<(Bytes, usize), ConvError> {
    let (patched, _times) = patch_docx(&docx).map_err(|e| ConvError::Preprocess(e.to_string()))?;

    let document = dxpdf::docx::parse(&patched).map_err(|e| ConvError::Parse(e.to_string()))?;

    // `render` 按值消费 Document（内部的 resolve 会把它整个吃掉），所以每个请求
    // 都必须重新 parse —— 没有「解析一次渲染多次」的路子。
    let pdf = with_font_mgr(|mgr| dxpdf::render::render_with_font_mgr(document, mgr, &opts))
        .map_err(|e| ConvError::Render(e.to_string()))?;

    // 回读校验：本仓库踩过「0 页却报成功、写出坏文件」的坑，这里必须挡住。
    let pages = probe_pdf(&pdf).map_err(ConvError::Output)?;
    if pages == 0 {
        return Err(ConvError::Output("渲染结果页数为 0".to_string()));
    }

    Ok((Bytes::from(pdf), pages))
}

/// 用 `lopdf` 回读输出：既校验是不是合法 PDF，又能拿到真实页数。
fn probe_pdf(bytes: &[u8]) -> Result<usize, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("输出缺少 %PDF- 魔数".to_string());
    }
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| e.to_string())?;
    Ok(doc.get_pages().len())
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知 panic".to_string()
    }
}

// ============================================================================
// 结果存储：TTL + 容量淘汰
// ============================================================================

fn store_put(state: &AppState, id: &str, item: Stored) {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    store.bytes = store.bytes.saturating_add(item.bytes.len());
    store.map.insert(id.to_string(), item);
    evict(
        &mut store,
        state.cfg.ttl,
        state.cfg.max_results,
        state.cfg.max_result_bytes,
    );
}

/// 先清 TTL 过期的，再按条数/总字节淘汰最旧的。
fn evict(store: &mut Store, ttl: Duration, max_results: usize, max_bytes: usize) {
    let now = Instant::now();
    let expired: Vec<String> = store
        .map
        .iter()
        .filter(|(_, v)| now.duration_since(v.created) > ttl)
        .map(|(k, _)| k.clone())
        .collect();
    for k in expired {
        if let Some(v) = store.map.remove(&k) {
            store.bytes = store.bytes.saturating_sub(v.bytes.len());
        }
    }

    while store.map.len() > max_results || store.bytes > max_bytes {
        let oldest = store
            .map
            .iter()
            .min_by_key(|(_, v)| v.created)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                if let Some(v) = store.map.remove(&k) {
                    store.bytes = store.bytes.saturating_sub(v.bytes.len());
                }
            }
            None => break,
        }
    }
}

/// 生成下载 ID。
///
/// 刻意不引入 `rand`/`uuid`：用 std 的 `RandomState`（种子来自操作系统，每进程随机）
/// 做一次 SipHash，得到不可预测的 token。默认只听回环，这足够；若要对外暴露，
/// 应换成真正的 CSPRNG。
fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(nanos);
    h.write_u64(seq);
    format!("{:016x}{:08x}", h.finish(), std::process::id())
}

// ============================================================================
// HTTP 处理
// ============================================================================

async fn handle_healthz(State(state): State<Arc<AppState>>) -> Response {
    let (cached, cached_bytes) = {
        let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        (store.map.len(), store.bytes)
    };

    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "uptime_secs": state.started.elapsed().as_secs(),
            "inflight": state.counters.inflight.load(Ordering::Relaxed),
            "concurrency": state.cfg.concurrency,
            "available_permits": state.permits.available_permits(),
            "ok": state.counters.ok.load(Ordering::Relaxed),
            "failed": state.counters.failed.load(Ordering::Relaxed),
            "rejected": state.counters.rejected.load(Ordering::Relaxed),
            "cached_results": cached,
            "cached_bytes": cached_bytes,
        })),
    )
        .into_response()
}

async fn handle_convert(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let t0 = Instant::now();

    let mut file: Option<(String, Bytes)> = None;
    let mut image_dpi: Option<f32> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            // 请求体超过 DefaultBodyLimit 时，axum 在这里就以 413 报出来
            Err(e) => {
                state.counters.failed.fetch_add(1, Ordering::Relaxed);
                return error_json(e.status(), "multipart", &format!("multipart 解析失败: {e}"));
            }
        };

        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let fname = field.file_name().unwrap_or("upload.docx").to_string();
                match field.bytes().await {
                    Ok(b) => file = Some((fname, b)),
                    Err(e) => {
                        state.counters.failed.fetch_add(1, Ordering::Relaxed);
                        return error_json(
                            e.status(),
                            "multipart",
                            &format!("读取 file 字段失败: {e}"),
                        );
                    }
                }
            }
            "image_dpi" => {
                if let Ok(txt) = field.text().await {
                    if let Ok(v) = txt.trim().parse::<f32>() {
                        image_dpi = Some(v);
                    }
                }
            }
            // 未知字段读掉即可，避免 Multipart 因为没读完而报错
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let Some((orig_name, docx)) = file else {
        state.counters.failed.fetch_add(1, Ordering::Relaxed);
        return error_json(
            StatusCode::BAD_REQUEST,
            "request",
            "缺少 file 字段（应以 multipart/form-data 上传 DOCX 文件）",
        );
    };
    if docx.is_empty() {
        state.counters.failed.fetch_add(1, Ordering::Relaxed);
        return error_json(StatusCode::BAD_REQUEST, "request", "file 字段为空");
    }
    let docx_len = docx.len();

    // 不做无界排队：拿不到许可立刻 503，避免请求体在内存里越堆越多。
    // （dxpdf 的 render 没有取消机制，就算设了超时也停不下后台那次渲染，
    //   所以这里靠限流控制水位，而不是靠超时。）
    let permit = match state.permits.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            state.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "并发已满，请稍后重试",
            );
        }
    };

    let opts = match image_dpi {
        Some(d) => state.opts.with_image_dpi(d),
        None => state.opts,
    };

    state.counters.inflight.fetch_add(1, Ordering::Relaxed);
    let join = tokio::task::spawn_blocking(move || convert_guarded(docx, opts)).await;
    state.counters.inflight.fetch_sub(1, Ordering::Relaxed);
    drop(permit);

    let (pdf, pages) = match join {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            state.counters.failed.fetch_add(1, Ordering::Relaxed);
            let ms = t0.elapsed().as_millis();
            log_line(&format!(
                "POST /convert {} stage={} {} ({} 字节入, {} ms)",
                e.status().as_u16(),
                e.stage(),
                e.message(),
                docx_len,
                ms
            ));
            return error_json(e.status(), e.stage(), e.message());
        }
        // spawn_blocking 的 JoinError：catch_unwind 没兜住的 panic（例如 abort 型 panic）
        Err(je) => {
            state.counters.failed.fetch_add(1, Ordering::Relaxed);
            log_line(&format!("POST /convert 500 任务异常终止: {je}"));
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "join",
                "转换任务异常终止",
            );
        }
    };

    let id = new_id();
    let out_name = pdf_filename(&orig_name);
    let size = pdf.len();
    store_put(
        &state,
        &id,
        Stored {
            bytes: pdf,
            filename: out_name.clone(),
            pages,
            created: Instant::now(),
        },
    );

    let ms = t0.elapsed().as_millis();
    state.counters.ok.fetch_add(1, Ordering::Relaxed);
    log_line(&format!(
        "POST /convert 200 {} 字节 DOCX → {} 页 {} 字节 PDF, {} ms, id={id}",
        docx_len, pages, size, ms
    ));

    let url = format!("{}/download/{}", base_url(&state, &headers), id);
    (
        StatusCode::OK,
        Json(json!({
            "id": id,
            "url": url,
            "filename": out_name,
            "pages": pages,
            "size": size,
            "ms": ms,
            "expires_in_secs": state.cfg.ttl.as_secs(),
        })),
    )
        .into_response()
}

async fn handle_download(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    // 只在锁内取出引用计数句柄，出锁后再构造响应，避免下载期间一直占着锁。
    let found = {
        let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store
            .map
            .get(&id)
            .map(|s| (s.bytes.clone(), s.filename.clone(), s.pages))
    };

    let Some((bytes, filename, pages)) = found else {
        return error_json(
            StatusCode::NOT_FOUND,
            "download",
            "下载地址不存在或已过期，请重新上传转换",
        );
    };

    let size = bytes.len();
    let mut resp = (StatusCode::OK, bytes).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/pdf"),
    );
    h.insert(header::CONTENT_DISPOSITION, content_disposition(&filename));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&pages.to_string()) {
        h.insert("x-page-count", v);
    }
    log_line(&format!("GET /download/{id} 200 {size} 字节"));
    resp
}

fn error_json(status: StatusCode, stage: &str, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "stage": stage }))).into_response()
}

/// 按请求的 `Host` 头（或 `DXPDFD_PUBLIC_BASE`）拼出对外的下载地址。
fn base_url(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(b) = &state.cfg.public_base {
        return b.trim_end_matches('/').to_string();
    }
    if let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        return format!("http://{h}");
    }
    format!("http://{}", state.cfg.addr)
}

/// `报告.docx` → `报告.pdf`；同时去掉路径分隔符，避免把目录写进响应头。
fn pdf_filename(original: &str) -> String {
    let base = original.rsplit(['/', '\\']).next().unwrap_or(original);
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    let stem = if stem.is_empty() { "output" } else { stem };
    format!("{stem}.pdf")
}

/// `Content-Disposition`：中文文件名不能直接进 header（要求可见 ASCII），
/// 所以按 RFC 5987 额外给一个 `filename*=UTF-8''...` 的百分号编码形式。
fn content_disposition(filename: &str) -> HeaderValue {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                if c == '"' || c == '\\' { '_' } else { c }
            } else {
                '_'
            }
        })
        .collect();

    let mut encoded = String::new();
    for b in filename.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*b as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }

    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

// ============================================================================
// 日志与退出
// ============================================================================

/// 单行日志：`[HH:MM:SSZ] ...`（UTC）。
///
/// 刻意不引入日志框架：服务只需要每请求一行访问日志。想排查 dxpdf 内部阶段时，
/// 临时加回 env_logger 并设 `RUST_LOG=debug` 即可看到它的 `registry:` 等计时。
fn log_line(msg: &str) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    eprintln!("[{h:02}:{m:02}:{s:02}Z] {msg}");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                log_line(&format!("无法注册 SIGTERM 处理: {e}"));
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => log_line("收到 Ctrl-C，开始优雅退出…"),
        _ = terminate => log_line("收到 SIGTERM，开始优雅退出…"),
    }
}

#[tokio::main]
async fn main() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置错误: {e}");
            std::process::exit(2);
        }
    };
    let addr = cfg.addr;

    if !addr.ip().is_loopback() {
        log_line(
            "警告：监听地址不是回环地址，而本服务既无鉴权也无 TLS，请确认前方有反向代理/防火墙",
        );
    }

    let state = Arc::new(AppState {
        opts: dxpdf::RenderOptions::default().with_image_dpi(cfg.default_image_dpi),
        permits: Semaphore::new(cfg.concurrency),
        store: Mutex::new(Store::default()),
        counters: Counters::default(),
        started: Instant::now(),
        cfg,
    });

    let app = Router::new()
        .route("/convert", post(handle_convert))
        .route("/download/{id}", get(handle_download))
        .route("/healthz", get(handle_healthz))
        .layer(DefaultBodyLimit::max(state.cfg.max_body_bytes))
        .with_state(state.clone());

    // 后台清理过期结果，免得只靠「下次上传时顺手清」
    tokio::spawn({
        let st = state.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let mut store = st.store.lock().unwrap_or_else(|e| e.into_inner());
                evict(
                    &mut store,
                    st.cfg.ttl,
                    st.cfg.max_results,
                    st.cfg.max_result_bytes,
                );
            }
        }
    });

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 {addr} 失败: {e}");
            std::process::exit(2);
        }
    };
    let local = listener.local_addr().unwrap_or(addr);

    log_line(&format!(
        "dxpdfd 已启动：http://{local}  并发上限={}  body 上限={}MB  结果 TTL={}s",
        state.cfg.concurrency,
        state.cfg.max_body_bytes / 1024 / 1024,
        state.cfg.ttl.as_secs()
    ));
    log_line("  POST /convert        上传 DOCX，返回 PDF 下载地址");
    log_line("  GET  /download/{id}  下载转换好的 PDF");
    log_line("  GET  /healthz        健康检查");

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("服务异常退出: {e}");
        std::process::exit(1);
    }

    log_line("已退出");
}
