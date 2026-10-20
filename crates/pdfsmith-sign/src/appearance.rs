//! Видимый штамп подписи: текст растеризуется в маску прозрачности (так не
//! нужно встраивать в PDF кириллический шрифт), рамка — векторная.

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

use crate::{SignError, SignField};

/// Плотность растра текста: 4 px на пункт ≈ 288 dpi.
const PX_PER_PT: f32 = 4.0;
/// Потолок размера растра, пикселей.
const MAX_PIXELS: f32 = 6_000_000.0;
/// Крупнее не пишем даже в большой рамке, пт.
const MAX_FONT_PT: f32 = 16.0;
/// Межстрочный интервал в долях высоты шрифта.
const LEADING: f32 = 1.15;
/// Цвет штампа (тёмно-синий).
const INK: [u8; 3] = [0x1f, 0x3a, 0x93];

/// Покрытие текстом, построчно сверху вниз, 0..=255.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

/// Вписывает строки в прямоугольник `w_pt`×`h_pt` (по левому краю, по центру
/// по вертикали) и растеризует.
pub fn render_text(lines: &[String], w_pt: f32, h_pt: f32, font: &[u8]) -> Result<Raster, String> {
    let font = FontRef::try_from_slice(font).map_err(|_| "шрифт не читается".to_string())?;
    let k = PX_PER_PT.min((MAX_PIXELS / (w_pt * h_pt).max(1.0)).sqrt()).max(0.5);
    let width = (w_pt * k).ceil().max(1.0) as u32;
    let height = (h_pt * k).ceil().max(1.0) as u32;
    let mut alpha = vec![0u8; (width * height) as usize];
    if lines.is_empty() {
        return Ok(Raster { width, height, alpha });
    }

    // При PxScale(s) высота шрифта (ascent − descent) равна s пикселям.
    let unit = font.as_scaled(PxScale::from(1.0));
    let line_width = |s: &str| {
        let mut w = 0.0;
        let mut prev = None;
        for c in s.chars() {
            let id = unit.glyph_id(c);
            if let Some(p) = prev {
                w += unit.kern(p, id);
            }
            w += unit.h_advance(id);
            prev = Some(id);
        }
        w
    };
    let max_w1 = lines.iter().map(|s| line_width(s)).fold(0.0f32, f32::max).max(1e-3);
    let pad = (w_pt.min(h_pt) * 0.08).clamp(1.0, 6.0) * k;
    let avail_w = (width as f32 - 2.0 * pad).max(1.0);
    let avail_h = (height as f32 - 2.0 * pad).max(1.0);
    let n = lines.len() as f32;
    let size = (avail_h / (n * LEADING)).min(avail_w / max_w1).min(MAX_FONT_PT * k).max(1.0);
    let sf = font.as_scaled(PxScale::from(size));
    let line_h = size * LEADING;

    let mut top = pad + (avail_h - line_h * n).max(0.0) / 2.0;
    for line in lines {
        let baseline = top + (line_h - size) / 2.0 + sf.ascent();
        let mut x = pad;
        let mut prev = None;
        for c in line.chars() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            if let Some(g) = font.outline_glyph(id.with_scale_and_position(size, point(x, baseline))) {
                let bb = g.px_bounds();
                g.draw(|gx, gy, cov| {
                    let px = bb.min.x as i32 + gx as i32;
                    let py = bb.min.y as i32 + gy as i32;
                    if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                        let i = py as usize * width as usize + px as usize;
                        alpha[i] = alpha[i].max((cov.clamp(0.0, 1.0) * 255.0).round() as u8);
                    }
                });
            }
            x += sf.h_advance(id);
            prev = Some(id);
        }
        top += line_h;
    }
    Ok(Raster { width, height, alpha })
}

/// Создаёт в `doc` Form XObject штампа для поля и возвращает его id.
pub(crate) fn build(doc: &mut Document, field: &SignField, font: &[u8]) -> Result<ObjectId, SignError> {
    let [l, b, r, t] = field.rect;
    let (w, h) = ((r - l).abs(), (t - b).abs());
    let rot = field.page_rotation % 4;
    // Размер штампа так, как его видит читатель (с учётом /Rotate страницы).
    let (vw, vh) = if rot % 2 == 1 { (h, w) } else { (w, h) };
    let raster = render_text(&field.lines, vw, vh, font).map_err(SignError::Structure)?;

    let image_dict = |cs: &str| {
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => raster.width as i64,
            "Height" => raster.height as i64,
            "ColorSpace" => cs,
            "BitsPerComponent" => 8,
        }
    };
    let mut mask = Stream::new(image_dict("DeviceGray"), raster.alpha.clone());
    mask.compress()?;
    let mask_id = doc.add_object(mask);
    let rgb = INK.repeat((raster.width * raster.height) as usize);
    let mut dict = image_dict("DeviceRGB");
    dict.set("SMask", mask_id);
    let mut image = Stream::new(dict, rgb);
    image.compress()?;
    let image_id = doc.add_object(image);

    let [cr, cg, cb] = INK.map(|c| c as f32 / 255.0);
    let content = format!(
        "q\n{cr:.3} {cg:.3} {cb:.3} RG 0.8 w\n0.4 0.4 {:.2} {:.2} re S\nq {vw:.2} 0 0 {vh:.2} 0 0 cm /Im0 Do Q\nQ\n",
        (vw - 0.8).max(0.0),
        (vh - 0.8).max(0.0)
    );
    // Страница с /Rotate поворачивает и аннотации — доворачиваем штамп обратно.
    let matrix: [i64; 6] = match rot {
        1 => [0, 1, -1, 0, 0, 0],
        2 => [-1, 0, 0, -1, 0, 0],
        3 => [0, -1, 1, 0, 0, 0],
        _ => [1, 0, 0, 1, 0, 0],
    };
    let mut form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), Object::Real(vw), Object::Real(vh)],
            "Matrix" => matrix.iter().map(|&v| Object::Integer(v)).collect::<Vec<_>>(),
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
        },
        content.into_bytes(),
    );
    form.compress()?;
    Ok(doc.add_object(form))
}
