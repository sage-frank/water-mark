# WaterMark PDF 水印处理系统

这是一个高性能的 PDF 水印处理方案，提供了 **Rust**（高性能／FFI 支持）和 **Python**（字体子集优化）两种实现方式。支持生成平铺、旋转、透明的矢量文字水印。

同一仓库里还包含 DOCX→PDF 相关的若干工具与服务：`dxpdfd`（常驻转换服务）、`rpad`（Python `rpa` 服务的 Rust 等价实现），以及若干方案验证用的小工具。

## 功能特性

- **Rust 实现**:
  - 基于 `lopdf` 和 `ab_glyph`，无需依赖庞大的 PDF 渲染引擎。
  - 提供 C-compatible **FFI 接口**，可供 PHP、Node.js、Go 等语言直接调用。
  - 另提供 **PHP 原生扩展**（`php_ext/`，基于 ext-php-rs），php.ini 配置后直接调用 `watermark_add()`。
  - 极高的处理性能，适合服务端高并发场景。

- **Python 实现**:
  - 基于 `PyMuPDF`(fitz) 和 `fontTools`。
  - **智能字体子集化**: 自动提取仅使用的字符生成子集字体，显著减小输出文件体积（例如从 10MB 字体缩减到几 KB）。
  - 优秀的兼容性与渲染效果。

- **通用特性**:
  - 支持自定义文本（如姓名、日期）。
  - 自动平铺全页，支持旋转角度、透明度调节。
  - 附带 `preview.html` 可直接在浏览器预览水印效果。

## 项目结构

```text
.
├── src/
│   ├── lib.rs              # 水印核心逻辑 & FFI 接口 add_pdf_watermark
│   ├── main.rs             # 水印 CLI（bin: water_mark_cli）
│   ├── docx2pdf.rs         # 方案对比：office2pdf（bin: docx2pdf）
│   ├── dxpdf2pdf.rs        # 方案对比：dxpdf（bin: dxpdf2pdf）
│   ├── lopure2pdf.rs       # 方案对比 A：libreoffice-pure（bin: lopure2pdf）
│   ├── unoserver2pdf.rs    # 方案对比 B：unoserver（bin: unoserver2pdf）
│   ├── dxpdfd.rs           # DOCX→PDF 常驻 HTTP 服务（bin: dxpdfd）
│   ├── dxpdfd/merge.rs     # 模板占位符替换 + 文档拼接（被 rpad 复用）
│   ├── rpad.rs             # Python rpa 服务的 Rust 等价实现（bin: rpad）
│   ├── rpad/               # config / crypto / http / oss / pdf / sigv4
│   └── bench_glyph.rs      # 临时基准：量化 Skia 字形 path 生成成本
├── php_ext/                # PHP 原生扩展（ext-php-rs，全局函数 watermark_add）
│   ├── src/lib.rs
│   └── Cargo.toml
├── add_water_mark.py       # Python 实现（含字体子集功能）
├── test_ext.php            # PHP 原生扩展测试脚本
├── test.php                # PHP FFI（FFI::cdef）调用示例
├── preview.html            # 基于 PDF.js 的水印效果预览
├── Cargo.toml              # Rust 项目配置（多 bin + feature rpad）
├── rpad.toml.example       # rpad 服务配置样例
└── STSongStd-Light-Acro/   # 字体资源目录
```

---

## 0. 环境准备（本机实测，Kylin V10 / PHP 8.0.30 / GCC 7.3）

| 用途 | 需要的东西 | 安装命令 |
|---|---|---|
| Rust 全部 bin | Rust 工具链（cargo / rustc） | 已装 |
| PHP 原生扩展 | PHP 开发环境（`php-config` + PHP 头文件） | `yum install -y php-devel` |
| Skia 源码构建（备选路径） | `ninja`、`clang`、C++20 标准库 | `yum install -y ninja-build`（本机 GCC 7.3 缺 `<bit>`，源码构建走不通，见 §3.4） |
| Python 版本 | Python 3.8+ | `pip install pymupdf fonttools` |

**国内网络**：crates.io 直连不可用，需要镜像。已写入 `~/.cargo/config.toml`：

```toml
[source.crates-io]
replace-with = 'rsproxy-sparse'

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy]
index = "sparse+https://rsproxy.cn/index/"

[net]
retry = 3
git-fetch-with-cli = true
```

---

## 1. Rust CLI（水印）

