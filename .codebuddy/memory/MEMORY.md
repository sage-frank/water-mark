# water-mark 项目长期记忆

## 项目概况
- Rust + PHP(FFI)：`src/lib.rs` 是核心（cdylib 供 PHP 调用，rlib 供 main.rs 调用），实现 PDF 矢量水印（字形轮廓转 PDF 路径 → Form XObject → 每页网格 `Do`）。
- 多个 `[[bin]]`：`water_mark_cli`(src/main.rs) / `docx2pdf` / `dxpdf2pdf` / `lopure2pdf` / `unoserver2pdf` / `convertd` / `bench_glyph`。
- `[patch.crates-io] dxpdf = { path = "vendor/dxpdf" }`：临时指向本地副本给 paint 阶段加计时埋点，定位完成后应删除该段。

## 构建环境（Kylin Linux Advanced Server V10 / ks10）
- 链接 `dxpdf` 需要 `freetype-devel`、`fontconfig-devel`；只有运行时库会报 `cannot find -lfreetype/-lfontconfig`。
- 已知环境限制：Kylin 镜像源 `update.cs2c.com.cn` 常超时；本机无 pdftoppm/qpdf/ghostscript/mutool、无 pypdf/pymupdf。
- 可视验证手段：项目自带 `preview.html`（PDF.js，需外网 CDN）。

## 代码约定 / 坑
- lopdf 0.33：`Dictionary::get/get_mut` **不跟随间接引用**，拿到 `Object::Reference` 时 `as_dict_mut()` 会返回 `lopdf::Error::Type`（Debug 输出就是字面量 `Type`）。
  PDF 中 `/Resources`、`/Resources/XObject` 都可能是间接引用，取值前必须先判断/解引用。
- 用 `doc.get_object(id)` / `get_object_mut(id)` / `get_dictionary_mut(id)` 可迭代解引用。
- 避免嵌套 `&mut Document`：多层容器改写应「先只读解析形态 → 再走最短的一条可变借用路径」。
- 临时埋点/基准文件（`bench_glyph.rs`、skia-safe 直接依赖、`vendor/dxpdf`）用完后应清理。
