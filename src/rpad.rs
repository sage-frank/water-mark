//! `rpad` —— Python `rpa` 服务（`app.py` + `rpa/views/api.py`）的 Rust 等价实现。
//!
//! 业务链路与 Python 一一对应：
//!
//! ```text
//! GET /api/v1/rpa/parse_file?src_file_key=..&template_file_key=..
//!                           &fund_cnname=..&letters_date=..&scheme=..
//!   ├─ 参数校验（缺参 → 409 {"code":-1,"msg":"缺少参数"}）
//!   ├─ 并发闸门（默认 1，等价 Python 的 Redis 全局锁 XLock）
//!   ├─ 按 scheme 从 S3 取两份 DOCX（含客户端解密，见 oss.rs）
//!   ├─ 填模板占位符 + 把源文档内容原封不动拼到模板后（复用 dxpdfd/merge.rs）
//!   └─ dxpdf + Skia 进程内转 PDF → 直接返回 PDF 附件流
//! ```
//!
//! 与 Python 的三点等价替换（对外契约不变）：
//! 1. **Word COM → dxpdf**：不再起 Word，不再「重试 3 次」，转换在进程内完成；
//! 2. **Redis 全局锁 → 进程内信号量**：默认 1 个许可（完全串行，与原行为一致），
//!    可用 `concurrency` 调大；
//! 3. **S3→临时文件→读回 → 全内存**：唯一例外是 `crm_v2` 走 Go DLL 那条路，
//!    DLL 的 API 就是「下载解密后落盘」，与 Python 完全一致。
//!
//! 为什么不用 aws-sdk-rust：它的 HTTPS 客户端只有 rustls(aws-lc-rs，要 cmake) /
//! ring(Windows 要 nasm) 两条路，本机构建环境都没有；因此用
//! `reqwest(native-tls)` + 自实现 SigV4（`rpad/sigv4.rs`）直连 S3 / KMS。
//!
//! 用法：
//! ```text
//! rpad                      # 启动服务（读 rpad.toml / RPAD_CONFIG / RPAD_SCHEMES）
//! rpad serve
//! rpad oneshot --template t.docx --src s.docx \
//!             --fund-cnname 某基金 --letters-date 2026-09-17 --out out.pdf
//! ```
//!
//! 构建需要打开 feature：`cargo build --release --bin rpad --features rpad`。

// `rpad.rs` 是 bin 的 crate root；merge 逻辑直接复用 `dxpdfd/merge.rs`
// （已用真实模板验收过的实现，不重复造轮子，也不改动 dxpdfd 的任何代码）。
#[path = "dxpdfd/merge.rs"]
mod merge;