```bash
# 指定目录批量：扫描目录下所有 *.pdf，输出「water-mark + 原文件名」
cargo run --release --bin water_mark_cli -- ./pdf

# 单文件：输出缺省为同目录下「water-mark + 原文件名」
cargo run --release --bin water_mark_cli -- ./in.pdf ./out.pdf

# 不传参数：批处理 DEFAULT_PDF_DIR（src/main.rs 中的常量）
cargo run --release --bin water_mark_cli
```

> 注意（Linux）：`src/main.rs` 里的 `DEFAULT_PDF_DIR` 与 `font_path` 目前是 Windows 硬编码路径
> （`D:\code\pycode\...`），在 Linux 上使用前需改为本机路径，例如
> `./SourceHanSerifCN-Bold.otf`。批处理会跳过以 `water-mark` 开头的文件，避免重复加水印；
> 有条目失败时进程返回非零退出码。

编译产物：`target/release/water_mark_cli`。

## 2. Rust FFI（供 PHP / C 调用）

`src/lib.rs` 导出 C 接口，`crate-type = ["cdylib", "rlib"]`，产物为 `target/release/libwater_mark.so`：

```bash
cargo build --release            # 产物：target/release/libwater_mark.so
```

```c
// 返回 0 表示成功，-1 表示失败
int add_pdf_watermark(
    const char* input_path,  // 输入 PDF 路径
    const char* output_path, // 输出 PDF 路径
    const char* font_path,   // 字体文件路径
    const char* user_name,   // 用户名（水印内容）
    const char* date_str     // 日期（水印内容）
);
```

### PHP 侧用 FFI 调用（`test.php`）

```bash
php test.php        # 需要 PHP 的 FFI 扩展；脚本内用 __DIR__ 定位 .so
```

`test.php` 通过 `FFI::cdef()` 声明签名后直接调用，返回 `0` 即成功。缺点：依赖 `FFI` 扩展，类型转换、异常都要自己处理 —— 需要更省事的方式请用下面的原生扩展。

## 3. PHP 原生扩展（`php_ext/`，推荐）

`php_ext/` 用 [ext-php-rs] 把 `water_mark::run_watermark_process` 编译成真正的 PHP 扩展（`.so`），
加载后直接调用全局函数 `watermark_add()`，**不需要 `FFI` 扩展、不需要 `FFI::cdef()`**，类型转换与异常由扩展完成。

PHP 侧签名：

```php
watermark_add(
    string $input,   // 输入 PDF 路径
    string $output,  // 输出 PDF 路径
    string $font,    // 字体文件路径（.otf/.ttf）
    string $user,    // 用户名，如 "张三"
    string $date,    // 日期，如 "2026-02-05"
): bool
```

成功返回 `true`；失败抛出 PHP `Exception`（消息含具体原因），调用方 `try/catch` 即可。
水印文案固定为 `致{user}-{date}:高度保密`。

### 3.1 编译

```bash
cd /root/code/water-mark/php_ext
cargo build --release
# 产物：php_ext/target/release/libwatermark.so
```

首次编译前确认 `php-config` 可用（来自 `php-devel`）：

```bash
php-config --version
php-config --extension-dir
```

### 3.2 安装到 PHP 扩展目录

```bash
# 去掉 lib 前缀，符合 PHP 命名习惯
cp /root/code/water-mark/php_ext/target/release/libwatermark.so \
   "$(php-config --extension-dir)/watermark.so"
```

编译时用的 PHP（`php-config` 指向的）必须与运行时的 PHP **版本、线程安全模型（ZTS/NTS）**一致，否则扩展加载失败。

### 3.3 让扩展生效（三种方式任选其一）

先看 CLI 用的是哪个 ini：

```bash
php --ini          # Loaded Configuration File 即 CLI 的 ini（本机为 /etc/php.ini）
```

**方式 A：单次运行（临时生效，不改配置）**

```bash
cd /root/code/water-mark && php -d extension=watermark.so test_ext.php
```

**方式 B：写入 php.ini（永久生效）**

```bash
echo "extension=watermark.so" >> /etc/php.ini
cd /root/code/water-mark && php test_ext.php
```

若 ini 里的 `extension_dir` 不是 `/usr/lib64/php/modules`，改用绝对路径：

```bash
echo "extension=/usr/lib64/php/modules/watermark.so" >> /etc/php.ini
```

**方式 C：单独的 ini 片段（推荐，不污染主配置）**

```bash
echo "extension=watermark.so" > /etc/php.d/watermark.ini
cd /root/code/water-mark && php test_ext.php
```

**验证是否加载成功**

```bash
php -m | grep watermark
php -r 'var_dump(function_exists("watermark_add"));'
```

