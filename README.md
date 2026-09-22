"# WaterMark PDF 水印处理系统

这是一个高性能的 PDF 水印处理方案，提供了 **Rust** (高性能/FFI支持) 和 **Python** (字体子集优化) 两种实现方式。支持生成平铺、旋转、透明的矢量文字水印。

## 🌟 功能特性

- **Rust 实现**:
  - 基于 `lopdf` 和 `ab_glyph`，无需依赖庞大的 PDF 渲染引擎。
  - 提供 C-compatible **FFI 接口**，可供 PHP、Node.js、Go 等语言直接调用。
  - 极高的处理性能，适合服务端高并发场景。

- **Python 实现**:
  - 基于 `PyMuPDF` (fitz) 和 `fontTools`。
  - **智能字体子集化**: 自动提取仅使用的字符生成子集字体，显著减小输出文件体积（例如从 10MB 字体缩减到几 KB）。
  - 优秀的兼容性与渲染效果。

- **通用特性**:
  - 支持自定义文本（如姓名、日期）。
  - 自动平铺全页，支持旋转角度、透明度调节。
  - 附带 `preview.html` 可直接在浏览器预览水印效果。

## 🛠️ 快速开始

### 前置要求

- Rust (Cargo)
- Python 3.8+ (如果使用 Python 版本)
- 字体文件: `STSongStd-Light-Acro.otf` (已包含在 `STSongStd-Light-Acro/` 目录下)

### 1. Rust 版本使用

可以直接编译运行命令行工具：

```bash
# 格式: cargo run -- [输入文件] [输出文件]
cargo run -- in.pdf out.pdf
```

#### FFI 接口 (供 PHP/C 调用)

编译为动态库 (`.dll` / `.so`) 后，通过 FFI 调用：

```c
// 返回 0 表示成功，-1 表示失败
int add_pdf_watermark(
    const char* input_path,  // 输入 PDF 路径
    const char* output_path, // 输出 PDF 路径
    const char* font_path,   // 字体文件路径
    const char* user_name,   // 用户名 (水印内容)
    const char* date_str     // 日期 (水印内容)
);
```

### 2. Python 版本使用

安装依赖：

```bash
pip install pymupdf fonttools
```

运行脚本：

```bash
python add_water_mark.py
```

*注意：Python 脚本默认读取 `in.pdf` 并输出 `output_py_watermarked.pdf`，可在脚本底部修改配置。*

## 📂 项目结构

```
.
├── src/
│   ├── lib.rs          # Rust 核心逻辑 & FFI 接口
│   └── main.rs         # Rust CLI 入口
├── add_water_mark.py   # Python 实现 (含字体子集功能)
├── preview.html        # 基于 PDF.js 的水印效果预览
├── Cargo.toml          # Rust 项目配置
└── STSongStd-Light-Acro/ # 字体资源目录
```

## 📝 许可证

MIT License" 

---

# rpad —— `rpa`（Python）服务的 Rust 等价实现

`src/rpad.rs` + `src/rpad/` 是内网 `rpa` 服务（Flask 版见 `../rpa/app.py` 与
`../rpa/rpa/views/api.py`）的 Rust 重写：**替换 docx 占位符 → 拼接两份 docx →
转 PDF**，全程不再依赖 Word/COM 和 Redis 锁。

## 对外契约（与 Python 版完全一致，调用方零改动）

```text
GET /api/v1/rpa/parse_file?src_file_key=..&template_file_key=..
                           &fund_cnname=..&letters_date=..&scheme=..
scheme ∈ { mfront, crm, crm_v2, hhcrm, other }
```

- 参数缺失 → `409 {"code":-1,"msg":"缺少参数"}`
- 任何失败 → `409 {"code":-1,"msg":"文件生成失败"}`（原因只进日志）
- 成功 → `application/pdf` 附件流（文件名 `template-<模板key>-<随机串>.pdf`）
- 新增（不影响兼容）：`GET /` 探活（同 Flask）、`GET /healthz` 运行计数

## 与 Python 的三点等价替换

| Python | rpad | 说明 |
|---|---|---|
| Word COM（InsertBreak+InsertFile+SaveAs，重试 3 次） | `dxpdfd/merge.rs` + dxpdf/Skia 进程内转换 | 合并模块直接复用已验收的实现 |
| Redis 全局锁 XLock（完全串行） | 进程内信号量，默认 `concurrency = 1` | 行为等价，可用配置调大 |
| S3 → 临时文件 → 读回 | 全内存（`crm_v2` 走 Go DLL 时除外） | DLL 的 API 本身就是落盘式 |

S3/KMS 不用 aws-sdk-rust：其 HTTPS 客户端需要 rustls(aws-lc-rs，构建要 cmake)
或 ring（Windows 要 nasm）；这里用 `reqwest(native-tls → schannel)` + 自实现
SigV4（`src/rpad/sigv4.rs`）。mfront/crm 是 AES-GCM（KMS 数据密钥），hhcrm 是
Fernet，crm_v2 优先复用 Go 编译的 `kms_x64.dll`（`libloading`），见
`src/rpad/oss.rs`。

## 构建与运行

```bash
# 需要 feature rpad（reqwest 的 native-tls 只在这里打开）
cargo build --release --bin rpad --features rpad

# 服务模式：读 ./rpad.toml（样例见 rpad.toml.example），
# 或 RPAD_CONFIG 指定路径，或 RPAD_SCHEMES 注入 JSON
./target/release/rpad.exe serve

# 本地一次性转换（不走 S3，便于回归对比）
./target/release/rpad.exe oneshot \
    --template template-glv_template2-*.docx --src test_src_ys.docx \
    --fund-cnname 某某基金 --letters-date 2026-09-17 --out out.pdf
```

> 注意：首次构建 skia 需要从 github 下载预编译产物，若网络不通请设置
> `HTTPS_PROXY`（例如本机 `http://127.0.0.1:7897`）。

## 验证记录（2026-09-17）

用仓库真实模板 + `test_src_ys.docx`：占位符替换 3/3（document.xml ×2 +
header4.xml ×1），`backfilled=23 media=1 notes=1`，输出 5 页 PDF；HTTP 契约
（`/`、409 文案、404、/healthz 计数）逐项比对通过。