#[path = "rpad/config.rs"]
mod config;
#[path = "rpad/crypto.rs"]
mod crypto;
#[path = "rpad/http.rs"]
mod http;
#[path = "rpad/oss.rs"]
mod oss;
#[path = "rpad/pdf.rs"]
mod pdf;
#[path = "rpad/sigv4.rs"]
mod sigv4;

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use config::Config;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("serve");
    match cmd {
        "serve" | "server" => serve(),
        "oneshot" => oneshot(&args[1..]),
        "help" | "--help" | "-h" => print_usage(),
        other => {
            eprintln!("未知命令: {other}");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    println!(
        "rpad —— Python rpa 服务的 Rust 等价实现\n\
         \n\
         用法:\n\
         \x20 rpad serve              启动 HTTP 服务（默认 0.0.0.0:8090）\n\
         \x20 rpad oneshot <参数>     本地一次性转换（不走 S3，便于回归对比）\n\
         \n\
         oneshot 参数:\n\
         \x20 --template <path>     模板 DOCX（含 {{{{...}}}} 占位符）\n\
         \x20 --src <path>          源 DOCX（内容追加到模板后）\n\
         \x20 --fund-cnname <val>   替换 {{{{Fund_cnname}}}}\n\
         \x20 --letters-date <val>  替换 {{{{letters_date}}}}\n\
         \x20 --out <path>          输出 PDF（默认 out.pdf）\n\
         \n\
         配置:\n\
         \x20 rpad.toml 或 $RPAD_CONFIG 指定的文件；$RPAD_SCHEMES 可注入 JSON 配置。\n\
         \x20 详见仓库根目录 rpad.toml.example。"
    );
}

// ---------------------------------------------------------------- serve

fn serve() {
    let cfg = match Config::load(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[FATAL] 加载配置失败: {e}");
            std::process::exit(1);
        }
    };

    // 日志落盘要尽早开：以服务方式运行时 stdout 常被丢弃（nohup / 服务管理器 /
    // 无控制台启动），只靠 stdout 的话 release 版就完全没有日志可查。
    init_log_file(cfg.log_file());

    // 启动体检：配置错就大声报出来，别等第一个请求才炸。
    if let Err(e) = cfg.validate_schemes() {
        eprintln!("[FATAL] 配置校验失败: {e}");
        std::process::exit(1);
    }
    let missing = cfg.missing_schemes();
    if !missing.is_empty() {
        log_line(
            "WARN",
            &format!(
                "以下 scheme 未配置（收到对应请求会 409）: {}",
                missing.join(", ")
            ),
        );
    }

    let addr: SocketAddr = cfg.addr;
    match cfg.log_file() {
        Some(p) => log_line("INFO", &format!("日志文件: {}（同时打到 stdout）", p.display())),
        None => log_line("INFO", "日志仅输出到 stdout（RPAD_LOG_FILE=off）"),
    }
    log_line("INFO", &format!("rpad 启动: 监听 {addr}"));
    log_line(
        "INFO",
        &format!(
            "配置来源: {}；scheme: [{}]；并发={}（1 等价 Python 全局锁）；抢许可超时={}ms",
            cfg.source,
            cfg.schemes.keys().cloned().collect::<Vec<_>>().join(","),
            cfg.server.concurrency,
            cfg.server.acquire_timeout_ms,
        ),
    );
    if let Some(d) = cfg.dump_dir() {
        log_line("WARN", &format!("已开启调试落盘: {}（正式环境不建议）", d.display()));
    }

    let state = Arc::new(http::AppState::new(cfg));
    let app = http::AppState::router(state);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建 tokio 运行时失败");
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|e| panic!("监听 {addr} 失败: {e}"));

        // 优雅退出：Ctrl+C / SIGTERM（Windows 上是 Ctrl+C / shutdown）。
        let shutdown = async {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("注册 SIGTERM 处理失败");
                tokio::select! {
                    _ = ctrl_c => {},
                    _ = term.recv() => {},
                }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
            log_line("INFO", "收到退出信号，开始优雅停机");
        };

        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            eprintln!("[FATAL] HTTP 服务异常: {e}");
            std::process::exit(1);
        }
        log_line("INFO", "rpad 已退出");
    });
}

// ---------------------------------------------------------------- oneshot