**Web / php-fpm 场景**：php-fpm 读的是自己的配置（`/etc/php-fpm.d/`、`/etc/php-fpm.conf` 或
fpm 专用 ini），CLI 的 `/etc/php.ini` 改动不会自动生效。改完需重启：

```bash
systemctl restart php-fpm
```

### 3.4 运行测试脚本

`test_ext.php` 包含两个示例：单文件（读 `in.pdf` → 写 `out_ext.pdf`）与批量（扫描 `pdf/` → 输出 `water-mark + 原名`）。
它需要仓库根目录下存在 `SourceHanSerifCN-Bold.otf` 与 `in.pdf`（批量还需 `pdf/` 目录）：

```bash
cd /root/code/water-mark
php test_ext.php
```

预期输出：

```text
水印添加成功：/root/code/water-mark/out_ext.pdf
[OK]   water-mark55-2.pdf
```

### 3.5 本机构建时踩过的坑（重装/换机请照做）

**① PHP 8.0 < ext-php-rs 0.15 的最低要求（8.1）**

ext-php-rs 0.15 的 `enum` 特性要求 PHP 8.1+，默认开启会直接报
`PHP version php80 is below minimum supported version`。本扩展只用 `#[php_function]`/`#[php_module]`，
不涉及 PHP 枚举，因此在 `php_ext/Cargo.toml` 中关掉该特性即可（已配置好）：

```toml
ext-php-rs = { version = "0.15", default-features = false, features = ["runtime"] }
```

保留 `runtime` 特性；去掉 `enum` 后 PHP 8.0 可用（构建时仍会打印一条 EOL 警告，可忽略）。

**② Skia 预编译产物 404 → 回退源码构建 → 本机编不过**

依赖链 `dxpdf` 启用了 `skia-safe` 的 `embed-freetype`，产物名里会带 `ftembed`
（`skia-binaries-<hash>-x86_64-unknown-linux-gnu-ftembed-jpegd-jpege-pdf-textlayout.tar.gz`），
而 skia-binaries 0.99.0 的 release 里**没有这个组合**，于是 404 并回退到源码构建；
本机 GCC 7.3 的 libstdc++ 没有 C++20 的 `<bit>` 头文件（`fatal error: 'bit' file not found`），源码构建必然失败。

解法：下载同一 hash、仅少一个 `ftembed` 标记的官方预编译包，重命名成构建脚本期望的文件名，
再用 `SKIA_BINARIES_URL` 指向本地文件（构建脚本下载后并不校验 key）：

```bash
# 1) 下载官方预编译包（17MB）
mkdir -p /tmp/skia-prebuilt && cd /tmp/skia-prebuilt
curl -L -o skia-binaries-a25a0fdb7d90429aa2d1-x86_64-unknown-linux-gnu-ftembed-jpegd-jpege-pdf-textlayout.tar.gz \
  "https://github.com/rust-skia/skia-binaries/releases/download/0.99.0/skia-binaries-a25a0fdb7d90429aa2d1-x86_64-unknown-linux-gnu-jpegd-jpege-pdf-textlayout.tar.gz"

# 2) 用它构建（file:// 模板，{key} 由构建脚本填入）
cd /root/code/water-mark/php_ext
SKIA_BINARIES_URL="file:///tmp/skia-prebuilt/skia-binaries-{key}.tar.gz" cargo build --release
```

说明：`embed-freetype` 仅表示「FreeType 静态编译进 Skia 而非用系统库」，功能一致；
该预编译包为 Ubuntu 构建，运行时依赖系统 `libfreetype.so.6` 等，已在 Kylin V10 实测可用。
本机完整的可复现构建日志：`4m 43s` 出 `libwatermark.so`（约 2.2MB）。

## 4. Python 版本使用

安装依赖：

```bash
pip install pymupdf fonttools
```

运行脚本：

```bash
python add_water_mark.py
```

*注意：脚本默认读取 `in.pdf` 并输出 `output_py_watermarked.pdf`，可在脚本底部 `__main__` 中修改配置；
其中的 `font_path` 目前是 Windows 硬编码路径，Linux 下需改为本机字体（如 `SourceHanSerifCN-Bold.otf`）。
若输入不存在，脚本会自动创建一个空白 PDF 用于测试。*

---

## 5. `dxpdfd`：DOCX → PDF 常驻服务

业务动作：**上传用户 DOCX + 两个业务参数 → 填模板占位符 → 把用户内容原封不动追加到模板末尾 → 整份转 PDF → 返回下载地址**。

