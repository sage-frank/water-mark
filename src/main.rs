use ab_glyph::{Font, FontRef, PxScale, OutlineCurve, Point, ScaleFont};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId, Stream};
use lopdf::dictionary;
use std::time::Instant;
use std::env;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start_time = Instant::now();

    // 获取命令行参数
    let args: Vec<String> = env::args().collect();
    let input_path = args.get(1).map(|s| s.as_str()).unwrap_or("in.pdf");

    println!("正在加载 PDF: {}", input_path);
    let mut doc = Document::load(input_path)?;
    
    println!("正在加载字体...");
    let font_path = ".\\STSongStd-Light-Acro\\STSongStd-Light-Acro.otf";
    let font_data = std::fs::read(font_path).map_err(|e| {
        format!("无法读取字体文件 {}: {}", font_path, e)
    })?;
    let font = FontRef::try_from_slice(&font_data)?;

    let name = "张三";
    let date = "2026-02-05";
    let text = format!("致{}-{}:高度保密", name, date);
    
    println!("创建水印 Form XObject...");

    // 调整：字体大小改为 26.0
    let watermark_ops = text_to_pdf_paths(&font, &text, 0.0, 0.0, 26.0);
    let watermark_content = Content { operations: watermark_ops };
    let watermark_stream = Stream::new(dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        // BBox 扩大范围，涵盖可能的负坐标和长文本
        // 字体坐标系 y 向上，基线以下（如 g, j, p, q, y）会有负 y 值
        "BBox" => vec![(-10).into(), (-50).into(), 2000.into(), 200.into()],
        "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
        "Resources" => dictionary! {
            "ExtGState" => dictionary! {
                "GS1" => dictionary! {
                    "Type" => "ExtGState",
                    "ca" => 0.1,
                    "CA" => 0.1,
                }
            }
        },
    }, watermark_content.encode().unwrap());
    
    let xobject_id = doc.add_object(watermark_stream);
    let xobject_name = "Watermark1";

    println!("注入水印到每一页...");
    for (page_num, object_id) in doc.get_pages() {
    let (w, h) = page_size(&doc, object_id).unwrap_or((595.0, 842.0));
    let rotation = get_page_rotation(&doc, object_id);
    
    // 1. 尝试添加资源，如果失败说明页面结构非标准
    if let Err(_) = add_xobject_to_page(&mut doc, object_id, xobject_name, xobject_id) {
        println!("警告：第 {} 页结构非标准，无法注入水印资源。", page_num);
        continue;
    }

    let ops = build_watermark_grid_ops_xobject(xobject_name, 26.0, 60.0, w, h, rotation, &font, &text);
    let content_ops = Content { operations: ops };

    // 2. 使用 match 捕获注入错误
    match doc.add_to_page_content(object_id, content_ops) {
        Ok(_) => (), // 成功处理
        Err(e) => {
            // 如果报错信息包含 "Type"，说明是该文件的内容流定义不标准
            if format!("{:?}", e).contains("Type") {
                println!("跳过第 {} 页：该页采用非标准 PDF 结构，无法直接解析注入。", page_num);
            } else {
                println!("跳过第 {} 页：发生未知错误 ({:?})", page_num, e);
            }
            continue;
        }
    }
}

    println!("保存文件...");
    
    // 根据输入文件名生成输出文件名
   
        let path = Path::new(input_path);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
      let output_path = format!("{}_watermarked.pdf", stem);
    

    doc.save(&output_path)?;
    
    let duration = start_time.elapsed();
    println!("Rust 矢量水印生成成功！保存为 {}", output_path);
    println!("总耗时: {:.2?}", duration);
    Ok(())
}

