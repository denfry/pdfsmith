//! Геометрия тайлов: какие тайлы страницы попадают в видимый прямоугольник.
//!
//! Страница на конкретном уровне зума имеет размер в device-пикселях.
//! Она режется на квадратные тайлы фиксированного размера. Рендерим и держим
//! в кэше только те тайлы, что пересекают вьюпорт.

/// Сторона тайла в device-пикселях.
pub type TileSize = u32;

/// Координата тайла в сетке страницы (столбец, строка), от верхнего-левого угла.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub col: u32,
    pub row: u32,
}

/// Прямоугольник в device-пикселях страницы (origin — верхний левый угол).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Возвращает тайлы, чьи пиксельные границы пересекают `view`, обрезая по
/// фактическому размеру страницы. Порядок — по строкам сверху вниз, в строке
/// слева направо.
pub fn tiles_in_view(
    page_px_width: f32,
    page_px_height: f32,
    tile_size: TileSize,
    view: PixelRect,
) -> Vec<TileCoord> {
    let ts = tile_size as f32;
    if ts <= 0.0 || page_px_width <= 0.0 || page_px_height <= 0.0 {
        return Vec::new();
    }

    // Пересечение вьюпорта с границами страницы.
    let left = view.x.max(0.0);
    let top = view.y.max(0.0);
    let right = (view.x + view.width).min(page_px_width);
    let bottom = (view.y + view.height).min(page_px_height);
    if right <= left || bottom <= top {
        return Vec::new();
    }

    // Диапазон столбцов/строк, покрывающих видимую область.
    let col_start = (left / ts).floor() as u32;
    let row_start = (top / ts).floor() as u32;
    // Последний тайл, который ещё содержит граничный пиксель (right/bottom
    // эксклюзивны, поэтому берём наибольший пиксель внутри области).
    let col_end = ((right - 1.0).max(left) / ts).floor() as u32;
    let row_end = ((bottom - 1.0).max(top) / ts).floor() as u32;

    let mut tiles = Vec::new();
    for row in row_start..=row_end {
        for col in col_start..=col_end {
            tiles.push(TileCoord { col, row });
        }
    }
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_tile_when_view_covers_only_first_tile() {
        let tiles = tiles_in_view(
            1000.0,
            1000.0,
            256,
            PixelRect { x: 0.0, y: 0.0, width: 100.0, height: 100.0 },
        );
        assert_eq!(tiles, vec![TileCoord { col: 0, row: 0 }]);
    }

    #[test]
    fn spans_a_two_by_two_block_across_a_tile_boundary() {
        // Вьюпорт 200..300 по обеим осям пересекает тайлы 0 и 1 (граница 256).
        let tiles = tiles_in_view(
            1000.0,
            1000.0,
            256,
            PixelRect { x: 200.0, y: 200.0, width: 100.0, height: 100.0 },
        );
        assert_eq!(
            tiles,
            vec![
                TileCoord { col: 0, row: 0 },
                TileCoord { col: 1, row: 0 },
                TileCoord { col: 0, row: 1 },
                TileCoord { col: 1, row: 1 },
            ]
        );
    }

    #[test]
    fn clamps_to_page_when_view_is_larger_than_page() {
        // Страница 300x300 (тайлы 0 и 1 по каждой оси), вьюпорт огромный.
        let tiles = tiles_in_view(
            300.0,
            300.0,
            256,
            PixelRect { x: -500.0, y: -500.0, width: 5000.0, height: 5000.0 },
        );
        assert_eq!(
            tiles,
            vec![
                TileCoord { col: 0, row: 0 },
                TileCoord { col: 1, row: 0 },
                TileCoord { col: 0, row: 1 },
                TileCoord { col: 1, row: 1 },
            ]
        );
    }

    #[test]
    fn empty_when_view_is_entirely_outside_the_page() {
        let tiles = tiles_in_view(
            1000.0,
            1000.0,
            256,
            PixelRect { x: 2000.0, y: 0.0, width: 100.0, height: 100.0 },
        );
        assert!(tiles.is_empty());
    }
}
