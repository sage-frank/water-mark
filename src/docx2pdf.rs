use std::env;
use std::time::{Duration, Instant};

/// 打印某个阶段占「被统计总时长」的百分比。
fn pct(d: Duration, total: Duration) -> f64 {
    let t = total.as_secs_f64();
    if t <= 0.0 { 0.0 } else { d.as_secs_f64() / t * 100.0 }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    let args: Vec<String> = env::args().collect();
    let input_path = args.get(1).map(|s| s.as_str()).unwrap_or("in.docx");
    let output_path = args.get(2).map(|s| s.as_str()).unwrap_or("docx2pdf-out.pdf");

    println!("正在转换 DOCX → PDF: {}", input_path);

    // ---------- [1/4] 读取输入 ----------
    let t = Instant::now();
    let input_size = std::fs::metadata(input_path)?.len();
    println!(
        "[1/4] 读取输入文件            {:>10.2?}  ({} 字节)",
        t.elapsed(),
        input_size
    );

    // ---------- [2/4] 转换（office2pdf 内部再分 parse / codegen / compile）----------
    let t = Instant::now();
    let result = office2pdf::convert(input_path)?;
    let convert_time = t.elapsed();
    println!("[2/4] office2pdf::convert      {:>10.2?}", convert_time);

    match &result.metrics {
        Some(m) => {
            let accounted = m.parse_duration + m.codegen_duration + m.compile_duration;
            let other = m.total_duration.saturating_sub(accounted);
            println!(
                "        ├─ parse   (DOCX → IR)   : {:>10.2?}  {:>5.1}%",
                m.parse_duration,
                pct(m.parse_duration, m.total_duration)
            );
            println!(
                "        ├─ codegen (IR → Typst)  : {:>10.2?}  {:>5.1}%",
                m.codegen_duration,
                pct(m.codegen_duration, m.total_duration)
            );
            println!(
                "        ├─ compile (Typst → PDF) : {:>10.2?}  {:>5.1}%",
                m.compile_duration,
                pct(m.compile_duration, m.total_duration)
            );
            println!(
                "        ├─ 其他(字体/组装/错误检查): {:>10.2?}  {:>5.1}%",
                other,
                pct(other, m.total_duration)
            );
            println!("        └─ 库内总计               : {:>10.2?}", m.total_duration);
            println!(
                "      输入 {} 字节 → 输出 {} 字节，{} 页，{} 条警告",
                m.input_size_bytes,
                m.output_size_bytes,
                m.page_count,
                result.warnings.len()
            );
        }
        None => println!("      （本次转换未返回 metrics）"),
    }

    // ---------- [3/4] 写出 PDF ----------
    let t = Instant::now();
    std::fs::write(output_path, &result.pdf)?;
    println!(
        "[3/4] 写出 {}    {:>10.2?}",
        output_path,
        t.elapsed()
    );

    // ---------- [4/4] 总计 ----------
    println!("[4/4] 总耗时(含进程内 I/O)     {:>10.2?}", total_start.elapsed());

    Ok(())
}
