//! 方案验证 B：`unoserver` 常驻（真 LibreOffice + UNO 桥，通过官方客户端 `unoconvert` 调用）
//!
//! 与 `lopure2pdf` 一一对应，方便横向对比：
//!   - 转换发生在**独立的常驻 LibreOffice 进程**里，本进程只是个客户端
//!   - 先 `unoping` 做健康检查，再 `unoconvert` 发起转换；`--repeat` 可看到"冷/热"差异
//!     （这正是常驻相比 `soffice --convert-to` 每次冷启动的价值所在）
//!
//! 用法：
//!   cargo run --release --bin unoserver2pdf -- in.docx out.pdf [--host=127.0.0.1] [--port=2003] [--repeat=3]
//!
//! 说明：这里 shell 调用官方 Python 客户端 `unoconvert`（随 `pip install unoserver` 一起安装）。
//! 如果不希望依赖外部命令，也可以直接 POST 到服务端的 XML-RPC 端点（`/RPC2`，方法名 `convert`）自行实现。

use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: &str = "2003";

struct Config {
    input: PathBuf,
    output: PathBuf,
    host: String,
    port: String,
    repeat: usize,
    timeout: Duration,
    no_probe: bool,
}

/// 运行子进程并施加超时：超时则杀掉子进程，避免一个卡死的 LibreOffice 把验证脚本一起挂住。
/// 返回 `(退出码, stdout, stderr)`。
fn run_with_timeout(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<(i32, String, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 `{program}`：{e}"))?;

    let start = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|e| format!("等待 `{program}` 失败：{e}"))?
        {
            Some(status) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return Ok((
                    status.code().unwrap_or(-1),
                    out.trim().to_string(),
                    err.trim().to_string(),
                ));
            }
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("超时（>{}s），已终止 `{program}`", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// 用 `lopdf` 回读输出：既校验是不是合法 PDF，又能拿到真实页数。
fn probe_pdf(bytes: &[u8]) -> Result<usize, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("输出缺少 %PDF- 魔数".to_string());
    }
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| e.to_string())?;
    Ok(doc.get_pages().len())
}

fn print_start_hint(host: &str, port: &str) {
    let uno_port: u32 = port.parse().unwrap_or(2003);
    let uno_port = if uno_port == 2003 { 2002 } else { uno_port - 1 };
    eprintln!("    --- 启动 unoserver（服务端）---");
    eprintln!("    sudo mkdir -p /var/lib/unoserver/profile /var/log/unoserver");
    eprintln!(
        "    sudo /opt/unoserver/bin/unoserver --interface {host} --port {port} \\\n      --uno-interface 127.0.0.1 --uno-port {uno_port} \\\n      --user-installation /var/lib/unoserver/profile \\\n      --conversion-timeout 120 --daemon \\\n      -p /run/unoserver.pid -f /var/log/unoserver/unoserver.log");
    eprintln!("    --- 安装（Ubuntu 24.04，中文字体必装，否则中文变空框）---");
    eprintln!("    sudo apt install -y libreoffice-writer python3-uno fonts-noto-cjk fonts-liberation2");
    eprintln!("    sudo python3 -m venv --system-site-packages /opt/unoserver");
    eprintln!("    sudo /opt/unoserver/bin/pip install unoserver");
}

/// 解析客户端可执行文件位置，避免依赖 PATH：
/// 1) 环境变量（如 `UNOCONVERT`）2) `/opt/unoserver/bin/`（本机 venv 安装位置）3) PATH 中的同名命令。
fn resolve_tool(env_key: &str, name: &str) -> String {
    if let Ok(p) = env::var(env_key) {
        if !p.is_empty() {
            return p;
        }
    }
    let venv = format!("/opt/unoserver/bin/{name}");
    if Path::new(&venv).exists() {
        return venv;
    }
    name.to_string()
}

