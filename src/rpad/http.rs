//! HTTP 层：与 Python `app.py` + `rpa/views/api.py` 严格同构。
//!
//! | Python                                    | 这里                                          |
//! |-------------------------------------------|-----------------------------------------------|
//! | `GET /` → "health"                        | 同                                            |
//! | `GET /api/v1/rpa/parse_file?...`          | 同（参数名一个不改）                          |
//! | 参数任缺 → 409 `{"code":-1,"msg":"缺少参数"}` | 同                                        |
//! | Redis XLock 抢不到 → 409 `{"code":-1,"msg":"文件生成失败"}` | 进程内信号量等价（默认 1 = 串行） |
//! | 任何失败 → 409 `{"code":-1,"msg":"文件生成失败"}` | 同（失败原因只进日志，不给调用方）       |
//! | 成功 → `application/pdf` 附件流            | 同（文件名规则一致）                          |
//! | 404 → "no router found"                   | 同                                            |
//!
//! 额外（不影响兼容）：`GET /healthz` 带运行计数，便于探活和排障。

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::merge;
use crate::oss::{OssClient, Scheme};
use crate::pdf;

/// Python `api.py` 里的 `SCHEME = ["mfront", "crm", "crm_v2", "hhcrm", "other"]`。

pub struct AppState {
    pub cfg: Config,
    /// 并发闸门。默认 1 个许可 = Python 那把全局锁（完全串行）。
    permits: Arc<Semaphore>,
    opts: dxpdf::RenderOptions,
    counters: Counters,
    started: Instant,
}

#[derive(Default)]
pub struct Counters {
    pub total: AtomicU64,
    pub ok: AtomicU64,
    pub failed: AtomicU64,
    /// 等闸门超时（对应 Python「抢锁失败」）。
    pub rejected: AtomicU64,
}

/// 一次请求各阶段的耗时（毫秒）。
///
/// 只在请求结束那一行汇总打印 —— 阶段日志本身已经各打一条，这里是为了
/// 「一眼看出慢在哪」：排队久 = 并发不够；下载久 = S3/网络；转 PDF 久 = 渲染。
#[derive(Default, Clone, Copy)]
pub struct Stages {
    /// 等并发许可（等价于 Python 抢全局锁）。
    pub permit_ms: u64,
    /// S3 下载两份 DOCX + 客户端解密。
    pub fetch_ms: u64,
    /// 填占位符 + 拼接。
    pub merge_ms: u64,
    /// DOCX → PDF 渲染。
    pub pdf_ms: u64,
}

impl Stages {
    fn summary(&self) -> String {
        format!(
            "排队{}ms / 下载{}ms / 合并{}ms / 转PDF{}ms",
            self.permit_ms, self.fetch_ms, self.merge_ms, self.pdf_ms
        )
    }
}

fn ms(d: std::time::Duration) -> u64 {
    d.as_millis() as u64
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let opts = dxpdf::RenderOptions::default().with_image_dpi(dxpdf::DEFAULT_IMAGE_DPI);
        Self {
            permits: Arc::new(Semaphore::new(cfg.server.concurrency)),
            cfg,
            opts,
            counters: Counters::default(),
            started: Instant::now(),
        }
    }

    pub fn router(state: Arc<Self>) -> Router {
        Router::new()
            .route("/", get(index))
            .route("/healthz", get(healthz))
            .route("/api/v1/rpa/parse_file", get(parse_file))
            .fallback(not_found)
            .with_state(state)
    }
}

// ---------------------------------------------------------------- 响应形状

/// Python：`Response(response=json.dumps({"code": -1, "msg": ...}), status=409)`。
fn err_409(msg: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "code": -1, "msg": msg })),
    )
        .into_response()
}

// ---------------------------------------------------------------- 路由

/// Python `app.py` 的 `GET /`。
async fn index() -> impl IntoResponse {
    (StatusCode::OK, "health")
}

