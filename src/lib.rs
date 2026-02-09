use ab_glyph::{Font, FontRef, PxScale, OutlineCurve, Point, ScaleFont};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId, Stream};
use lopdf::dictionary;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

const DEFAULT_FONT_SIZE: f32 = 26.0;
const GRID_HORIZONTAL_GAP: f32 = 30.0;
const GRID_VERTICAL_MULTIPLIER: f32 = 6.0;
const WATERMARK_ANGLE_DEG: f32 = 60.0;
const CENTER_X_OFFSET: f32 = 50.0;
const CENTER_Y_OFFSET: f32 = 100.0;
const COVERAGE_MULTIPLIER: f32 = 2.5;
const VISIBILITY_MARGIN: f32 = 50.0;
const MAX_ALLOWED_WATERMARKS: usize = 1_000_000;

// --- FFI 接口：供其他语言调用 ---
#[unsafe(no_mangle)]
pub extern "C" fn add_pdf_watermark(
    input_path: *const c_char,
    output_path: *const c_char,
    font_path: *const c_char,
    user_name: *const c_char,
    date_str: *const c_char,
) -> i32 {
    // 参数空指针检查
    if input_path.is_null()
        || output_path.is_null()
        || font_path.is_null()
        || user_name.is_null()
        || date_str.is_null()
    {
        eprintln!("ERROR: NULL pointer passed to add_pdf_watermark");
        return -2;
    }

    // 将 CStr 转为 &str 并校验 UTF-8
    let input = unsafe {
        match CStr::from_ptr(input_path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in input_path");
                return -3;
            }
        }
    };
    let output = unsafe {
        match CStr::from_ptr(output_path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in output_path");
                return -3;
            }
        }
    };
    let font_p = unsafe {
        match CStr::from_ptr(font_path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in font_path");
                return -3;
            }
        }
    };
    let name = unsafe {
        match CStr::from_ptr(user_name).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in user_name");
                return -3;
            }
        }
    };
    let date = unsafe {
        match CStr::from_ptr(date_str).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                eprintln!("ERROR: Invalid UTF-8 in date_str");
                return -3;
            }
        }
    };

    let text = format!("致{}-{}:高度保密", name, date);

    match run_watermark_process(&input, &output, &font_p, &text) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("ERROR: add_pdf_watermark failed: {:?}", e);
            -1
        }
    }
}

// --- 公共处理函数：供 main.rs 和 FFI 调用 ---
pub fn run_watermark_process(
    input_path: &str,
    output_path: &str,
    font_path: &str,
    text: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // 加载 PDF
    let mut doc = Document::load(input_path)?;

    // 读取并解析字体（一次性）
    let font_data = std::fs::read(font_path)?;
    let font = FontRef::try_from_slice(&font_data)?;

    // 预计算文本矢量（只做一次）
    let watermark_ops = text_to_pdf_paths(&font, text, 0.0, 0.0, DEFAULT_FONT_SIZE)?;
    let watermark_content = Content {
        operations: watermark_ops,
    };
    let encoded = watermark_content
        .encode()
        .map_err(|e| format!("encode watermark content failed: {:?}", e))?;
    let watermark_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            // 使用常量而不是魔数（如果需要可以改为基于字体大小的 bbox）
            "BBox" => vec![(-10).into(), (-50).into(), 2000.into(), 200.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => dictionary! {
                "ExtGState" => dictionary! {
                    "GS1" => dictionary! {
                        "Type" => "ExtGState",
                        "ca" => 0.1f32, // fill alpha
                        "CA" => 0.1f32, // stroke alpha
                    }
                }
            },
        },
        encoded,
    );

    let xobject_id = doc.add_object(watermark_stream);
    let xobject_name = "Watermark1";

    // 预计算文本宽度，避免重复计算
    let text_w = measure_text_width(&font, text, DEFAULT_FONT_SIZE);

    // 遍历页面并注入资源与内容
    for (page_num, object_id) in doc.get_pages() {
        let (w, h) = page_size(&doc, object_id).unwrap_or((595.0, 842.0));
        let rotation = get_page_rotation(&doc, object_id);

        if let Err(e) = add_xobject_to_page(&mut doc, object_id, xobject_name, xobject_id) {
            eprintln!(
                "WARN: 第 {} 页结构非标准，无法注入资源。错误：{:?}",
                page_num, e
            );
            continue;
        }

        let ops = match build_watermark_grid_ops_xobject_optimized(
            xobject_name,
            DEFAULT_FONT_SIZE,
            WATERMARK_ANGLE_DEG,
            w,
            h,
            rotation,
            text_w,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN: 生成水印网格失败，跳过第 {} 页：{:?}", page_num, e);
                continue;
            }
        };

        let content_ops = Content { operations: ops };

        match doc.add_to_page_content(object_id, content_ops) {
            Ok(_) => {}
            Err(e) => {
                let se = format!("{:?}", e);
                if se.contains("Type") {
                    eprintln!("WARN: 跳过第 {} 页：非标准结构。", page_num);
                } else {
                    eprintln!("WARN: 跳过第 {} 页：未知错误 ({:?})", page_num, e);
                }
                continue;
            }
        }
    }

    doc.save(&output_path)?;

    // 验证文件确实保存
    if !Path::new(output_path).exists() {
        return Err("输出文件保存失败".into());
    }

    Ok(output_path.to_string())
}

