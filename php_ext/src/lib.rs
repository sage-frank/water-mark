//! watermark —— water_mark 的 PHP 原生扩展封装。
//!
//! 与 `test.php` 使用的 FFI 方式不同：本 crate 用 [ext-php-rs] 把
//! `water_mark::run_watermark_process` 编译成真正的 PHP 扩展（.so），
//! 在 php.ini 配置后即可直接调用全局函数 `watermark_add()`，
//! 不需要 `FFI` 扩展、不需要 `FFI::cdef()`，类型转换与异常都由扩展完成。
//!
//! # Linux 编译（需 PHP 开发环境：php、php-dev/php-devel、php-config）
//!
//! ```sh
//! # Debian/Ubuntu:  apt install php-dev
//! # CentOS/RHEL:    yum install php-devel
//! cd water_mark/php_ext
//! cargo build --release
//! # 产物: target/release/libwatermark.so
//!
//! # 安装到 PHP 扩展目录（改名去掉 lib 前缀，符合 PHP 命名习惯）
//! cp target/release/libwatermark.so "$(php-config --extension-dir)/watermark.so"
//! ```
//!
//! # php.ini 配置
//!
//! ```ini
//! extension=watermark.so
//! ```
//!
//! 修改后重启 php-fpm / Web 服务；CLI 验证：
//!
//! ```sh
//! php -m | grep watermark
//! ```
//!
//! 注意：编译时用的 PHP（php-config 指向的）必须与运行时的 PHP
//! 版本、线程安全（ZTS/NTS）一致，否则扩展加载会失败。
//!
//! [ext-php-rs]: https://ext-php.rs/

// Windows 编译需要 nightly 的 abi_vectorcall 特性；Linux 上此行无副作用
#![cfg_attr(windows, feature(abi_vectorcall))]

use ext_php_rs::prelude::*;

/// 给 PDF 添加平铺矢量水印。
///
/// PHP 侧签名：
///
/// ```php
/// watermark_add(
///     string $input,   // 输入 PDF 路径
///     string $output,  // 输出 PDF 路径
///     string $font,    // 字体文件路径（.otf/.ttf）
///     string $user,    // 用户名，如 "张三"
///     string $date,    // 日期，如 "2026-02-05"
/// ): bool
/// ```
///
/// 成功返回 `true`；失败抛出 PHP `Exception`（消息含具体原因），
/// 调用方用 try/catch 处理即可。
#[php_function]
pub fn watermark_add(
    input: &str,
    output: &str,
    font: &str,
    user: &str,
    date: &str,
) -> PhpResult<bool> {
    // 与 lib.rs 的 FFI 入口 add_pdf_watermark 保持一致的水印文案
    let text = format!("致{}-{}:高度保密", user, date);

    water_mark::run_watermark_process(input, output, font, &text)
        .map_err(|e| PhpException::default(format!("PDF 加水印失败: {}", e)))?;

    Ok(true)
}

/// 扩展入口：PHP 加载 .so 时通过 get_module 发现本扩展。
/// 扩展名即这里的模块名 "watermark"。
#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    module.function(wrap_function!(watermark_add))
}
