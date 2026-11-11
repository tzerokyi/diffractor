//! Rust port of Diffractor's source-space zoom contract (src/model_zoom.h).
//! The source raster dimensions are independent of the currently decoded texture.
//! A texture upgrade must never change scale, center or the visible subject.
use eframe::egui::{self, Rect, Vec2};
#[cfg(test)]
use eframe::egui::Pos2;

pub const SCALE_LADDER: [f32; 19] = [
    0.05, 0.07, 0.10, 0.15, 0.20, 0.25, 0.33, 0.50, 0.67, 0.75,
    1.00, 1.50, 2.00, 3.00, 4.00, 6.00, 8.00, 12.00, 16.00,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Fit,
    Explicit(f32),
}

#[derive(Clone, Copy, Debug)]
pub struct Zoom {
    mode: Mode,
    center: Vec2, // normalized source-space center
}

impl Default for Zoom {
    fn default() -> Self {
        Self { mode: Mode::Fit, center: egui::vec2(0.5, 0.5) }
    }
}

impl Zoom {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn fit(&mut self) {
        self.mode = Mode::Fit;
        self.center = egui::vec2(0.5, 0.5);
    }

    pub fn fit_scale(source: Vec2, viewport: Vec2) -> f32 {
        if source.x <= 0.0 || source.y <= 0.0
            || viewport.x <= 0.0 || viewport.y <= 0.0 {
            return 1.0;
        }
        (viewport.x / source.x).min(viewport.y / source.y).min(1.0)
    }

    pub fn scale(&self, source: Vec2, viewport: Vec2) -> f32 {
        match self.mode {
            Mode::Fit => Self::fit_scale(source, viewport),
            Mode::Explicit(value) => value,
        }
    }

    pub fn is_magnified(&self, source: Vec2, viewport: Vec2) -> bool {
        self.scale(source, viewport) > Self::fit_scale(source, viewport) + 0.000_001
    }

    fn clamped_center(&self, source: Vec2, viewport: Vec2, scale: f32) -> Vec2 {
        fn axis(center: f32, source: f32, viewport: f32, scale: f32) -> f32 {
            if source <= 0.0 || source * scale <= viewport {
                return 0.5;
            }
            let half = viewport / (2.0 * source * scale);
            center.clamp(half, 1.0 - half)
        }
        egui::vec2(
            axis(self.center.x, source.x, viewport.x, scale),
            axis(self.center.y, source.y, viewport.y, scale),
        )
    }

    pub fn rect(&self, bounds: Rect, source: Vec2) -> Rect {
        let viewport = bounds.size();
        let scale = self.scale(source, viewport);
        let extent = source * scale;
        let center = self.clamped_center(source, viewport, scale);
        let local_min = viewport * 0.5 - center * extent;
        Rect::from_min_size(bounds.min + local_min, extent)
    }

    // anchor is in viewport-local coordinates, not absolute window coordinates.
    pub fn set_anchored(&mut self, new_scale: f32, source: Vec2, viewport: Vec2, anchor: Vec2) {
        if source.x <= 0.0 || source.y <= 0.0
            || viewport.x <= 0.0 || viewport.y <= 0.0
            || !new_scale.is_finite() || new_scale <= 0.0 {
            return;
        }
        let current = self.scale(source, viewport);
        let center = self.clamped_center(source, viewport, current);
        let from_center = anchor - viewport * 0.5;
        let source_anchor = center * source + from_center / current;
        let new_center = (source_anchor - from_center / new_scale) / source;
        self.center = new_center.clamp(Vec2::ZERO, Vec2::splat(1.0));
        self.mode = Mode::Explicit(new_scale);
    }

    pub fn actual_pixels(&mut self, source: Vec2, viewport: Vec2) {
        self.set_anchored(1.0, source, viewport, viewport * 0.5);
    }

    pub fn step(&mut self, direction: i8, source: Vec2, viewport: Vec2, anchor: Vec2) {
        if direction == 0 || source.x <= 0.0 || source.y <= 0.0 {
            return;
        }
        let fit = Self::fit_scale(source, viewport);
        let current = self.scale(source, viewport);
        if direction < 0 && current <= fit + 0.000_001 {
            self.fit();
            return;
        }
        let mut next = current;
        for candidate in SCALE_LADDER.iter().copied().chain(std::iter::once(fit)) {
            if direction > 0 {
                if candidate > current + 0.000_001
                    && (next <= current || candidate < next) {
                    next = candidate;
                }
            } else if candidate < current - 0.000_001
                && (next >= current || candidate > next) {
                next = candidate;
            }
        }
        if direction < 0 && next <= fit + 0.000_001 {
            self.fit();
        } else if next != current {
            self.set_anchored(next, source, viewport, anchor);
        }
    }

    pub fn pan_by(&mut self, screen_delta: Vec2, source: Vec2, viewport: Vec2) {
        let scale = self.scale(source, viewport);
        if source.x <= 0.0 || source.y <= 0.0 || scale <= 0.0 {
            return;
        }
        let center = self.clamped_center(source, viewport, scale);
        self.center = self.clamped_center_for(
            center - screen_delta / (source * scale), source, viewport, scale,
        );
    }

    pub fn set_center(&mut self, center: Vec2, source: Vec2, viewport: Vec2) {
        if source.x <= 0.0 || source.y <= 0.0 { return; }
        self.center = self.clamped_center_for(center.clamp(Vec2::ZERO, Vec2::splat(1.0)),
            source, viewport, self.scale(source, viewport));
    }

