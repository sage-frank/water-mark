//! 方案验证 A：`libreoffice-pure`（纯 Rust 实现，进程内转换）
//!
//! 与 `unoserver2pdf` 一一对应，方便横向对比：
//!   - 无外部进程 / 无常驻内存，转换在**当前进程内**完成
//!   - 入口：`libreoffice_pure::docx_to_pdf_bytes(&[u8]) -> Result<Vec<u8>>`
//!     （`Result` 是 `lo_core::Result`，错误类型为 `lo_core::LoError`）
//!
//! 用法：
//!   cargo run --release --bin lopure2pdf -- in.docx out.pdf [--repeat=N]

use std::env;
use std::time::{Duration, Instant};

/// 用 `lopdf` 回读输出：既校验是不是合法 PDF，又能拿到真实页数。
fn probe_pdf(bytes: &[u8]) -> Result<usize, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("输出缺少 %PDF- 魔数".to_string());
    }
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| e.to_string())?;
    Ok(doc.get_pages().len())
}

/// 校验输入像不像一个 OOXML 包（docx 就是 zip）。
fn looks_like_docx(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

fn parse_args() -> Result<(String, String, usize), String> {
    let mut positional: Vec<String> = Vec::new();
    let mut repeat: usize = 1;

    for arg in env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--repeat=") {
            repeat = v
                .parse()
                .map_err(|_| format!("--repeat 需要正整数，收到：{v}"))?;
        } else if arg == "-h" || arg == "--help" {
            println!("用法: lopure2pdf [in.docx] [out.pdf] [--repeat=N]");
            std::process::exit(0);
        } else if arg.starts_with("--") {
            return Err(format!("未知参数：{arg}"));
        } else {
            positional.push(arg);
        }
    }

    if repeat == 0 {
        return Err("--repeat 必须大于 0".to_string());
    }

    let input = positional
        .first()
        .cloned()
        .unwrap_or_else(|| "in.docx".to_string());
    let output = positional
        .get(1)
        .cloned()
        .unwrap_or_else(|| "lopure-out.pdf".to_string());
    Ok((input, output, repeat))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (input_path, output_path, repeat) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("参数错误：{e}");
            std::process::exit(2);
        }
    };

    let total_start = Instant::now();
    println!("=== 方案 A：libreoffice-pure（纯 Rust，进程内） ===");
    println!("输入: {input_path}  →  输出: {output_path}  (repeat={repeat})");

    // ---------- [1/4] 读取输入 ----------
    let t = Instant::now();
    let docx_bytes = std::fs::read(&input_path)
        .map_err(|e| format!("读取 {input_path} 失败：{e}"))?;
    let read_time = t.elapsed();
    println!(
        "[1/4] 读取输入               {:>10.2?}  ({} 字节)",
        read_time,
        docx_bytes.len()
    );
    if !looks_like_docx(&docx_bytes) {
        eprintln!("警告：输入看起来不是 OOXML(zip) 包，libreoffice-pure 可能会解析失败");
    }

    // ---------- [2/4] 转换（进程内，跑 repeat 次）----------
    let mut convert_times: Vec<Duration> = Vec::with_capacity(repeat);
    let mut pdf_bytes = Vec::new();

    for i in 0..repeat {
        let t = Instant::now();
        match libreoffice_pure::docx_to_pdf_bytes(&docx_bytes) {
            Ok(bytes) => {
                let d = t.elapsed();
                convert_times.push(d);
                pdf_bytes = bytes;
                println!("[2/4] 第 {}/{} 次转换       {:>10.2?}", i + 1, repeat, d);
            }
            Err(e) => {
                // LoError 实现了 Display + std::error::Error
                eprintln!("[2/4] 第 {}/{} 次转换失败：{}", i + 1, repeat, e);
                return Err(Box::new(e));
            }
        }
    }

    // ---------- [3/4] 校验输出 ----------
    let t = Instant::now();
    let pages = probe_pdf(&pdf_bytes).map_err(|e| format!("输出不是有效 PDF：{e}"))?;
    let probe_time = t.elapsed();
    println!(
        "[3/4] 校验输出(lopdf 回读)    {:>10.2?}   {} 页, {} 字节",
        probe_time,
        pages,
        pdf_bytes.len()
    );

    // ---------- [4/4] 写出 + 汇总 ----------
    let t = Instant::now();
    std::fs::write(&output_path, &pdf_bytes)?;
    let write_time = t.elapsed();
    println!(
        "[4/4] 写出 {} {:>10.2?}",
        output_path,
        write_time
    );

    let total = total_start.elapsed();
    println!("---- 耗时汇总 ----");
    println!("  读取输入        {:>10.2?}", read_time);
    println!("  转换(docx→pdf)  {:>10.2?}   ← 核心指标", convert_times[0]);
    if repeat > 1 {
        let warm: Duration = convert_times[1..].iter().sum::<Duration>() / (repeat as u32 - 1);
        println!(
            "    首次(冷)      {:>10.2?}\n    后续均值(热)  {:>10.2?}",
            convert_times[0], warm
        );
    }
    println!("  校验输出        {:>10.2?}", probe_time);
    println!("  写出文件        {:>10.2?}", write_time);
    println!("  总计(含进程内)  {:>10.2?}", total);
    println!("转换成功：{output_path}（{pages} 页，{} 字节）", pdf_bytes.len());

    Ok(())
}
