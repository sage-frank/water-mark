import fitz  # PyMuPDF
from fontTools import subset
from fontTools.ttLib import TTFont
import io
import os
import math

def create_subset_font(font_path, text):
    """
    提取字体子集，仅包含指定的字符。
    关键：保留 cmap 表以确保在 PDF.js 等浏览器环境中能正确渲染。
    """
    print(f"正在提取字体子集: {font_path}")
    
    # 确保包含一些基础字符，防止数字或标点缺失
    # 注意：subsetter 会自动处理 text 中的重复字符
    subset_text = text + " 0123456789-:,"
    
    options = subset.Options()
    
    # 设置 flavor 为 None，保持原始格式 (通常是 TTF 或 OTF)
    # PyMuPDF 支持原始字体数据
    options.flavor = None
    
    # 关键设置：保留 GID (Glyph IDs) 和 CMap
    # PDF.js 在渲染子集字体时，如果 GID 被重排且 ToUnicode 映射不完整，可能会显示乱码或数字
    options.retain_gids = True
    
    # 对于 OTF (CFF) 字体，解包子程序有助于提高兼容性
    options.desubroutinize = True
    
    # 显式防止丢弃关键表
    # 虽然默认不会丢弃 cmap，但显式保留更安全
    # 这里的 '*' 表示不丢弃任何表，或者我们可以指定空列表 []
    options.drop_tables = []
    
    # 保留名称表，有时浏览器依赖它
    options.name_IDs = '*'
    options.name_languages = '*'
    
    font = TTFont(font_path)
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(text=subset_text)
    subsetter.subset(font)
    
    # 保存到内存 buffer
    buf = io.BytesIO()
    font.save(buf)
    buf.seek(0)
    
    print(f"字体子集提取完成，大小: {len(buf.getvalue()) / 1024:.2f} KB")
    return buf.getvalue()

def add_watermark(input_pdf, output_pdf, font_path, text, font_size=26, opacity=0.1, angle=60):
    """
    给 PDF 添加平铺水印，使用提取的子集字体以减小文件体积。
    """
    # 1. 提取子集字体
    font_buffer = create_subset_font(font_path, text)
    # 创建 PyMuPDF 字体对象 (用于测量宽度)
    watermark_font = fitz.Font(fontname="WatermarkFont", fontbuffer=font_buffer)
    
    # 2. 打开 PDF
    doc = fitz.open(input_pdf)
    
    print("开始添加水印...")
    
    # 预计算一些参数
    # 颜色: 深灰色 (0.1, 0.1, 0.1)
    color = (0.1, 0.1, 0.1)
    
    for page_index, page in enumerate(doc):
        # 必须显式注册字体，才能在 insert_text 中通过 fontname 引用
        # 这样可以确保嵌入的是子集字体
        page.insert_font(fontname="WatermarkFont", fontbuffer=font_buffer)
        
        w = page.rect.width
        h = page.rect.height
        
        # 估算文本宽度
        text_len = watermark_font.text_length(text, fontsize=font_size)
        
        step_x = text_len + 40  # 水平间距
        step_y = font_size * 6  # 垂直间距
        
        # 旋转矩阵参数
        rad = math.radians(angle)
        cos_a = math.cos(rad)
        sin_a = math.sin(rad)
        
        # 覆盖范围
        range_x = w * 2.5
        range_y = h * 2.5
        
        y = -range_y
        while y < range_y:
            x = -range_x
            while x < range_x:
                # 坐标变换
                rx = x * cos_a - y * sin_a
                ry = x * sin_a + y * cos_a
                
                # 使用 morph 参数支持任意角度旋转
                # insert_text 的 rotate 参数在某些版本仅支持 90 度倍数
                point = fitz.Point(rx, ry)
                mat = fitz.Matrix(angle)
                
                page.insert_text(
                    point,
                    text,
                    fontname="WatermarkFont",
                    fontsize=font_size,
                    morph=(point, mat),
                    color=color,
                    fill_opacity=opacity
                )
                
                x += step_x
            y += step_y
        
    # 3. 保存
    # garbage=4, deflate=True 进一步压缩
    doc.save(output_pdf, garbage=4, deflate=True)
    print(f"水印添加完成，已保存至: {output_pdf}")

if __name__ == "__main__":
    # 配置路径
    input_pdf = "in.pdf"  # 输入 PDF
    output_pdf = "output_py_watermarked.pdf"
    
    # 字体路径
    font_path = r"D:\code\rust\water_mark\STSongStd-Light-Acro\STSongStd-Light-Acro.otf"
    
    # 水印内容
    name = "张三"
    date = "2026-02-05"
    text = f"作者:{name}, 日期:{date} 内部查看"
    
    # 检查文件是否存在
    if not os.path.exists(font_path):
        print(f"错误: 找不到字体文件 {font_path}")
        # 尝试使用当前目录下的字体
        font_path = "STSongStd-Light-Acro.otf"
        if not os.path.exists(font_path):
            print("请确保字体文件存在。")
            exit(1)
            
    if not os.path.exists(input_pdf):
        print(f"错误: 找不到输入文件 {input_pdf}")
        # 创建一个空白 PDF 用于测试
        print("创建一个空白 in.pdf 用于测试...")
        doc = fitz.open()
        doc.new_page()
        doc.save("in.pdf")
        doc.close()
    
    try:
        add_watermark(input_pdf, output_pdf, font_path, text)
    except Exception as e:
        print(f"发生错误: {e}")
