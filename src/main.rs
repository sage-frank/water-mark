use rayon::prelude::*;
use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use water_mark::run_watermark_process; // 调用 lib 中的公开函数

/// 默认字体路径。单文件模式可用第 3 个参数、目录模式用第 2 个参数覆盖，
/// 便于在别的机器上做基准测试而不必改代码。
const DEFAULT_FONT_PATH: &str =
    r"D:\code\pycode\rpa\pythonProject\water_mark\SourceHanSerifCN-Bold.otf";

/// 批量输出的文件名前缀。同时也是“已处理过”的判断依据：
/// 同名前缀的文件会被跳过，避免重复加水印。
const OUTPUT_PREFIX: &str = "water-mark";

/// 默认批处理目录：项目下的 pdf 文件夹
const DEFAULT_PDF_DIR: &str = r"D:\code\pycode\rpa\pythonProject\water_mark\pdf";

fn main() {
    let args: Vec<String> = env::args().collect();

    // 用法：
    //   water_mark_cli                          -> 批处理默认 pdf 文件夹
    //   water_mark_cli <目录>                   -> 批处理指定目录
    //   water_mark_cli <文件.pdf> [输出.pdf]    -> 单文件模式
    let arg1 = args.get(1).map(|s| s.as_str());

    let result = match arg1 {
        // 未传参数：批处理默认 pdf 文件夹
        None => process_dir(Path::new(DEFAULT_PDF_DIR), DEFAULT_FONT_PATH),
        // 传入的是目录：批处理（第 2 个参数可覆盖字体路径）
        Some(p) if Path::new(p).is_dir() => process_dir(
            Path::new(p),
            args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_FONT_PATH),
        ),
        // 传入的是文件：单文件模式（第 3 个参数可覆盖字体路径）
        Some(input_path) => {
            let output_path = args
                .get(2)
                .map(|s| s.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| default_single_output(input_path));
            let font_path = args
                .get(3)
                .map(|s| s.as_str())
                .unwrap_or(DEFAULT_FONT_PATH);
            process_single(input_path, &output_path, font_path)
        }
    };

    // 失败时打印原因并返回非零退出码：
    // 只 exit(1) 而不打印的话，调用方（含人工排查）会完全看不到失败原因。
    if let Err(e) = result {
        eprintln!("错误：{}", e);
        std::process::exit(1);
    }
}

/// 单文件模式缺省输出名：同目录下 water-mark + 原文件名
fn default_single_output(input_path: &str) -> String {
    let p = Path::new(input_path);
    let dir = p.parent().unwrap_or(Path::new("."));
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    dir.join(format!("{}{}", OUTPUT_PREFIX, name))
        .to_string_lossy()
        .to_string()
}

/// 单文件加水印
fn process_single(
    input_path: &str,
    output_path: &str,
    font_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = "张三";
    let date = "2026-02-05";
    let text = format!("致{}-{}:高度保密", name, date);

    let start_time = Instant::now();
    println!("正在处理 PDF: {}", input_path);

    run_watermark_process(input_path, output_path, font_path, &text)?;

    println!(
        "Rust 矢量水印生成成功！保存为 {}（耗时 {:.2?}）",
        output_path,
        start_time.elapsed()
    );
    Ok(())
}

/// 扫描目录，给所有 PDF 加水印，输出到同一目录：water-mark + 原文件名
fn process_dir(dir: &Path, font_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let name = "张三";
    let date = "2026-02-05";
    let text = format!("致{}-{}:高度保密", name, date);

    let entries = std::fs::read_dir(dir)?;
    let mut pdf_files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            // 只处理 PDF（扩展名不区分大小写）
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
                // 跳过本工具的输出（water-mark 前缀），避免重复加水印
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(OUTPUT_PREFIX))
        })
        .collect();
    pdf_files.sort();

    if pdf_files.is_empty() {
        println!("目录 {} 中没有找到待处理的 PDF 文件", dir.display());
        return Ok(());
    }

    println!(
        "目录 {} 共找到 {} 个 PDF 文件，开始并行加水印（rayon 线程数 {}）...",
        dir.display(),
        pdf_files.len(),
        rayon::current_num_threads()
    );

    let total_start = Instant::now();

    // 单个 PDF 内部必须串行（Document 要 &mut），但 PDF 之间完全独立：无共享
    // 可变状态，lib 侧字形资产缓存是只读 RwLock。批处理由此吃满所有核心。
    let results: Vec<(String, String, Duration, Result<(), String>)> = pdf_files
        .par_iter()
        .map(|input| {
            let file_name = input
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let output = dir.join(format!("{}{}", OUTPUT_PREFIX, file_name));
            let out_label = output.display().to_string();
            let start = Instant::now();
            // Box<dyn Error> 不是 Send，出闭包前先转成 String。
            let res = run_watermark_process(
                &input.to_string_lossy(),
                &output.to_string_lossy(),
                font_path,
                &text,
            )
            .map_err(|e| e.to_string());
            (file_name, out_label, start.elapsed(), res)
        })
        .collect();

    let mut ok_count = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    for (file_name, out_label, elapsed, res) in &results {
        match res {
            Ok(_) => {
                ok_count += 1;
                println!("  -> {}（耗时 {:.2?}）", out_label, elapsed);
            }
            Err(e) => {
                eprintln!("  -> 失败: {}", e);
                failed.push((file_name.clone(), e.clone()));
            }
        }
    }

    println!(
        "批处理完成：成功 {} / {} 个，总耗时 {:.2?}",
        ok_count,
        pdf_files.len(),
        total_start.elapsed()
    );

    if !failed.is_empty() {
        eprintln!("以下文件处理失败：");
        for (f, e) in &failed {
            eprintln!("  {} : {}", f, e);
        }
        return Err(format!("{} 个 PDF 加水印失败", failed.len()).into());
    }

    Ok(())
}