```text
POST /convert          multipart/form-data
                         file=<用户 docx>（必填）
                         Fund_cnname=<基金中文名>（必填）
                         letters_date=<日期字面量>（必填）
                         image_dpi=<可选>
  → 200 {"id":"...","url":"http://host/download/<id>","pages":N,...}
GET  /download/<id>    → 200 application/pdf
GET  /healthz          → 200 {"status":"ok",...}
```

```bash
cargo build --release --bin dxpdfd
./target/release/dxpdfd
```

结果只落在进程内存里（带 TTL），不写磁盘。模板编译进二进制，可用 `DXPDFD_TEMPLATE` 覆盖。配置项（全部走环境变量）：

| 环境变量 | 默认值 | 说明 |
|---|---|---|
| `DXPDFD_ADDR` | `127.0.0.1:8080` | 监听地址 |
| `DXPDFD_TEMPLATE` | 内置模板 | 模板 DOCX 路径 |
| `DXPDFD_CONCURRENCY` | CPU 核数 | 并发上限（render 是同步 CPU 密集，0 会让请求全部 503） |
| `DXPDFD_MAX_BODY_MB` | `64` | 请求体上限 |
| `DXPDFD_IMAGE_DPI` | `dxpdf::DEFAULT_IMAGE_DPI` | 图片默认 DPI |
| `DXPDFD_TTL_SECS` | `1800` | 结果保留时长 |
| `DXPDFD_MAX_RESULTS` | `64` | 内存中最多保留的结果数 |
| `DXPDFD_MAX_RESULT_MB` | `256` | 单个结果大小上限 |
| `DXPDFD_PUBLIC_BASE` | 空（按 `Host` 头推断） | 反向代理/HTTPS 下必须显式设置，否则下载地址错误 |
| `DXPDFD_DEBUG_DUMP_DIR` | 未设置 | 设了会把「模板+用户内容」的合并 DOCX 落盘，便于人工核对 |

## 6. 方案验证工具（DOCX → PDF）

这些 bin 用于横向对比不同转换方案，均不影响正式服务：

```bash
# 对比 1：office2pdf（Typst 链路），带 parse / codegen / compile 分阶段耗时
cargo run --release --bin docx2pdf   -- in.docx docx2pdf-out.pdf

# 对比 2：dxpdf + Skia，带 unzip / patch_xml / rezip / parse / render 分阶段耗时
cargo run --release --bin dxpdf2pdf  -- in.docx dxpdf-out.pdf

# 对比 3：libreoffice-pure（纯 Rust，进程内转换）
cargo run --release --bin lopure2pdf -- in.docx out.pdf [--repeat=N]

# 对比 4：unoserver 常驻（真 LibreOffice，需先启动 unoserver 且装好 unoconvert）
cargo run --release --bin unoserver2pdf -- in.docx out.pdf \
    [--host=127.0.0.1] [--port=2003] [--repeat=3]

# 临时基准：量化 Skia 字形 path 生成成本（定位 paint 慢点用）
cargo run --release --bin bench_glyph -- glyph|pdf|pdfone|pdfrep|raster <font> [n] [size]
```

> `dxpdf2pdf` 会先预处理 DOCX（补全 `dxpdf` 必填但非 Word 生成的 `w:ilvl` 属性），
> 这段逻辑被 `dxpdfd` 直接拷贝复用。

## 7. 浏览器预览水印效果

直接用浏览器打开 `preview.html`（基于 CDN 上的 PDF.js），选择 PDF 即可分页预览效果：

```bash
# 或用任意静态服务器
python3 -m http.server 8000   # 然后访问 http://localhost:8000/preview.html
```

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
./target/release/rpad serve

# 本地一次性转换（不走 S3，便于回归对比）
./target/release/rpad oneshot \
    --template template-glv_template2-*.docx --src test_src_ys.docx \
    --fund-cnname 某某基金 --letters-date 2026-09-17 --out out.pdf
```

> 注意：首次构建 skia 需要从 github 下载预编译产物；若网络不通，可参考 §3.5 的
> `SKIA_BINARIES_URL` 本地预编译包方案，或设置 `HTTPS_PROXY`（例如本机 `http://127.0.0.1:7897`）。

## 验证记录（2026-09-17）

用仓库真实模板 + `test_src_ys.docx`：占位符替换 3/3（document.xml ×2 +
header4.xml ×1），`backfilled=23 media=1 notes=1`，输出 5 页 PDF；HTTP 契约
（`/`、409 文案、404、/healthz 计数）逐项比对通过。

## 许可证

MIT License