fn text_to_pdf_paths(font: &FontRef, text: &str, x_start: f32, y_start: f32, size: f32) -> Vec<Operation> {
    let scale = PxScale::from(size);
    let scaled_font = font.as_scaled(scale);
    let h_factor = scaled_font.h_scale_factor();
    let v_factor = scaled_font.v_scale_factor();
    
    let mut ops = vec![
        Operation::new("q", vec![]), // 保存图形状态
        // 设置扩展图形状态 (透明度)
        Operation::new("gs", vec!["GS1".into()]),
        // 调整：颜色改为深灰色 (0.1, 0.1, 0.1)
        Operation::new("rg", vec![0.1.into(), 0.1.into(), 0.1.into()]), 
    ];

    let mut x_cursor = x_start;
    
    for c in text.chars() {
        let glyph_id = font.glyph_id(c);
        
        if let Some(outline) = font.outline(glyph_id) {
            let mut last_point = Point { x: f32::NAN, y: f32::NAN };
            
            for curve in outline.curves {
                let p0 = match curve {
                    OutlineCurve::Line(p0, _) => p0,
                    OutlineCurve::Quad(p0, _, _) => p0,
                    OutlineCurve::Cubic(p0, _, _, _) => p0,
                };

                // 检查是否需要移动到新起点 (新轮廓开始)
                // 浮点数比较需要 epsilon，但这里我们主要关心是否是同一个点序列
                let is_new_contour = last_point.x.is_nan() || 
                                     (p0.x - last_point.x).abs() > 0.001 || 
                                     (p0.y - last_point.y).abs() > 0.001;

                if is_new_contour {
                    // 如果不是第一个轮廓，闭合上一个
                    if !last_point.x.is_nan() {
                        ops.push(Operation::new("h", vec![]));
                    }
                    
                    // 移动到新起点
                    ops.push(Operation::new("m", vec![
                        (x_cursor + p0.x * h_factor).into(),
                        (y_start + p0.y * v_factor).into()
                    ]));
                }

                // 绘制曲线
                match curve {
                    OutlineCurve::Line(_, p1) => {
                        ops.push(Operation::new("l", vec![
                            (x_cursor + p1.x * h_factor).into(),
                            (y_start + p1.y * v_factor).into()
                        ]));
                        last_point = p1;
                    }
                    OutlineCurve::Quad(p0, p1, p2) => {
                        // 二次贝塞尔转三次
                        // Q1 = P0 + 2/3 * (P1 - P0)
                        // Q2 = P2 + 2/3 * (P1 - P2)
                        
                        let q1_x = p0.x + (2.0/3.0) * (p1.x - p0.x);
                        let q1_y = p0.y + (2.0/3.0) * (p1.y - p0.y);
                        
                        let q2_x = p2.x + (2.0/3.0) * (p1.x - p2.x);
                        let q2_y = p2.y + (2.0/3.0) * (p1.y - p2.y);
                        
                        ops.push(Operation::new("c", vec![
                            (x_cursor + q1_x * h_factor).into(), (y_start + q1_y * v_factor).into(),
                            (x_cursor + q2_x * h_factor).into(), (y_start + q2_y * v_factor).into(),
                            (x_cursor + p2.x * h_factor).into(), (y_start + p2.y * v_factor).into()
                        ]));
                        last_point = p2;
                    }
                    OutlineCurve::Cubic(_, p1, p2, p3) => {
                        ops.push(Operation::new("c", vec![
                            (x_cursor + p1.x * h_factor).into(), (y_start + p1.y * v_factor).into(),
                            (x_cursor + p2.x * h_factor).into(), (y_start + p2.y * v_factor).into(),
                            (x_cursor + p3.x * h_factor).into(), (y_start + p3.y * v_factor).into()
                        ]));
                        last_point = p3;
                    }
                }
            }
            // 闭合最后一个轮廓
            if !last_point.x.is_nan() {
                 ops.push(Operation::new("h", vec![]));
            }
        }
        
        // 更新 x_cursor
        x_cursor += scaled_font.h_advance(glyph_id);
    }

    ops.push(Operation::new("f", vec![])); // 填充所有路径
    ops.push(Operation::new("Q", vec![])); // 恢复状态
    ops
}

fn measure_text_width(font: &FontRef, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        w += scaled.h_advance(gid);
    }
    w
}

