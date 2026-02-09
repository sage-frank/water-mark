use ab_glyph::{Font, FontRef, PxScale, OutlineCurve, Point, ScaleFont};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId, Stream};
use lopdf::dictionary;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

// ============================================================================
// 常量定义 - Constants
// ============================================================================

/// 默认字体大小（点数）
const DEFAULT_FONT_SIZE: f32 = 26.0;

/// 水平方向水印间距（固定像素值）
/// 注意：与 GRID_VERTICAL_MULTIPLIER 的含义不同（见下文）
const GRID_HORIZONTAL_GAP: f32 = 30.0;

/// 垂直方向水印间距倍数（相对于字体大小）
/// 实际垂直间距 = DEFAULT_FONT_SIZE * GRID_VERTICAL_MULTIPLIER = 26.0 * 6.0 = 156.0
/// 注意：这个值故意设置得比水平间距大，以避免垂直方向的水印过于密集
const GRID_VERTICAL_MULTIPLIER: f32 = 6.0;

/// 水印旋转角度（度数）
const WATERMARK_ANGLE_DEG: f32 = 60.0;

/// 水印网格中心在页面X轴的偏移（用于视觉居中调整）
/// 根据字体和角度微调，使水印视觉上更居中
const CENTER_X_OFFSET: f32 = 0.0;

/// 水印网格中心在页面Y轴的偏移（用于视觉居中调整）
const CENTER_Y_OFFSET: f32 = 0.0;

/// 覆盖范围倍数（相对于页面对角线长度）
/// 较大的值能确保页面各个角落都被水印覆盖，但也会增加计算量
/// 建议范围：1.5 ~ 2.5
const COVERAGE_MULTIPLIER: f32 = 1.5;

/// 可见性裁剪边界（单位：点数）
/// 超出此边界外的水印将被忽略，以避免渲染页面外的内容
const VISIBILITY_MARGIN: f32 = 50.0;

/// 单个PDF允许的最大水印数量
/// 防止极端情况（极小的页面或间距）导致生成过多水印对象
const MAX_ALLOWED_WATERMARKS: usize = 1_000_000;

/// 网格间距最小值校验
/// 如果水平或垂直间距小于此值，拒绝生成以避免过度计算
const MIN_GRID_STEP_SIZE: f32 = 0.1;

// ============================================================================
// FFI 接口 - C语言互操作
// ============================================================================

/// 供C/其他语言调用的FFI接口
/// 
/// # 参数
/// - `input_path`: 输入PDF文件路径（C字符串）
/// - `output_path`: 输出PDF文件路径（C字符串）
/// - `font_path`: 字体文件路径（C字符串）
/// - `user_name`: 用户名（C字符串）
/// - `date_str`: 日期字符串（C字符串）
///
/// # 返回值
/// - `0`: 成功
/// - `-1`: 处理过程中发生错误
/// - `-2`: 空指针参数
/// - `-3`: UTF-8编码错误
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

// ============================================================================
// 公共处理函数 - 供 main.rs 和 FFI 调用
// ============================================================================