/// 额外探活端点（Python 没有的新增项，不影响兼容）。
async fn healthz(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime = st.started.elapsed().as_secs();
    Json(json!({
        "status": "ok",
        "uptime_secs": uptime,
        "concurrency": st.cfg.server.concurrency,
        "counters": {
            "total": st.counters.total.load(Ordering::Relaxed),
            "ok": st.counters.ok.load(Ordering::Relaxed),
            "failed": st.counters.failed.load(Ordering::Relaxed),
            "rejected": st.counters.rejected.load(Ordering::Relaxed),
        },
        "schemes_configured": st.cfg.schemes.keys().cloned().collect::<Vec<_>>(),
    }))
}

/// Python `app.py` 的 404 处理器。
async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "no router found")
}

/// `GET /api/v1/rpa/parse_file`
///
/// 参数（与 Python 完全一致）：
/// `src_file_key` / `template_file_key` / `fund_cnname` / `letters_date` / `scheme`
async fn parse_file(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    st.counters.total.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();

    let get_param = |k: &str| q.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());

    let (Some(src_file_key), Some(template_file_key), Some(fund_cnname), Some(letters_date), Some(scheme)) = (
        get_param("src_file_key"),
        get_param("template_file_key"),
        get_param("fund_cnname"),
        get_param("letters_date"),
        get_param("scheme"),
    ) else {
        crate::log_line("WARN", "获取到的参数为空，直接返回（缺少参数）");
        return err_409("缺少参数");
    };

    // Python：scheme 不在 SCHEME 里 → process_work 返回 -1 → 统一 409
    let Some(scheme_kind) = Scheme::parse(scheme) else {
        crate::log_line(
            "WARN",
            &format!("{scheme} 不符合要求（不在 SCHEME 列表里）"),
        );
        return err_409("文件生成失败");
    };

    crate::log_line(
        "INFO",
        &format!(
            "收到请求: scheme={scheme} src={src_file_key} template={template_file_key} fund={fund_cnname} date={letters_date}"
        ),
    );

    // Python：XLock.acquire()（acquire_timeout=30s）抢不到 → 统一 409
    let permit_started = Instant::now();
    let _permit =
        match tokio::time::timeout(st.cfg.acquire_timeout(), st.permits.clone().acquire_owned()).await {
            Ok(Ok(p)) => p,
            Ok(Err(_)) => {
                // 信号量被关闭（本服务不会发生）
                st.counters.rejected.fetch_add(1, Ordering::Relaxed);
                crate::log_line("WARN", "并发闸门已关闭");
                return err_409("文件生成失败");
            }
            Err(_) => {
                st.counters.rejected.fetch_add(1, Ordering::Relaxed);
                crate::log_line("WARN", "未获取到并发许可（等价于 Python 抢锁超时），请稍后再试");
                return err_409("文件生成失败");
            }
        };

    let permit_ms = ms(permit_started.elapsed());
    if permit_ms > 0 {
        crate::log_line("INFO", &format!("拿到并发许可: 等待 {permit_ms}ms"));
    }

    // 单请求整体超时（默认 0 = 不限，与 Python 一致）。超时后后台转换线程
    // 会继续跑完（只是不再等它），许可已随 await 结束归还——等价于 Python
    // 侧 gunicorn worker 超时后换新 worker 的行为。
    let work = process_work(
        &st,
        scheme_kind,
        src_file_key,
        template_file_key,
        fund_cnname,
        letters_date,
        permit_ms,
    );
    let out = match st.cfg.request_timeout() {
        Some(d) => match tokio::time::timeout(d, work).await {
            Ok(r) => r,
            Err(_) => Err(format!("单请求超时（{}ms），放弃等待", d.as_millis())),
        },
        None => work.await,
    };

    match out {
        Ok((pdf_bytes, filename, pages, stages)) => {
            st.counters.ok.fetch_add(1, Ordering::Relaxed);
            crate::log_line(
                "INFO",
                &format!(
                    "生成文件成功: {filename} pages={pages} bytes={} cost={:.1}s（{}）",
                    pdf_bytes.len(),
                    started.elapsed().as_secs_f32(),
                    stages.summary()
                ),
            );
            // Python：flask.send_file(..., mimetype='application/pdf',
            //                         as_attachment=True, download_name=basename)
            // axum 对固定大小的 Body 会自动带上 Content-Length。
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/pdf")
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", sanitize_filename(&filename)),
                )
                .body(Body::from(pdf_bytes))
                .expect("构造 PDF 响应不可能失败")
        }
        Err(reason) => {
            st.counters.failed.fetch_add(1, Ordering::Relaxed);
            crate::log_line(
                "ERROR",
                &format!(
                    "生成文件失败:返回409；原因: {reason}；cost={:.1}s",
                    started.elapsed().as_secs_f32()
                ),
            );
            err_409("文件生成失败")
        }
    }
}

