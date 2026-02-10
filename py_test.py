import ctypes
from ctypes import c_char_p, c_int32
import os

import platform

# 1. 加载动态链接库
# 自动检测系统类型选择正确的库文件
system_name = platform.system()
if system_name == "Windows":
    lib_name = "water_mark.dll"
elif system_name == "Darwin":
    lib_name = "libwater_mark.dylib"
else:
    lib_name = "libwater_mark.so"

lib_path = os.path.abspath(os.path.join("./target/release", lib_name))

if not os.path.exists(lib_path):
    print(f"Error: Library file not found at {lib_path}")
    print("Please run 'cargo build --release' first.")
    exit(1)

lib = ctypes.CDLL(lib_path)

# 2. 定义函数签名 (Argtypes & Restype)
# 这步非常重要，能防止传参类型错误导致段错误 (Segmentation fault)
lib.add_pdf_watermark.argtypes = [c_char_p, c_char_p, c_char_p, c_char_p, c_char_p]
lib.add_pdf_watermark.restype = c_int32

def add_watermark(pdf_path, out_path, font_path, name, date):
    # 3. 将 Python 字符串编码为字节流 (bytes) 传给 C 接口
    # 因为 Rust 接收的是 const char* (C 字符串)
    b_pdf = pdf_path.encode('utf-8')
    b_out = out_path.encode('utf-8')
    b_font = font_path.encode('utf-8')
    b_name = name.encode('utf-8')
    b_date = date.encode('utf-8')

    # 4. 执行调用
    result = lib.add_pdf_watermark(b_pdf, b_out, b_font, b_name, b_date)
    return result

# 使用测试
if __name__ == "__main__":
    pdf = "in.pdf"
    out = "out.pdf"
    font = "./STSongStd-Light-Acro/STSongStd-Light-Acro.otf"
    user = "张三"
    date_str = "2026-02-05"

    print(f"正在处理: {pdf}...")
    res = add_watermark(pdf, out, font, user, date_str)
    
    if res == 0:
        print("成功！水印已生成。")
    else:
        print(f"失败！错误代码: {res}")
