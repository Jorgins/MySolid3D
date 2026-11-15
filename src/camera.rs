//! Орбитальная камера с ортографической проекцией (ось Z — вверх).

use crate::model::Plane;
use eframe::egui::{Pos2, Rect, Vec2};
use glam::DVec3;

#[derive(Clone)]
pub struct Camera {
    pub yaw: f64,
    pub pitch: f64,
    pub target: DVec3,
    /// Пикселей на миллиметр (в плоскости, проходящей через цель).
    pub scale: f64,
    /// Перспектива включена (иначе параллельная проекция).
    pub perspective: bool,
    /// Угол обзора по вертикали в градусах: чем он больше, тем сильнее перспектива.
    pub fov: f64,
    /// В эскизе вид всегда параллельный — чертить в перспективе неудобно.
    pub flat: bool,
    /// Камера не подходит к цели ближе этого расстояния (мм), чтобы деталь не оказалась за спиной.
    pub min_dist: f64,
}

impl Default for Camera {
    fn default() -> Self {
        let mut c = Camera { yaw: 0.0, pitch: 0.0, target: DVec3::ZERO, scale: 4.0, perspective: false, fov: 40.0, flat: false, min_dist: 0.0 };
        c.set_iso();
        c
    }
}

/// Снимок камеры для быстрой проекции точек на экран.
#[derive(Clone, Copy)]
pub struct Projector {
    target: DVec3,
    right: DVec3,
    up: DVec3,
    eye: DVec3,
    scale: f64,
    center: Pos2,
    dist: Option<f64>,
}

impl Projector {
    pub fn project(&self, p: DVec3) -> Pos2 {
        let d = p - self.target;
        // В перспективе всё, что дальше цели, мельче, а ближе — крупнее.
        let k = match self.dist {
            Some(dist) => dist / (dist - d.dot(self.eye)).max(dist * 1e-3),
            None => 1.0,
        };
        Pos2::new(
            self.center.x + (d.dot(self.right) * self.scale * k) as f32,
            self.center.y - (d.dot(self.up) * self.scale * k) as f32,
        )
    }
}

