use std::env;
use std::time::Instant;
use water_mark::run_watermark_process; // 调用 lib 中的公开函数

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start_time = Instant::now();

    // 1. 环境准备
    let args: Vec<String> = env::args().collect();
    let input_path = args.get(1).map(|s| s.as_str()).unwrap_or("in.pdf");
    let output_path = args.get(2).map(|s| s.as_str()).unwrap_or("out.pdf");
    let font_path = ".\\STSongStd-Light-Acro\\STSongStd-Light-Acro.otf";
    
    let name = "张三";
    let date = "2026-02-05";
    let text = format!("致{}-{}:高度保密", name, date);

    println!("正在处理 PDF: {}", input_path);
    
    // 2. 调用库中的核心逻辑
    match run_watermark_process(input_path, output_path, font_path, &text) {
        Ok(output) => {
            let duration = start_time.elapsed();
            println!("Rust 矢量水印生成成功！保存为 {}", output);
            println!("总耗时: {:.2?}", duration);
        },
        Err(e) => {
            eprintln!("错误: 无法处理 PDF 文件: {}", e);
        }
    }

    Ok(())
}