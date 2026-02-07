<?php
// 1. 定义必须与 Rust 中的参数个数、类型完全匹配
$ffi = FFI::cdef("
    int add_pdf_watermark(const char* input, const char* output, const char* font, const char* name, const char* date);
", __DIR__ . "/target/release/libwater_mark.so"); // 建议使用绝对路径

// 2. 准备参数 (注意这里没有 $output 了)
$input = __DIR__ . "/in.pdf";
$output = __DIR__ . "/out.pdf";
$font = __DIR__ . "/STSongStd-Light-Acro/STSongStd-Light-Acro.otf";
$name = "张三";
$date = "2026-02-05";

// 3. 调用 Rust 函数
$result = $ffi->add_pdf_watermark($input, $output, $font, $name, $date);

if ($result === 0) {
    echo "水印添加成功！生成了 in_watermarked.pdf\n";
} else {
    echo "水印添加失败，返回值: $result\n";
}