impl Camera {
    /// Направление от цели к наблюдателю.
    fn eye_dir(&self) -> DVec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        DVec3::new(cp * cy, cp * sy, sp)
    }

    pub fn forward(&self) -> DVec3 {
        -self.eye_dir()
    }

    pub fn right(&self) -> DVec3 {
        DVec3::new(-self.yaw.sin(), self.yaw.cos(), 0.0)
    }

    pub fn up(&self) -> DVec3 {
        self.eye_dir().cross(self.right())
    }

    pub fn set_iso(&mut self) {
        self.yaw = (-45.0f64).to_radians();
        self.pitch = (35.264f64).to_radians();
    }

    /// Смотреть перпендикулярно плоскости, ось X плоскости — вправо.
    pub fn look_at_plane(&mut self, plane: &Plane) {
        let n = plane.normal();
        self.pitch = n.z.clamp(-1.0, 1.0).asin();
        self.yaw = if n.z.abs() > 0.999 { (-plane.x.x).atan2(plane.x.y) } else { n.y.atan2(n.x) };
    }

    /// Расстояние от глаза до цели в перспективе; `None` — проекция параллельная.
    /// Выбрано так, чтобы вертикальный угол обзора был `fov` (но не ближе `min_dist`).
    fn eye_distance(&self, rect: Rect) -> Option<f64> {
        if !self.perspective || self.flat {
            return None;
        }
        let half_height = rect.height().max(1.0) as f64 * 0.5 / self.scale;
        let tan = (self.fov.clamp(5.0, 120.0).to_radians() * 0.5).tan();
        Some((half_height / tan).max(self.min_dist).max(1e-3))
    }

    /// Единичное направление от точки к глазу — по нему проверяют, не закрыта ли точка деталью.
    pub fn dir_to_eye(&self, rect: Rect, p: DVec3) -> DVec3 {
        match self.eye_distance(rect) {
            Some(dist) => (self.target - self.forward() * dist - p).try_normalize().unwrap_or(-self.forward()),
            None => -self.forward(),
        }
    }

    pub fn project(&self, rect: Rect, p: DVec3) -> Pos2 {
        self.projector(rect).project(p)
    }

    /// Обрезает отрезок по плоскости перед камерой: в перспективе то, что позади глаза, не рисуется.
    /// В параллельной проекции отрезок не меняется.
    pub fn clip_to_view(&self, rect: Rect, a: DVec3, b: DVec3) -> Option<(DVec3, DVec3)> {
        let Some(dist) = self.eye_distance(rect) else { return Some((a, b)) };
        // Глубина точки к глазу: чем больше, тем ближе к камере; на 0,95·dist лежит сам глаз с небольшим запасом.
        let limit = dist * 0.95;
        let e = self.eye_dir();
        let (sa, sb) = ((a - self.target).dot(e), (b - self.target).dot(e));
        match (sa <= limit, sb <= limit) {
            (true, true) => Some((a, b)),
            (false, false) => None,
            (true, false) => Some((a, a + (b - a) * ((limit - sa) / (sb - sa)))),
            (false, true) => Some((a + (b - a) * ((limit - sa) / (sb - sa)), b)),
        }
    }

    /// Проекция с заранее посчитанными осями камеры — для циклов по тысячам точек.
    pub fn projector(&self, rect: Rect) -> Projector {
        Projector {
            target: self.target,
            right: self.right(),
            up: self.up(),
            eye: self.eye_dir(),
            scale: self.scale,
            center: rect.center(),
            dist: self.eye_distance(rect),
        }
    }

    /// Луч из точки экрана: (начало далеко перед сценой, направление взгляда).
    pub fn ray(&self, rect: Rect, pos: Pos2) -> (DVec3, DVec3) {
        let o = pos - rect.center();
        let f = self.forward();
        let on_plane = self.target + self.right() * (o.x as f64 / self.scale)
            - self.up() * (o.y as f64 / self.scale);
        match self.eye_distance(rect) {
            // Из глаза через точку экрана.
            Some(dist) => {
                let eye = self.target - f * dist;
                (eye, (on_plane - eye).normalize())
            }
            None => (on_plane - f * 1.0e5, f),
        }
    }

    /// Точка экрана в координатах плоскости эскиза.
    pub fn screen_to_plane(&self, rect: Rect, pos: Pos2, plane: &Plane) -> Option<glam::DVec2> {
        let (o, d) = self.ray(rect, pos);
        let n = plane.normal();
        let denom = d.dot(n);
        if denom.abs() < 1e-6 {
            return None;
        }
        let t = (plane.origin - o).dot(n) / denom;
        Some(plane.to_local(o + d * t))
    }

    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x as f64 * 0.008;
        let limit = std::f64::consts::FRAC_PI_2;
        self.pitch = (self.pitch + delta.y as f64 * 0.008).clamp(-limit, limit);
    }

    pub fn pan(&mut self, delta: Vec2) {
        self.target += (self.up() * delta.y as f64 - self.right() * delta.x as f64) / self.scale;
    }

    /// Масштабирование с сохранением точки под курсором.
    pub fn zoom(&mut self, rect: Rect, pos: Pos2, factor: f64) {
        let new_scale = (self.scale * factor).clamp(0.01, 5000.0);
        let o = pos - rect.center();
        let k = 1.0 / self.scale - 1.0 / new_scale;
        self.target += (self.right() * o.x as f64 - self.up() * o.y as f64) * k;
        self.scale = new_scale;
    }

    pub fn fit(&mut self, rect: Rect, bounds: Option<(DVec3, DVec3)>) {
        let (min, max) = bounds.unwrap_or((DVec3::splat(-30.0), DVec3::splat(30.0)));
        self.target = (min + max) * 0.5;
        let diag = (max - min).length().max(1.0);
        self.scale = rect.width().min(rect.height()).max(50.0) as f64 / (diag * 1.25);
    }

    /// Матрица мир → clip-пространство OpenGL (по столбцам).
    pub fn view_proj(&self, rect: Rect, depth_range: f64) -> [f32; 16] {
        let (r, u, f, t) = (self.right(), self.up(), self.forward(), self.target);
        if let Some(dist) = self.eye_distance(rect) {
            // Перспектива: w = расстояние от глаза вдоль взгляда, глубина — по формуле OpenGL.
            let sx = self.scale * dist / (rect.width().max(1.0) as f64 * 0.5);
            let sy = self.scale * dist / (rect.height().max(1.0) as f64 * 0.5);
            let (near, far) = ((dist * 0.01).max(0.05), dist + depth_range);
            let a = (far + near) / (far - near);
            let b = -2.0 * far * near / (far - near);
            let ft = dist - f.dot(t);
            return [
                r.x * sx, u.x * sy, a * f.x, f.x,
                r.y * sx, u.y * sy, a * f.y, f.y,
                r.z * sx, u.z * sy, a * f.z, f.z,
                -r.dot(t) * sx, -u.dot(t) * sy, a * ft + b, ft,
            ]
            .map(|v| v as f32);
        }
        let sx = self.scale / (rect.width() as f64 * 0.5);
        let sy = self.scale / (rect.height() as f64 * 0.5);
        let sz = 1.0 / depth_range;
        [
            r.x * sx, u.x * sy, f.x * sz, 0.0,
            r.y * sx, u.y * sy, f.y * sz, 0.0,
            r.z * sx, u.z * sy, f.z * sz, 0.0,
            -r.dot(t) * sx, -u.dot(t) * sy, -f.dot(t) * sz, 1.0,
        ]
        .map(|v| v as f32)
    }

    /// Поворот нормалей в пространство вида (по столбцам): x — вправо, y — вверх, z — к зрителю.
    pub fn normal_matrix(&self) -> [f32; 9] {
        let (r, u, e) = (self.right(), self.up(), self.eye_dir());
        [r.x, u.x, e.x, r.y, u.y, e.y, r.z, u.z, e.z].map(|v| v as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Экранные координаты по матрице для OpenGL (так их видит видеокарта).
    fn via_matrix(cam: &Camera, rect: Rect, p: DVec3, depth: f64) -> (f32, f32, f32) {
        let m = cam.view_proj(rect, depth).map(|v| v as f64);
        let clip = |row: usize| m[row] * p.x + m[4 + row] * p.y + m[8 + row] * p.z + m[12 + row];
        let w = clip(3);
        let (x, y, z) = (clip(0) / w, clip(1) / w, clip(2) / w);
        (
            rect.center().x + (x * rect.width() as f64 * 0.5) as f32,
            rect.center().y - (y * rect.height() as f64 * 0.5) as f32,
            z as f32,
        )
    }

    /// Прямая через начало координат остаётся прямой через проекцию начала при любом вращении.
    #[test]
    fn axis_lines_pass_through_projected_origin() {
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0));
        let mut cam = Camera::default();
        cam.perspective = true;
        cam.fov = 60.0;
        cam.target = DVec3::new(20.0, 10.0, 5.0);
        cam.scale = 5.0;
        cam.min_dist = 60.0;
        let reach = cam.target.length() + 2.0 * 800.0 / cam.scale;
        for step in 0..72 {
            cam.yaw = (step as f64 * 5.0).to_radians();
            for pitch in [-80.0f64, -30.0, 0.0, 35.0, 85.0] {
                cam.pitch = pitch.to_radians();
                let o = cam.project(rect, DVec3::ZERO);
                for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
                    let Some((a, b)) = cam.clip_to_view(rect, axis * -reach, axis * reach) else { continue };
                    for q in [a, b, (a + b) * 0.5] {
                        let (pa, pq) = (cam.project(rect, a), cam.project(rect, q));
                        let (u, v) = (pq - pa, o - pa);
                        let cross = (u.x * v.y - u.y * v.x).abs();
                        let scale = (u.length() * v.length()).max(1.0);
                        assert!(cross / scale < 1e-3, "yaw {} pitch {pitch}: ось {axis:?} мимо начала", cam.yaw);
                    }
                }
            }
        }
    }

    #[test]
    fn matrix_and_projection_agree() {
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0));
        for perspective in [false, true] {
            let mut cam = Camera::default();
            cam.perspective = perspective;
            cam.fov = 50.0;
            cam.target = DVec3::new(10.0, -5.0, 3.0);
            cam.scale = 6.0;
            let points = [
                DVec3::new(10.0, -5.0, 3.0),
                DVec3::new(40.0, 20.0, 10.0),
                DVec3::new(-30.0, 15.0, -12.0),
                DVec3::new(5.0, -60.0, 25.0),
            ];
            for p in points {
                let (mx, my, mz) = via_matrix(&cam, rect, p, 500.0);
                let s = cam.project(rect, p);
                assert!((mx - s.x).abs() < 0.05 && (my - s.y).abs() < 0.05, "{perspective}: {mx},{my} против {s:?}");
                assert!((-1.0..=1.0).contains(&mz), "глубина вне диапазона: {mz}");
                // Луч из этого пикселя проходит через саму точку.
                let (o, d) = cam.ray(rect, s);
                let miss = (p - o).cross(d).length();
                assert!(miss < 0.05, "{perspective}: луч мимо точки на {miss}");
            }
            // Ближе к камере — крупнее (только в перспективе); цель на месте в обоих режимах.
            let near = cam.project(rect, cam.target + cam.up() * 10.0 + cam.eye_dir() * 30.0);
            let far = cam.project(rect, cam.target + cam.up() * 10.0 - cam.eye_dir() * 30.0);
            let (near_h, far_h) = (rect.center().y - near.y, rect.center().y - far.y);
            assert_eq!(near_h > far_h + 0.5, perspective, "{perspective}: {near_h} {far_h}");
        }
    }
}
