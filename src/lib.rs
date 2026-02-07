use ab_glyph::{Font, FontRef, PxScale, OutlineCurve, Point, ScaleFont};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId, Stream};
use lopdf::dictionary;
use std::ffi::CStr;
use std::os::raw::c_char;

// --- FFI 接口：供 PHP 调用 ---

#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_pdf_watermark(
    input_path: *const c_char,
    output_path: *const c_char,
    font_path: *const c_char,
    user_name: *const c_char,
    date_str: *const c_char,
) -> i32 {
   let (input, output,font_p, name, date) = unsafe {
        (
            CStr::from_ptr(input_path).to_string_lossy(),
            CStr::from_ptr(output_path).to_string_lossy(),
            CStr::from_ptr(font_path).to_string_lossy(),
            CStr::from_ptr(user_name).to_string_lossy(),
            CStr::from_ptr(date_str).to_string_lossy(),
        )
    };

    let text = format!("致{}-{}:高度保密", name, date);

    match run_watermark_process(&input, &output, &font_p, &text) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

// --- 公共处理函数：供 main.rs 和 FFI 调用 ---

pub fn run_watermark_process(input_path: &str, output_path: &str, font_path: &str, text: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut doc = Document::load(input_path)?;
    let font_data = std::fs::read(font_path)?;
    let font = FontRef::try_from_slice(&font_data)?;

    let watermark_ops = text_to_pdf_paths(&font, text, 0.0, 0.0, 26.0);
    let watermark_content = Content { operations: watermark_ops };
    let watermark_stream = Stream::new(dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
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

    for (page_num, object_id) in doc.get_pages() {
        let (w, h) = page_size(&doc, object_id).unwrap_or((595.0, 842.0));
        let rotation = get_page_rotation(&doc, object_id);
        
        if let Err(_) = add_xobject_to_page(&mut doc, object_id, xobject_name, xobject_id) {
            println!("警告：第 {} 页结构非标准，无法注入资源。", page_num);
            continue;
        }

        let ops = build_watermark_grid_ops_xobject(xobject_name, 26.0, 60.0, w, h, rotation, &font, text);
        let content_ops = Content { operations: ops };

        match doc.add_to_page_content(object_id, content_ops) {
            Ok(_) => (),
            Err(e) => {
                if format!("{:?}", e).contains("Type") {
                    println!("跳过第 {} 页：非标准结构。", page_num);
                } else {
                    println!("跳过第 {} 页：未知错误 ({:?})", page_num, e);
                }
                continue;
            }
        }
    }

    doc.save(&output_path)?;
    Ok(output_path.to_string())
}

// --- 内部算法逻辑 (私有) ---

fn text_to_pdf_paths(font: &FontRef, text: &str, x_start: f32, y_start: f32, size: f32) -> Vec<Operation> {
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
            let mut last_point = Point { x: f32::NAN, y: f32::NAN };
            for curve in outline.curves {
                let p0 = match curve {
                    OutlineCurve::Line(p0, _) => p0,
                    OutlineCurve::Quad(p0, _, _) => p0,
                    OutlineCurve::Cubic(p0, _, _, _) => p0,
                };
                let is_new_contour = last_point.x.is_nan() || (p0.x - last_point.x).abs() > 0.001 || (p0.y - last_point.y).abs() > 0.001;
                if is_new_contour {
                    if !last_point.x.is_nan() { ops.push(Operation::new("h", vec![])); }
                    ops.push(Operation::new("m", vec![(x_cursor + p0.x * h_factor).into(), (y_start + p0.y * v_factor).into()]));
                }
                match curve {
                    OutlineCurve::Line(_, p1) => {
                        ops.push(Operation::new("l", vec![(x_cursor + p1.x * h_factor).into(), (y_start + p1.y * v_factor).into()]));
                        last_point = p1;
                    }
                    OutlineCurve::Quad(p0, p1, p2) => {
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
            if !last_point.x.is_nan() { ops.push(Operation::new("h", vec![])); }
        }
        x_cursor += scaled_font.h_advance(glyph_id);
    }
    ops.push(Operation::new("f", vec![]));
    ops.push(Operation::new("Q", vec![]));
    ops
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
    let mut current_id = Some(page_id);
    while let Some(id) = current_id {
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
                        if let Ok(Object::Integer(r)) = doc.get_object(*ref_id) { Some(*r) } else { None }
                    }
                    _ => None,
                };
                if let Some(val) = r { return val as f32; }
            }
            current_id = if let Ok(Object::Reference(p)) = dict.get(b"Parent") { Some(*p) } else { None };
        } else { break; }
    }
    0.0
}

fn add_xobject_to_page(doc: &mut Document, page_id: ObjectId, x_name: &str, x_id: ObjectId) -> Result<(), Box<dyn std::error::Error>> {
    if let Object::Dictionary(page_dict) = doc.get_object_mut(page_id)? {
        if !page_dict.has(b"Resources") { page_dict.set(b"Resources", dictionary! {}); }
        let resources = page_dict.get_mut(b"Resources")?.as_dict_mut()?;
        if !resources.has(b"XObject") { resources.set(b"XObject", dictionary! {}); }
        let xobjects = resources.get_mut(b"XObject")?.as_dict_mut()?;
        xobjects.set(x_name.as_bytes().to_vec(), Object::Reference(x_id));
    }
    Ok(())
}

fn build_watermark_grid_ops_xobject(x_name: &str, size: f32, angle: f32, width: f32, height: f32, _rot: f32, font: &FontRef, text: &str) -> Vec<Operation> {
    let text_w = measure_text_width(font, text, size);
    let step_inner = text_w + 30.0; 
    let step_outer = size * 6.0;   
    let rad = angle.to_radians();
    let (c, s) = (rad.cos(), rad.sin());
    let mut ops = Vec::new();
    let diag = (width.powi(2) + height.powi(2)).sqrt() * 2.5;
    let cx = width / 2.0 + 50.0;
    let cy = height / 2.0 - 100.0;
    let mut v = -diag - 200.0;
    while v < diag {
        let mut u = -diag;
        while u < diag {
            let x = cx + u * c - v * s;
            let y = cy + u * s + v * c;
            if x > -text_w && x < width + 100.0 && y > -200.0 && y < height + 100.0 {
                ops.push(Operation::new("q", vec![]));
                ops.push(Operation::new("cm", vec![c.into(), s.into(), (-s).into(), c.into(), x.into(), y.into()]));
                ops.push(Operation::new("Do", vec![x_name.into()]));
                ops.push(Operation::new("Q", vec![]));
            }
            u += step_inner;
        }
        v += step_outer;
    }
    ops
}