// ---------------------------------------------------------------- 业务主流程

/// Python `process_work` 的等价物（拿到并发许可之后才进来）。
///
/// 与 Python 的差异只有两处，都是「等价替换」：
/// 1. S3 → 本地临时文件 → 再读回来，改成全程内存（crm_v2 走 DLL 的分支除外，
///    那条路必须落盘，行为与 Python 一致）；
/// 2. Word COM（InsertBreak + InsertFile + SaveAs PDF）换成
///    `merge::build_merged_docx` + `dxpdf` 渲染。
async fn process_work(
    st: &AppState,
    scheme: Scheme,
    src_file_key: &str,
    template_file_key: &str,
    fund_cnname: &str,
    letters_date: &str,
    // 等并发许可的耗时（外层 parse_file 里打的点，带进来只为汇总）。
    permit_ms: u64,
) -> Result<(Vec<u8>, String, usize, Stages), String> {
    let mut stages = Stages {
        permit_ms,
        ..Default::default()
    };

    // ---- 1. 从 S3 取两份文件（含客户端解密） ----
    let scheme_cfg = st.cfg.scheme_config(scheme.as_str())?;
    let oss = OssClient::new(scheme, scheme_cfg, &st.cfg.oss, st.cfg.tmp_dir())
        .map_err(|e| e.to_string())?;

    let fetch_started = Instant::now();
    crate::log_line("INFO", &format!("从s3中获取src文件key={src_file_key}"));
    crate::log_line("INFO", &format!("从s3中获取template文件key={template_file_key}"));

    // 两份文件互不依赖，**并发**拉取，省掉一整次 RTT。
    // 例外：crm_v2 走 Go DLL 时是「DLL 自己下载 + 解密 + 落盘」，DLL 内部有进程级
    // 状态，两个调用并发不安全 —— 这条分支保持串行（与 Python 完全一致）。
    let (src_bytes, template_bytes) = if scheme == Scheme::CrmV2 {
        (
            oss.get(src_file_key).await.map_err(|e| e.to_string())?,
            oss.get(template_file_key).await.map_err(|e| e.to_string())?,
        )
    } else {
        tokio::try_join!(oss.get(src_file_key), oss.get(template_file_key))
            .map_err(|e| e.to_string())?
    };
    stages.fetch_ms = ms(fetch_started.elapsed());
    crate::log_line(
        "INFO",
        &format!(
            "s3 下载完成: src={}B template={}B cost={}ms",
            src_bytes.len(),
            template_bytes.len(),
            stages.fetch_ms
        ),
    );

    // ---- 落盘策略 ----
    // - 显式配了 `RPAD_DUMP_DIR`：src / template / merged 三份都存，成功失败都存；
    // - 没配：**只在失败时**兜底存（目录 `<tmp>/rpad_dump`），这样生产环境平时
    //   不产生文件，出问题时又一定有材料可查 —— 见 [`dump_on_failure`]。
    //
    // 两份输入必须在合并**之前**就留好：合并一失败，后面就再也读不到原始输入了。
    let dump_dir = st.cfg.dump_dir();
    let fallback: Option<PathBuf> = if dump_dir.is_none() {
        Some(st.cfg.tmp_dir().join("rpad_dump"))
    } else {
        None
    };
    // Arc 包一层：闭包拿去合并，失败兜底还要用同一份字节（不额外拷贝几百 KB）。
    let src_bytes = Arc::new(src_bytes);
    let template_bytes = Arc::new(template_bytes);
    if let Some(dir) = &dump_dir {
        dump_file(dir, "src", src_file_key, "docx", src_bytes.as_slice());
        dump_file(dir, "template", template_file_key, "docx", template_bytes.as_slice());
    }

    // ---- 2. 填占位符 + 拼接 + 转 PDF（CPU 密集，放阻塞线程） ----
    let fund = fund_cnname.to_string();
    let date = letters_date.to_string();
    let strict = st.cfg.strict_placeholders();
    let opts = st.opts;
    let dump_fb = fallback.clone();
    let src_for_block = src_bytes.clone();
    let tpl_for_block = template_bytes.clone();
    // 合并件 / PDF 的落盘文件名要用 src 的 key：这样一次请求产出的四份材料
    // （src、template、merged、pdf）时间戳相同、key 相同，按名字排序就在一起。
    let src_key_owned = src_file_key.to_string();

    let merged_and_pdf = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, usize, usize, u64, u64), String> {
        let merge_started = Instant::now();
        // `strict = false`：与 Python `docxtpl` 对齐 —— 未知占位符渲染成空串而不是报错。
        let (merged, stats) =
            merge::build_merged_docx(tpl_for_block.as_slice(), src_for_block.as_slice(), &fund, &date, strict)
                .map_err(|e| format!("合并失败: {e}"))?;
        let merge_ms = ms(merge_started.elapsed());
        crate::log_line(
            "INFO",
            &format!(
                "合并完成: replaced={} backfilled={} media={} notes={} renumbered={} docx={}B cost={}ms",
                stats.replaced, stats.backfilled, stats.media, stats.notes,
                stats.renumbered, merged.len(), merge_ms
            ),
        );
        // 这些占位符在 Python 那边会被 Jinja2 静默渲染成空串；这里保留该行为，
        // 但必须记进日志，避免「少填了变量却无人知晓」。
        for u in &stats.unknowns {
            crate::log_line("WARN", &format!("占位符无对应值，已渲染为空: {u}"));
        }

        if let Some(dir) = &dump_dir {
            dump_file(dir, "merged", &src_key_owned, "docx", &merged);
        }

        let pdf_started = Instant::now();
        // 转换失败时合并件已经生成 —— 它是唯一的排查材料，没开 dump_dir 也要留下来。
        let (pdf, pages, breakdown) = pdf::docx_to_pdf(&merged, &opts).map_err(|e| {
            match &dump_fb {
                Some(dir) => match dump_file(dir, "merged", &src_key_owned, "docx", &merged) {
                    Some(p) => format!("{e}；合并件已保存: {}", p.display()),
                    None => e.to_string(),
                },
                None => e.to_string(),
            }
        })?;
        let pdf_ms = ms(pdf_started.elapsed());
        crate::log_line(
            "INFO",
            &format!(
                "转 PDF 完成: pages={pages} pdf={}B docx={}B cost={}ms（{breakdown}）",
                pdf.len(),
                merged.len(),
                pdf_ms
            ),
        );
        // PDF 也留一份：合并件和 PDF 对着看，才能判断「docx 的版式有没有被
        // 转换引擎改坏」——只留 docx 或只留 PDF 都看不出差异出在哪一步。
        if let Some(dir) = &dump_dir {
            dump_file(dir, "pdf", &src_key_owned, "pdf", &pdf);
        }
        Ok((pdf, pages, merged.len(), merge_ms, pdf_ms))
    })
    .await;

    // 失败（含 panic）兜底：把两份输入落盘，路径直接附在错误原因后面 ——
    // 线上报错往往只有一行原因，没有输入材料就只能靠猜。
    let merged_and_pdf = match merged_and_pdf {
        Ok(Ok(v)) => v,
        Ok(Err(reason)) => {
            return Err(dump_on_failure(
                fallback.as_deref(),
                src_file_key,
                src_bytes.as_slice(),
                template_file_key,
                template_bytes.as_slice(),
                &reason,
            ))
        }
        Err(panic) => {
            return Err(dump_on_failure(
                fallback.as_deref(),
                src_file_key,
                src_bytes.as_slice(),
                template_file_key,
                template_bytes.as_slice(),
                &format!("转换任务 panic: {panic}"),
            ))
        }
    };

    let (pdf_bytes, pages, merged_len, merge_ms, pdf_ms) = merged_and_pdf;
    stages.merge_ms = merge_ms;
    stages.pdf_ms = pdf_ms;

    // ---- 3. 下载文件名（与 Python 的规则一致） ----
    // Python: out_pdf_file = .../template-{key去掉首个.之后}-{uuid.hex}.pdf，
    //         send_file 用 basename() —— key 带目录前缀时前缀会被 basename 吃掉。
    let stem = template_file_key.split('.').next().unwrap_or(template_file_key);
    let raw_name = format!("template-{}-{}.pdf", stem, rand_hex(32));
    let filename = basename(&raw_name);

    // 文件名和字节数在「转 PDF 完成」「生成文件成功」两行里都已有，这里不重复打。
    let _ = merged_len;
    Ok((pdf_bytes, filename, pages, stages))
}