// --- 内部算法逻辑 (私有) ---

fn text_to_pdf_paths(
    font: &FontRef,
    text: &str,
    x_start: f32,
    y_start: f32,
    size: f32,
) -> Result<Vec<Operation>, Box<dyn std::error::Error>> {
    let scale = PxScale::from(size);
    let scaled_font = font.as_scaled(scale);
    let h_factor = scaled_font.h_scale_factor();
    let v_factor = scaled_font.v_scale_factor();

    let mut ops = vec![
        Operation::new("q", vec![]),
        Operation::new("gs", vec!["GS1".into()]),
        Operation::new("rg", vec![0.1.into(), 0.1.into(), 0.1.into()]),
    ];

    let mut x_cursor = x_start;
    for c in text.chars() {
        let glyph_id = font.glyph_id(c);
        if let Some(outline) = font.outline(glyph_id) {
            // 使用 Option<Point> 替代 NaN 作为 sentinel
            let mut last_point: Option<Point> = None;
            for curve in outline.curves {
                let p0 = match curve {
                    OutlineCurve::Line(p0, _) => p0,
                    OutlineCurve::Quad(p0, _, _) => p0,
                    OutlineCurve::Cubic(p0, _, _, _) => p0,
                };

                let is_new_contour = match last_point {
                    None => true,
                    Some(lp) => ((p0.x - lp.x).abs() > 0.001) || ((p0.y - lp.y).abs() > 0.001),
                };

                if is_new_contour {
                    if last_point.is_some() {
                        ops.push(Operation::new("h", vec![]));
                    }
                    ops.push(Operation::new(
                        "m",
                        vec![
                            (x_cursor + p0.x * h_factor).into(),
                            (y_start + p0.y * v_factor).into(),
                        ],
                    ));
                }

                match curve {
                    OutlineCurve::Line(_, p1) => {
                        ops.push(Operation::new(
                            "l",
                            vec![
                                (x_cursor + p1.x * h_factor).into(),
                                (y_start + p1.y * v_factor).into(),
                            ],
                        ));
                        last_point = Some(p1);
                    }
                    OutlineCurve::Quad(_, p1, p2) => {
                        let q1_x = p0.x + (2.0 / 3.0) * (p1.x - p0.x);
                        let q1_y = p0.y + (2.0 / 3.0) * (p1.y - p0.y);
                        let q2_x = p2.x + (2.0 / 3.0) * (p1.x - p2.x);
                        let q2_y = p2.y + (2.0 / 3.0) * (p1.y - p2.y);
                        ops.push(Operation::new(
                            "c",
                            vec![
                                (x_cursor + q1_x * h_factor).into(),
                                (y_start + q1_y * v_factor).into(),
                                (x_cursor + q2_x * h_factor).into(),
                                (y_start + q2_y * v_factor).into(),
                                (x_cursor + p2.x * h_factor).into(),
                                (y_start + p2.y * v_factor).into(),
                            ],
                        ));
                        last_point = Some(p2);
                    }
                    OutlineCurve::Cubic(_, p1, p2, p3) => {
                        ops.push(Operation::new(
                            "c",
                            vec![
                                (x_cursor + p1.x * h_factor).into(),
                                (y_start + p1.y * v_factor).into(),
                                (x_cursor + p2.x * h_factor).into(),
                                (y_start + p2.y * v_factor).into(),
                                (x_cursor + p3.x * h_factor).into(),
                                (y_start + p3.y * v_factor).into(),
                            ],
                        ));
                        last_point = Some(p3);
                    }
                }
            }
            if last_point.is_some() {
                ops.push(Operation::new("h", vec![]));
            }
        }
        x_cursor += scaled_font.h_advance(glyph_id);
    }
    ops.push(Operation::new("f", vec![]));
    ops.push(Operation::new("Q", vec![]));
    Ok(ops)
}

fn measure_text_width(font: &FontRef, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    for c in text.chars() {
        w += scaled.h_advance(font.glyph_id(c));
    }
    w
}

fn page_size(doc: &Document, page_id: ObjectId) -> Option<(f32, f32)> {
    let page_obj = doc.get_object(page_id).ok()?;
    let dict = match page_obj {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &s.dict,
        _ => return None,
    };
    if let Ok(Object::Array(arr)) = dict.get(b"MediaBox") {
        if arr.len() >= 4 {
            let llx = obj_to_f32(&arr[0]);
            let lly = obj_to_f32(&arr[1]);
            let urx = obj_to_f32(&arr[2]);
            let ury = obj_to_f32(&arr[3]);
            return Some((urx - llx, ury - lly));
        }
    }
    None
}