fn parse_args() -> Result<Config, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT.to_string();
    let mut repeat: usize = 1;
    let mut timeout_secs: u64 = 120;
    let mut no_probe = false;

    for arg in env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--host=") {
            host = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--port=") {
            port = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--repeat=") {
            repeat = v
                .parse()
                .map_err(|_| format!("--repeat 需要正整数，收到：{v}"))?;
        } else if let Some(v) = arg.strip_prefix("--timeout=") {
            timeout_secs = v
                .parse()
                .map_err(|_| format!("--timeout 需要正整数（秒），收到：{v}"))?;
        } else if arg == "--no-probe" {
            no_probe = true;
        } else if arg == "-h" || arg == "--help" {
            println!(
                "用法: unoserver2pdf [in.docx] [out.pdf] [--host=127.0.0.1] [--port=2003] [--repeat=N] [--timeout=120] [--no-probe]"
            );
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
        .unwrap_or_else(|| "unoserver-out.pdf".to_string());

    // 服务端与客户端同机（host_location=local）时，服务端要靠路径找文件，所以统一转成绝对路径。
    let input = std::fs::canonicalize(&input).map_err(|e| format!("输入文件不可用：{e}"))?;
    let output = std::path::absolute(Path::new(&output)).unwrap_or_else(|_| PathBuf::from(&output));

    Ok(Config {
        input,
        output,
        host,
        port,
        repeat,
        timeout: Duration::from_secs(timeout_secs),
        no_probe,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("参数错误：{e}");
            std::process::exit(2);
        }
    };

    let total_start = Instant::now();
    // 客户端不一定在 PATH 里（pip 装进 venv 时就在 /opt/unoserver/bin）
    let unoconvert = resolve_tool("UNOCONVERT", "unoconvert");
    let unoping = resolve_tool("UNOPING", "unoping");
    println!("=== 方案 B：unoserver 常驻（真 LibreOffice） ===");
    println!(
        "输入: {}  →  输出: {}  ({}:{}  repeat={})",
        cfg.input.display(),
        cfg.output.display(),
        cfg.host,
        cfg.port,
        cfg.repeat
    );

    // ---------- [0/4] 客户端可用性 ----------
    let t = Instant::now();
    match run_with_timeout(&unoconvert, &["--version".to_string()], Duration::from_secs(10)) {
        Ok((code, out, err)) => {
            println!(
                "[0/4] unoconvert 可用         {:>10.2?}  (退出码 {code}) {} {}",
                t.elapsed(),
                out,
                err
            );
            println!("      客户端路径: {unoconvert}");
        }
        Err(e) => {
            eprintln!("[0/4] 找不到可用的 `unoconvert`：{e}");
            print_start_hint(&cfg.host, &cfg.port);
            std::process::exit(3);
        }
    }

    // ---------- [1/4] 服务健康检查 ----------
    if cfg.no_probe {
        println!("[1/4] 已跳过健康检查（--no-probe）");
    } else {
        let t = Instant::now();
        let args = vec![
            "--host".to_string(),
            cfg.host.clone(),
            "--port".to_string(),
            cfg.port.clone(),
        ];
        match run_with_timeout(&unoping, &args, Duration::from_secs(10)) {
            Ok((0, out, _)) => {
                println!("[1/4] unoping 服务健康        {:>10.2?}", t.elapsed());
                for line in out.lines() {
                    println!("        {line}");
                }
            }
            Ok((code, _, err)) => {
                eprintln!("[1/4] unoping 失败（退出码 {code}）：{err}");
                print_start_hint(&cfg.host, &cfg.port);
                std::process::exit(4);
            }
            Err(e) => {
                eprintln!("[1/4] {e}");
                print_start_hint(&cfg.host, &cfg.port);
                std::process::exit(4);
            }
        }
    }

    // ---------- [2/4] 转换（服务端常驻，跑 repeat 次）----------
    let mut durations: Vec<Duration> = Vec::with_capacity(cfg.repeat);
    let input_str = cfg.input.to_string_lossy().to_string();
    let output_str = cfg.output.to_string_lossy().to_string();

    for i in 0..cfg.repeat {
        // 先删掉旧输出，确保测到的是真实转换，而不是复用上一轮的文件
        let _ = std::fs::remove_file(&cfg.output);

        let args = vec![
            "--host".to_string(),
            cfg.host.clone(),
            "--port".to_string(),
            cfg.port.clone(),
            "--host-location".to_string(),
            "local".to_string(),
            "--convert-to".to_string(),
            "pdf".to_string(),
            input_str.clone(),
            output_str.clone(),
        ];

        let t = Instant::now();
        let result = run_with_timeout(&unoconvert, &args, cfg.timeout);
        let d = t.elapsed();

        match result {
            Ok((0, _, _)) => {
                durations.push(d);
                println!("[2/4] 第 {}/{} 次转换       {:>10.2?}", i + 1, cfg.repeat, d);
            }
            Ok((code, out, err)) => {
                eprintln!("[2/4] 第 {}/{} 次转换失败（退出码 {code}）", i + 1, cfg.repeat);
                if !out.is_empty() {
                    eprintln!("      stdout: {out}");
                }
                if !err.is_empty() {
                    eprintln!("      stderr: {err}");
                }
                print_start_hint(&cfg.host, &cfg.port);
                std::process::exit(5);
            }
            Err(e) => {
                eprintln!("[2/4] 第 {}/{} 次转换失败：{e}", i + 1, cfg.repeat);
                print_start_hint(&cfg.host, &cfg.port);
                std::process::exit(5);
            }
        }
    }

    // ---------- [3/4] 校验输出 ----------
    let t = Instant::now();
    let pdf = std::fs::read(&cfg.output).map_err(|e| format!("读取输出失败：{e}"))?;
    let pages = probe_pdf(&pdf).map_err(|e| format!("输出不是有效 PDF：{e}"))?;
    let probe_time = t.elapsed();
    println!(
        "[3/4] 校验输出(lopdf 回读)    {:>10.2?}   {} 页, {} 字节",
        probe_time,
        pages,
        pdf.len()
    );

    // ---------- [4/4] 汇总 ----------
    let cold = durations[0];
    println!("---- 耗时汇总 ----");
    println!("  首次(冷启动含 soffice 唤醒) {:>10.2?}   ← 别把这次算进 SLA", cold);
    if cfg.repeat > 1 {
        let warm: Duration = durations[1..].iter().sum::<Duration>() / (cfg.repeat as u32 - 1);
        println!("  后续均值(热, 常驻收益)      {:>10.2?}", warm);
        if warm.as_secs_f64() > 0.0 {
            println!("  冷/热 倍数                  {:.2}x", cold.as_secs_f64() / warm.as_secs_f64());
        }
    }
    println!("  校验输出                    {:>10.2?}", probe_time);
    println!("  总耗时(含进程启动/调度)     {:>10.2?}", total_start.elapsed());
    println!(
        "转换成功：{}（{} 页，{} 字节）",
        cfg.output.display(),
        pages,
        pdf.len()
    );

    Ok(())
}
