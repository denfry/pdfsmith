//! Уровни детализации (LOD).
//!
//! Масштаб отображения непрерывен, но рендерим тайлы только на дискретных
//! ступенях-степенях двойки. Для текущего масштаба берём ближайшую ступень
//! НЕ МЕНЬШЕ него — тогда тайлы не грубее экрана, а GPU лишь уменьшает их
//! (резко). Дискретность ограничивает число вариантов в кэше.

/// LOD как целочисленный уровень `k`, которому соответствует масштаб `2^k`
/// device-px на пункт.
pub fn lod_scale_for(display_scale: f32, min_lod: i32, max_lod: i32) -> (i32, f32) {
    let raw = if display_scale <= 0.0 {
        min_lod
    } else {
        display_scale.log2().ceil() as i32
    };
    let lod = raw.clamp(min_lod, max_lod);
    (lod, (lod as f32).exp2())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_power_of_two_maps_to_its_level() {
        assert_eq!(lod_scale_for(1.0, -4, 6), (0, 1.0));
        assert_eq!(lod_scale_for(2.0, -4, 6), (1, 2.0));
        assert_eq!(lod_scale_for(0.5, -4, 6), (-1, 0.5));
    }

    #[test]
    fn rounds_up_to_next_level_so_tiles_are_at_least_screen_resolution() {
        // 1.5 → ступень 2.0 (≥ 1.5).
        assert_eq!(lod_scale_for(1.5, -4, 6), (1, 2.0));
        // 0.3 → ступень 0.5 (≥ 0.3).
        assert_eq!(lod_scale_for(0.3, -4, 6), (-1, 0.5));
    }

    #[test]
    fn clamps_to_bounds() {
        assert_eq!(lod_scale_for(1000.0, -4, 6), (6, 64.0));
        assert_eq!(lod_scale_for(0.001, -4, 6), (-4, 0.0625));
    }
}