fn obj_to_f32(o: &Object) -> f32 {
    match o {
        Object::Real(r) => *r as f32,
        Object::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

fn get_page_rotation(doc: &Document, page_id: ObjectId) -> f32 {
    // 限制搜索深度，防止极端文档导致长时间循环
    let mut current_id = Some(page_id);
    let mut depth = 0usize;
    while let Some(id) = current_id {
        if depth > 10 {
            break;
        }
        if let Ok(obj) = doc.get_object(id) {
            let dict = match obj {
                Object::Dictionary(d) => d,
                Object::Stream(s) => &s.dict,
                _ => break,
            };
            if let Ok(rotate_obj) = dict.get(b"Rotate") {
                let r = match rotate_obj {
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
                if let Some(val) = r {
                    return val as f32;
                }
            }
            current_id = if let Ok(Object::Reference(p)) = dict.get(b"Parent") {
                Some(*p)
            } else {
                None
            };
        } else {
            break;
        }
        depth += 1;
    }
    0.0
}

fn add_xobject_to_page(
    doc: &mut Document,
    page_id: ObjectId,
    x_name: &str,
    x_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    // 获取可变页面对象
    let obj = doc.get_object_mut(page_id)?;
    match obj {
        Object::Dictionary(page_dict) => {
            if !page_dict.has(b"Resources") {
                page_dict.set(b"Resources", dictionary! {});
            }
            let resources = page_dict.get_mut(b"Resources")?.as_dict_mut()?;
            if !resources.has(b"XObject") {
                resources.set(b"XObject", dictionary! {});
            }
            let xobjects = resources.get_mut(b"XObject")?.as_dict_mut()?;
            xobjects.set(x_name.as_bytes().to_vec(), Object::Reference(x_id));
            Ok(())
        }
        Object::Stream(s) => {
            let page_dict = &mut s.dict;
            if !page_dict.has(b"Resources") {
                page_dict.set(b"Resources", dictionary! {});
            }
            let resources = page_dict.get_mut(b"Resources")?.as_dict_mut()?;
            if !resources.has(b"XObject") {
                resources.set(b"XObject", dictionary! {});
            }
            let xobjects = resources.get_mut(b"XObject")?.as_dict_mut()?;
            xobjects.set(x_name.as_bytes().to_vec(), Object::Reference(x_id));
            Ok(())
        }
        _ => Err("page object is not a Dictionary or Stream".into()),
    }
}

fn build_watermark_grid_ops_xobject_optimized(
    x_name: &str,
    size: f32,
    angle: f32,
    width: f32,
    height: f32,
    _rot: f32,
    text_w: f32,
) -> Result<Vec<Operation>, Box<dyn std::error::Error>> {
    let step_inner = text_w + GRID_HORIZONTAL_GAP;
    let step_outer = size * GRID_VERTICAL_MULTIPLIER;

    if !(step_inner > 0.0 && step_outer > 0.0) {
        return Err("Invalid grid parameters".into());
    }

    let rad = angle.to_radians();
    let (c, s) = (rad.cos(), rad.sin());

    let mut ops = Vec::new();

    let diag = (width.powi(2) + height.powi(2)).sqrt() * COVERAGE_MULTIPLIER;
    let cx = width / 2.0 + CENTER_X_OFFSET;
    let cy = height / 2.0 - CENTER_Y_OFFSET;

    // 计算索引上限，避免浮点累积误差与无限循环
    let v_start = -diag - 200.0;
    let v_end = diag;
    let total_v_span = v_end - v_start;
    let v_count = ((total_v_span / step_outer).ceil() as isize).max(0) as usize;

    let u_start = -diag;
    let u_end = diag;
    let total_u_span = u_end - u_start;
    let u_count = ((total_u_span / step_inner).ceil() as isize).max(0) as usize;

    let estimated = v_count.saturating_mul(u_count);
    if estimated > MAX_ALLOWED_WATERMARKS {
        return Err(format!("Too many watermarks to render: {}", estimated).into());
    }

    // 使用整数循环消除累积误差
    for vi in 0..=v_count {
        let v = v_start + (vi as f32) * step_outer;
        for ui in 0..=u_count {
            let u = u_start + (ui as f32) * step_inner;
            let x = cx + u * c - v * s;
            let y = cy + u * s + v * c;

            // 更严格的可见性裁剪
            if x > -VISIBILITY_MARGIN
                && x < width + VISIBILITY_MARGIN
                && y > -VISIBILITY_MARGIN
                && y < height + VISIBILITY_MARGIN
            {
                ops.push(Operation::new("q", vec![]));
                // cm 参数序： a b c d e f
                ops.push(Operation::new(
                    "cm",
                    vec![
                        c.into(),
                        s.into(),
                        (-s).into(),
                        c.into(),
                        x.into(),
                        y.into(),
                    ],
                ));
                ops.push(Operation::new("Do", vec![x_name.into()]));
                ops.push(Operation::new("Q", vec![]));
            }
        }
    }

    Ok(ops)
}