/// 本地一次性转换（T7：不依赖 S3，用真实模板/源文档做回归对比）。
fn oneshot(args: &[String]) {
    let mut template: Option<PathBuf> = None;
    let mut src: Option<PathBuf> = None;
    let mut fund = String::new();
    let mut date = String::new();
    let mut out = PathBuf::from("out.pdf");

    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match a.as_str() {
            "--template" => template = Some(PathBuf::from(val())),
            "--src" => src = Some(PathBuf::from(val())),
            "--fund-cnname" | "--fund_cnname" => fund = val(),
            "--letters-date" | "--letters_date" => date = val(),
            "--out" => out = PathBuf::from(val()),
            other => {
                eprintln!("未知参数: {other}");
                print_usage();
                std::process::exit(2);
            }
        }
    }

    let (Some(template), Some(src)) = (template, src) else {
        eprintln!("oneshot 需要 --template 和 --src");
        print_usage();
        std::process::exit(2);
    };
    if fund.is_empty() || date.is_empty() {
        eprintln!("oneshot 需要 --fund-cnname 和 --letters-date");
        std::process::exit(2);
    }

    let template_bytes = match std::fs::read(&template) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[FATAL] 读取模板 {} 失败: {e}", template.display());
            std::process::exit(1);
        }
    };
    let src_bytes = match std::fs::read(&src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[FATAL] 读取源文档 {} 失败: {e}", src.display());
            std::process::exit(1);
        }
    };

    let started = std::time::Instant::now();
    let opts = dxpdf::RenderOptions::default().with_image_dpi(dxpdf::DEFAULT_IMAGE_DPI);

    // oneshot 与线上一致：非严格（未知占位符渲染成空串），便于复现线上行为。
    let strict = std::env::var("RPAD_STRICT_PLACEHOLDERS")
        .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (merged, stats) =
            merge::build_merged_docx(&template_bytes, &src_bytes, &fund, &date, strict)
                .map_err(|e| format!("合并失败: {e}"))?;
        println!(
            "合并完成: replaced={} backfilled={} media={} notes={} docx={}B",
            stats.replaced, stats.backfilled, stats.media, stats.notes,
            merged.len()
        );
        for u in &stats.unknowns {
            println!("WARN 占位符无对应值，已渲染为空: {u}");
        }
        let (pdf, pages, breakdown) = pdf::docx_to_pdf(&merged, &opts).map_err(|e| e.to_string())?;
        println!(
            "转 PDF 完成: pages={pages} pdf={}B（{breakdown}）",
            pdf.len()
        );
        Ok::<(Vec<u8>, usize), String>((pdf, pages))
    }));

    match result {
        Ok(Ok((pdf, pages))) => {
            if let Err(e) = std::fs::write(&out, &pdf) {
                eprintln!("[FATAL] 写出 {} 失败: {e}", out.display());
                std::process::exit(1);
            }
            println!(
                "完成: {} ({} 字节, {} 页), 耗时 {:.1}s",
                out.display(),
                pdf.len(),
                pages,
                started.elapsed().as_secs_f32()
            );
        }
        Ok(Err(e)) => {
            eprintln!("[FATAL] {e}");
            std::process::exit(1);
        }
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(非文本 panic)".to_string());
            eprintln!("[FATAL] panic: {msg}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------- 日志

/// 极简日志：`时间 [级别] 消息`，打到 stdout，**并在开了日志文件时追加写一份**。
///
/// Python 侧走 logging/reqID 那套；这里保持「一条一行、人能读」即可，
/// 需要结构化日志时再换 tracing。
///
/// 为什么一定要落盘：以服务方式运行时 stdout 经常被丢弃（nohup、服务管理器、
/// 无控制台启动），release 版就表现为「完全没有日志」。见 [`Config::log_file`]。
pub(crate) fn log_line(level: &str, msg: &str) {
    let line = format!("{} [{}] {}", now_utc_string(), level, msg);
    println!("{line}");
    if let Some(Some(f)) = LOG_FILE.get() {
        // 单条日志写失败不该影响请求；仅忽略。
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// 日志文件句柄（追加写）。`None` = 只走 stdout。
static LOG_FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

/// 打开日志文件。重复调用只有第一次生效（与 stdout 并行输出）。
pub(crate) fn init_log_file(path: Option<PathBuf>) {
    let file = path.and_then(|p| match std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("[WARN] 打开日志文件 {} 失败（只走 stdout）: {e}", p.display());
            None
        }
    });
    let _ = LOG_FILE.set(file.map(Mutex::new));
}

/// `20260919-052000` 形式的 UTC 时间戳，用于落盘文件名（与日志同一时区，便于对照）。
pub(crate) fn utc_stamp() -> String {
    let s = now_utc_string();
    match s.split_once(' ') {
        Some((d, t)) => format!("{}-{}", d.replace('-', ""), t.replace(':', "")),
        None => s,
    }
}

fn now_utc_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// 「1970-01-01 起的天数」→ 公历（Howard Hinnant 算法）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