    fn clamped_center_for(&self, desired: Vec2, source: Vec2, viewport: Vec2, scale: f32) -> Vec2 {
        Self { mode: self.mode, center: desired }
            .clamped_center(source, viewport, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: Vec2 = Vec2::new(6000.0, 4000.0);
    const VIEW: Vec2 = Vec2::new(1200.0, 800.0);

    #[test]
    fn fit_is_not_upscaled_and_actual_is_1_to_1() {
        let mut z = Zoom::default();
        assert!((z.scale(SOURCE, VIEW) - 0.2).abs() < 0.0001);
        assert_eq!(Zoom::fit_scale(Vec2::new(100.0, 100.0), VIEW), 1.0);
        z.actual_pixels(SOURCE, VIEW);
        assert_eq!(z.mode(), Mode::Explicit(1.0));
        assert_eq!(z.scale(SOURCE, VIEW), 1.0);
    }

    #[test]
    fn steps_include_fit_and_original_fixed_ladder() {
        let mut z = Zoom::default();
        z.step(1, SOURCE, VIEW, VIEW * 0.5);
        assert_eq!(z.mode(), Mode::Explicit(0.25));
        z.step(1, SOURCE, VIEW, VIEW * 0.5);
        assert_eq!(z.mode(), Mode::Explicit(0.33));
        z.step(-1, SOURCE, VIEW, VIEW * 0.5);
        assert_eq!(z.mode(), Mode::Explicit(0.25));
        z.step(-1, SOURCE, VIEW, VIEW * 0.5);
        assert_eq!(z.mode(), Mode::Fit);
        z.step(-1, SOURCE, VIEW, VIEW * 0.5);
        assert_eq!(z.mode(), Mode::Fit);
    }

    #[test]
    fn pointer_anchor_survives_absolute_zoom() {
        let mut z = Zoom::default();
        let anchor = Vec2::new(200.0, 110.0);
        let before = z.rect(Rect::from_min_size(Pos2::ZERO, VIEW), SOURCE);
        let source_point = (anchor - before.min.to_vec2()) / z.scale(SOURCE, VIEW);
        z.set_anchored(1.0, SOURCE, VIEW, anchor);
        let after = z.rect(Rect::from_min_size(Pos2::ZERO, VIEW), SOURCE);
        let result = (anchor - after.min.to_vec2()) / z.scale(SOURCE, VIEW);
        assert!((source_point - result).length() < 0.01);
    }

    #[test]
    fn drag_clamps_immediately_at_edges() {
        let mut z = Zoom::default();
        z.actual_pixels(SOURCE, VIEW);
        z.pan_by(Vec2::new(100000.0, 100000.0), SOURCE, VIEW);
        let bounds = Rect::from_min_size(Pos2::ZERO, VIEW);
        let rect = z.rect(bounds, SOURCE);
        assert!(rect.min.x.abs() < 0.01 && rect.min.y.abs() < 0.01);
        z.pan_by(Vec2::new(-100000.0, -100000.0), SOURCE, VIEW);
        let rect = z.rect(bounds, SOURCE);
        assert!((rect.max.x - VIEW.x).abs() < 0.01);
        assert!((rect.max.y - VIEW.y).abs() < 0.01);
    }

    #[test]
    fn navigator_center_is_clamped_to_visible_source_edges() {
        let mut z = Zoom::default();
        z.actual_pixels(SOURCE, VIEW);
        z.set_center(Vec2::ZERO, SOURCE, VIEW);
        let bounds = Rect::from_min_size(Pos2::ZERO, VIEW);
        let rect = z.rect(bounds, SOURCE);
        assert!(rect.min.x.abs() < 0.01 && rect.min.y.abs() < 0.01);
        z.set_center(Vec2::splat(1.0), SOURCE, VIEW);
        let rect = z.rect(bounds, SOURCE);
        assert!((rect.max.x - VIEW.x).abs() < 0.01);
        assert!((rect.max.y - VIEW.y).abs() < 0.01);
    }

    #[test]
    fn comparison_uses_one_scale_and_normalized_center_for_different_sources() {
        let mut zoom = Zoom::default();
        let left = Vec2::new(6000.0, 4000.0);
        let right = Vec2::new(3000.0, 2000.0);
        let pane = Vec2::new(500.0, 400.0);
        let bounds = Rect::from_min_size(Pos2::ZERO, pane);
        zoom.actual_pixels(left, pane);
        zoom.set_center(Vec2::new(0.3, 0.6), left, pane);
        let normalized_center = |source| {
            let rect = zoom.rect(bounds, source);
            (bounds.center() - rect.min) / rect.size()
        };
        assert!((normalized_center(left) - normalized_center(right)).length() < 0.0001);
        assert_eq!(zoom.scale(left, pane), zoom.scale(right, pane));
    }

    #[test]
    fn decode_texture_size_does_not_affect_geometry() {
        let mut z = Zoom::default();
        z.actual_pixels(SOURCE, VIEW);
        let rect = z.rect(Rect::from_min_size(Pos2::ZERO, VIEW), SOURCE);
        // A 1600x1067 preview can be replaced by a 6000x4000 texture.
        // Source-space zoom never references either decoded size.
        assert_eq!(rect.size(), SOURCE);
    }
}
