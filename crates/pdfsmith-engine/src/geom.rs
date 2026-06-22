//! Перевод координат текстового слоя PDF в "повёрнутое page-point" пространство.
//!
//! PDF: origin внизу-слева, ось Y вверх, пункты, БЕЗ поворота.
//! page-point: origin вверху-слева, ось Y вниз, пункты, размер = page_pt()
//! (для поворотов 1/3 ширина/высота меняются местами).
//! Повороты согласованы с матрицами `render_region` (rotation=1 — 90° по часовой).

/// Прямоугольник в PDF-координатах (пункты, origin внизу-слева).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfRect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

/// Прямоугольник в page-point пространстве (пункты, origin вверху-слева, y вниз).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePtRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Переводит точку PDF (x_pt, y_pt) в page-point пространство для поворота.
/// `page_w`/`page_h` — размер НЕповёрнутой страницы в пунктах.
pub fn pdf_point_to_page_pt(x_pt: f32, y_pt: f32, page_w: f32, page_h: f32, rotation: u8) -> (f32, f32) {
    match rotation & 3 {
        0 => (x_pt, page_h - y_pt),
        1 => (y_pt, x_pt),
        2 => (page_w - x_pt, y_pt),
        _ => (page_h - y_pt, page_w - x_pt),
    }
}

/// Переводит PDF-прямоугольник в page-point прямоугольник для поворота.
/// Переводит два противоположных угла и нормализует (повороты на 90° меняют,
/// какой угол минимальный).
pub fn pdf_rect_to_page_pt(r: PdfRect, page_w: f32, page_h: f32, rotation: u8) -> PagePtRect {
    let (ax, ay) = pdf_point_to_page_pt(r.left, r.bottom, page_w, page_h, rotation);
    let (bx, by) = pdf_point_to_page_pt(r.right, r.top, page_w, page_h, rotation);
    PagePtRect {
        x: ax.min(bx),
        y: ay.min(by),
        w: (ax - bx).abs(),
        h: (ay - by).abs(),
    }
}

/// Объединяющий прямоугольник набора page-point прямоугольников.
pub fn union(rects: &[PagePtRect]) -> Option<PagePtRect> {
    let first = rects.first()?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x + first.w;
    let mut max_y = first.y + first.h;
    for r in &rects[1..] {
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.x + r.w);
        max_y = max_y.max(r.y + r.h);
    }
    Some(PagePtRect { x: min_x, y: min_y, w: max_x - min_x, h: max_y - min_y })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Страница 200×100 pt. Бокс символа: left=10, bottom=20, right=30, top=40.
    const W: f32 = 200.0;
    const H: f32 = 100.0;
    fn r() -> PdfRect {
        PdfRect { left: 10.0, bottom: 20.0, right: 30.0, top: 40.0 }
    }

    #[test]
    fn rot0_flips_y_only() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 0),
            PagePtRect { x: 10.0, y: 60.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn rot1_is_90cw() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 1),
            PagePtRect { x: 20.0, y: 10.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn rot2_is_180() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 2),
            PagePtRect { x: 170.0, y: 20.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn rot3_is_270cw() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 3),
            PagePtRect { x: 60.0, y: 170.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn point_corner_maps_per_rotation() {
        // PDF нижне-левый угол (0,0) при rot0 → верхне-левый по X, низ по Y.
        assert_eq!(pdf_point_to_page_pt(0.0, 0.0, W, H, 0), (0.0, 100.0));
        assert_eq!(pdf_point_to_page_pt(0.0, 0.0, W, H, 1), (0.0, 0.0));
    }

    #[test]
    fn union_covers_all() {
        let a = PagePtRect { x: 10.0, y: 60.0, w: 20.0, h: 20.0 };
        let b = PagePtRect { x: 30.0, y: 60.0, w: 20.0, h: 20.0 };
        assert_eq!(union(&[a, b]), Some(PagePtRect { x: 10.0, y: 60.0, w: 40.0, h: 20.0 }));
        assert_eq!(union(&[]), None);
    }
}
