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
