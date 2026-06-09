//! Математика вьюпорта: какие тайлы какого LOD видны и в каком порядке их
//! рендерить.
//!
//! Система координат экрана: точка страницы `p` (в пунктах) отображается в
//! экранную точку `offset + p * display_scale`. `offset` — экранная позиция
//! верхнего-левого угла страницы.

use crate::tile::{tiles_in_view, PixelRect, TileCoord, TileSize};

/// Прямоугольник страницы, видимый сейчас, в device-пикселях уровня `lod_scale`.
pub fn visible_page_rect(
    offset: (f32, f32),
    display_scale: f32,
    screen: (f32, f32),
    lod_scale: f32,
) -> PixelRect {
    let s = if display_scale <= 0.0 { 1.0 } else { display_scale };
    let k = lod_scale / s;
    PixelRect {
        x: (-offset.0) / s * lod_scale,
        y: (-offset.1) / s * lod_scale,
        width: screen.0 * k,
        height: screen.1 * k,
    }
}

/// Видимые тайлы на уровне `lod_scale`, упорядоченные от центра экрана к краям
/// (центр рендерится первым — пользователь видит результат раньше).
pub fn visible_tiles_center_out(
    offset: (f32, f32),
    display_scale: f32,
    screen: (f32, f32),
    lod_scale: f32,
    page_pt: (f32, f32),
    tile_size: TileSize,
) -> Vec<TileCoord> {
    let rect = visible_page_rect(offset, display_scale, screen, lod_scale);
    let page_px_w = page_pt.0 * lod_scale;
    let page_px_h = page_pt.1 * lod_scale;
    let mut tiles = tiles_in_view(page_px_w, page_px_h, tile_size, rect);

    let cx = rect.x + rect.width * 0.5;
    let cy = rect.y + rect.height * 0.5;
    let ts = tile_size as f32;
    tiles.sort_by(|a, b| {
        dist2(a, ts, cx, cy)
            .partial_cmp(&dist2(b, ts, cx, cy))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    tiles
}

fn dist2(t: &TileCoord, ts: f32, cx: f32, cy: f32) -> f32 {
    let tx = (t.col as f32 + 0.5) * ts;
    let ty = (t.row as f32 + 0.5) * ts;
    let dx = tx - cx;
    let dy = ty - cy;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_rect_at_origin_matches_screen_in_lod_pixels() {
        let r = visible_page_rect((0.0, 0.0), 1.0, (800.0, 600.0), 1.0);
        assert_eq!(r, PixelRect { x: 0.0, y: 0.0, width: 800.0, height: 600.0 });
    }

    #[test]
    fn scrolling_page_up_left_shifts_visible_rect_into_the_page() {
        // Страница сдвинута так, что её угол ушёл за левый-верхний край экрана.
        let r = visible_page_rect((-100.0, -50.0), 1.0, (800.0, 600.0), 1.0);
        assert_eq!(r.x, 100.0);
        assert_eq!(r.y, 50.0);
    }

    #[test]
    fn higher_lod_scales_visible_rect_in_device_pixels() {
        // При том же экране на LOD-масштабе 2.0 device-пиксели вдвое крупнее.
        let r = visible_page_rect((0.0, 0.0), 1.0, (800.0, 600.0), 2.0);
        assert_eq!(r, PixelRect { x: 0.0, y: 0.0, width: 1600.0, height: 1200.0 });
    }

    #[test]
    fn center_tile_comes_first() {
        // Экран 800×600 в начале координат, LOD 1.0, большая страница, тайл 256.
        // Центр экрана (400,300) лежит в тайле (1,1) — он должен быть первым.
        let tiles = visible_tiles_center_out(
            (0.0, 0.0),
            1.0,
            (800.0, 600.0),
            1.0,
            (10000.0, 10000.0),
            256,
        );
        assert_eq!(tiles.first(), Some(&TileCoord { col: 1, row: 1 }));
    }
}