/// 执行水印处理的主函数
///
/// # 流程
/// 1. 加载PDF文档
/// 2. 读取并解析字体（只做一次）
/// 3. 预计算文本矢量路径（只做一次）
/// 4. 将文本作为XObject流对象嵌入PDF
/// 5. 遍历所有页面，生成水印网格（考虑页面旋转）
/// 6. 保存处理后的PDF
///
/// # 参数
/// - `input_path`: 输入PDF路径
/// - `output_path`: 输出PDF路径
/// - `font_path`: 字体文件路径
/// - `text`: 水印文本
///
/// # 返回
/// - `Ok(String)`: 输出文件路径
/// - `Err`: 处理过程中的错误信息
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
    let watermark_ops = text_to_pdf_paths(&font, text, 0.0, 0.0, DEFAULT_FONT_SIZE);
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
            // 使用常量而不是魔数（基于字体大小的 bbox）
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

        // 获取页面旋转角度（支持旋转PDF）
        let page_rotation = get_page_rotation(&doc, object_id);

        // 添加XObject资源到页面
        if let Err(e) = add_xobject_to_page(&mut doc, object_id, xobject_name, xobject_id) {
            eprintln!(
                "WARN: 第 {} 页结构非标准，无法注入资源。错误：{:?}",
                page_num, e
            );
            continue;
        }

        // 生成水印网格操作（传入页面旋转角度）
        let ops = match build_watermark_grid_ops_xobject_optimized(
            xobject_name,
            DEFAULT_FONT_SIZE,
            WATERMARK_ANGLE_DEG,
            w,
            h,
            text_w,
            page_rotation,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN: 生成水印网格失败，跳过第 {} 页：{:?}", page_num, e);
                continue;
            }
        };

        let content_ops = Content { operations: ops };

        // 将水印内容添加到页面
        if let Err(e) = doc.add_to_page_content(object_id, content_ops) {
            eprintln!("WARN: 添加页面内容失败，跳过第 {} 页：{:?}", page_num, e);
            continue;
        }
    }

    doc.save(&output_path)?;

    // 验证文件确实保存
    if !Path::new(output_path).exists() {
        return Err("输出文件保存失败".into());
    }

    Ok(output_path.to_string())
}

// ============================================================================
// 内部算法逻辑 (私有函数)
// ============================================================================

/// 将文本转换为PDF路径操作序列
///
/// # 功能
/// - 遍历文本中的每个字符
/// - 从字体中提取字形轮廓
/// - 将轮廓曲线转换为PDF图形操作指令
///
/// # 参数
/// - `font`: 字体引用
/// - `text`: 要转换的文本
/// - `x_start`: 水平起始位置
/// - `y_start`: 垂直起始位置
/// - `size`: 字体大小（点数）
///
/// # 返回
/// PDF操作向量（包括移动、线段、贝塞尔曲线等）
fn text_to_pdf_paths(
    font: &FontRef,
    text: &str,
    x_start: f32,
    y_start: f32,
    size: f32,
) -> Vec<Operation> {
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
            // 使用 Option<Point> 替代 NaN 作为轮廓分界的标记
            let mut last_point: Option<Point> = None;
            for curve in outline.curves {
                let p0 = match curve {
                    OutlineCurve::Line(p0, _) => p0,
                    OutlineCurve::Quad(p0, _, _) => p0,
                    OutlineCurve::Cubic(p0, _, _, _) => p0,
                };

                // 判断是否为新轮廓（新的子轮廓起点）
                let is_new_contour = match last_point {
                    None => true,
                    Some(lp) => ((p0.x - lp.x).abs() > 0.001) || ((p0.y - lp.y).abs() > 0.001),
                };

                if is_new_contour {
                    if last_point.is_some() {
                        ops.push(Operation::new("h", vec![])); // 闭合上一个轮廓
                    }
                    ops.push(Operation::new(
                        "m",
                        vec![
                            (x_cursor + p0.x * h_factor).into(),
                            (y_start + p0.y * v_factor).into(),
                        ],
                    )); // 移动到新起点
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
                        // 将二次贝塞尔转换为三次贝塞尔（PDF只支持三次）
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
                ops.push(Operation::new("h", vec![])); // 闭合最后一个轮廓
            }
        }
        x_cursor += scaled_font.h_advance(glyph_id);
    }
    ops.push(Operation::new("f", vec![])); // 填充路径
    ops.push(Operation::new("Q", vec![])); // 恢复图形状态

    ops
}

/// 计算文本宽度
///
/// # 参数
/// - `font`: 字体引用
/// - `text`: 文本内容
/// - `size`: 字体大小
///
/// # 返回
/// 文本总宽度（点数）
fn measure_text_width(font: &FontRef, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    for c in text.chars() {
        w += scaled.h_advance(font.glyph_id(c));
    }
    w
}

/// 从PDF页面对象中提取媒体框尺寸
///
/// # 返回
/// - `Some((width, height))`: 页面宽高
/// - `None`: 无法提取时返回默认值
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