// ---------------------------------------------------------------- 小工具

/// 把一份中间材料写到落盘目录，返回写入的完整路径。写失败只告警，不影响主流程。
///
/// 文件名形如 `20260919-052000-a1b2c3-template-glv_template2.docx`：带时间前缀，
/// 同一次请求的四份材料（src / template / merged / pdf）时间戳相同，按名字排序
/// 就在一起，便于人工检查。
fn dump_file(dir: &Path, role: &str, key: &str, ext: &str, bytes: &[u8]) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        crate::log_line("WARN", &format!("创建落盘目录 {} 失败: {e}", dir.display()));
        return None;
    }
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' })
        .collect();
    let safe = if safe.is_empty() {
        role.to_string()
    } else {
        safe
    };
    // key 自己带了扩展名就别再加一个（src / template 的 key 通常带 .docx）。
    let dot_ext = format!(".{ext}");
    let suffix = if safe.to_ascii_lowercase().ends_with(&dot_ext) {
        ""
    } else {
        dot_ext.as_str()
    };
    let name = format!(
        "{}-{}-{}-{}{}",
        crate::utc_stamp(),
        rand_hex(6),
        role,
        safe,
        suffix
    );
    let path = dir.join(name);
    match std::fs::write(&path, bytes) {
        Ok(()) => {
            crate::log_line("INFO", &format!("已保存 {role}: {}", path.display()));
            Some(path)
        }
        Err(e) => {
            crate::log_line("WARN", &format!("落盘 {role} 失败: {e}"));
            None
        }
    }
}

