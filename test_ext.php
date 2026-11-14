<?php
// ============================================================================
// watermark 扩展调用示例（对比 test.php 的 FFI 方式）
//
// 前提（Linux 服务器）：
//   1. cd water_mark/php_ext && cargo build --release
//   2. cp target/release/libwatermark.so "$(php-config --extension-dir)/watermark.so"
//   3. php.ini 中添加：extension=watermark.so，并重启 php-fpm
//   4. 验证加载：php -m | grep watermark
// ============================================================================

if (!function_exists('watermark_add')) {
    exit("watermark 扩展未加载，请检查 php.ini 中的 extension=watermark.so\n");
}

$font = __DIR__ . '/SourceHanSerifCN-Bold.otf';

// ---------------------------------------------------------------------------
// 示例 1：单文件加水印
// ---------------------------------------------------------------------------
$input  = __DIR__ . '/in.pdf';
$output = __DIR__ . '/out_ext.pdf';

try {
    // 与 FFI 版参数一致：输入、输出、字体、用户名、日期
    // 成功返回 true，失败直接抛 Exception（无需检查返回码）
    watermark_add($input, $output, $font, '张三', '2026-02-05');
    echo "水印添加成功：{$output}\n";
} catch (Throwable $e) {
    echo "水印添加失败：", $e->getMessage(), "\n";
}

// ---------------------------------------------------------------------------
// 示例 2：批量——扫描目录，输出 water-mark + 同名文件
// ---------------------------------------------------------------------------
$dir = __DIR__ . '/pdf';

foreach (glob($dir . '/*.pdf') as $file) {
    $name = basename($file);

    // 跳过本工具的输出，避免重复加水印
    if (str_starts_with($name, 'water-mark')) {
        continue;
    }

    try {
        watermark_add(
            $file,
            $dir . '/water-mark' . $name,
            $font,
            '张三',
            '2026-02-05'
        );
        echo "[OK]   water-mark{$name}\n";
    } catch (Throwable $e) {
        echo "[FAIL] {$name}: ", $e->getMessage(), "\n";
    }
}