/// 将PDF对象转换为f32
fn obj_to_f32(o: &Object) -> f32 {
    match o {
        Object::Real(r) => *r as f32,
        Object::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

/// 从PDF页面对象中提取页面旋转角度
///
/// # 说明
/// - 搜索页面及其父页面的 Rotate 属性
/// - 限制搜索深度为10级以防止无限循环
/// - 返回值为 0, 90, 180, 270（PDF标准值）
/// - 现在被 run_watermark_process 调用以支持旋转 PDF
fn get_page_rotation(doc: &Document, page_id: ObjectId) -> f32 {
    let mut current_id = Some(page_id);
    let mut depth = 0usize;
    const MAX_PARENT_DEPTH: usize = 10;

    while let Some(id) = current_id {
        if depth > MAX_PARENT_DEPTH {
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

/// 将XObject资源添加到PDF页面
///
/// # 说明
/// 创建或更新页面的Resources > XObject字典，
/// 使其能引用水印XObject对象
fn add_xobject_to_page(
    doc: &mut Document,
    page_id: ObjectId,
    x_name: &str,
    x_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
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

/// 生成水印网格PDF操作指令（优化版本）
///
/// # 功能
/// - 基于旋转角度计算水印网格位置
/// - 支持 PDF 页面旋转（90°、180°、270°）
/// - 生成PDF操作指令来绘制网格中的水印
/// - 裁剪超出页面可见区域的水印以优化性能
///
/// # 参数
/// - `x_name`: XObject资源名称
/// - `size`: 字体大小（用于计算垂直间距）
/// - `angle`: 水印旋转角度（度数）
/// - `width`: 页面宽度
/// - `height`: 页面高度
/// - `text_w`: 文本宽度（预计算）
/// - `page_rotation`: 页面旋转角度（度数，来自 PDF Rotate 属性）
///
/// # 返回
/// - `Ok(Vec<Operation>)`: PDF操作指令向量
/// - `Err`: 参数错误或水印数量超限
fn build_watermark_grid_ops_xobject_optimized(
    x_name: &str,
    size: f32,
    angle: f32,
    width: f32,
    height: f32,
    text_w: f32,
    page_rotation: f32,
) -> Result<Vec<Operation>, Box<dyn std::error::Error>> {
    let step_inner = text_w + GRID_HORIZONTAL_GAP;
    let step_outer = size * GRID_VERTICAL_MULTIPLIER;

    // 添加最小间距校验，防止过度计算
    if !(step_inner > MIN_GRID_STEP_SIZE && step_outer > MIN_GRID_STEP_SIZE) {
        return Err(format!(
            "Grid step too small: inner={}, outer={}",
            step_inner, step_outer
        )
        .into());
    }

    // 叠加页面旋转角度，确保水印相对于内容方向正确
    let effective_angle = angle + page_rotation;
    let rad = effective_angle.to_radians();
    let (c, s) = (rad.cos(), rad.sin());

    let mut ops = Vec::new();

    // 计算覆盖范围
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

    // 防止生成过多水印对象导致性能问题
    let estimated = v_count.saturating_mul(u_count);
    if estimated > MAX_ALLOWED_WATERMARKS {
        return Err(format!("Too many watermarks to render: {}", estimated).into());
    }

    // 使用整数循环消除浮点累积误差
    for vi in 0..=v_count {
        let v = v_start + (vi as f32) * step_outer;
        for ui in 0..=u_count {
            let u = u_start + (ui as f32) * step_inner;
            // 应用2D旋转变换
            let x = cx + u * c - v * s;
            let y = cy + u * s + v * c;

            // 裁剪超出页面可见区域的水印
            if x > -VISIBILITY_MARGIN
                && x < width + VISIBILITY_MARGIN
                && y > -VISIBILITY_MARGIN
                && y < height + VISIBILITY_MARGIN
            {
                ops.push(Operation::new("q", vec![])); // 保存图形状态
                // cm 操作参数顺序：a b c d e f
                // | a c e |   | cos  -sin  x |
                // | b d f | = | sin   cos  y |
                // | 0 0 1 |   | 0     0    1 |
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
                ops.push(Operation::new("Do", vec![x_name.into()])); // 绘制XObject
                ops.push(Operation::new("Q", vec![])); // 恢复图形状态
            }
        }
    }

    Ok(ops)
}