/// 请求失败时的兜底：把两份输入落到 `dir`，把路径附在错误原因后面。
///
/// 这是「凡是失败，必有两份输入可查」的保证 —— 没有显式配 `RPAD_DUMP_DIR`
/// 时尤其重要，否则线上只能看到一行原因、材料全丢。
fn dump_on_failure(
    dir: Option<&Path>,
    src_key: &str,
    src: &[u8],
    tpl_key: &str,
    tpl: &[u8],
    reason: &str,
) -> String {
    let Some(dir) = dir else {
        return reason.to_string();
    };
    let mut saved = Vec::new();
    for (role, key, bytes) in [("src", src_key, src), ("template", tpl_key, tpl)] {
        if let Some(p) = dump_file(dir, role, key, "docx", bytes) {
            saved.push(format!("{role} 已保存: {}", p.display()));
        }
    }
    if saved.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}；{}", saved.join("，"))
    }
}

/// 取路径最后一个分量（同时认 `/` 和 `\`，对齐 Windows 上 `os.path.basename`）。
fn basename(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_string()
}

/// Content-Disposition 里的文件名只保留安全字符，避免头注入。
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "download.pdf".to_string()
    } else {
        cleaned
    }
}

/// 随机十六进制串（对齐 Python `uuid.uuid4().hex` 的用途）。
///
/// 不引 uuid/rand 依赖：`RandomState` 的种子每次进程启动都不同，加上计数器
/// 与时间戳，足够给临时文件名提供唯一性。
pub fn rand_hex(n: usize) -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut h1 = std::collections::hash_map::RandomState::new().build_hasher();
    h1.write_u64(c);
    h1.write_u128(t);
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(c ^ 0x9E37_79B9_7F4A_7C15);
    h2.write_u128(t);

    let a = format!("{:016x}{:016x}", h1.finish(), h2.finish());
    a.chars().take(n).collect()
}