fn page_size(doc: &Document, page_id: ObjectId) -> Option<(f32, f32)> {
    let page_obj = doc.get_object(page_id).ok()?;
    match page_obj {
        Object::Dictionary(dict) => {
            if let Ok(mb_obj) = dict.get(b"MediaBox") {
                if let Object::Array(arr) = mb_obj {
                    if arr.len() >= 4 {
                        let llx = obj_to_f32(&arr[0]);
                        let lly = obj_to_f32(&arr[1]);
                        let urx = obj_to_f32(&arr[2]);
                        let ury = obj_to_f32(&arr[3]);
                        return Some((urx - llx, ury - lly));
                    }
                }
            }
            None
        }
        Object::Stream(stream) => {
            if let Ok(mb_obj) = stream.dict.get(b"MediaBox") {
                if let Object::Array(arr) = mb_obj {
                    if arr.len() >= 4 {
                        let llx = obj_to_f32(&arr[0]);
                        let lly = obj_to_f32(&arr[1]);
                        let urx = obj_to_f32(&arr[2]);
                        let ury = obj_to_f32(&arr[3]);
                        return Some((urx - llx, ury - lly));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn obj_to_f32(o: &Object) -> f32 {
    match o {
        Object::Real(r) => *r as f32,
        Object::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

fn get_page_rotation(doc: &Document, page_id: ObjectId) -> f32 {
    let mut current_id = Some(page_id);
    
    // 向上遍历 Parent 链查找 Rotate 属性
    while let Some(id) = current_id {
        if let Ok(obj) = doc.get_object(id) {
            match obj {
                Object::Dictionary(dict) => {
                    // 1. 检查当前节点是否有 Rotate
                    if let Ok(rotate_obj) = dict.get(b"Rotate") {
                        // 处理 Rotate 是 Integer 或 Reference 的情况
                        let rotate_val = match rotate_obj {
                            Object::Integer(r) => Some(*r),
                            Object::Reference(ref_id) => {
                                // 解析引用
                                if let Ok(Object::Integer(r)) = doc.get_object(*ref_id) {
                                    Some(*r)
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        
                        if let Some(r) = rotate_val {
                            return r as f32;
                        }
                    }
                    
                    // 2. 获取 Parent ID 继续向上查找
                    if let Ok(Object::Reference(parent_id)) = dict.get(b"Parent") {
                        current_id = Some(*parent_id);
                    } else {
                        current_id = None;
                    }
                }
                Object::Stream(stream) => {
                    // Stream 对象也可能有 dictionary
                    if let Ok(rotate_obj) = stream.dict.get(b"Rotate") {
                         let rotate_val = match rotate_obj {
                            Object::Integer(r) => Some(*r),
                            Object::Reference(ref_id) => {
                                if let Ok(Object::Integer(r)) = doc.get_object(*ref_id) {
                                    Some(*r)
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        
                        if let Some(r) = rotate_val {
                            return r as f32;
                        }
                    }
                     if let Ok(Object::Reference(parent_id)) = stream.dict.get(b"Parent") {
                        current_id = Some(*parent_id);
                    } else {
                        current_id = None;
                    }
                }
                _ => current_id = None,
            }
        } else {
            break;
        }
    }
    
    0.0
}

fn add_xobject_to_page(
    doc: &mut Document,
    page_id: ObjectId,
    xobject_name: &str,
    xobject_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    // 正确的做法是重新获取 page dict
    if let Object::Dictionary(page_dict) = doc.get_object_mut(page_id)? {
        if !page_dict.has(b"Resources") {
             page_dict.set(b"Resources", dictionary! {});
        }
        let resources = page_dict.get_mut(b"Resources")?.as_dict_mut()?;
        
        if !resources.has(b"XObject") {
            resources.set(b"XObject", dictionary! {});
        }
        let xobjects = resources.get_mut(b"XObject")?.as_dict_mut()?;
        
        xobjects.set(xobject_name.as_bytes().to_vec(), Object::Reference(xobject_id));
    }
    
    Ok(())
}



fn build_watermark_grid_ops_xobject(
    xobject_name: &str,
    size: f32,
    angle_deg: f32,
    width: f32,
    height: f32,
    _page_rotation: f32,
    font: &FontRef,
    text: &str,
) -> Vec<Operation> {
    let text_w = measure_text_width(font, text, size);
    let step_inner = text_w + 30.0; 
    let step_outer = size * 6.0;   

    let rad = angle_deg.to_radians();
    let (c, s) = (rad.cos(), rad.sin());

    let mut ops = Vec::new();

    let diag = (width.powi(2) + height.powi(2)).sqrt() * 2.5; // 从 2.0 增加到 2.5

    // 2. 偏移中心点：为了填补右下角，我们把逻辑中心往右下角“压”一点
    // 在 PDF 坐标系中（左下角为 0,0），增加 cx 是向右，减少 cy 是向下
    let cx = width / 2.0 + 50.0;  // 往右挪 50 像素
    let cy = height / 2.0 - 100.0; // 往下挪 100 像素，这能最有效地把右下角缺失的那一行拽出来

    // 3. 扫描起始位置：让 v 从更小的负数开始扫，确保覆盖右下角的负向空间
    let mut v = -diag - 200.0; // 额外给一个 buffer
    while v < diag {
        let mut u = -diag;
        while u < diag {
            let x = cx + u * c - v * s;
            let y = cy + u * s + v * c;

            // 4. 判定条件：放宽 y 轴的底部判定（y > -200.0）
            // 确保被“拽”下来的那一行不会因为 y 坐标稍微小于 -size 而被过滤掉
            if x > -text_w && x < width + 100.0 && y > -200.0 && y < height + 100.0 {
                ops.push(Operation::new("q", vec![]));
                ops.push(Operation::new("cm", vec![
                    c.into(), s.into(),
                    (-s).into(), c.into(),
                    x.into(), y.into(),
                ]));
                ops.push(Operation::new("Do", vec![xobject_name.into()]));
                ops.push(Operation::new("Q", vec![]));
            }
            u += step_inner;
        }
        v += step_outer;
    }
    ops
}