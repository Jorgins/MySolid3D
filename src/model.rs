//! Параметрическая модель детали: дерево построения и его пересчёт в тело.

use glam::{DVec2, DVec3};
use opencascade::primitives::{Edge, Face, Shape, Wire};
use serde::{Deserialize, Serialize};
use std::rc::Rc;

pub const MIN_SIZE: f64 = 0.01;
const THROUGH_ALL: f64 = 10_000.0;
const MESH_TOLERANCE: f64 = 0.02;
/// Для проверки целостности поверхности хватает грубой сетки — она строится заметно быстрее.
const TORN_CHECK_TOLERANCE: f64 = 0.08;
/// Насколько может сместиться центр грани, чтобы она считалась той же самой.
const FACE_MATCH: f64 = 0.05;

/// Плоскость эскиза: начало и два единичных ортогональных вектора.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plane {
    pub origin: DVec3,
    pub x: DVec3,
    pub y: DVec3,
}

impl Plane {
    pub const TOP: Plane = Plane { origin: DVec3::ZERO, x: DVec3::X, y: DVec3::Y };
    pub const FRONT: Plane = Plane { origin: DVec3::ZERO, x: DVec3::X, y: DVec3::Z };
    pub const RIGHT: Plane = Plane { origin: DVec3::ZERO, x: DVec3::Y, y: DVec3::Z };

    pub fn normal(&self) -> DVec3 {
        self.x.cross(self.y)
    }

    pub fn from_normal(origin: DVec3, normal: DVec3) -> Plane {
        let n = normal.normalize();
        let x = if n.z.abs() > 0.999 { DVec3::X } else { DVec3::Z.cross(n).normalize() };
        let y = n.cross(x).normalize();
        Plane { origin, x, y }
    }

    pub fn to_world(&self, p: DVec2) -> DVec3 {
        self.origin + self.x * p.x + self.y * p.y
    }

    pub fn to_local(&self, p: DVec3) -> DVec2 {
        let d = p - self.origin;
        DVec2::new(d.dot(self.x), d.dot(self.y))
    }

    fn shifted(&self, along_normal: f64) -> Plane {
        Plane { origin: self.origin + self.normal() * along_normal, ..*self }
    }
}


/// Точка контура. `bulge` описывает сегмент от этой точки к следующей:
/// 0 — отрезок, иначе дуга (тангенс четверти её угла; «+» — против часовой стрелки).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub bulge: f64,
    /// Сегмент от этой точки — линия построения (штриховая): в тело не входит.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub construction: bool,
}

impl PathPoint {
    pub fn new(p: DVec2) -> Self {
        Self { x: p.x, y: p.y, bulge: 0.0, construction: false }
    }

    pub fn pos(&self) -> DVec2 {
        DVec2::new(self.x, self.y)
    }
}

/// Параметр дуги из `a` в `b`, проходящей через точку `p`.
pub fn bulge_through(a: DVec2, b: DVec2, p: DVec2) -> f64 {
    let chord = (b - a).length();
    let cross = (b - a).perp_dot(p - a);
    if chord < 1e-9 || cross.abs() < 1e-6 * chord {
        return 0.0;
    }
    let (u, v) = (a - p, b - p);
    let inscribed = u.perp_dot(v).abs().atan2(u.dot(v));
    let bulge = (1.0 / (inscribed / 2.0).tan()).min(50.0);
    if cross < 0.0 {
        bulge
    } else {
        -bulge
    }
}

/// Средняя точка дуги.
pub fn arc_mid(a: DVec2, b: DVec2, bulge: f64) -> DVec2 {
    let d = b - a;
    (a + b) * 0.5 + DVec2::new(d.y, -d.x) * (bulge * 0.5)
}

/// Центр дуги.
pub fn arc_center(a: DVec2, b: DVec2, bulge: f64) -> DVec2 {
    let n = DVec2::new(b.y - a.y, a.x - b.x) / (b - a).length();
    arc_mid(a, b, bulge) - n * arc_radius(a, b, bulge) * bulge.signum()
}

pub fn arc_radius(a: DVec2, b: DVec2, bulge: f64) -> f64 {
    (b - a).length() * (1.0 + bulge * bulge) / (4.0 * bulge.abs())
}

/// Добавляет в `out` точки сегмента `a → b` (без `a`, включая `b`).
pub fn segment_points(a: DVec2, b: DVec2, bulge: f64, out: &mut Vec<DVec2>) {
    let chord = (b - a).length();
    if bulge.abs() < 1e-9 || chord < 1e-9 {
        out.push(b);
        return;
    }
    let r = arc_radius(a, b, bulge);
    let n = DVec2::new(b.y - a.y, a.x - b.x) / chord;
    let center = arc_mid(a, b, bulge) - n * r * bulge.signum();
    let start = (a.y - center.y).atan2(a.x - center.x);
    let sweep = 4.0 * bulge.atan();
    let steps = ((sweep.abs() / (std::f64::consts::TAU / 72.0)).ceil() as usize).max(2);
    for i in 1..steps {
        let ang = start + sweep * i as f64 / steps as f64;
        out.push(center + DVec2::new(ang.cos(), ang.sin()) * r);
    }
    out.push(b);
}

fn point_in_polygon(poly: &[DVec2], p: DVec2) -> bool {
    let mut inside = false;
    for w in poly.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            inside = !inside;
        }
    }
    inside
}

fn segment_distance(a: DVec2, b: DVec2, p: DVec2) -> f64 {
    let d = b - a;
    let len2 = d.length_squared();
    let t = if len2 < 1e-18 { 0.0 } else { ((p - a).dot(d) / len2).clamp(0.0, 1.0) };
    (a + d * t - p).length()
}

fn segments_cross(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> bool {
    let side = |p: DVec2, q: DVec2, r: DVec2| (q - p).perp_dot(r - p);
    let (d1, d2, d3, d4) = (side(c, d, a), side(c, d, b), side(a, b, c), side(a, b, d));
    let eps = 1e-9;
    ((d1 > eps && d2 < -eps) || (d1 < -eps && d2 > eps)) && ((d3 > eps && d4 < -eps) || (d3 < -eps && d4 > eps))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SketchShape {
    /// `x`, `y` — левый нижний угол.
    Rect { x: f64, y: f64, w: f64, h: f64 },
    Circle { cx: f64, cy: f64, r: f64 },
    /// Контур из отрезков и дуг.
    Path { points: Vec<PathPoint>, closed: bool },
}

impl SketchShape {
    pub fn is_closed(&self) -> bool {
        match self {
            SketchShape::Path { closed, .. } => *closed,
            _ => true,
        }
    }

    /// Характерные точки: вершины контура, углы прямоугольника (с левого нижнего, против часовой), центр окружности.
    pub fn points(&self) -> Vec<DVec2> {
        match self {
            SketchShape::Rect { x, y, w, h } => {
                [(*x, *y), (x + w, *y), (x + w, y + h), (*x, y + h)].map(|(px, py)| DVec2::new(px, py)).to_vec()
            }
            SketchShape::Circle { cx, cy, .. } => vec![DVec2::new(*cx, *cy)],
            SketchShape::Path { points, .. } => points.iter().map(PathPoint::pos).collect(),
        }
    }

    /// Сегмент `i` (от точки `i` к следующей): начало, конец, выпуклость дуги.
    pub fn segment(&self, i: usize) -> Option<(DVec2, DVec2, f64)> {
        match self {
            SketchShape::Rect { .. } => {
                let c = self.points();
                (i < 4).then(|| (c[i], c[(i + 1) % 4], 0.0))
            }
            SketchShape::Circle { .. } => None,
            SketchShape::Path { points, closed } => {
                let n = points.len();
                let count = if *closed { n } else { n.saturating_sub(1) };
                (i < count).then(|| (points[i].pos(), points[(i + 1) % n].pos(), points[i].bulge))
            }
        }
    }

    /// Растягивает фигуру от неподвижной точки `anchor`: по X в `sx` раз, по Y в `sy` раз (знак — отражение).
    pub fn stretch(&mut self, anchor: DVec2, sx: f64, sy: f64) {
        let map = |p: DVec2| anchor + (p - anchor) * DVec2::new(sx, sy);
        match self {
            SketchShape::Rect { x, y, w, h } => {
                let (a, b) = (map(DVec2::new(*x, *y)), map(DVec2::new(*x + *w, *y + *h)));
                *x = a.x.min(b.x);
                *y = a.y.min(b.y);
                *w = (b.x - a.x).abs();
                *h = (b.y - a.y).abs();
            }
            SketchShape::Circle { cx, cy, r } => {
                let c = map(DVec2::new(*cx, *cy));
                (*cx, *cy) = (c.x, c.y);
                *r *= (sx.abs() + sy.abs()) / 2.0;
            }
            SketchShape::Path { points, .. } => {
                for p in points.iter_mut() {
                    let q = map(p.pos());
                    (p.x, p.y) = (q.x, q.y);
                    // Отражение по одной оси разворачивает и дугу.
                    if sx * sy < 0.0 {
                        p.bulge = -p.bulge;
                    }
                }
            }
        }
    }

    /// Сегмент `i` — линия построения.
    pub fn segment_construction(&self, i: usize) -> bool {
        matches!(self, SketchShape::Path { points, .. } if points.get(i).is_some_and(|p| p.construction))
    }

    /// Вся фигура состоит из линий построения: в тело она не входит.
    pub fn is_construction(&self) -> bool {
        let mut i = 0;
        while self.segment(i).is_some() {
            if !self.segment_construction(i) {
                return false;
            }
            i += 1;
        }
        i > 0
    }

    /// Превращает прямоугольник в обычный контур из четырёх отрезков (для поворота, скругления углов).
    fn make_path(&mut self) {
        if matches!(self, SketchShape::Rect { .. }) {
            let points = self.points().into_iter().map(PathPoint::new).collect();
            *self = SketchShape::Path { points, closed: true };
        }
    }

    /// Центр габаритного прямоугольника фигуры.
    pub fn center(&self) -> DVec2 {
        let outline = self.outline();
        let lo = outline.iter().fold(DVec2::splat(f64::INFINITY), |a, b| a.min(*b));
        let hi = outline.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, b| a.max(*b));
        if outline.is_empty() { DVec2::ZERO } else { (lo + hi) * 0.5 }
    }

    /// Поворот вокруг точки `pivot` на `angle` радиан (против часовой стрелки).
    pub fn rotate(&mut self, pivot: DVec2, angle: f64) {
        let rot = DVec2::from_angle(angle);
        let turn = |p: DVec2| pivot + rot.rotate(p - pivot);
        self.make_path();
        match self {
            SketchShape::Circle { cx, cy, .. } => {
                let c = turn(DVec2::new(*cx, *cy));
                (*cx, *cy) = (c.x, c.y);
            }
            SketchShape::Path { points, .. } => {
                for p in points {
                    let q = turn(p.pos());
                    (p.x, p.y) = (q.x, q.y);
                }
            }
            SketchShape::Rect { .. } => {}
        }
    }

    /// Масштабирование относительно точки `pivot`.
    pub fn scale(&mut self, pivot: DVec2, factor: f64) -> Result<(), String> {
        if factor < 1e-3 {
            return Err("масштаб должен быть больше нуля".into());
        }
        let map = |p: DVec2| pivot + (p - pivot) * factor;
        match self {
            SketchShape::Rect { x, y, w, h } => {
                let c = map(DVec2::new(*x, *y));
                (*x, *y, *w, *h) = (c.x, c.y, *w * factor, *h * factor);
            }
            SketchShape::Circle { cx, cy, r } => {
                let c = map(DVec2::new(*cx, *cy));
                (*cx, *cy, *r) = (c.x, c.y, *r * factor);
            }
            SketchShape::Path { points, .. } => {
                for p in points {
                    let q = map(p.pos());
                    (p.x, p.y) = (q.x, q.y);
                }
            }
        }
        Ok(())
    }

    /// Отражение относительно прямой через `origin` с направлением `dir`.
    pub fn mirror(&mut self, origin: DVec2, dir: DVec2) {
        let dir = dir.normalize();
        let flip = |p: DVec2| origin + dir * (2.0 * (p - origin).dot(dir)) - (p - origin);
        match self {
            SketchShape::Rect { x, y, w, h } if dir.x.abs() < 1e-12 || dir.y.abs() < 1e-12 => {
                let (a, b) = (flip(DVec2::new(*x, *y)), flip(DVec2::new(*x + *w, *y + *h)));
                (*x, *y) = (a.x.min(b.x), a.y.min(b.y));
            }
            SketchShape::Circle { cx, cy, .. } => {
                let c = flip(DVec2::new(*cx, *cy));
                (*cx, *cy) = (c.x, c.y);
            }
            _ => {
                self.make_path();
                if let SketchShape::Path { points, .. } = self {
                    for p in points {
                        let q = flip(p.pos());
                        // Отражение меняет направление обхода дуг.
                        (p.x, p.y, p.bulge) = (q.x, q.y, -p.bulge);
                    }
                }
            }
        }
    }

    /// Контур, отстоящий от этого на `distance` (плюс — наружу, минус — внутрь).
    pub fn offset(&self, distance: f64) -> Result<SketchShape, String> {
        const SMALL: &str = "смещение внутрь больше самой фигуры";
        match self {
            SketchShape::Rect { x, y, w, h } => {
                let (w, h) = (w + 2.0 * distance, h + 2.0 * distance);
                if w < MIN_SIZE || h < MIN_SIZE {
                    return Err(SMALL.into());
                }
                Ok(SketchShape::Rect { x: x - distance, y: y - distance, w, h })
            }
            SketchShape::Circle { cx, cy, r } => {
                if r + distance < MIN_SIZE {
                    return Err(SMALL.into());
                }
                Ok(SketchShape::Circle { cx: *cx, cy: *cy, r: r + distance })
            }
            SketchShape::Path { points, closed } => {
                let n = points.len();
                if !closed || n < 2 {
                    return Err("смещать можно только замкнутый контур".into());
                }
                let outline = self.outline();
                let area: f64 = outline.windows(2).map(|w| w[0].perp_dot(w[1])).sum();
                // Наружу — вправо по ходу обхода, если контур идёт против часовой стрелки.
                let d = if area >= 0.0 { distance } else { -distance };
                // Смещённые концы каждого сегмента.
                let mut ends = Vec::with_capacity(n);
                for i in 0..n {
                    let (a, b, bulge) = (points[i].pos(), points[(i + 1) % n].pos(), points[i].bulge);
                    let chord = b - a;
                    if chord.length() < 1e-9 {
                        return Err("в контуре есть совпадающие точки".into());
                    }
                    if bulge == 0.0 {
                        let normal = DVec2::new(chord.y, -chord.x) / chord.length();
                        ends.push((a + normal * d, b + normal * d));
                    } else {
                        let center = arc_center(a, b, bulge);
                        let radius = arc_radius(a, b, bulge);
                        let grown = radius + d * bulge.signum();
                        if grown < MIN_SIZE {
                            return Err("дуга контура при таком смещении исчезает".into());
                        }
                        ends.push((center + (a - center) * (grown / radius), center + (b - center) * (grown / radius)));
                    }
                }
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    let prev = (i + n - 1) % n;
                    let (end, start) = (ends[prev].1, ends[i].0);
                    let pos = if (end - start).length() < 1e-6 {
                        start // сегменты стыкуются по касательной
                    } else if points[prev].bulge == 0.0 && points[i].bulge == 0.0 {
                        let (u, v) = (ends[prev].1 - ends[prev].0, ends[i].1 - ends[i].0);
                        let cross = u.perp_dot(v);
                        if cross.abs() < 1e-9 {
                            start
                        } else {
                            ends[prev].0 + u * ((ends[i].0 - ends[prev].0).perp_dot(v) / cross)
                        }
                    } else {
                        return Err("углы между дугой и соседним сегментом пока не смещаются — скруглите их".into());
                    };
                    out.push(PathPoint { x: pos.x, y: pos.y, bulge: points[i].bulge, construction: points[i].construction });
                }
                // Сегмент, развернувшийся в обратную сторону, — признак того, что контур схлопнулся.
                let flipped = (0..n).any(|i| {
                    let was = points[(i + 1) % n].pos() - points[i].pos();
                    (out[(i + 1) % n].pos() - out[i].pos()).dot(was) <= 1e-9
                });
                let shape = SketchShape::Path { points: out, closed: true };
                if flipped || shape.self_intersects() {
                    return Err(SMALL.into());
                }
                Ok(shape)
            }
        }
    }

    /// Скругляет (`chamfer == false`) или срезает фаской угол контура в точке `vertex`;
    /// без точки — все углы, где это возможно. Возвращает число обработанных углов.
    pub fn round_corners(&mut self, vertex: Option<usize>, size: f64, chamfer: bool) -> Result<usize, String> {
        if size < MIN_SIZE {
            return Err("размер должен быть больше нуля".into());
        }
        if matches!(self, SketchShape::Circle { .. }) {
            return Err("у окружности нет углов".into());
        }
        self.make_path();
        let SketchShape::Path { points, closed } = self else { unreachable!() };
        let closed = *closed;
        let corner = |points: &mut Vec<PathPoint>, i: usize| -> Result<(), String> {
            let n = points.len();
            if n < 3 || (!closed && (i == 0 || i + 1 >= n)) {
                return Err("в этой точке нет угла".into());
            }
            let (prev, next) = ((i + n - 1) % n, (i + 1) % n);
            if points[prev].bulge != 0.0 || points[i].bulge != 0.0 {
                return Err("угол должен быть между двумя отрезками".into());
            }
            let v = points[i].pos();
            let (to_prev, to_next) = (points[prev].pos() - v, points[next].pos() - v);
            let (len1, len2) = (to_prev.length(), to_next.length());
            if len1 < 1e-9 || len2 < 1e-9 {
                return Err("в контуре есть совпадающие точки".into());
            }
            let (u1, u2) = (to_prev / len1, to_next / len2);
            let angle = u1.dot(u2).clamp(-1.0, 1.0).acos();
            if angle < 1e-3 || angle > std::f64::consts::PI - 1e-3 {
                return Err("в этой точке нет угла".into());
            }
            // Отступ от вершины вдоль каждой стороны.
            let cut = if chamfer { size } else { size / (angle / 2.0).tan() };
            if cut > len1 - MIN_SIZE || cut > len2 - MIN_SIZE {
                return Err("размер не помещается на сторонах угла".into());
            }
            let (p1, p2) = (v + u1 * cut, v + u2 * cut);
            let bulge = if chamfer {
                0.0
            } else {
                // Дуга поворачивает в ту же сторону, что и контур в этом углу.
                ((std::f64::consts::PI - angle) / 4.0).tan().copysign((-u1).perp_dot(u2))
            };
            points[i] = PathPoint { x: p1.x, y: p1.y, bulge, construction: points[i].construction };
            points.insert(i + 1, PathPoint::new(p2));
            Ok(())
        };
        match vertex {
            Some(i) if i < points.len() => corner(points, i).map(|()| 1),
            Some(_) => Err("точка контура не найдена".into()),
            None => {
                let done = (0..points.len()).rev().filter(|&i| corner(points, i).is_ok()).count();
                if done == 0 {
                    return Err("подходящих углов нет или размер слишком велик".into());
                }
                Ok(done)
            }
        }
    }

    /// Ломаная, приближающая фигуру (у замкнутых первая точка повторяется в конце).
    pub fn outline(&self) -> Vec<DVec2> {
        match self {
            SketchShape::Rect { x, y, w, h } => {
                [(*x, *y), (x + w, *y), (x + w, y + h), (*x, y + h), (*x, *y)]
                    .map(|(px, py)| DVec2::new(px, py))
                    .to_vec()
            }
            SketchShape::Circle { cx, cy, r } => (0..=72)
                .map(|i| {
                    let a = i as f64 / 72.0 * std::f64::consts::TAU;
                    DVec2::new(cx + r * a.cos(), cy + r * a.sin())
                })
                .collect(),
            SketchShape::Path { points, closed } => {
                let mut out = Vec::new();
                let Some(first) = points.first() else { return out };
                out.push(first.pos());
                let count = if *closed { points.len() } else { points.len() - 1 };
                for i in 0..count {
                    let next = points[(i + 1) % points.len()];
                    segment_points(points[i].pos(), next.pos(), points[i].bulge, &mut out);
                }
                out
            }
        }
    }

    /// Полностью ли `self` содержит `other` (для определения отверстий в контуре).
    fn contains(&self, other: &SketchShape) -> bool {
        let poly = self.outline();
        other.outline().iter().all(|p| point_in_polygon(&poly, *p))
    }

    fn self_intersects(&self) -> bool {
        let o = self.outline();
        let n = o.len().saturating_sub(1);
        for i in 0..n {
            for j in i + 2..n {
                if i == 0 && j == n - 1 {
                    continue; // соседние через замыкание
                }
                if segments_cross(o[i], o[i + 1], o[j], o[j + 1]) {
                    return true;
                }
            }
        }
        false
    }

    pub fn translate(&mut self, d: DVec2) {
        match self {
            SketchShape::Rect { x, y, .. } => {
                *x += d.x;
                *y += d.y;
            }
            SketchShape::Circle { cx, cy, .. } => {
                *cx += d.x;
                *cy += d.y;
            }
            SketchShape::Path { points, .. } => {
                for p in points {
                    p.x += d.x;
                    p.y += d.y;
                }
            }
        }
    }

    /// Расстояние от точки до контура фигуры (в единицах эскиза).
    pub fn outline_distance(&self, p: DVec2) -> f64 {
        let o = self.outline();
        match o.len() {
            0 => f64::INFINITY,
            1 => (o[0] - p).length(),
            _ => o.windows(2).map(|w| segment_distance(w[0], w[1], p)).fold(f64::INFINITY, f64::min),
        }
    }

    fn wire(&self, plane: &Plane) -> Result<Wire, String> {
        match self {
            SketchShape::Rect { x, y, w, h } => {
                if *w < MIN_SIZE || *h < MIN_SIZE {
                    return Err("прямоугольник нулевого размера".into());
                }
                let pts = [(*x, *y), (x + w, *y), (x + w, y + h), (*x, y + h)]
                    .map(|(px, py)| plane.to_world(DVec2::new(px, py)));
                Wire::from_ordered_points(pts).map_err(|e| e.to_string())
            }
            SketchShape::Circle { cx, cy, r } => {
                if *r < MIN_SIZE {
                    return Err("окружность нулевого радиуса".into());
                }
                let edge = Edge::circle(plane.to_world(DVec2::new(*cx, *cy)), plane.normal(), *r);
                Ok(Wire::from_edges([&edge]))
            }
            SketchShape::Path { points, closed } => {
                if !closed {
                    return Err("контур не замкнут — завершите его щелчком по первой точке".into());
                }
                let n = points.len();
                if n < 2 || (n == 2 && points.iter().all(|p| p.bulge == 0.0)) {
                    return Err("в контуре слишком мало сегментов".into());
                }
                if (0..n).any(|i| (points[i].pos() - points[(i + 1) % n].pos()).length() < MIN_SIZE) {
                    return Err("в контуре есть совпадающие точки".into());
                }
                if self.self_intersects() {
                    return Err("контур пересекает сам себя".into());
                }
                let edges: Vec<Edge> = (0..n)
                    .map(|i| {
                        let (a, b) = (points[i].pos(), points[(i + 1) % n].pos());
                        if points[i].bulge == 0.0 {
                            Edge::segment(plane.to_world(a), plane.to_world(b))
                        } else {
                            let mid = arc_mid(a, b, points[i].bulge);
                            Edge::arc(plane.to_world(a), plane.to_world(mid), plane.to_world(b))
                        }
                    })
                    .collect();
                Ok(Wire::from_edges(edges.iter()))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sketch {
    pub plane: Plane,
    pub shapes: Vec<SketchShape>,
    /// Размеры, поставленные пользователем. Показывают текущее значение; ввод нового меняет геометрию.
    #[serde(default)]
    pub dims: Vec<Dim>,
    /// Картинка-подложка, по которой удобно обводить контуры.
    #[serde(default)]
    pub image: Option<SketchImage>,
    /// Где стоит размерная линия (подпись) у поставленных размеров, как их разместил пользователь.
    #[serde(default)]
    pub dim_places: Vec<(Dim, DVec2)>,
    /// Какие области (номера фигур) вытягивает каждая операция (номер элемента); нет записи — области по умолчанию.
    #[serde(default)]
    pub region_sel: Vec<(u64, Vec<usize>)>,
    /// Точки и отрезки более ранних эскизов в плоскости этого эскиза (обновляется перед показом; в файл не пишется).
    #[serde(skip)]
    pub ext_cache: std::collections::HashMap<ExtKey, (DVec2, DVec2)>,
}

/// Картинка-подложка эскиза. В файле детали хранится только путь к ней.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchImage {
    pub path: String,
    /// Левый нижний угол картинки в координатах эскиза.
    pub x: f64,
    pub y: f64,
    /// Ширина в миллиметрах; высота получается из пропорций картинки.
    pub width: f64,
    pub opacity: f32,
}

impl Sketch {
    /// Замкнутые фигуры, из которых можно выбирать области вытягивания (линии построения не в счёт).
    pub fn region_candidates(&self) -> Vec<usize> {
        (0..self.shapes.len()).filter(|&i| !self.shapes[i].is_construction() && self.shapes[i].is_closed()).collect()
    }

    /// Области по умолчанию: внешние контуры и «острова» внутри отверстий (чётная вложенность).
    pub fn default_regions(&self) -> Vec<usize> {
        let list = self.region_candidates();
        list.iter()
            .copied()
            .filter(|&i| list.iter().filter(|&&j| j != i && self.shapes[j].contains(&self.shapes[i])).count() % 2 == 0)
            .collect()
    }

    /// Области, выбранные для операции; `None` — по умолчанию.
    pub fn regions_of(&self, feature: u64) -> Option<&[usize]> {
        self.region_sel.iter().find(|(f, _)| *f == feature).map(|(_, l)| l.as_slice())
    }

    /// Области, которые вытянет операция: выбранные или по умолчанию.
    pub fn active_regions(&self, feature: u64) -> Vec<usize> {
        self.regions_of(feature).map(<[usize]>::to_vec).unwrap_or_else(|| self.default_regions())
    }

    /// Включает или выключает область `shape` в операции `feature`.
    pub fn toggle_region(&mut self, feature: u64, shape: usize) {
        let mut list = self.active_regions(feature);
        match list.iter().position(|&i| i == shape) {
            Some(at) => drop(list.remove(at)),
            None => list.push(shape),
        }
        list.sort_unstable();
        self.region_sel.retain(|(f, _)| *f != feature);
        if list != self.default_regions() {
            self.region_sel.push((feature, list));
        }
    }

    /// Возвращает операции области по умолчанию.
    pub fn reset_regions(&mut self, feature: u64) {
        self.region_sel.retain(|(f, _)| *f != feature);
    }

    /// Тела-призмы из выбранных областей эскиза: область — фигура без вложенных в неё фигур (они — отверстия).
    /// `regions` — номера фигур; `None` — по умолчанию.
    fn prisms(&self, dir: DVec3, regions: Option<&[usize]>) -> Result<Vec<Shape>, String> {
        // Линии построения в тело не входят.
        let list: Vec<(usize, &SketchShape)> =
            self.shapes.iter().enumerate().filter(|(_, s)| !s.is_construction()).collect();
        if list.is_empty() {
            return Err("эскиз пуст".into());
        }
        if list.iter().any(|(_, s)| !s.is_closed()) {
            return Err("в эскизе есть незамкнутый контур — завершите его щелчком по первой точке".into());
        }
        let n = list.len();
        let depth: Vec<usize> = (0..n)
            .map(|i| (0..n).filter(|&j| j != i && list[j].1.contains(list[i].1)).count())
            .collect();
        let chosen: Vec<usize> = (0..n)
            .filter(|&i| match regions {
                None => depth[i] % 2 == 0,
                Some(r) => r.contains(&list[i].0),
            })
            .collect();
        if chosen.is_empty() {
            return Err("не выбрано ни одной области эскиза".into());
        }
        let dir_unit = dir.normalize();
        // Отверстия вытягиваем с запасом в обе стороны, чтобы булева операция была устойчивой.
        let hole_plane = self.plane.shifted(-dir_unit.dot(self.plane.normal()));
        let hole_dir = dir + dir_unit * 2.0;

        let mut out = Vec::new();
        for i in chosen {
            let face = Face::from_wire(&list[i].1.wire(&self.plane)?);
            let mut solid: Shape = face.extrude(dir).into();
            for k in 0..n {
                if depth[k] == depth[i] + 1 && list[i].1.contains(list[k].1) {
                    let hole_face = Face::from_wire(&list[k].1.wire(&hole_plane)?);
                    let hole: Shape = hole_face.extrude(hole_dir).into();
                    solid = solid.subtract(&hole).shape;
                }
            }
            out.push(solid);
        }
        Ok(out)
    }
}

/// Склеивает фигуры попарно (дерево), чтобы каждая склейка работала с небольшими телами.
fn fuse_all(mut shapes: Vec<Shape>) -> Option<Shape> {
    while shapes.len() > 1 {
        let mut next = Vec::with_capacity(shapes.len() / 2 + 1);
        let mut it = shapes.into_iter();
        while let Some(a) = it.next() {
            match it.next() {
                Some(b) => next.push(a.union(&b).shape),
                None => next.push(a),
            }
        }
        shapes = next;
    }
    shapes.pop()
}

/// Элемент эскиза, к которому привязывается размер.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DimRef {
    /// Начало координат эскиза.
    Origin,
    /// Характерная точка фигуры: (фигура, номер в `SketchShape::points`).
    Point(usize, usize),
    /// Сегмент контура или сторона прямоугольника: (фигура, номер сегмента).
    Segment(usize, usize),
    Circle(usize),
    /// Элемент более раннего эскиза (он только измеряется, не двигается): (эскиз, фигура, номер, это сегмент).
    Ext(u64, usize, usize, bool),
}

/// Ключ элемента более раннего эскиза: (эскиз, фигура, номер точки или сегмента, это сегмент).
pub type ExtKey = (u64, usize, usize, bool);

/// Нельзя двигать при подгонке размера: начало координат и элементы других эскизов.
fn immovable(r: DimRef) -> bool {
    matches!(r, DimRef::Origin | DimRef::Ext(..))
}

/// Точка, которую стрелкой выбирают для связи: обычная точка (или начало координат) либо середина сегмента.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    Ref(DimRef),
    /// Середина сегмента: (фигура, номер сегмента).
    Mid(usize, usize),
}

/// Как измеряется расстояние между двумя точками.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DimMode {
    Aligned,
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Dim {
    /// Длина отрезка, радиус дуги, сторона прямоугольника или диаметр окружности.
    Size(DimRef),
    /// Расстояние между двумя элементами. При вводе значения первый остаётся на месте, сдвигается второй.
    Distance(DimRef, DimRef, DimMode),
    /// Угол между двумя отрезками в градусах (0…180). При вводе значения поворачивается второй.
    Angle(DimRef, DimRef),
}

/// Взаимосвязь двух элементов эскиза. Применяется один раз: второй элемент подгоняется под первый.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Relation {
    Parallel,
    Perpendicular,
    Collinear,
    Coincident,
    /// Точка в середине отрезка.
    Midpoint,
    /// Две точки на одной горизонтали (одинаковая Y).
    Horizontal,
    /// Две точки на одной вертикали (одинаковая X).
    Vertical,
}

impl Relation {
    pub fn name(self) -> &'static str {
        match self {
            Relation::Parallel => "Параллельно",
            Relation::Perpendicular => "Перпендикулярно",
            Relation::Collinear => "Коллинеарно",
            Relation::Coincident => "Совпадение",
            Relation::Midpoint => "Середина отрезка",
            Relation::Horizontal => "Горизонтально",
            Relation::Vertical => "Вертикально",
        }
    }
}

/// Точка или прямая, которую задаёт элемент, — для размеров-расстояний.
enum RefGeom {
    Point(DVec2),
    Line(DVec2, DVec2),
}

const DIM_GONE: &str = "элемент размера больше не существует";
const DIM_SHARED: &str = "элементы размера имеют общую точку — расстояние между ними задать нельзя";

fn closer(best: &mut Option<(f64, DimRef)>, tol: f64, d: f64, r: DimRef) {
    if d < tol && best.map_or(true, |(bd, _)| d < bd) {
        *best = Some((d, r));
    }
}

/// Параметр дуги с теми же концами и направлением, но радиусом `radius`.
fn bulge_for_radius(a: DVec2, b: DVec2, bulge: f64, radius: f64) -> Result<f64, String> {
    let half = (b - a).length() / 2.0;
    if radius < half - 1e-9 {
        return Err(format!("радиус не может быть меньше половины хорды ({half:.2} мм)"));
    }
    let minor = 2.0 * (half / radius).min(1.0).asin();
    // Дуга больше полуокружности остаётся «большой».
    let sweep = if bulge.abs() > 1.0 { std::f64::consts::TAU - minor } else { minor };
    Ok((sweep / 4.0).tan().copysign(bulge))
}

/// Сдвигает точки `start` контура на `d`, по возможности сохраняя направления соседних сегментов:
/// сдвиг передаётся по цепочке дальше, пока не встретится отрезок, параллельный `d`
/// (он просто меняет длину), или конец незамкнутого контура. Точки `anchors` неподвижны.
fn shift_chain(
    points: &mut [PathPoint],
    closed: bool,
    start: &[usize],
    anchors: &[usize],
    d: DVec2,
) -> Result<(), String> {
    if start.iter().any(|i| anchors.contains(i)) {
        return Err(DIM_SHARED.into());
    }
    let n = points.len();
    if d.length() < 1e-12 {
        return Ok(());
    }
    let mut moved = vec![false; n];
    for &i in start {
        moved[i] = true;
    }
    let absorbs = |seg: usize| {
        let e = points[(seg + 1) % n].pos() - points[seg].pos();
        points[seg].bulge == 0.0 && e.length() > 1e-9 && e.perp_dot(d).abs() < 1e-6 * e.length() * d.length()
    };
    for &s in start {
        for forward in [true, false] {
            let mut tail = Vec::new();
            let mut i = s;
            let keep = loop {
                if !closed && (if forward { i + 1 >= n } else { i == 0 }) {
                    break true;
                }
                let next = if forward { (i + 1) % n } else { (i + n - 1) % n };
                if moved[next] || tail.contains(&next) {
                    break true;
                }
                // Упёрлись в неподвижную точку — цепочку не трогаем, меняется только соседний сегмент.
                if anchors.contains(&next) {
                    break false;
                }
                if absorbs(if forward { i } else { next }) {
                    break true;
                }
                tail.push(next);
                i = next;
            };
            if keep {
                for i in tail {
                    moved[i] = true;
                }
            }
        }
    }
    for (p, m) in points.iter_mut().zip(moved) {
        if m {
            p.x += d.x;
            p.y += d.y;
        }
    }
    Ok(())
}

impl Sketch {
    pub fn new(plane: Plane, shapes: Vec<SketchShape>) -> Self {
        Self { plane, shapes, dims: Vec::new(), image: None, dim_places: Vec::new(), region_sel: Vec::new(), ext_cache: Default::default() }
    }

    /// Удаляет фигуру вместе с её размерами; размеры остальных фигур остаются на своих местах.
    pub fn remove_shape(&mut self, index: usize) {
        if index >= self.shapes.len() {
            return;
        }
        self.shapes.remove(index);
        // Выбранные области остаются теми же, хотя номера фигур сдвинулись.
        for (_, list) in &mut self.region_sel {
            list.retain(|&i| i != index);
            list.iter_mut().for_each(|i| *i -= usize::from(*i > index));
        }
        let shift = |s: usize| (s != index).then(|| if s > index { s - 1 } else { s });
        let fix = |r: DimRef| match r {
            DimRef::Origin => Some(r),
            DimRef::Point(s, i) => shift(s).map(|s| DimRef::Point(s, i)),
            DimRef::Segment(s, i) => shift(s).map(|s| DimRef::Segment(s, i)),
            DimRef::Circle(s) => shift(s).map(DimRef::Circle),
            DimRef::Ext(..) => Some(r),
        };
        self.dims = self
            .dims
            .iter()
            .filter_map(|dim| match *dim {
                Dim::Size(r) => fix(r).map(Dim::Size),
                Dim::Distance(a, b, mode) => Some(Dim::Distance(fix(a)?, fix(b)?, mode)),
                Dim::Angle(a, b) => Some(Dim::Angle(fix(a)?, fix(b)?)),
            })
            .collect();
    }

    /// Скругляет или срезает только указанные углы фигуры `index`. Если хоть один угол не получается,
    /// фигура остаётся как была, а ошибка говорит почему.
    pub fn round_corner_set(&mut self, index: usize, vertices: &[usize], size: f64, chamfer: bool) -> Result<usize, String> {
        let backup = (self.shapes.get(index).ok_or("фигура не найдена")?.clone(), self.dims.clone());
        // С большего номера: вставка новой точки сдвигает только следующие за ней номера.
        let mut order: Vec<usize> = vertices.to_vec();
        order.sort_unstable_by(|a, b| b.cmp(a));
        order.dedup();
        let mut done = 0;
        for v in order {
            match self.round_corners(index, Some(v), size, chamfer) {
                Ok(n) => done += n,
                Err(e) => {
                    self.shapes[index] = backup.0;
                    self.dims = backup.1;
                    return Err(format!("точка {}: {e}", v + 1));
                }
            }
        }
        Ok(done)
    }

    /// Скругляет или срезает углы фигуры `index` (см. `SketchShape::round_corners`).
    pub fn round_corners(
        &mut self,
        index: usize,
        vertex: Option<usize>,
        size: f64,
        chamfer: bool,
    ) -> Result<usize, String> {
        let done = self.shapes.get_mut(index).ok_or("фигура не найдена")?.round_corners(vertex, size, chamfer)?;
        // Номера точек фигуры сдвинулись — её размеры больше ни к чему не привязаны.
        let on_shape = |r: DimRef| matches!(r, DimRef::Point(s, _) | DimRef::Segment(s, _) | DimRef::Circle(s) if s == index);
        self.dims.retain(|dim| match *dim {
            Dim::Size(r) => !on_shape(r),
            Dim::Distance(a, b, _) | Dim::Angle(a, b) => !on_shape(a) && !on_shape(b),
        });
        Ok(done)
    }

    /// Копии фигуры `index` сеткой `columns` × `rows` с шагами `step` (сама фигура — первая в сетке).
    /// Возвращает число добавленных копий.
    pub fn pattern_grid(&mut self, index: usize, columns: usize, rows: usize, step: DVec2) -> Result<usize, String> {
        let shape = self.shapes.get(index).ok_or("фигура не найдена")?.clone();
        if columns == 0 || rows == 0 || columns * rows < 2 {
            return Err("в массиве должно быть хотя бы две фигуры".into());
        }
        if columns * rows > 1000 {
            return Err("слишком много копий".into());
        }
        for row in 0..rows {
            for column in 0..columns {
                if row + column > 0 {
                    let mut copy = shape.clone();
                    copy.translate(DVec2::new(step.x * column as f64, step.y * row as f64));
                    self.shapes.push(copy);
                }
            }
        }
        Ok(columns * rows - 1)
    }

    /// Круговой массив: `count` фигур (вместе с исходной) вокруг `center` в секторе `sweep` градусов.
    /// При полном круге фигуры расставляются равномерно, иначе первая и последняя стоят на краях сектора.
    pub fn pattern_circle(&mut self, index: usize, count: usize, center: DVec2, sweep: f64) -> Result<usize, String> {
        let shape = self.shapes.get(index).ok_or("фигура не найдена")?.clone();
        if !(2..=1000).contains(&count) {
            return Err("в массиве должно быть от 2 до 1000 фигур".into());
        }
        let full = (sweep.abs() - 360.0).abs() < 1e-9;
        let step = sweep.to_radians() / if full { count as f64 } else { (count - 1) as f64 };
        for k in 1..count {
            let mut copy = shape.clone();
            copy.rotate(center, step * k as f64);
            self.shapes.push(copy);
        }
        Ok(count - 1)
    }

    /// Ставит размер в эскиз (повторно тот же размер не добавляется). Возвращает его номер.
    pub fn store_dim(&mut self, dim: Dim) -> usize {
        match self.dims.iter().position(|d| *d == dim) {
            Some(i) => i,
            None => {
                self.dims.push(dim);
                self.dims.len() - 1
            }
        }
    }

    /// Размер одного элемента: у отрезка — расстояние между его концами (его можно мерить
    /// напрямую, по X или по Y), у дуги — радиус, у окружности — диаметр.
    pub fn element_dim(&self, r: DimRef) -> Option<Dim> {
        match r {
            DimRef::Segment(s, i) => {
                let shape = self.shapes.get(s)?;
                let (_, _, bulge) = shape.segment(i)?;
                Some(if bulge == 0.0 {
                    let next = (i + 1) % shape.points().len();
                    Dim::Distance(DimRef::Point(s, i), DimRef::Point(s, next), DimMode::Aligned)
                } else {
                    Dim::Size(r)
                })
            }
            DimRef::Circle(_) => self.dim_value(&Dim::Size(r)).map(|_| Dim::Size(r)),
            _ => None,
        }
    }

    /// Прямой отрезок рядом с точкой `p`: (фигура, номер сегмента).
    pub fn nearest_line(&self, p: DVec2, tol: f64) -> Option<(usize, usize)> {
        let mut best: Option<(f64, (usize, usize))> = None;
        for (s, shape) in self.shapes.iter().enumerate() {
            let mut i = 0;
            while let Some((a, b, bulge)) = shape.segment(i) {
                let d = segment_distance(a, b, p);
                if bulge == 0.0 && d < tol && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, (s, i)));
                }
                i += 1;
            }
        }
        best.map(|(_, r)| r)
    }

    /// Делает отрезок линией построения или возвращает его в контур.
    pub fn set_construction(&mut self, shape: usize, seg: usize, on: bool) -> Result<(), String> {
        let sh = self.shapes.get_mut(shape).ok_or(DIM_GONE)?;
        sh.segment(seg).ok_or(DIM_GONE)?;
        sh.make_path();
        let SketchShape::Path { points, .. } = sh else { return Err(DIM_GONE.into()) };
        points[seg].construction = on;
        Ok(())
    }

    /// Делает отрезок горизонтальным или вертикальным: его конец уходит на ось, длина сохраняется.
    pub fn make_axis(&mut self, shape: usize, seg: usize, horizontal: bool) -> Result<(), String> {
        let sh = self.shapes.get_mut(shape).ok_or(DIM_GONE)?;
        let (a, b, bulge) = sh.segment(seg).ok_or(DIM_GONE)?;
        if bulge != 0.0 {
            return Err("это дуга, а не отрезок".into());
        }
        let d = b - a;
        let len = d.length();
        let dir = if horizontal { DVec2::new(d.x.signum_or(1.0), 0.0) } else { DVec2::new(0.0, d.y.signum_or(1.0)) };
        let end = a + dir * len;
        if (end - b).length() < 1e-12 {
            return Ok(());
        }
        sh.make_path();
        let SketchShape::Path { points, .. } = sh else { return Err(DIM_GONE.into()) };
        let n = points.len();
        let j = (seg + 1) % n;
        (points[j].x, points[j].y) = (end.x, end.y);
        Ok(())
    }

    /// Положение выбранной точки.
    pub fn pick_pos(&self, pick: Pick) -> Option<DVec2> {
        match pick {
            Pick::Ref(r) => match self.ref_geom(r)? {
                RefGeom::Point(q) => Some(q),
                RefGeom::Line(..) => None,
            },
            Pick::Mid(s, i) => {
                let (a, b, bulge) = self.shapes.get(s)?.segment(i)?;
                Some(if bulge == 0.0 { (a + b) * 0.5 } else { arc_mid(a, b, bulge) })
            }
        }
    }

    /// Выбираемая точка рядом с `p`: сначала точки контура и начало координат, потом середины сегментов.
    pub fn pick_point(&self, p: DVec2, tol: f64) -> Option<Pick> {
        if let Some(r @ (DimRef::Point(..) | DimRef::Origin | DimRef::Ext(_, _, _, false))) = self.pick_ref(p, tol) {
            return Some(Pick::Ref(r));
        }
        let mut best: Option<(f64, Pick)> = None;
        for (s, shape) in self.shapes.iter().enumerate() {
            let mut i = 0;
            while shape.segment(i).is_some() {
                let pick = Pick::Mid(s, i);
                if let Some(q) = self.pick_pos(pick) {
                    let d = (q - p).length();
                    if d < tol && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, pick));
                    }
                }
                i += 1;
            }
        }
        best.map(|(_, pick)| pick)
    }

    /// Совмещает две выбранные точки или ставит их на одну горизонталь или вертикаль.
    /// Первая остаётся на месте, вторая (вместе с отрезком или фигурой) сдвигается.
    pub fn align(&mut self, a: Pick, b: Pick, relation: Relation) -> Result<(), String> {
        let (Some(pa), Some(pb)) = (self.pick_pos(a), self.pick_pos(b)) else { return Err(DIM_GONE.into()) };
        let d = match relation {
            Relation::Coincident => pa - pb,
            Relation::Horizontal => DVec2::new(0.0, pa.y - pb.y),
            Relation::Vertical => DVec2::new(pa.x - pb.x, 0.0),
            _ => return Err("для точек есть совпадение, горизонталь и вертикаль".into()),
        };
        if d.length() < 1e-12 {
            return Ok(());
        }
        let as_ref = |p: Pick| match p {
            Pick::Ref(r) => r,
            Pick::Mid(s, i) => DimRef::Segment(s, i),
        };
        self.nudge(as_ref(a), as_ref(b), d, None)
    }

    /// Сдвигает `b` на `d` относительно `a`; если `b` неподвижен (начало координат, другой эскиз) —
    /// вместо него на `-d` сдвигается `a`.
    fn nudge(&mut self, a: DimRef, b: DimRef, d: DVec2, except: Option<&Dim>) -> Result<(), String> {
        let (mover, other, delta) = match (immovable(a), immovable(b)) {
            (true, true) => return Err("оба элемента неподвижны — ничего двигать".into()),
            (_, true) => (a, b, -d),
            _ => (b, a, d),
        };
        // Если другой элемент этой же фигуры уже закреплён размером, фигуру целиком не сдвигаем:
        // меняется её форма (например, размер прямоугольника), а прежнее условие остаётся в силе.
        let own = match mover {
            DimRef::Point(s, _) | DimRef::Segment(s, _) | DimRef::Circle(s) => Some(s),
            _ => None,
        };
        match own.and_then(|s| self.pinned_ref(s, mover, except)) {
            Some(pin) => self.move_ref(mover, pin, delta),
            None => self.move_ref(mover, other, delta),
        }
    }

    /// Элемент фигуры `shape` (кроме `mover` и общих с ним точек), положение которого уже задано размером
    /// от чего-то вне фигуры; размер `except` не в счёт — его как раз сейчас меняют.
    fn pinned_ref(&self, shape: usize, mover: DimRef, except: Option<&Dim>) -> Option<DimRef> {
        let n = self.shapes.get(shape)?.points().len();
        if matches!(self.shapes[shape], SketchShape::Circle { .. }) {
            return None;
        }
        let points = |r: DimRef| match r {
            DimRef::Point(_, i) => vec![i],
            DimRef::Segment(_, i) => vec![i, (i + 1) % n.max(1)],
            _ => vec![],
        };
        let on_shape = |r: DimRef| matches!(r, DimRef::Point(s, _) | DimRef::Segment(s, _) if s == shape);
        let mover_points = points(mover);
        for dim in self.dims.iter().filter(|d| Some(*d) != except) {
            let Dim::Distance(a, b, _) = dim else { continue };
            // Размер между двумя элементами одной фигуры — внутренний, он положение не закрепляет.
            if on_shape(*a) && on_shape(*b) {
                continue;
            }
            for r in [*a, *b] {
                if on_shape(r) && points(r).iter().all(|p| !mover_points.contains(p)) {
                    return Some(r);
                }
            }
        }
        None
    }

    /// Элемент эскиза рядом с точкой `p`; точки важнее линий.
    pub fn pick_ref(&self, p: DVec2, tol: f64) -> Option<DimRef> {
        let mut best = None;
        for (s, shape) in self.shapes.iter().enumerate() {
            for (i, q) in shape.points().iter().enumerate() {
                closer(&mut best, tol, (*q - p).length(), DimRef::Point(s, i));
            }
        }
        closer(&mut best, tol, p.length(), DimRef::Origin);
        if best.is_none() {
            for (&(a, s, i, seg), (q, _)) in &self.ext_cache {
                if !seg {
                    closer(&mut best, tol, (*q - p).length(), DimRef::Ext(a, s, i, false));
                }
            }
        }
        if best.is_none() {
            for (s, shape) in self.shapes.iter().enumerate() {
                if matches!(shape, SketchShape::Circle { .. }) {
                    closer(&mut best, tol, shape.outline_distance(p), DimRef::Circle(s));
                }
                let mut i = 0;
                while let Some((a, b, bulge)) = shape.segment(i) {
                    let mut pts = vec![a];
                    segment_points(a, b, bulge, &mut pts);
                    let d = pts.windows(2).map(|w| segment_distance(w[0], w[1], p)).fold(f64::INFINITY, f64::min);
                    closer(&mut best, tol, d, DimRef::Segment(s, i));
                    i += 1;
                }
            }
            if best.is_none() {
                for (&(a, s, i, seg), (q0, q1)) in &self.ext_cache {
                    if seg {
                        closer(&mut best, tol, segment_distance(*q0, *q1, p), DimRef::Ext(a, s, i, true));
                    }
                }
            }
        }
        best.map(|(_, r)| r)
    }

    /// Ломаная элемента для подсветки (у точки — одна точка).
    pub fn ref_outline(&self, r: DimRef) -> Vec<DVec2> {
        match r {
            DimRef::Origin => vec![DVec2::ZERO],
            DimRef::Point(s, i) => {
                self.shapes.get(s).and_then(|shape| shape.points().get(i).copied()).into_iter().collect()
            }
            DimRef::Segment(s, i) => match self.shapes.get(s).and_then(|shape| shape.segment(i)) {
                Some((a, b, bulge)) => {
                    let mut pts = vec![a];
                    segment_points(a, b, bulge, &mut pts);
                    pts
                }
                None => vec![],
            },
            DimRef::Circle(s) => match self.shapes.get(s) {
                Some(shape @ SketchShape::Circle { .. }) => shape.outline(),
                _ => vec![],
            },
            DimRef::Ext(a, s, i, seg) => match self.ext_cache.get(&(a, s, i, seg)) {
                Some((p, q)) if seg => vec![*p, *q],
                Some((p, _)) => vec![*p],
                None => vec![],
            },
        }
    }

    fn ref_geom(&self, r: DimRef) -> Option<RefGeom> {
        match r {
            DimRef::Origin => Some(RefGeom::Point(DVec2::ZERO)),
            DimRef::Point(s, i) => self.shapes.get(s)?.points().get(i).copied().map(RefGeom::Point),
            DimRef::Segment(s, i) => {
                let (a, b, bulge) = self.shapes.get(s)?.segment(i)?;
                (bulge == 0.0 && (b - a).length() > 1e-9).then_some(RefGeom::Line(a, b))
            }
            DimRef::Circle(_) => None,
            DimRef::Ext(a, s, i, seg) => {
                let (p, q) = *self.ext_cache.get(&(a, s, i, seg))?;
                if !seg {
                    Some(RefGeom::Point(p))
                } else {
                    ((q - p).length() > 1e-9).then_some(RefGeom::Line(p, q))
                }
            }
        }
    }

    /// Расстояние со знаком от `a` до `b` и единичное направление, вдоль которого оно измеряется.
    fn distance_axis(&self, a: DimRef, b: DimRef, mode: DimMode) -> Option<(f64, DVec2)> {
        let normal = |p: DVec2, q: DVec2| {
            let d = (q - p).normalize();
            DVec2::new(-d.y, d.x)
        };
        Some(match (self.ref_geom(a)?, self.ref_geom(b)?) {
            (RefGeom::Point(p), RefGeom::Point(q)) => match mode {
                DimMode::Horizontal => (q.x - p.x, DVec2::X),
                DimMode::Vertical => (q.y - p.y, DVec2::Y),
                DimMode::Aligned => {
                    let len = (q - p).length();
                    if len < 1e-9 { (0.0, DVec2::X) } else { (len, (q - p) / len) }
                }
            },
            (RefGeom::Line(p0, p1), RefGeom::Point(q)) => {
                let n = normal(p0, p1);
                ((q - p0).dot(n), n)
            }
            (RefGeom::Point(p), RefGeom::Line(q0, q1)) => {
                let n = normal(q0, q1);
                ((q0 - p).dot(n), n)
            }
            (RefGeom::Line(p0, p1), RefGeom::Line(q0, q1)) => {
                let (d, e) = (p1 - p0, q1 - q0);
                if d.perp_dot(e).abs() > 1e-6 * d.length() * e.length() {
                    return None; // не параллельны
                }
                let n = normal(p0, p1);
                ((q0 - p0).dot(n), n)
            }
        })
    }

    /// Размер-расстояние между двумя элементами, если для этой пары он имеет смысл:
    /// точка–точка, точка–отрезок или два параллельных отрезка. Окружность в паре — это её центр.
    /// Для непараллельных отрезков получается размер-угол.
    pub fn pair_dim(&self, a: DimRef, b: DimRef) -> Option<Dim> {
        let center = |r: DimRef| match r {
            DimRef::Circle(s) => DimRef::Point(s, 0),
            r => r,
        };
        let (a, b) = (center(a), center(b));
        if a == b {
            return None;
        }
        let mode = match (self.ref_geom(a)?, self.ref_geom(b)?) {
            (RefGeom::Point(p), RefGeom::Point(q)) => {
                let d = (q - p).abs();
                if d.x >= d.y { DimMode::Horizontal } else { DimMode::Vertical }
            }
            _ => DimMode::Aligned,
        };
        if self.distance_axis(a, b, mode).is_some() {
            Some(Dim::Distance(a, b, mode))
        } else {
            self.line_angle(a, b).map(|_| Dim::Angle(a, b))
        }
    }

    /// Угол поворота от отрезка `a` к отрезку `b` (радианы, со знаком).
    /// Если у отрезков есть общая вершина, оба считаются направленными от неё — получается угол при вершине.
    fn line_angle(&self, a: DimRef, b: DimRef) -> Option<f64> {
        let (RefGeom::Line(p0, p1), RefGeom::Line(q0, q1)) = (self.ref_geom(a)?, self.ref_geom(b)?) else {
            return None;
        };
        let (mut d, mut e) = (p1 - p0, q1 - q0);
        let same = |u: DVec2, v: DVec2| (u - v).length() < 1e-9;
        if same(p1, q0) {
            d = -d;
        } else if same(p0, q1) {
            e = -e;
        } else if same(p1, q1) {
            (d, e) = (-d, -e);
        }
        Some(d.perp_dot(e).atan2(d.dot(e)))
    }

    /// Взаимосвязи, которые можно наложить на пару элементов (как их вернул `pair_dim`).
    pub fn relations(&self, a: DimRef, b: DimRef) -> Vec<Relation> {
        match (self.ref_geom(a), self.ref_geom(b)) {
            _ if a == b => vec![],
            (Some(RefGeom::Line(..)), Some(RefGeom::Line(..))) => {
                vec![Relation::Parallel, Relation::Perpendicular, Relation::Collinear]
            }
            (Some(RefGeom::Point(_)), Some(RefGeom::Line(..))) | (Some(RefGeom::Line(..)), Some(RefGeom::Point(_))) => {
                vec![Relation::Coincident, Relation::Midpoint]
            }
            (Some(RefGeom::Point(_)), Some(RefGeom::Point(_))) => {
                vec![Relation::Coincident, Relation::Horizontal, Relation::Vertical]
            }
            _ => vec![],
        }
    }

    pub fn apply_relation(&mut self, a: DimRef, b: DimRef, relation: Relation) -> Result<(), String> {
        match relation {
            Relation::Coincident => self.set_dim(&Dim::Distance(a, b, DimMode::Aligned), 0.0),
            Relation::Perpendicular => self.set_dim(&Dim::Angle(a, b), 90.0),
            // Одинаковая Y (по вертикали расстояние ноль) и одинаковая X.
            Relation::Horizontal => self.set_dim(&Dim::Distance(a, b, DimMode::Vertical), 0.0),
            Relation::Vertical => self.set_dim(&Dim::Distance(a, b, DimMode::Horizontal), 0.0),
            Relation::Midpoint => match (self.ref_geom(a), self.ref_geom(b)) {
                // Первый элемент остаётся на месте: точка идёт к середине отрезка, а отрезок — к точке.
                (Some(RefGeom::Line(p, q)), Some(RefGeom::Point(pt))) => self.nudge(a, b, (p + q) * 0.5 - pt, None),
                (Some(RefGeom::Point(pt)), Some(RefGeom::Line(p, q))) => self.nudge(a, b, pt - (p + q) * 0.5, None),
                _ => Err("середина — это точка и отрезок".into()),
            },
            Relation::Parallel | Relation::Collinear => {
                // Поворачиваем в ближайшую сторону: сонаправленно или встречно.
                let angle = self.line_angle(a, b).ok_or(DIM_GONE)?;
                let target = if angle.abs() < std::f64::consts::FRAC_PI_2 { 0.0 } else { 180.0 };
                self.set_dim(&Dim::Angle(a, b), target)?;
                if relation == Relation::Collinear {
                    self.set_dim(&Dim::Distance(a, b, DimMode::Aligned), 0.0)?;
                }
                Ok(())
            }
        }
    }

    /// Поворачивает отрезок `moving` на `angle` относительно `fixed`. Отрезок другой фигуры
    /// поворачивает её целиком (вокруг своей середины); внутри одного контура сдвигается один конец отрезка.
    fn rotate_ref(&mut self, moving: DimRef, fixed: DimRef, angle: f64) -> Result<(), String> {
        let DimRef::Segment(s, j) = moving else { return Err(DIM_GONE.into()) };
        let fixed_seg = match fixed {
            DimRef::Segment(fs, i) if fs == s => Some(i),
            _ => None,
        };
        let shape = self.shapes.get_mut(s).ok_or(DIM_GONE)?;
        let (q0, q1, _) = shape.segment(j).ok_or(DIM_GONE)?;
        if matches!(shape, SketchShape::Rect { .. }) {
            if fixed_seg.is_some() {
                return Err("стороны прямоугольника всегда параллельны или перпендикулярны".into());
            }
            // Повёрнутый прямоугольник хранится как обычный контур.
            shape.make_path();
        }
        let SketchShape::Path { points, .. } = shape else { return Err(DIM_GONE.into()) };
        let rot = DVec2::from_angle(angle);
        let turn = |p: &mut PathPoint, pivot: DVec2| {
            let q = pivot + rot.rotate(p.pos() - pivot);
            (p.x, p.y) = (q.x, q.y);
        };
        match fixed_seg {
            None => points.iter_mut().for_each(|p| turn(p, (q0 + q1) * 0.5)),
            Some(i) => {
                let n = points.len();
                let (next, anchors) = ((j + 1) % n, [i, (i + 1) % n]);
                let (pivot, end) = if anchors.contains(&next) { (next, j) } else { (j, next) };
                if anchors.contains(&end) {
                    return Err(DIM_SHARED.into());
                }
                let center = points[pivot].pos();
                turn(&mut points[end], center);
            }
        }
        Ok(())
    }

    /// Можно ли у размера выбирать способ измерения (только расстояние между двумя точками).
    pub fn dim_has_modes(&self, dim: &Dim) -> bool {
        match *dim {
            Dim::Distance(a, b, _) => {
                matches!((self.ref_geom(a), self.ref_geom(b)), (Some(RefGeom::Point(_)), Some(RefGeom::Point(_))))
            }
            _ => false,
        }
    }

    /// Текущее значение размера; `None` — если его элементы исчезли или размер к ним неприменим.
    pub fn dim_value(&self, dim: &Dim) -> Option<f64> {
        match *dim {
            Dim::Size(DimRef::Segment(s, i)) => {
                let (a, b, bulge) = self.shapes.get(s)?.segment(i)?;
                Some(if bulge == 0.0 { (b - a).length() } else { arc_radius(a, b, bulge) })
            }
            Dim::Size(DimRef::Circle(s)) => match self.shapes.get(s)? {
                SketchShape::Circle { r, .. } => Some(r * 2.0),
                _ => None,
            },
            Dim::Size(_) => None,
            Dim::Distance(a, b, mode) => self.distance_axis(a, b, mode).map(|(v, _)| v.abs()),
            Dim::Angle(a, b) => self.line_angle(a, b).map(|v| v.abs().to_degrees()),
        }
    }

    /// Место размерной линии размера `dim`, если пользователь его выбрал.
    pub fn dim_place(&self, dim: &Dim) -> Option<DVec2> {
        self.dim_places.iter().find(|(d, _)| d == dim).map(|(_, p)| *p)
    }

    pub fn set_dim_place(&mut self, dim: Dim, place: DVec2) {
        self.dim_places.retain(|(d, _)| *d != dim);
        self.dim_places.push((dim, place));
    }

    /// Размер изменил способ измерения — его место переходит к новому виду размера.
    pub fn rekey_dim_place(&mut self, old: Dim, new: Dim) {
        if let Some(place) = self.dim_place(&old) {
            self.dim_places.retain(|(d, _)| *d != old);
            self.set_dim_place(new, place);
        }
    }

    /// Забывает места размеров, которых в эскизе уже нет.
    pub fn prune_dim_places(&mut self) {
        let dims = &self.dims;
        self.dim_places.retain(|(d, _)| dims.contains(d));
    }

    /// Отрезок, вдоль которого измеряется расстояние, — для показа на экране.
    pub fn dim_span(&self, dim: &Dim) -> Option<(DVec2, DVec2)> {
        let Dim::Distance(a, b, mode) = *dim else { return None };
        let (signed, axis) = self.distance_axis(a, b, mode)?;
        let to = match self.ref_geom(b)? {
            RefGeom::Point(p) => p,
            RefGeom::Line(p, q) => (p + q) * 0.5,
        };
        Some((to - axis * signed, to))
    }

    /// Задаёт размеру новое значение, меняя геометрию эскиза.
    pub fn set_dim(&mut self, dim: &Dim, value: f64) -> Result<(), String> {
        match *dim {
            Dim::Size(r) => {
                if value < MIN_SIZE {
                    return Err("размер должен быть больше нуля".into());
                }
                let (s, i) = match r {
                    DimRef::Segment(s, i) => (s, i),
                    DimRef::Circle(s) => (s, 0),
                    _ => return Err(DIM_GONE.into()),
                };
                let shape = self.shapes.get_mut(s).ok_or(DIM_GONE)?;
                let segment = shape.segment(i);
                match (shape, r) {
                    (SketchShape::Circle { r, .. }, DimRef::Circle(_)) => *r = value / 2.0,
                    (SketchShape::Rect { w, h, .. }, DimRef::Segment(..)) if i < 4 => {
                        *(if i % 2 == 0 { w } else { h }) = value;
                    }
                    (SketchShape::Path { points, closed }, DimRef::Segment(..)) => {
                        let (a, b, bulge) = segment.ok_or(DIM_GONE)?;
                        let len = (b - a).length();
                        if bulge != 0.0 {
                            points[i].bulge = bulge_for_radius(a, b, bulge, value)?;
                        } else if len < 1e-9 {
                            return Err("у отрезка нулевой длины нет направления".into());
                        } else {
                            let end = (i + 1) % points.len();
                            shift_chain(points, *closed, &[end], &[i], (b - a) / len * (value - len))?;
                        }
                    }
                    _ => return Err(DIM_GONE.into()),
                }
                Ok(())
            }
            Dim::Distance(a, b, mode) => {
                if value < 0.0 {
                    return Err("расстояние не может быть отрицательным".into());
                }
                let (signed, axis) = self.distance_axis(a, b, mode).ok_or(DIM_GONE)?;
                let sign = if signed < 0.0 { -1.0 } else { 1.0 };
                let d = axis * (sign * value - signed);
                if d.length() < 1e-12 {
                    return Ok(());
                }
                // Начало координат и элементы других эскизов неподвижны: двигается тот, что свой.
                self.nudge(a, b, d, Some(dim))
            }
            Dim::Angle(a, b) => {
                if !(0.0..=180.0).contains(&value) {
                    return Err("угол должен быть от 0 до 180°".into());
                }
                let angle = self.line_angle(a, b).ok_or(DIM_GONE)?;
                let sign = if angle < 0.0 { -1.0 } else { 1.0 };
                let turn = sign * value.to_radians() - angle;
                // Отрезок другого эскиза поворачивать нельзя — поворачивается свой, в обратную сторону.
                match (immovable(a), immovable(b)) {
                    (true, true) => Err("оба элемента неподвижны — ничего двигать".into()),
                    (_, true) => self.rotate_ref(a, b, -turn),
                    _ => self.rotate_ref(b, a, turn),
                }
            }
        }
    }

    /// Сдвигает элемент `moving` на `d` относительно `fixed`. Элемент другой фигуры переносит
    /// её целиком; внутри одной фигуры меняется её форма.
    fn move_ref(&mut self, moving: DimRef, fixed: DimRef, d: DVec2) -> Result<(), String> {
        let shape_of = |r: DimRef| match r {
            DimRef::Origin | DimRef::Ext(..) => None,
            DimRef::Point(s, _) | DimRef::Segment(s, _) | DimRef::Circle(s) => Some(s),
        };
        let s = shape_of(moving).ok_or(DIM_GONE)?;
        let shape = self.shapes.get_mut(s).ok_or(DIM_GONE)?;
        if shape_of(fixed) != Some(s) {
            shape.translate(d);
            return Ok(());
        }
        match shape {
            SketchShape::Path { points, closed } => {
                let n = points.len();
                let ends = |r: DimRef| match r {
                    DimRef::Point(_, i) => vec![i],
                    DimRef::Segment(_, i) => vec![i, (i + 1) % n],
                    _ => vec![],
                };
                shift_chain(points, *closed, &ends(moving), &ends(fixed), d)
            }
            SketchShape::Rect { x, y, w, h } => {
                // Стороны прямоугольника: левая, правая, нижняя, верхняя.
                let sides = |r: DimRef| match r {
                    DimRef::Point(_, k) => [k == 0 || k == 3, k == 1 || k == 2, k < 2, k >= 2],
                    DimRef::Segment(_, k) => [k == 3, k == 1, k == 0, k == 2],
                    _ => [false; 4],
                };
                let (moves, fixed) = (sides(moving), sides(fixed));
                let shift = [d.x, d.x, d.y, d.y];
                let mut side = [*x, *x + *w, *y, *y + *h];
                for k in 0..4 {
                    if moves[k] && shift[k].abs() > 1e-12 {
                        if fixed[k] {
                            return Err(DIM_SHARED.into());
                        }
                        side[k] += shift[k];
                    }
                }
                if side[1] - side[0] < MIN_SIZE || side[3] - side[2] < MIN_SIZE {
                    return Err("прямоугольник с таким размером вырождается".into());
                }
                (*x, *y, *w, *h) = (side[0], side[2], side[1] - side[0], side[3] - side[2]);
                Ok(())
            }
            SketchShape::Circle { .. } => Err(DIM_SHARED.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FeatureKind {
    Sketch(Sketch),
    Boss { sketch: u64, depth: f64, flip: bool },
    Cut { sketch: u64, depth: f64, through_all: bool, flip: bool },
    /// Рёбра запоминаются точками на них (серединами) в момент выбора.
    Fillet { edges: Vec<DVec3>, radius: f64 },
    Chamfer { edges: Vec<DVec3>, distance: f64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Feature {
    pub id: u64,
    pub name: String,
    pub kind: FeatureKind,
    #[serde(skip)]
    pub error: Option<String>,
}

/// Результат построения одного элемента, сохранённый до следующего пересчёта.
#[derive(Clone)]
struct Cached {
    id: u64,
    /// Исходные данные элемента (см. `Part::feature_key`): пока они те же и не менялись
    /// предыдущие элементы, результат можно взять готовым.
    key: String,
    result: Result<Option<Rc<Shape>>, String>,
    /// Сетка тела после этого элемента — строится при первом показе.
    mesh: Option<Rc<RenderMesh>>,
}

/// Кэш построения: по записи на каждый элемент дерева, по порядку.
#[derive(Clone, Default)]
struct Cache(Vec<Cached>);

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cache({} элементов)", self.0.len())
    }
}

impl Feature {
    pub fn sketch_ref(&self) -> Option<u64> {
        match self.kind {
            FeatureKind::Boss { sketch, .. } | FeatureKind::Cut { sketch, .. } => Some(sketch),
            _ => None,
        }
    }
}

pub type Rgb = [u8; 3];
pub const DEFAULT_COLOR: Rgb = [179, 189, 204];
const SELECTED_FACE: Rgb = [51, 158, 255];

/// Материал: как поверхность отражает свет.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Material {
    /// Матовый пластик — обычный вид детали.
    #[default]
    Plastic,
    Metal,
    Glass,
    Stone,
}

impl Material {
    pub const ALL: [Material; 4] = [Material::Plastic, Material::Metal, Material::Glass, Material::Stone];

    pub fn name(self) -> &'static str {
        match self {
            Material::Plastic => "Пластик",
            Material::Metal => "Металл",
            Material::Glass => "Стекло",
            Material::Stone => "Камень",
        }
    }

    /// Номер материала для шейдера.
    pub fn id(self) -> f32 {
        match self {
            Material::Plastic => 0.0,
            Material::Metal => 1.0,
            Material::Glass => 2.0,
            Material::Stone => 3.0,
        }
    }
}

/// Цвет и материал грани. Грань запоминается своим центром — так она находится и после пересчёта тела.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FaceColor {
    pub point: DVec3,
    /// Нормаль грани при покраске (нулевая в старых файлах): после правки цвет ищет грань с той же нормалью.
    #[serde(default)]
    pub normal: DVec3,
    /// `None` — цвет детали.
    pub color: Option<Rgb>,
    /// `None` — материал детали.
    #[serde(default)]
    pub material: Option<Material>,
}

/// Группа элементов в дереве построения. Только для удобства показа: на порядок построения не влияет.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: u64,
    pub name: String,
    pub members: Vec<u64>,
    #[serde(default)]
    pub collapsed: bool,
}

/// Строка дерева построения.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TreeItem {
    Feature(u64),
    Group(u64),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Part {
    pub features: Vec<Feature>,
    next_id: u64,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Цвет детали целиком; `None` — стандартный серый.
    #[serde(default)]
    pub color: Option<Rgb>,
    /// Цвета отдельных граней.
    #[serde(default)]
    pub face_colors: Vec<FaceColor>,
    /// Материал детали целиком.
    #[serde(default)]
    pub material: Material,
    #[serde(skip)]
    cache: Cache,
    /// Направления рёбер скруглений и фасок (по номеру элемента): помогают найти те же рёбра,
    /// когда изменение размеров сдвинуло их.
    #[serde(default)]
    edge_dirs: std::collections::BTreeMap<u64, Vec<DVec3>>,
}

impl Part {
    pub fn add(&mut self, base_name: &str, kind: FeatureKind) -> u64 {
        self.next_id += 1;
        let count = self
            .features
            .iter()
            .filter(|f| std::mem::discriminant(&f.kind) == std::mem::discriminant(&kind))
            .count();
        let name = format!("{base_name}{}", count + 1);
        self.features.push(Feature { id: self.next_id, name, kind, error: None });
        self.next_id
    }

    pub fn feature(&self, id: u64) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    pub fn feature_mut(&mut self, id: u64) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }

    /// Точки и отрезки более ранних эскизов в плоскости эскиза `id`: к ним можно привязывать размеры.
    /// Заполняет `Sketch::ext_cache`; вызывается перед показом эскиза при редактировании.
    pub fn refresh_ext(&mut self, id: u64) {
        let Some(index) = self.features.iter().position(|f| f.id == id) else { return };
        let FeatureKind::Sketch(current) = &self.features[index].kind else { return };
        let plane = current.plane;
        let mut cache = std::collections::HashMap::new();
        for f in &self.features[..index] {
            let FeatureKind::Sketch(other) = &f.kind else { continue };
            let local = |p: DVec2| plane.to_local(other.plane.to_world(p));
            for (s, shape) in other.shapes.iter().enumerate() {
                for (i, p) in shape.points().into_iter().enumerate() {
                    cache.insert((f.id, s, i, false), (local(p), DVec2::ZERO));
                }
                let mut i = 0;
                while let Some((a, b, bulge)) = shape.segment(i) {
                    if bulge == 0.0 {
                        cache.insert((f.id, s, i, true), (local(a), local(b)));
                    }
                    i += 1;
                }
            }
        }
        if let Some(sketch) = self.sketch_mut(id) {
            sketch.ext_cache = cache;
        }
    }

    pub fn sketch(&self, id: u64) -> Option<&Sketch> {
        match &self.feature(id)?.kind {
            FeatureKind::Sketch(s) => Some(s),
            _ => None,
        }
    }

    pub fn sketch_mut(&mut self, id: u64) -> Option<&mut Sketch> {
        match &mut self.feature_mut(id)?.kind {
            FeatureKind::Sketch(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_sketch_used(&self, id: u64) -> bool {
        self.features.iter().any(|f| f.sketch_ref() == Some(id))
    }

    /// Удаляет элемент; вместе с эскизом удаляются и построенные на нём операции.
    pub fn remove(&mut self, id: u64) {
        self.features.retain(|f| f.id != id && f.sketch_ref() != Some(id));
        self.prune_groups();
    }

    /// Убирает из групп удалённые элементы, а опустевшие группы — совсем.
    fn prune_groups(&mut self) {
        let features = &self.features;
        for g in &mut self.groups {
            g.members.retain(|id| features.iter().any(|f| f.id == *id));
        }
        self.groups.retain(|g| !g.members.is_empty());
    }

    /// Запись о покраске грани с центром `point` вынимается из списка (если она была).
    fn take_face_entry(&mut self, point: DVec3) -> Option<FaceColor> {
        let at = self.face_colors.iter().position(|c| (c.point - point).length() <= FACE_MATCH)?;
        Some(self.face_colors.remove(at))
    }

    /// Красит грань с центром `point`; `None` возвращает ей цвет детали. Материал грани не меняется.
    pub fn set_face_color(&mut self, point: DVec3, normal: DVec3, color: Option<Rgb>) {
        let material = self.take_face_entry(point).and_then(|c| c.material);
        if color.is_some() || material.is_some() {
            self.face_colors.push(FaceColor { point, normal, color, material });
        }
    }

    /// Задаёт материал грани с центром `point`; `None` возвращает материал детали. Цвет грани не меняется.
    pub fn set_face_material(&mut self, point: DVec3, normal: DVec3, material: Option<Material>) {
        let color = self.take_face_entry(point).and_then(|c| c.color);
        if color.is_some() || material.is_some() {
            self.face_colors.push(FaceColor { point, normal, color, material });
        }
    }

    /// Красит всю деталь: цвета отдельных граней сбрасываются, материалы граней остаются.
    pub fn paint_part_color(&mut self, color: Option<Rgb>) {
        self.color = color;
        for c in &mut self.face_colors {
            c.color = None;
        }
        self.face_colors.retain(|c| c.material.is_some());
    }

    /// Материал всей детали: материалы отдельных граней сбрасываются, цвета остаются.
    pub fn paint_part_material(&mut self, material: Material) {
        self.material = material;
        for c in &mut self.face_colors {
            c.material = None;
        }
        self.face_colors.retain(|c| c.color.is_some());
    }

    /// Запись о покраске каждой грани сетки (номер в `face_colors`). Грань после правки размеров
    /// сдвигается, поэтому запись достаётся ближайшей грани с той же нормалью, а не только совпавшей по точке.
    fn painted_faces(&self, mesh: &RenderMesh) -> Vec<Option<usize>> {
        let mut out = vec![None; mesh.faces.len()];
        let reach = mesh.bounds.map_or(FACE_MATCH, |(lo, hi)| ((hi - lo).length() * 0.5).max(FACE_MATCH));
        for (k, c) in self.face_colors.iter().enumerate() {
            let known = c.normal.length_squared() > 0.5;
            let best = mesh
                .faces
                .iter()
                .enumerate()
                .filter(|(_, f)| !known || f.normal.normalize_or_zero().dot(c.normal.normalize_or_zero()) > 0.95)
                .map(|(i, f)| (i, (c.point - f.origin).length()))
                .filter(|(_, d)| *d <= reach)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, _)) = best {
                out[i] = Some(k);
            }
        }
        out
    }

    /// Цвета вершин сетки: на вершину r, g, b и номер материала (4 числа). Цвет и материал берутся
    /// у детали и раскрашенных граней. Грани из `selected` подкрашиваются цветом выделения.
    pub fn vertex_colors(&self, mesh: &RenderMesh, selected: &[usize]) -> Vec<f32> {
        let body = self.color.unwrap_or(DEFAULT_COLOR);
        let painted = self.painted_faces(mesh);
        let faces: Vec<(Rgb, f32)> = (0..mesh.faces.len())
            .map(|i| {
                let entry = painted[i].map(|k| &self.face_colors[k]);
                let color = entry.and_then(|c| c.color).unwrap_or(body);
                let material = entry.and_then(|c| c.material).unwrap_or(self.material);
                let color = if selected.contains(&i) {
                    [0, 1, 2].map(|k| ((color[k] as u16 + SELECTED_FACE[k] as u16) / 2) as u8)
                } else {
                    color
                };
                (color, material.id())
            })
            .collect();
        let mut out = Vec::with_capacity(mesh.vertices.len() / 7 * 4);
        for v in mesh.vertices.chunks_exact(7) {
            let (color, material) = faces.get(v[6] as usize).copied().unwrap_or((body, self.material.id()));
            out.extend(color.map(|c| c as f32 / 255.0));
            out.push(material);
        }
        out
    }

    /// Собирает элементы в новую группу (из прежних групп они уходят).
    pub fn add_group(&mut self, members: &[u64]) -> u64 {
        self.ungroup(members);
        self.next_id += 1;
        let name = format!("Группа{}", self.groups.len() + 1);
        self.groups.push(Group { id: self.next_id, name, members: members.to_vec(), collapsed: false });
        self.prune_groups();
        self.next_id
    }

    /// Вынимает элементы из их групп.
    pub fn ungroup(&mut self, members: &[u64]) {
        for g in &mut self.groups {
            g.members.retain(|id| !members.contains(id));
        }
        self.prune_groups();
    }

    /// Расформировывает группу; её элементы остаются.
    pub fn remove_group(&mut self, id: u64) {
        self.groups.retain(|g| g.id != id);
    }

    /// Строки дерева по порядку показа: (строка, вложена ли в группу). Группа стоит на месте
    /// своего первого элемента; элементы свёрнутой группы не показываются.
    pub fn tree_rows(&self) -> Vec<(TreeItem, bool)> {
        let mut rows = Vec::new();
        let mut shown = Vec::new();
        for f in &self.features {
            match self.groups.iter().find(|g| g.members.contains(&f.id)) {
                None => rows.push((TreeItem::Feature(f.id), false)),
                Some(g) if !shown.contains(&g.id) => {
                    shown.push(g.id);
                    rows.push((TreeItem::Group(g.id), false));
                    if !g.collapsed {
                        let members = self.features.iter().filter(|m| g.members.contains(&m.id));
                        rows.extend(members.map(|m| (TreeItem::Feature(m.id), true)));
                    }
                }
                Some(_) => {}
            }
        }
        rows
    }

    fn end_index(&self, stop_before: Option<u64>) -> usize {
        stop_before.and_then(|id| self.features.iter().position(|f| f.id == id)).unwrap_or(self.features.len())
    }

    /// Всё, от чего зависит результат элемента, кроме тела до него: его параметры и эскиз.
    /// У самого эскиза ключ пустой — он ничего не строит, а его изменения видны в ключах операций.
    fn feature_key(&self, index: usize) -> String {
        let feature = &self.features[index];
        if matches!(feature.kind, FeatureKind::Sketch(_)) {
            return String::new();
        }
        let sketch = feature.sketch_ref().and_then(|id| {
            self.features[..index].iter().find_map(|f| match &f.kind {
                // Размеры эскиза на тело не влияют.
                FeatureKind::Sketch(s) if f.id == id => serde_json::to_string(&(&s.plane, &s.shapes, s.regions_of(feature.id))).ok(),
                _ => None,
            })
        });
        format!("{}|{}", serde_json::to_string(&feature.kind).unwrap_or_default(), sketch.unwrap_or_default())
    }

    /// Пересчёт дерева сверху вниз. `stop_before` — «откат» перед указанным элементом
    /// (используется при редактировании эскиза). Элементы, у которых не изменились ни они сами,
    /// ни всё построенное до них, заново не строятся — их результат берётся из кэша.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn rebuild(&mut self, stop_before: Option<u64>) -> Option<Rc<Shape>> {
        self.rebuild_step(stop_before, usize::MAX).0
    }

    /// Пересчёт по времени: строит элементы, пока не пройдёт `limit`, и сообщает, на чём остановился.
    /// Одна сетка на пачку элементов выходит заметно дешевле, чем по сетке на каждый элемент.
    pub fn rebuild_timed(
        &mut self,
        stop_before: Option<u64>,
        limit: std::time::Duration,
    ) -> (Option<Rc<Shape>>, Option<usize>) {
        let start = std::time::Instant::now();
        loop {
            let (shape, pending) = self.rebuild_step(stop_before, 1);
            if pending.is_none() || start.elapsed() >= limit {
                return (shape, pending);
            }
        }
    }

    /// Пересчёт по частям: строит не больше `budget` элементов, которых нет в кэше, и возвращает
    /// тело на этот момент и номер элемента, на котором остановился (`None` — всё построено).
    /// Так интерфейс может показывать, как деталь строится, и не замирает на всё время пересчёта.
    pub fn rebuild_step(&mut self, stop_before: Option<u64>, budget: usize) -> (Option<Rc<Shape>>, Option<usize>) {
        let end = self.end_index(stop_before);
        let mut body: Option<Rc<Shape>> = None;
        let mut reuse = true;
        let mut built = 0;
        for i in 0..end {
            let id = self.features[i].id;
            let key = self.feature_key(i);
            reuse = reuse && self.cache.0.get(i).is_some_and(|c| c.id == id && c.key == key);
            if !reuse {
                self.cache.0.truncate(i);
                // Эскиз ничего не строит и делается мгновенно — в бюджет не входит.
                if !matches!(self.features[i].kind, FeatureKind::Sketch(_)) {
                    if built == budget {
                        return (body, Some(i));
                    }
                    built += 1;
                }
                let result = self.apply(i, body.as_deref()).map(|shape| shape.map(Rc::new));
                // Ключ считаем заново: скругление могло переставить свои рёбра.
                let key = self.feature_key(i);
                self.cache.0.push(Cached { id, key, result, mesh: None });
            }
            let error = match &self.cache.0[i].result {
                Ok(Some(new_body)) => {
                    body = Some(new_body.clone());
                    None
                }
                Ok(None) => None,
                Err(e) => Some(e.clone()),
            };
            self.features[i].error = error;
        }
        (body, None)
    }

    /// Сетка тела, построенного последним вызовом `rebuild` с тем же `stop_before`.
    pub fn mesh(&mut self, stop_before: Option<u64>) -> Rc<RenderMesh> {
        let end = self.end_index(stop_before).min(self.cache.0.len());
        let last = self.cache.0[..end].iter_mut().rev().find_map(|c| match &c.result {
            Ok(Some(shape)) => Some((shape.clone(), &mut c.mesh)),
            _ => None,
        });
        match last {
            Some((shape, mesh)) => mesh.get_or_insert_with(|| Rc::new(RenderMesh::from_shape(&shape))).clone(),
            None => Rc::default(),
        }
    }

    fn apply(&mut self, index: usize, body: Option<&Shape>) -> Result<Option<Shape>, String> {
        let kind = self.features[index].kind.clone();
        let find_sketch = |id: u64| {
            self.features[..index]
                .iter()
                .find_map(|f| match &f.kind {
                    FeatureKind::Sketch(s) if f.id == id => Some(s),
                    _ => None,
                })
                .ok_or_else(|| "эскиз не найден".to_string())
        };
        match &kind {
            FeatureKind::Sketch(_) => Ok(None),
            FeatureKind::Boss { sketch, depth, flip } => {
                let sketch = find_sketch(*sketch)?;
                if *depth < MIN_SIZE {
                    return Err("глубина должна быть больше нуля".into());
                }
                let sign = if *flip { -1.0 } else { 1.0 };
                let id = self.features[index].id;
                // Призмы склеиваются между собой (они маленькие), а с деталью — одной операцией:
                // булева операция над всей деталью на каждую призму отдельно очень дорога.
                let tool = fuse_all(sketch.prisms(sketch.plane.normal() * *depth * sign, sketch.regions_of(id))?);
                let result = tool.map(|t| match body {
                    Some(b) => b.union(&t).shape,
                    None => t,
                });
                Ok(result.map(|s| s.clean()))
            }
            FeatureKind::Cut { sketch, depth, through_all, flip } => {
                let sketch = find_sketch(*sketch)?;
                let Some(body) = body else {
                    return Err("нет тела, из которого можно вырезать".into());
                };
                let depth = if *through_all { THROUGH_ALL } else { *depth };
                if depth < MIN_SIZE {
                    return Err("глубина должна быть больше нуля".into());
                }
                // Вырез по умолчанию идёт вглубь материала — против нормали эскиза.
                let sign = if *flip { 1.0 } else { -1.0 };
                let id = self.features[index].id;
                let tool = fuse_all(sketch.prisms(sketch.plane.normal() * depth * sign, sketch.regions_of(id))?);
                Ok(tool.map(|t| body.subtract(&t).shape.clean()))
            }
            FeatureKind::Fillet { edges, radius: size } | FeatureKind::Chamfer { edges, distance: size } => {
                let chamfer = matches!(kind, FeatureKind::Chamfer { .. });
                let (what, size_name) = if chamfer { ("фаску", "размер фаски") } else { ("скругление", "радиус") };
                let body = body.ok_or_else(|| format!("нет тела, на котором можно построить {what}"))?;
                if *size < MIN_SIZE {
                    return Err(format!("{size_name} должен быть больше нуля"));
                }
                let id = self.features[index].id;
                let known = self.edge_dirs.get(&id).filter(|d| d.len() == edges.len()).cloned().unwrap_or_default();
                let (mut found, mut mids, mut dirs) = (Vec::new(), Vec::new(), Vec::new());
                for (edge, mid, dir) in find_edges(body, edges, &known)? {
                    found.push(edge);
                    mids.push(mid);
                    dirs.push(dir);
                }
                let build = |order: &[&Edge]| {
                    if chamfer {
                        body.try_chamfer_edges(*size, order.iter().copied())
                    } else {
                        body.try_fillet_edges(*size, order.iter().copied())
                    }
                };
                match blend_edges(body, &found, build) {
                    Ok((shape, order)) => {
                        // Запоминаем удачный порядок рёбер — в следующий раз он сработает с первой попытки.
                        if let FeatureKind::Fillet { edges: stored, .. } | FeatureKind::Chamfer { edges: stored, .. } =
                            &mut self.features[index].kind
                        {
                            // Заодно запоминаем, где рёбра теперь: так они переживают изменение размеров.
                            *stored = order.iter().map(|&i| mids[i]).collect();
                        }
                        self.edge_dirs.insert(id, order.iter().map(|&i| dirs[i]).collect());
                        Ok(Some(shape))
                    }
                    Err(e) => Err(format!("не удалось построить {what} — {e}")),
                }
            }
        }
    }
}

/// Число рёбер сетки с нечётным числом треугольников (признак разорванной поверхности).
/// Как `RenderMesh::torn_edges`, но по одной грубой сетке всего тела, без остальной подготовки к показу.
fn torn_edges_of(shape: &Shape) -> usize {
    let Ok(mesh) = shape.mesh_with_tolerance(TORN_CHECK_TOLERANCE) else { return 0 };
    let key = |p: DVec3| [(p.x * 1e3).round() as i64, (p.y * 1e3).round() as i64, (p.z * 1e3).round() as i64];
    let mut count = std::collections::HashMap::new();
    for t in mesh.indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (key(mesh.vertices[t[k]]), key(mesh.vertices[t[(k + 1) % 3]]));
            if a != b {
                *count.entry((a.min(b), a.max(b))).or_insert(0usize) += 1;
            }
        }
    }
    count.values().filter(|&&c| c % 2 == 1).count()
}

/// Скругление или фаска с проверкой результата. На сложных цепочках рёбер ядро иногда строит
/// дефектные грани (поверхность тела выглядит разорванной), причём исход зависит от порядка,
/// в котором переданы рёбра. Поэтому пробуем несколько порядков и берём первый результат
/// с замкнутой поверхностью. Возвращает тело и сработавший порядок рёбер.
fn blend_edges(
    body: &Shape,
    edges: &[Edge],
    build: impl Fn(&[&Edge]) -> Option<Shape>,
) -> Result<(Shape, Vec<usize>), String> {
    let n = edges.len();
    let attempts = if n == 1 { 1 } else { (2 * n).min(12) };
    // Разрывы, которые были в теле и до операции, ей в вину не ставим.
    let mut allowed: Option<usize> = None;
    let mut defective = false;
    for attempt in 0..attempts {
        let mut order: Vec<usize> = (0..n).collect();
        order.rotate_left(attempt / 2);
        if attempt % 2 == 1 {
            order.reverse();
        }
        let refs: Vec<&Edge> = order.iter().map(|&i| &edges[i]).collect();
        if let Some(shape) = build(&refs) {
            // Вывернутую грань выдаёт отрицательная площадь. Эта проверка мгновенная, а сетка
            // для такой грани может строиться минуту — до сетки дело доводим только без неё.
            if shape.faces().all(|f| f.surface_area() > 0.0) {
                let torn = torn_edges_of(&shape);
                if torn == 0 || torn <= *allowed.get_or_insert_with(|| torn_edges_of(body)) {
                    return Ok((shape, order));
                }
            }
            defective = true;
        }
    }
    Err(if defective {
        "ядро строит поверхность с разрывами; уменьшите размер или обработайте это ребро вместе с соседними".into()
    } else {
        "попробуйте меньший размер".into()
    })
}

/// Точка на середине ломаной (по длине).
fn polyline_mid(pts: &[DVec3]) -> DVec3 {
    let total: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let mut left = total * 0.5;
    for w in pts.windows(2) {
        let len = (w[1] - w[0]).length();
        if left <= len && len > 0.0 {
            return w[0] + (w[1] - w[0]) * (left / len);
        }
        left -= len;
    }
    pts.last().copied().unwrap_or(DVec3::ZERO)
}

fn polyline_distance(pts: &[DVec3], p: DVec3) -> f64 {
    pts.windows(2)
        .map(|w| {
            let d = w[1] - w[0];
            let len2 = d.length_squared();
            let t = if len2 < 1e-18 { 0.0 } else { ((p - w[0]).dot(d) / len2).clamp(0.0, 1.0) };
            (w[0] + d * t - p).length()
        })
        .fold(f64::INFINITY, f64::min)
}

/// Направление ребра (от начала к концу); у замкнутых и кривых рёбер без определённого направления — ноль.
fn polyline_dir(pts: &[DVec3]) -> DVec3 {
    match (pts.first(), pts.last()) {
        (Some(a), Some(b)) => (*b - *a).try_normalize().unwrap_or(DVec3::ZERO),
        _ => DVec3::ZERO,
    }
}

/// Находит рёбра тела по запомненным серединам. Сначала точное совпадение (точка лежит на ребре);
/// если геометрия сдвинулась (изменили размер), ребро ищется среди рёбер того же направления
/// (`dirs`, если они известны) по ближайшей середине, но только когда ответ однозначен.
/// Возвращает рёбра, их нынешние середины и направления — их запоминают, чтобы следующий
/// пересчёт нашёл рёбра точно.
fn find_edges(body: &Shape, refs: &[DVec3], dirs: &[DVec3]) -> Result<Vec<(Edge, DVec3, DVec3)>, String> {
    if refs.is_empty() {
        return Err("не выбрано ни одного ребра".into());
    }
    let mut edges: Vec<Option<(Edge, Vec<DVec3>)>> =
        body.edges().map(|e| { let pts = e.approximation_segments().collect(); Some((e, pts)) }).collect();
    let mut found: Vec<Option<(Edge, DVec3, DVec3)>> = refs.iter().map(|_| None).collect();
    let take = |edges: &mut Vec<Option<(Edge, Vec<DVec3>)>>, i: usize| {
        let (edge, pts) = edges[i].take().unwrap();
        (edge, polyline_mid(&pts), polyline_dir(&pts))
    };
    // Точное совпадение.
    for (k, r) in refs.iter().enumerate() {
        let best = edges
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|(_, pts)| (i, polyline_distance(pts, *r))))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, d)| *d < 0.01);
        if let Some((i, _)) = best {
            found[k] = Some(take(&mut edges, i));
        }
    }
    // Остальные — по ближайшей середине среди рёбер того же направления.
    for (k, r) in refs.iter().enumerate() {
        if found[k].is_some() {
            continue;
        }
        let want = dirs.get(k).copied().filter(|d| d.length() > 0.5);
        let mut dists: Vec<(usize, f64)> = edges
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|(_, pts)| (i, pts)))
            .filter(|(_, pts)| {
                let dir = polyline_dir(pts);
                want.is_none_or(|w| dir.length() < 0.5 || dir.dot(w).abs() > 0.98)
            })
            .map(|(i, pts)| (i, (polyline_mid(pts) - *r).length()))
            .collect();
        dists.sort_by(|a, b| a.1.total_cmp(&b.1));
        // Без известного направления ответ принимаем только при явном преимуществе.
        let ratio = if want.is_some() { 0.9 } else { 0.6 };
        // Одно ребро в теле встречается дважды (по разу у каждой из соседних граней) — дубликаты не в счёт.
        let Some(&(i, best)) = dists.first() else {
            return Err("ребро не найдено — геометрия изменилась, выберите рёбра заново".into());
        };
        let second = dists.iter().find(|d| d.1 > best + 1e-6).map(|d| d.1);
        if second.is_some_and(|second| best >= second * ratio) {
            return Err("ребро не найдено — геометрия изменилась, выберите рёбра заново".into());
        }
        found[k] = Some(take(&mut edges, i));
    }
    Ok(found.into_iter().flatten().collect())
}

pub struct EdgeInfo {
    pub points: Vec<DVec3>,
    /// Точка на середине ребра — по ней ребро запоминается в операциях.
    pub mid: DVec3,
}

#[derive(Clone)]
pub struct FaceInfo {
    pub planar: bool,
    pub origin: DVec3,
    pub normal: DVec3,
    /// Середины рёбер, ограничивающих грань (швы самой грани не в счёт), — для скругления по граням.
    pub edges: Vec<DVec3>,
}

/// Треугольная сетка тела для отображения и выбора граней.
#[derive(Default)]
pub struct RenderMesh {
    /// На вершину: позиция (3), нормаль (3), номер грани (1).
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
    /// Отрезки рёбер: по две точки (x, y, z) на отрезок.
    pub edge_lines: Vec<f32>,
    pub faces: Vec<FaceInfo>,
    pub edges: Vec<EdgeInfo>,
    pub bounds: Option<(DVec3, DVec3)>,
}

impl RenderMesh {
    pub fn from_shape(shape: &Shape) -> RenderMesh {
        let mut out = RenderMesh::default();
        // Сначала сетка всего тела — общие рёбра граней получают одинаковое разбиение.
        let _ = shape.mesh_with_tolerance(MESH_TOLERANCE);

        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);

        for face in shape.faces() {
            let Ok(mesh) = Shape::from(&face).mesh_with_tolerance(MESH_TOLERANCE) else {
                continue;
            };
            if mesh.indices.is_empty() {
                continue;
            }
            let face_id = out.faces.len();
            let base = (out.vertices.len() / 7) as u32;
            let tri_normal = |t: &[usize]| {
                let (a, b, c) = (mesh.vertices[t[0]], mesh.vertices[t[1]], mesh.vertices[t[2]]);
                (b - a).cross(c - a)
            };

            let mut normals = mesh.normals.clone();
            if normals.len() != mesh.vertices.len() {
                // Нормали от ядра не получены — считаем по треугольникам.
                normals = vec![DVec3::ZERO; mesh.vertices.len()];
                for t in mesh.indices.chunks_exact(3) {
                    let n = tri_normal(t);
                    for &i in t {
                        normals[i] += n;
                    }
                }
            } else if let Some(t) =
                mesh.indices.chunks_exact(3).find(|t| tri_normal(t).length_squared() > 1e-18)
            {
                // Нормали ядра не учитывают ориентацию грани — сверяем с обходом треугольника.
                if normals[t[0]].dot(tri_normal(t)) < 0.0 {
                    normals.iter_mut().for_each(|n| *n = -*n);
                }
            }
            let normals: Vec<DVec3> = normals.iter().map(|n| n.normalize_or_zero()).collect();

            let first = normals[0];
            let planar = normals.iter().all(|n| n.dot(first) > 1.0 - 1e-6);
            let mut edges: Vec<DVec3> = Vec::new();
            let mut seams = Vec::new();
            for edge in Shape::from(&face).edges() {
                let pts: Vec<DVec3> = edge.approximation_segments().collect();
                let length: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                if length < 1e-6 {
                    continue;
                }
                let mid = polyline_mid(&pts);
                // Шов (например, у цилиндра) встречается в контуре грани дважды.
                match edges.iter().position(|e| (*e - mid).length() < 1e-6) {
                    Some(_) => seams.push(mid),
                    None => edges.push(mid),
                }
            }
            edges.retain(|e| !seams.iter().any(|s| (*s - *e).length() < 1e-6));
            out.faces.push(FaceInfo { planar, origin: face.center_of_mass(), normal: first, edges });

            for (p, n) in mesh.vertices.iter().zip(&normals) {
                min = min.min(*p);
                max = max.max(*p);
                out.vertices.extend([p.x as f32, p.y as f32, p.z as f32]);
                out.vertices.extend([n.x as f32, n.y as f32, n.z as f32]);
                out.vertices.push(face_id as f32);
            }
            out.indices.extend(mesh.indices.iter().map(|&i| base + i as u32));
        }

        // Обход тела выдаёт каждое ребро по разу на каждую прилегающую грань — убираем повторы.
        let mut seen = std::collections::HashSet::new();
        for edge in shape.edges() {
            let pts: Vec<DVec3> = edge.approximation_segments().collect();
            if pts.len() < 2 {
                continue;
            }
            let mid = polyline_mid(&pts);
            let key = |p: DVec3| [(p.x * 1e3).round() as i64, (p.y * 1e3).round() as i64, (p.z * 1e3).round() as i64];
            let (a, b) = (key(pts[0]), key(pts[pts.len() - 1]));
            if !seen.insert((key(mid), a.min(b), a.max(b))) {
                continue;
            }
            out.edges.push(EdgeInfo { points: pts.clone(), mid });
            for w in pts.windows(2) {
                for p in w {
                    out.edge_lines.extend([p.x as f32, p.y as f32, p.z as f32]);
                }
            }
        }

        if min.x.is_finite() {
            out.bounds = Some((min, max));
        }
        out
    }

    /// Ребро, проходящее через точку (так рёбра запоминаются в скруглениях и фасках).
    pub fn edge_at(&self, point: DVec3) -> Option<usize> {
        self.edges.iter().position(|e| polyline_distance(&e.points, point) < 0.01)
    }

    /// Число рёбер сетки, к которым примыкает нечётное число треугольников, — признак разрыва
    /// поверхности. (Чётное число больше двух даёт безвредный шов грани, касающейся самой себя.)
    #[cfg(test)]
    pub fn torn_edges(&self) -> usize {
        let key = |v: u32| {
            let p = self.position(v);
            [(p.x * 1e3).round() as i64, (p.y * 1e3).round() as i64, (p.z * 1e3).round() as i64]
        };
        let mut count = std::collections::HashMap::new();
        for t in self.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
                if a != b {
                    *count.entry((a.min(b), a.max(b))).or_insert(0usize) += 1;
                }
            }
        }
        count.values().filter(|&&c| c % 2 == 1).count()
    }

    fn position(&self, vertex: u32) -> DVec3 {
        let i = vertex as usize * 7;
        DVec3::new(self.vertices[i] as f64, self.vertices[i + 1] as f64, self.vertices[i + 2] as f64)
    }

    /// Ближайшая грань, которую пересекает луч.
    #[cfg(test)]
    pub fn pick(&self, origin: DVec3, dir: DVec3) -> Option<usize> {
        self.pick_hit(origin, dir).map(|(face, _)| face)
    }

    /// Ближайшая грань под лучом и расстояние до неё вдоль луча.
    pub fn pick_hit(&self, origin: DVec3, dir: DVec3) -> Option<(usize, f64)> {
        let mut best: Option<(f64, usize)> = None;
        for tri in self.indices.chunks_exact(3) {
            let (a, b, c) = (self.position(tri[0]), self.position(tri[1]), self.position(tri[2]));
            let (e1, e2) = (b - a, c - a);
            let p = dir.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let s = origin - a;
            let u = s.dot(p) / det;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = dir.dot(q) / det;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let t = e2.dot(q) / det;
            // Только впереди начала луча: позади лежащее не «закрывает» точку.
            if t <= 0.0 {
                continue;
            }
            if best.map_or(true, |(bt, _)| t < bt) {
                let face = self.vertices[tri[0] as usize * 7 + 6] as usize;
                best = Some((t, face));
            }
        }
        best.map(|(t, face)| (face, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ядро не рассчитано на одновременную работу из нескольких потоков — тесты с ним идут по очереди.
    fn kernel() -> std::sync::MutexGuard<'static, ()> {
        static KERNEL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        KERNEL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> SketchShape {
        SketchShape::Rect { x, y, w, h }
    }

    #[test]
    fn boss_then_cut() {
        let _kernel = kernel();
        let mut part = Part::default();
        let base = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch::new(Plane::TOP, vec![rect(0.0, 0.0, 40.0, 30.0)])),
        );
        part.add("Бобышка", FeatureKind::Boss { sketch: base, depth: 10.0, flip: false });

        let body = part.rebuild(None).expect("тело после бобышки");
        let mesh = RenderMesh::from_shape(&body);
        assert_eq!(mesh.faces.len(), 6);
        assert!(mesh.faces.iter().all(|f| f.planar));
        let (min, max) = mesh.bounds.unwrap();
        assert!((max - min - DVec3::new(40.0, 30.0, 10.0)).length() < 1e-3);

        // Луч сверху попадает в верхнюю грань с нормалью +Z.
        let top = mesh.pick(DVec3::new(20.0, 15.0, 100.0), -DVec3::Z).expect("грань под лучом");
        assert!(mesh.faces[top].normal.dot(DVec3::Z) > 0.999);
        assert!((mesh.faces[top].origin.z - 10.0).abs() < 1e-6);

        // Эскиз на верхней грани и сквозное отверстие.
        let plane = Plane::from_normal(mesh.faces[top].origin, mesh.faces[top].normal);
        let hole = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch::new(plane, vec![SketchShape::Circle { cx: 0.0, cy: 0.0, r: 5.0 }])),
        );
        part.add("Вырез", FeatureKind::Cut { sketch: hole, depth: 5.0, through_all: true, flip: false });

        let body = part.rebuild(None).expect("тело после выреза");
        assert!(part.features.iter().all(|f| f.error.is_none()), "{:?}", part.features);
        let mesh = RenderMesh::from_shape(&body);
        assert_eq!(mesh.faces.len(), 7, "6 граней бруска + цилиндр отверстия");
        assert!(mesh.pick(DVec3::new(20.0, 15.0, 100.0), -DVec3::Z).is_none(), "в центре — дыра");
        assert!(mesh.pick(DVec3::new(3.0, 3.0, 100.0), -DVec3::Z).is_some());
        assert!(!mesh.edge_lines.is_empty());
    }

    #[test]
    fn nested_contour_is_a_hole() {
        let _kernel = kernel();
        let mut part = Part::default();
        let shapes = vec![rect(0.0, 0.0, 20.0, 20.0), SketchShape::Circle { cx: 10.0, cy: 10.0, r: 4.0 }];
        let s = part.add("Эскиз", FeatureKind::Sketch(Sketch::new(Plane::FRONT, shapes)));
        part.add("Бобышка", FeatureKind::Boss { sketch: s, depth: 5.0, flip: false });
        let mesh = RenderMesh::from_shape(&part.rebuild(None).unwrap());
        assert_eq!(mesh.faces.len(), 7);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let _kernel = kernel();
        let mut part = Part::default();
        let s = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch::new(Plane::TOP, vec![rect(0.0, 0.0, 5.0, 5.0)])),
        );
        part.add("Вырез", FeatureKind::Cut { sketch: s, depth: 5.0, through_all: false, flip: false });
        assert!(part.rebuild(None).is_none());
        assert!(part.features[1].error.is_some());
    }

    fn path(points: &[(f64, f64, f64)], closed: bool) -> SketchShape {
        SketchShape::Path {
            points: points.iter().map(|&(x, y, bulge)| PathPoint { x, y, bulge, construction: false }).collect(),
            closed,
        }
    }

    fn extruded(shape: SketchShape, depth: f64) -> (Part, Option<Rc<Shape>>) {
        let mut part = Part::default();
        let s = part.add("Эскиз", FeatureKind::Sketch(Sketch::new(Plane::TOP, vec![shape])));
        part.add("Бобышка", FeatureKind::Boss { sketch: s, depth, flip: false });
        let body = part.rebuild(None);
        (part, body)
    }

    #[test]
    fn l_shaped_contour() {
        let _kernel = kernel();
        let l = path(&[(0.0, 0.0, 0.0), (30.0, 0.0, 0.0), (30.0, 10.0, 0.0), (10.0, 10.0, 0.0), (10.0, 25.0, 0.0), (0.0, 25.0, 0.0)], true);
        let (part, body) = extruded(l, 5.0);
        assert!(part.features[1].error.is_none(), "{:?}", part.features[1].error);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        assert_eq!(mesh.faces.len(), 8, "6 боковых граней + 2 торца");
        assert_eq!(mesh.edges.len(), 18, "рёбра без повторов");
        // В вырезе «Г» материала нет, в полке — есть.
        assert!(mesh.pick(DVec3::new(20.0, 20.0, 50.0), -DVec3::Z).is_none());
        assert!(mesh.pick(DVec3::new(5.0, 20.0, 50.0), -DVec3::Z).is_some());
    }

    #[test]
    fn arc_geometry() {
        // Полуокружность из (0,0) в (20,0) через (10,-10): против часовой стрелки, выпуклость вправо от хода.
        let (a, b) = (DVec2::new(0.0, 0.0), DVec2::new(20.0, 0.0));
        let bulge = bulge_through(a, b, DVec2::new(10.0, -10.0));
        assert!((bulge - 1.0).abs() < 1e-9, "{bulge}");
        assert!((arc_mid(a, b, bulge) - DVec2::new(10.0, -10.0)).length() < 1e-9);
        assert!((arc_radius(a, b, bulge) - 10.0).abs() < 1e-9);
        let mut pts = vec![a];
        segment_points(a, b, bulge, &mut pts);
        assert!(pts.iter().all(|p| ((*p - DVec2::new(10.0, 0.0)).length() - 10.0).abs() < 1e-9));
        assert!((bulge_through(a, b, DVec2::new(10.0, 10.0)) + 1.0).abs() < 1e-9);
    }

    #[test]
    fn contour_with_arc() {
        let _kernel = kernel();
        // Прямоугольник 20×10 с полукруглым торцом справа.
        let slot = path(&[(0.0, 0.0, 0.0), (20.0, 0.0, 1.0), (20.0, 10.0, 0.0), (0.0, 10.0, 0.0)], true);
        let (part, body) = extruded(slot, 4.0);
        assert!(part.features[1].error.is_none(), "{:?}", part.features[1].error);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        assert_eq!(mesh.faces.len(), 6);
        assert_eq!(mesh.faces.iter().filter(|f| !f.planar).count(), 1, "одна цилиндрическая грань");
        let (min, max) = mesh.bounds.unwrap();
        assert!((max.x - 25.0).abs() < 0.05 && min.x.abs() < 1e-6, "{min:?} {max:?}");
    }

    fn l_shape() -> SketchShape {
        path(&[(0.0, 0.0, 0.0), (30.0, 0.0, 0.0), (30.0, 10.0, 0.0), (10.0, 10.0, 0.0), (10.0, 25.0, 0.0), (0.0, 25.0, 0.0)], true)
    }

    fn sketch(shapes: Vec<SketchShape>) -> Sketch {
        Sketch::new(Plane::TOP, shapes)
    }

    fn close(a: DVec2, x: f64, y: f64) -> bool {
        (a - DVec2::new(x, y)).length() < 1e-9
    }

    #[test]
    fn segment_length_keeps_neighbours_square() {
        // Нижняя сторона «Г» 30 → 40: правая сторона остаётся вертикальной, полка удлиняется.
        let mut s = sketch(vec![l_shape()]);
        let dim = Dim::Size(DimRef::Segment(0, 0));
        assert_eq!(s.dim_value(&dim), Some(30.0));
        s.set_dim(&dim, 40.0).unwrap();
        let p = s.shapes[0].points();
        assert!(close(p[0], 0.0, 0.0) && close(p[1], 40.0, 0.0) && close(p[2], 40.0, 10.0), "{p:?}");
        assert!(close(p[3], 10.0, 10.0) && close(p[5], 0.0, 25.0), "{p:?}");

        // В треугольнике поглотить сдвиг нечему — двигается только конец отрезка.
        let mut s = sketch(vec![path(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (5.0, 8.0, 0.0)], true)]);
        s.set_dim(&Dim::Size(DimRef::Segment(0, 0)), 20.0).unwrap();
        let p = s.shapes[0].points();
        assert!(close(p[1], 20.0, 0.0) && close(p[2], 5.0, 8.0), "{p:?}");
        assert!(s.set_dim(&Dim::Size(DimRef::Segment(0, 0)), 0.0).is_err());
    }

    #[test]
    fn size_dimensions() {
        let slot = path(&[(0.0, 0.0, 0.0), (20.0, 0.0, 1.0), (20.0, 10.0, 0.0), (0.0, 10.0, 0.0)], true);
        let mut s = sketch(vec![slot, rect(0.0, 0.0, 4.0, 3.0), SketchShape::Circle { cx: 0.0, cy: 0.0, r: 2.0 }]);
        // Радиус дуги: концы на месте, меняется выпуклость.
        let arc = Dim::Size(DimRef::Segment(0, 1));
        assert!((s.dim_value(&arc).unwrap() - 5.0).abs() < 1e-9);
        s.set_dim(&arc, 13.0).unwrap();
        assert!((s.dim_value(&arc).unwrap() - 13.0).abs() < 1e-9);
        let p = s.shapes[0].points();
        assert!(close(p[1], 20.0, 0.0) && close(p[2], 20.0, 10.0));
        assert!(s.set_dim(&arc, 4.0).unwrap_err().contains("хорды"));

        s.set_dim(&Dim::Size(DimRef::Segment(1, 2)), 8.0).unwrap();
        s.set_dim(&Dim::Size(DimRef::Segment(1, 1)), 6.0).unwrap();
        assert_eq!(s.shapes[1], rect(0.0, 0.0, 8.0, 6.0));
        s.set_dim(&Dim::Size(DimRef::Circle(2)), 10.0).unwrap();
        assert_eq!(s.shapes[2], SketchShape::Circle { cx: 0.0, cy: 0.0, r: 5.0 });
    }

    #[test]
    fn distance_dimensions() {
        let mut s = sketch(vec![rect(0.0, 0.0, 40.0, 30.0), SketchShape::Circle { cx: 7.0, cy: 22.0, r: 3.0 }]);
        // Центр отверстия от начала координат: по X и по Y; окружность переносится целиком.
        let dim = s.pair_dim(DimRef::Origin, DimRef::Circle(1)).unwrap();
        assert_eq!(dim, Dim::Distance(DimRef::Origin, DimRef::Point(1, 0), DimMode::Vertical));
        assert!(s.dim_has_modes(&dim));
        s.set_dim(&dim, 15.0).unwrap();
        s.set_dim(&Dim::Distance(DimRef::Origin, DimRef::Point(1, 0), DimMode::Horizontal), 10.0).unwrap();
        assert_eq!(s.shapes[1], SketchShape::Circle { cx: 10.0, cy: 15.0, r: 3.0 });
        // Начало координат вторым элементом — сдвигается всё равно фигура.
        s.set_dim(&Dim::Distance(DimRef::Point(1, 0), DimRef::Origin, DimMode::Aligned), 5.0).unwrap();
        assert!((s.shapes[1].points()[0].length() - 5.0).abs() < 1e-9);

        // От стороны прямоугольника до центра окружности.
        let dim = s.pair_dim(DimRef::Segment(0, 1), DimRef::Circle(1)).unwrap();
        s.set_dim(&dim, 12.0).unwrap();
        assert!((s.shapes[1].points()[0].x - 28.0).abs() < 1e-9);

        // Противоположные стороны одного прямоугольника: двигается вторая указанная.
        let dim = s.pair_dim(DimRef::Segment(0, 1), DimRef::Segment(0, 3)).unwrap();
        assert_eq!(s.dim_value(&dim), Some(40.0));
        s.set_dim(&dim, 25.0).unwrap();
        assert_eq!(s.shapes[0], rect(15.0, 0.0, 25.0, 30.0));
        // Сторона и её же угол: расстояние задать нельзя.
        let dim = s.pair_dim(DimRef::Segment(0, 0), DimRef::Point(0, 0)).unwrap();
        assert!(s.set_dim(&dim, 5.0).is_err());

        // Параллельные отрезки одного контура: вторая сторона сдвигается, соседние меняют длину.
        let mut s = sketch(vec![l_shape()]);
        let dim = s.pair_dim(DimRef::Segment(0, 0), DimRef::Segment(0, 2)).unwrap();
        assert_eq!(s.dim_value(&dim), Some(10.0));
        s.set_dim(&dim, 14.0).unwrap();
        let p = s.shapes[0].points();
        assert!(close(p[1], 30.0, 0.0) && close(p[2], 30.0, 14.0) && close(p[3], 10.0, 14.0) && close(p[4], 10.0, 25.0), "{p:?}");
    }

    #[test]
    fn angles_and_relations() {
        // Угол при вершине треугольника.
        let mut s = sketch(vec![path(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (10.0, 10.0, 0.0)], true)]);
        let dim = s.pair_dim(DimRef::Segment(0, 0), DimRef::Segment(0, 2)).unwrap();
        assert_eq!(dim, Dim::Angle(DimRef::Segment(0, 0), DimRef::Segment(0, 2)));
        assert!((s.dim_value(&dim).unwrap() - 45.0).abs() < 1e-9);
        s.set_dim(&dim, 90.0).unwrap();
        let p = s.shapes[0].points();
        assert!(close(p[0], 0.0, 0.0) && close(p[1], 10.0, 0.0), "{p:?}");
        assert!((p[2] - DVec2::new(0.0, 200f64.sqrt())).length() < 1e-9, "длина стороны сохраняется: {p:?}");

        // Отрезок другой фигуры поворачивает её целиком; прямоугольник при этом становится контуром.
        let mut s = sketch(vec![path(&[(0.0, 0.0, 0.0), (10.0, 10.0, 0.0), (0.0, 10.0, 0.0)], true), rect(20.0, 0.0, 10.0, 4.0)]);
        let (a, b) = (DimRef::Segment(0, 0), DimRef::Segment(1, 0));
        assert_eq!(s.relations(a, b), vec![Relation::Parallel, Relation::Perpendicular, Relation::Collinear]);
        s.apply_relation(a, b, Relation::Parallel).unwrap();
        assert!(s.dim_value(&Dim::Angle(a, b)).unwrap().abs() < 1e-9);
        assert!(matches!(&s.shapes[1], SketchShape::Path { points, closed: true } if points.len() == 4));
        assert!((s.dim_value(&Dim::Size(DimRef::Segment(1, 0))).unwrap() - 10.0).abs() < 1e-9);
        s.apply_relation(a, b, Relation::Perpendicular).unwrap();
        assert!((s.dim_value(&Dim::Angle(a, b)).unwrap() - 90.0).abs() < 1e-9);
        s.apply_relation(a, b, Relation::Collinear).unwrap();
        let q = s.shapes[1].points();
        assert!((q[0].x - q[0].y).abs() < 1e-9 && (q[1].x - q[1].y).abs() < 1e-9, "на прямой y = x: {q:?}");

        // Совпадение: центр окружности — в вершину контура, точка — на отрезок.
        let mut s = sketch(vec![l_shape(), SketchShape::Circle { cx: 50.0, cy: 40.0, r: 2.0 }]);
        let Dim::Distance(a, b, _) = s.pair_dim(DimRef::Point(0, 2), DimRef::Circle(1)).unwrap() else { panic!() };
        assert_eq!(s.relations(a, b), vec![Relation::Coincident, Relation::Horizontal, Relation::Vertical]);
        s.apply_relation(a, b, Relation::Coincident).unwrap();
        assert!(close(s.shapes[1].points()[0], 30.0, 10.0));
        s.apply_relation(DimRef::Segment(0, 5), DimRef::Point(1, 0), Relation::Coincident).unwrap();
        assert!(close(s.shapes[1].points()[0], 0.0, 10.0));
        // Стороны прямоугольника между собой не поворачиваются.
        let mut s = sketch(vec![rect(0.0, 0.0, 5.0, 5.0)]);
        assert!(s.apply_relation(DimRef::Segment(0, 0), DimRef::Segment(0, 1), Relation::Parallel).is_err());
    }

    #[test]
    fn rebuild_reuses_unchanged_features() {
        let _kernel = kernel();
        let (mut part, first) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let first = first.unwrap();
        let top = Plane::from_normal(DVec3::new(20.0, 15.0, 10.0), DVec3::Z);
        let hole = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch::new(top, vec![SketchShape::Circle { cx: 0.0, cy: 0.0, r: 5.0 }])),
        );
        let cut = part.add("Вырез", FeatureKind::Cut { sketch: hole, depth: 5.0, through_all: true, flip: false });
        let full = part.rebuild(None).unwrap();
        let mesh = part.mesh(None);
        assert_eq!(mesh.faces.len(), 7);
        // Повторный пересчёт и «откат» перед эскизом берут готовые тела и сетки.
        assert!(Rc::ptr_eq(&full, &part.rebuild(None).unwrap()));
        assert!(Rc::ptr_eq(&mesh, &part.mesh(None)));
        assert!(Rc::ptr_eq(&first, &part.rebuild(Some(hole)).unwrap()));
        assert_eq!(part.mesh(Some(hole)).faces.len(), 6);
        // Размер в эскизе тело не меняет; новая геометрия эскиза пересчитывает только вырез.
        part.sketch_mut(hole).unwrap().store_dim(Dim::Size(DimRef::Circle(0)));
        assert!(Rc::ptr_eq(&full, &part.rebuild(None).unwrap()));
        part.sketch_mut(hole).unwrap().shapes[0] = rect(-5.0, -5.0, 10.0, 10.0);
        let changed = part.rebuild(None).unwrap();
        assert!(!Rc::ptr_eq(&full, &changed));
        assert_eq!(part.mesh(None).faces.len(), 10);
        assert!(Rc::ptr_eq(&first, &part.rebuild(Some(hole)).unwrap()));
        // Пошаговое построение: по одному новому элементу за вызов, готовые берутся из кэша.
        let mut fresh: Part = serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
        let (body, pending) = fresh.rebuild_step(None, 1);
        assert_eq!((fresh.mesh(None).faces.len(), pending), (6, Some(3)));
        assert!(body.is_some());
        let (_, pending) = fresh.rebuild_step(None, 1);
        assert_eq!((fresh.mesh(None).faces.len(), pending), (10, None));
        // Ошибка тоже запоминается и снимается, когда элемент исправлен.
        let FeatureKind::Cut { depth, through_all, .. } = &mut part.feature_mut(cut).unwrap().kind else { panic!() };
        (*depth, *through_all) = (0.0, false);
        assert!(Rc::ptr_eq(&first, &part.rebuild(None).unwrap()));
        assert!(part.feature(cut).unwrap().error.is_some());
        let FeatureKind::Cut { depth, .. } = &mut part.feature_mut(cut).unwrap().kind else { panic!() };
        *depth = 4.0;
        part.rebuild(None).unwrap();
        assert!(part.feature(cut).unwrap().error.is_none());
        // Удаление элемента сдвигает остальные — кэш это замечает.
        part.remove(hole);
        assert!(Rc::ptr_eq(&first, &part.rebuild(None).unwrap()));
        assert_eq!(part.mesh(None).faces.len(), 6);
    }

    #[test]
    fn fillet_follows_moved_edges() {
        let _kernel = kernel();
        let (mut part, _) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        part.rebuild(None);
        let mesh = part.mesh(None);
        let mid = mesh
            .edges
            .iter()
            .map(|e| e.mid)
            .find(|m| (m.x - 40.0).abs() < 1e-6 && (m.z - 10.0).abs() < 1e-6)
            .unwrap();
        let fillet = part.add("Скругление", FeatureKind::Fillet { edges: vec![mid], radius: 2.0 });
        part.rebuild(None);
        assert!(part.feature(fillet).unwrap().error.is_none());
        // Изменили размер эскиза: ребро сдвинулось, но скругление его находит и запоминает новое место.
        let sketch = part.features[0].id;
        part.sketch_mut(sketch).unwrap().shapes[0] = rect(0.0, 0.0, 55.0, 30.0);
        part.rebuild(None);
        assert_eq!(part.feature(fillet).unwrap().error, None);
        let FeatureKind::Fillet { edges, .. } = &part.feature(fillet).unwrap().kind else { panic!() };
        assert!((edges[0].x - 55.0).abs() < 1e-6, "{:?}", edges[0]);
    }

    #[test]
    fn midpoint_relation() {
        let shapes = || vec![rect(0.0, 0.0, 10.0, 4.0), SketchShape::Circle { cx: 3.0, cy: 9.0, r: 1.0 }];
        let seg = DimRef::Segment(0, 0);
        let point = DimRef::Point(1, 0);
        let mut s = sketch(shapes());
        assert!(s.relations(seg, point).contains(&Relation::Midpoint));
        // Точка идёт к середине отрезка.
        let before = s.ref_outline(seg);
        s.apply_relation(seg, point, Relation::Midpoint).unwrap();
        let mid = (before[0] + before[1]) * 0.5;
        assert!((s.ref_outline(point)[0] - mid).length() < 1e-9);
        // Первой выбрана точка — отрезок переезжает к ней.
        let mut s = sketch(shapes());
        s.apply_relation(point, seg, Relation::Midpoint).unwrap();
        let moved = s.ref_outline(seg);
        assert!(((moved[0] + moved[1]) * 0.5 - DVec2::new(3.0, 9.0)).length() < 1e-9);
        assert!(s.apply_relation(seg, DimRef::Segment(0, 1), Relation::Midpoint).is_err());
    }

    #[test]
    fn hidden_edges_are_found_by_ray() {
        let _kernel = kernel();
        let (mut part, _) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        part.rebuild(None);
        let mesh = part.mesh(None);
        let up = DVec3::Z;
        let hidden = |p: DVec3| mesh.pick_hit(p + up * 0.05, up).is_some();
        let front_top = DVec3::new(20.0, 0.0, 10.0);
        let front_bottom = DVec3::new(20.0, 0.0, 0.0);
        assert!(mesh.edge_at(front_top).is_some() && mesh.edge_at(front_bottom).is_some());
        // Смотрим сверху: верхнее ребро видно, нижнее закрыто деталью.
        assert!(!hidden(front_top));
        assert!(hidden(front_bottom));
    }

    #[test]
    fn face_and_part_colors() {
        let _kernel = kernel();
        let (mut part, body) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        let top = mesh.pick(DVec3::new(20.0, 15.0, 100.0), -DVec3::Z).unwrap();
        part.color = Some([255, 0, 0]);
        part.set_face_color(mesh.faces[top].origin, mesh.faces[top].normal, Some([0, 0, 255]));
        part.set_face_color(mesh.faces[top].origin, mesh.faces[top].normal, Some([0, 255, 0]));
        assert_eq!(part.face_colors.len(), 1, "повторная покраска заменяет прежнюю");
        let colors = part.vertex_colors(&mesh, &[]);
        assert_eq!(colors.len(), mesh.vertices.len() / 7 * 4);
        for (v, c) in mesh.vertices.chunks_exact(7).zip(colors.chunks_exact(4)) {
            assert_eq!(c[..3], if v[6] as usize == top { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] });
        }
        // Цвета сохраняются в файле; сброс возвращает грани цвет детали.
        let mut saved: Part = serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
        assert_eq!((saved.color, saved.face_colors[0].color), (Some([255, 0, 0]), Some([0, 255, 0])));
        saved.set_face_color(mesh.faces[top].origin, mesh.faces[top].normal, None);
        assert!(saved.vertex_colors(&mesh, &[]).chunks_exact(4).all(|c| c[..3] == [1.0, 0.0, 0.0]));
    }

    #[test]
    fn materials_for_faces_and_part() {
        let _kernel = kernel();
        let (mut part, body) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        let top = mesh.pick(DVec3::new(20.0, 15.0, 100.0), -DVec3::Z).unwrap();
        let ids = |part: &Part| -> Vec<(usize, f32)> {
            let colors = part.vertex_colors(&mesh, &[]);
            mesh.vertices.chunks_exact(7).zip(colors.chunks_exact(4)).map(|(v, c)| (v[6] as usize, c[3])).collect()
        };
        // Металл на одной грани не трогает остальные и не мешает цвету.
        part.set_face_color(mesh.faces[top].origin, mesh.faces[top].normal, Some([255, 0, 0]));
        part.set_face_material(mesh.faces[top].origin, mesh.faces[top].normal, Some(Material::Metal));
        assert_eq!(part.face_colors.len(), 1);
        assert!(ids(&part).iter().all(|&(f, m)| m == if f == top { 1.0 } else { 0.0 }));
        // Перекраска грани не сбрасывает её материал.
        part.set_face_color(mesh.faces[top].origin, mesh.faces[top].normal, Some([0, 255, 0]));
        assert_eq!(part.face_colors[0].material, Some(Material::Metal));
        // Файл хранит материалы.
        let saved: Part = serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
        assert_eq!(saved.face_colors[0].material, Some(Material::Metal));
        // Материал всей детали заменяет материалы граней, а их цвета остаются.
        part.paint_part_material(Material::Glass);
        assert!(ids(&part).iter().all(|&(_, m)| m == 2.0));
        assert_eq!(part.face_colors.len(), 1);
        // Цвет всей детали убирает цвета граней.
        part.paint_part_color(Some([1, 2, 3]));
        assert!(part.face_colors.is_empty());
    }

    #[test]
    fn face_color_survives_resize() {
        let _kernel = kernel();
        let (mut part, body) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        let top = mesh.pick(DVec3::new(20.0, 15.0, 100.0), -DVec3::Z).unwrap();
        part.set_face_color(mesh.faces[top].origin, mesh.faces[top].normal, Some([0, 0, 255]));
        // Деталь стала шире: центр верхней грани уехал, цвет остаётся на ней.
        let sketch = part.features[0].id;
        part.sketch_mut(sketch).unwrap().shapes[0] = rect(0.0, 0.0, 70.0, 30.0);
        let body = part.rebuild(None).unwrap();
        let mesh = RenderMesh::from_shape(&body);
        let top = mesh.pick(DVec3::new(35.0, 15.0, 100.0), -DVec3::Z).unwrap();
        let colors = part.vertex_colors(&mesh, &[]);
        let v = mesh.vertices.chunks_exact(7).position(|v| v[6] as usize == top).unwrap();
        assert_eq!(&colors[v * 4..v * 4 + 3], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn construction_and_axis_lines() {
        let _kernel = kernel();
        let (mut part, _) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let id = part.features[0].id;
        let sk = part.sketch_mut(id).unwrap();
        sk.shapes.push(path(&[(0.0, 50.0, 0.0), (30.0, 54.0, 0.0)], false));
        assert_eq!(sk.nearest_line(DVec2::new(15.0, 52.0), 3.0), Some((1, 0)));
        sk.make_axis(1, 0, true).unwrap();
        assert!((sk.shapes[1].points()[1].y - 50.0).abs() < 1e-9);
        assert!((sk.shapes[1].points()[1].x - (30.0f64 * 30.0 + 16.0).sqrt()).abs() < 1e-9);
        sk.make_axis(1, 0, false).unwrap();
        assert!((sk.shapes[1].points()[1].x).abs() < 1e-9);
        // Линия построения в тело не входит, даже незамкнутая.
        sk.set_construction(1, 0, true).unwrap();
        assert!(sk.shapes[1].is_construction());
        let body = part.rebuild(None);
        assert!(part.features[1].error.is_none(), "{:?}", part.features[1].error);
        assert!(body.is_some());
    }

    #[test]
    fn points_horizontal_vertical_and_origin() {
        let mut s = Sketch::new(Plane::TOP, vec![path(&[(5.0, 3.0, 0.0), (30.0, 10.0, 0.0), (50.0, 40.0, 0.0)], false)]);
        let (a, b, c) = (DimRef::Point(0, 0), DimRef::Point(0, 1), DimRef::Point(0, 2));
        assert_eq!(s.relations(a, b), vec![Relation::Coincident, Relation::Horizontal, Relation::Vertical]);
        s.apply_relation(b, c, Relation::Horizontal).unwrap();
        assert!((s.shapes[0].points()[2].y - 10.0).abs() < 1e-9);
        s.apply_relation(b, a, Relation::Vertical).unwrap();
        assert!((s.shapes[0].points()[0].x - 30.0).abs() < 1e-9);
        // Начало координат неподвижно: к нему подтягивается точка.
        s.apply_relation(DimRef::Origin, b, Relation::Vertical).unwrap();
        assert!(s.shapes[0].points()[1].x.abs() < 1e-9);
        s.apply_relation(DimRef::Origin, b, Relation::Coincident).unwrap();
        assert!(s.shapes[0].points()[1].length() < 1e-9);
    }

    #[test]
    fn choose_extrude_regions() {
        let _kernel = kernel();
        // Два отдельных прямоугольника и окружность внутри первого.
        let shapes = vec![
            rect(0.0, 0.0, 40.0, 30.0),
            rect(60.0, 0.0, 20.0, 20.0),
            SketchShape::Circle { cx: 20.0, cy: 15.0, r: 5.0 },
        ];
        let mut part = Part::default();
        let sk = part.add("Эскиз", FeatureKind::Sketch(Sketch::new(Plane::TOP, shapes)));
        let boss = part.add("Бобышка", FeatureKind::Boss { sketch: sk, depth: 10.0, flip: false });
        let volume = |part: &mut Part| {
            let body = part.rebuild(None).expect("тело");
            let mesh = RenderMesh::from_shape(&body);
            let (lo, hi) = mesh.bounds.unwrap();
            (hi - lo, part.features.iter().find_map(|f| f.error.clone()))
        };
        // По умолчанию: оба прямоугольника, окружность — отверстие.
        assert_eq!(part.sketch(sk).unwrap().active_regions(boss), vec![0, 1]);
        let (size, error) = volume(&mut part);
        assert!(error.is_none() && (size.x - 80.0).abs() < 1e-6, "{size:?} {error:?}");
        // Только второй прямоугольник.
        let s = part.sketch_mut(sk).unwrap();
        s.toggle_region(boss, 0);
        assert_eq!(s.regions_of(boss), Some(&[1usize][..]));
        let (size, _) = volume(&mut part);
        assert!((size.x - 20.0).abs() < 1e-6, "{size:?}");
        // Остров: окружность сама по себе.
        let s = part.sketch_mut(sk).unwrap();
        s.toggle_region(boss, 1);
        s.toggle_region(boss, 2);
        let (size, _) = volume(&mut part);
        assert!((size.x - 10.0).abs() < 1e-6, "{size:?}");
        // Удаление фигуры сдвигает номера выбранных областей.
        let s = part.sketch_mut(sk).unwrap();
        s.remove_shape(0);
        assert_eq!(s.regions_of(boss), Some(&[1usize][..]));
        // Возврат к областям по умолчанию.
        s.reset_regions(boss);
        assert!(s.regions_of(boss).is_none());
        // Пустой выбор — ошибка построения, а не паника.
        s.toggle_region(boss, 0);
        s.toggle_region(boss, 1);
        let s = part.sketch_mut(sk).unwrap();
        if s.default_regions().len() == 2 {
            s.toggle_region(boss, 0);
            s.toggle_region(boss, 1);
        }
        let _ = part.rebuild(None);
    }

    #[test]
    fn align_segment_midpoints() {
        let mut s = Sketch::new(
            Plane::TOP,
            vec![path(&[(0.0, 0.0, 0.0), (20.0, 0.0, 0.0)], false), path(&[(5.0, 10.0, 0.0), (15.0, 18.0, 0.0)], false)],
        );
        let (mid_a, mid_b) = (Pick::Mid(0, 0), Pick::Mid(1, 0));
        assert_eq!(s.pick_pos(mid_a), Some(DVec2::new(10.0, 0.0)));
        // Середина первой на 10 по X: вторая фигура сдвигается так, чтобы её начало встало на X = 10.
        s.align(mid_a, Pick::Ref(DimRef::Point(1, 0)), Relation::Vertical).unwrap();
        assert!((s.shapes[1].points()[0].x - 10.0).abs() < 1e-9);
        // Середина второй встаёт на горизонталь середины первой.
        s.align(mid_a, mid_b, Relation::Horizontal).unwrap();
        assert!(s.pick_pos(mid_b).unwrap().y.abs() < 1e-9);
        // Совпадение середин; начало координат — только первой точкой.
        s.align(mid_a, mid_b, Relation::Coincident).unwrap();
        assert!((s.pick_pos(mid_b).unwrap() - DVec2::new(10.0, 0.0)).length() < 1e-9);
        s.align(Pick::Ref(DimRef::Origin), mid_b, Relation::Vertical).unwrap();
        assert!(s.pick_pos(mid_b).unwrap().x.abs() < 1e-9);
        // Рядом с серединой отрезка она находится щелчком, а вершины важнее.
        assert_eq!(s.pick_point(DVec2::new(0.1, 0.1), 1.0), Some(Pick::Ref(DimRef::Point(0, 0))));
        assert!(matches!(s.pick_point(s.pick_pos(mid_a).unwrap() + DVec2::new(0.2, 0.0), 1.0), Some(Pick::Mid(0, 0))));
    }

    #[test]
    fn dimension_to_previous_sketch() {
        let _kernel = kernel();
        let mut part = Part::default();
        let first = part.add("Эскиз", FeatureKind::Sketch(Sketch::new(Plane::TOP, vec![rect(0.0, 0.0, 40.0, 30.0)])));
        let second = part.add("Эскиз", FeatureKind::Sketch(Sketch::new(Plane::TOP, vec![rect(50.0, 5.0, 10.0, 10.0)])));
        part.refresh_ext(second);
        let sk = part.sketch(second).unwrap();
        // Точки и стороны первого эскиза видны во втором и находятся щелчком.
        let corner = sk.pick_ref(DVec2::new(40.2, 30.1), 1.0);
        assert_eq!(corner, Some(DimRef::Ext(first, 0, 2, false)));
        let side = sk.pick_ref(DVec2::new(20.0, 0.2), 1.0);
        assert_eq!(side, Some(DimRef::Ext(first, 0, 0, true)));
        // Расстояние от угла первого эскиза до угла нового: 10 по X и 25 по Y.
        let near = DimRef::Point(0, 0);
        let dim = sk.pair_dim(corner.unwrap(), near).unwrap();
        assert_eq!(dim, Dim::Distance(corner.unwrap(), near, DimMode::Vertical));
        assert!((sk.dim_value(&dim).unwrap() - 25.0).abs() < 1e-9);
        // Новое значение двигает свою фигуру, внешняя остаётся на месте.
        let sk = part.sketch_mut(second).unwrap();
        sk.set_dim(&dim, 5.0).unwrap();
        assert!((sk.shapes[0].points()[0].y - 25.0).abs() < 1e-9);
        // Если внешний элемент указан вторым, всё равно двигается свой.
        let flipped = Dim::Distance(near, corner.unwrap(), DimMode::Horizontal);
        sk.set_dim(&flipped, 20.0).unwrap();
        assert!((sk.shapes[0].points()[0].x - 60.0).abs() < 1e-9, "{:?}", sk.shapes[0].points());
        // Расстояние от точки до стороны (внешний отрезок — первый).
        let line_dim = sk.pair_dim(side.unwrap(), near).unwrap();
        assert!((sk.dim_value(&line_dim).unwrap() - 25.0).abs() < 1e-9);
        // Совпадение с внешней точкой: своя фигура встаёт на неё.
        sk.align(Pick::Ref(corner.unwrap()), Pick::Ref(near), Relation::Coincident).unwrap();
        assert!((sk.shapes[0].points()[0] - DVec2::new(40.0, 30.0)).length() < 1e-9);
    }

    #[test]
    fn round_only_chosen_corners() {
        let mut s = Sketch::new(Plane::TOP, vec![rect(0.0, 0.0, 40.0, 30.0)]);
        // Два угла из четырёх: остальные остаются острыми.
        assert_eq!(s.round_corner_set(0, &[0, 2], 5.0, false), Ok(2));
        let SketchShape::Path { points, closed: true } = &s.shapes[0] else { panic!() };
        assert_eq!(points.len(), 6);
        assert_eq!(points.iter().filter(|p| p.bulge != 0.0).count(), 2);
        // Невозможный угол: фигура не меняется целиком.
        let mut t = Sketch::new(Plane::TOP, vec![rect(0.0, 0.0, 4.0, 4.0)]);
        let before = t.shapes[0].clone();
        assert!(t.round_corner_set(0, &[0, 1], 3.0, false).is_err());
        assert_eq!(t.shapes[0], before);
    }

    #[test]
    fn stretch_from_anchor() {
        let mut r = rect(10.0, 10.0, 40.0, 30.0);
        // Якорь — левый нижний угол (10, 10); противоположный угол (50, 40) уходит в (90, 70).
        r.stretch(DVec2::new(10.0, 10.0), 2.0, 2.0);
        assert_eq!(r, rect(10.0, 10.0, 80.0, 60.0));
        // Растяжение с отражением: прямоугольник остаётся прямоугольником с положительными размерами.
        r.stretch(DVec2::new(10.0, 10.0), -0.5, 1.0);
        assert_eq!(r, rect(-30.0, 10.0, 40.0, 60.0));
        let mut p = path(&[(0.0, 0.0, 0.5), (10.0, 0.0, 0.0), (10.0, 10.0, 0.0)], true);
        p.stretch(DVec2::ZERO, 2.0, 2.0);
        assert_eq!(p.points()[2], DVec2::new(20.0, 20.0));
        let SketchShape::Path { points, .. } = &p else { panic!() };
        assert!((points[0].bulge - 0.5).abs() < 1e-12);
    }

    #[test]
    fn second_offset_resizes_instead_of_moving() {
        let mut s = Sketch::new(Plane::TOP, vec![rect(0.0, 0.0, 40.0, 30.0)]);
        let (left, right) = (DimRef::Point(0, 0), DimRef::Point(0, 1));
        let from_left = Dim::Distance(DimRef::Origin, left, DimMode::Horizontal);
        let from_right = Dim::Distance(DimRef::Origin, right, DimMode::Horizontal);
        // Первый отступ сдвигает фигуру целиком.
        s.store_dim(from_left);
        s.set_dim(&from_left, 5.0).unwrap();
        assert_eq!(s.shapes[0], rect(5.0, 0.0, 40.0, 30.0));
        // Второй — от противоположной стороны: первый отступ остаётся, меняется ширина.
        s.store_dim(from_right);
        s.set_dim(&from_right, 50.0).unwrap();
        assert_eq!(s.shapes[0], rect(5.0, 0.0, 45.0, 30.0));
        assert!((s.dim_value(&from_left).unwrap() - 5.0).abs() < 1e-9);
        // Теперь меняем первый: правый край стоит на месте, меняется ширина с другой стороны.
        s.set_dim(&from_left, 8.0).unwrap();
        assert_eq!(s.shapes[0], rect(8.0, 0.0, 42.0, 30.0));
        assert!((s.dim_value(&from_right).unwrap() - 50.0).abs() < 1e-9);
    }

    /// Замер времени построения по элементам файла (запуск вручную: cargo test time_file -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn time_file() {
        let _kernel = kernel();
        let path = std::env::var("MS3D_FILE").unwrap_or_else(|_| "samples/КрышкаRP2040.ms3d".into());
        let mut part: Part = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let total = std::time::Instant::now();
        let mut last = total.elapsed();
        loop {
            let (_, pending) = part.rebuild_step(None, 1);
            let now = total.elapsed();
            let i = pending.map_or(part.features.len(), |i| i);
            if now - last > std::time::Duration::from_millis(30) {
                println!("до элемента {i:>2}: {:>7.0} мс (очередной построен: {})", (now - last).as_secs_f64() * 1000.0, part.features.get(i.saturating_sub(1)).map_or("", |f| f.name.as_str()));
            }
            last = now;
            if pending.is_none() {
                break;
            }
        }
        println!("итого {:.1} с", total.elapsed().as_secs_f64());
    }

    #[test]
    fn groups_in_tree() {
        use TreeItem::{Feature as F, Group as G};
        let mut part = Part::default();
        let ids: Vec<u64> = (0..4)
            .map(|_| part.add("Эскиз", FeatureKind::Sketch(sketch(vec![rect(0.0, 0.0, 1.0, 1.0)]))))
            .collect();
        let g = part.add_group(&[ids[1], ids[3]]);
        assert_eq!(
            part.tree_rows(),
            [(F(ids[0]), false), (G(g), false), (F(ids[1]), true), (F(ids[3]), true), (F(ids[2]), false)]
        );
        part.groups[0].collapsed = true;
        assert_eq!(part.tree_rows(), [(F(ids[0]), false), (G(g), false), (F(ids[2]), false)]);
        // Группы сохраняются в файле; старые файлы без групп читаются.
        let saved: Part = serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
        assert!(saved.groups[0].collapsed && saved.groups[0].members == [ids[1], ids[3]]);
        assert!(serde_json::from_str::<Part>(r#"{"features":[],"next_id":0}"#).unwrap().groups.is_empty());
        // Элемент переходит в новую группу; опустевшая и удалённая группы исчезают.
        let g2 = part.add_group(&[ids[3], ids[2]]);
        part.remove(ids[1]);
        assert_eq!(part.tree_rows(), [(F(ids[0]), false), (G(g2), false), (F(ids[2]), true), (F(ids[3]), true)]);
        part.remove_group(g2);
        assert!(part.groups.is_empty() && part.features.len() == 3);
    }

    #[test]
    fn sketch_transforms() {
        // Поворот прямоугольника делает из него контур.
        let mut r = rect(0.0, 0.0, 4.0, 2.0);
        assert!(close(r.center(), 2.0, 1.0));
        r.rotate(DVec2::ZERO, std::f64::consts::FRAC_PI_2);
        let p = r.points();
        assert!(close(p[0], 0.0, 0.0) && close(p[1], 0.0, 4.0) && close(p[2], -2.0, 4.0) && close(p[3], -2.0, 0.0), "{p:?}");
        // Масштаб.
        let mut c = SketchShape::Circle { cx: 2.0, cy: 0.0, r: 1.0 };
        c.scale(DVec2::ZERO, 3.0).unwrap();
        assert_eq!(c, SketchShape::Circle { cx: 6.0, cy: 0.0, r: 3.0 });
        assert!(c.scale(DVec2::ZERO, 0.0).is_err());
        // Зеркало: прямоугольник остаётся прямоугольником, у контура дуги меняют направление.
        let mut r = rect(1.0, 2.0, 4.0, 3.0);
        r.mirror(DVec2::ZERO, DVec2::Y);
        assert_eq!(r, rect(-5.0, 2.0, 4.0, 3.0));
        let mut slot = path(&[(0.0, 0.0, 0.0), (20.0, 0.0, 1.0), (20.0, 10.0, 0.0), (0.0, 10.0, 0.0)], true);
        slot.mirror(DVec2::ZERO, DVec2::Y);
        let SketchShape::Path { points, .. } = &slot else { panic!() };
        assert!(close(points[1].pos(), -20.0, 0.0) && points[1].bulge == -1.0);
        let far = slot.outline().iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        assert!((far + 25.0).abs() < 1e-6, "полукруг по-прежнему выпуклый наружу: {far}");
    }

    #[test]
    fn contour_offset() {
        assert_eq!(rect(0.0, 0.0, 4.0, 2.0).offset(1.0).unwrap(), rect(-1.0, -1.0, 6.0, 4.0));
        assert!(rect(0.0, 0.0, 4.0, 2.0).offset(-1.0).is_err());
        assert_eq!(
            SketchShape::Circle { cx: 1.0, cy: 1.0, r: 2.0 }.offset(-0.5).unwrap(),
            SketchShape::Circle { cx: 1.0, cy: 1.0, r: 1.5 }
        );
        // «Г»: наружу — независимо от направления обхода.
        let out = l_shape().offset(1.0).unwrap().points();
        assert!(close(out[0], -1.0, -1.0) && close(out[1], 31.0, -1.0) && close(out[2], 31.0, 11.0), "{out:?}");
        assert!(close(out[3], 11.0, 11.0) && close(out[4], 11.0, 26.0) && close(out[5], -1.0, 26.0), "{out:?}");
        let mut reversed = l_shape();
        if let SketchShape::Path { points, .. } = &mut reversed {
            points.reverse();
        }
        let out = reversed.offset(1.0).unwrap().points();
        assert!(out.iter().any(|p| close(*p, -1.0, -1.0)) && out.iter().any(|p| close(*p, 31.0, 11.0)), "{out:?}");
        let inner = l_shape().offset(-2.0).unwrap().points();
        assert!(close(inner[0], 2.0, 2.0) && close(inner[3], 8.0, 8.0), "{inner:?}");
        assert!(l_shape().offset(-6.0).is_err(), "полка «Г» схлопывается");
        // Паз с полукруглым торцом: дуга стыкуется по касательной и остаётся дугой.
        let slot = path(&[(0.0, 0.0, 0.0), (20.0, 0.0, 1.0), (20.0, 10.0, 0.0), (0.0, 10.0, 0.0)], true);
        let grown = slot.offset(2.0).unwrap();
        let SketchShape::Path { points, .. } = &grown else { panic!() };
        assert!(close(points[1].pos(), 20.0, -2.0) && close(points[2].pos(), 20.0, 12.0) && points[1].bulge == 1.0);
        let reach = grown.outline().iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        assert!((reach - 27.0).abs() < 1e-6, "{reach}");
    }

    #[test]
    fn rounded_and_chamfered_corners() {
        let _kernel = kernel();
        // Скругление одного угла прямоугольника.
        let mut r = rect(0.0, 0.0, 40.0, 30.0);
        assert_eq!(r.round_corners(Some(1), 5.0, false), Ok(1));
        let SketchShape::Path { points, closed: true } = &r else { panic!() };
        assert_eq!(points.len(), 5);
        assert!(close(points[1].pos(), 35.0, 0.0) && close(points[2].pos(), 40.0, 5.0));
        let (a, b, bulge) = r.segment(1).unwrap();
        assert!((arc_radius(a, b, bulge) - 5.0).abs() < 1e-9 && close(arc_center(a, b, bulge), 35.0, 5.0));
        // Все углы; тело из такого контура строится, по вертикали — 4 цилиндрические грани.
        let mut all = rect(0.0, 0.0, 40.0, 30.0);
        assert_eq!(all.round_corners(None, 5.0, false), Ok(4));
        let (part, body) = extruded(all, 10.0);
        assert!(part.features[1].error.is_none(), "{:?}", part.features[1].error);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        assert_eq!((mesh.faces.len(), mesh.faces.iter().filter(|f| !f.planar).count()), (10, 4));
        assert!(mesh.pick(DVec3::new(0.5, 0.5, 50.0), -DVec3::Z).is_none());
        // Внутренний угол «Г» скругляется в другую сторону; фаска — отрезком.
        let mut l = l_shape();
        l.round_corners(Some(3), 2.0, false).unwrap();
        let (a, b, bulge) = l.segment(3).unwrap();
        assert!(bulge < 0.0 && close(arc_center(a, b, bulge), 12.0, 12.0), "{a:?} {b:?} {bulge}");
        let mut c = rect(0.0, 0.0, 10.0, 10.0);
        c.round_corners(Some(0), 2.0, true).unwrap();
        let p = c.points();
        assert!(close(p[0], 0.0, 2.0) && close(p[1], 2.0, 0.0) && c.segment(0).unwrap().2 == 0.0, "{p:?}");
        // Не помещается / не угол.
        assert!(rect(0.0, 0.0, 4.0, 4.0).round_corners(Some(0), 5.0, false).is_err());
        // В эскизе скругление убирает размеры этой фигуры (номера её точек сдвинулись), чужие остаются.
        let mut s = sketch(vec![rect(0.0, 0.0, 10.0, 10.0), SketchShape::Circle { cx: 0.0, cy: 0.0, r: 1.0 }]);
        s.store_dim(s.element_dim(DimRef::Segment(0, 0)).unwrap());
        s.store_dim(Dim::Size(DimRef::Circle(1)));
        assert_eq!(s.round_corners(0, None, 1.0, true), Ok(4));
        assert_eq!(s.dims, [Dim::Size(DimRef::Circle(1))]);
        // Место размера хранится вместе с эскизом и забывается вместе с размером.
        s.set_dim_place(Dim::Size(DimRef::Circle(1)), DVec2::new(3.0, 4.0));
        s.set_dim_place(Dim::Size(DimRef::Circle(0)), DVec2::ONE);
        s.prune_dim_places();
        let saved: Sketch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(saved.dim_places, [(Dim::Size(DimRef::Circle(1)), DVec2::new(3.0, 4.0))]);
        assert!(SketchShape::Circle { cx: 0.0, cy: 0.0, r: 1.0 }.round_corners(None, 1.0, false).is_err());
    }

    #[test]
    fn sketch_patterns() {
        let mut s = sketch(vec![SketchShape::Circle { cx: 10.0, cy: 0.0, r: 1.0 }]);
        assert_eq!(s.pattern_grid(0, 3, 2, DVec2::new(5.0, 4.0)), Ok(5));
        assert_eq!(s.shapes.len(), 6);
        assert_eq!(s.shapes[5], SketchShape::Circle { cx: 20.0, cy: 4.0, r: 1.0 });
        assert!(s.pattern_grid(0, 1, 1, DVec2::ONE).is_err());
        let mut s = sketch(vec![SketchShape::Circle { cx: 10.0, cy: 0.0, r: 1.0 }]);
        assert_eq!(s.pattern_circle(0, 4, DVec2::ZERO, 360.0), Ok(3));
        assert!(close(s.shapes[1].points()[0], 0.0, 10.0) && close(s.shapes[3].points()[0], 0.0, -10.0));
        // Неполный круг: первая и последняя — на краях сектора.
        let mut s = sketch(vec![rect(10.0, -1.0, 2.0, 2.0)]);
        s.pattern_circle(0, 3, DVec2::ZERO, 180.0).unwrap();
        assert!(close(s.shapes[2].center(), -11.0, 0.0), "{:?}", s.shapes[2].center());
    }

    #[test]
    fn faces_know_their_edges() {
        let _kernel = kernel();
        let (_, body) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        assert!(mesh.faces.iter().all(|f| f.edges.len() == 4));
        // У цилиндра шов боковой грани ребром не считается.
        let (_, body) = extruded(SketchShape::Circle { cx: 0.0, cy: 0.0, r: 5.0 }, 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        let side = mesh.faces.iter().find(|f| !f.planar).unwrap();
        assert_eq!(side.edges.len(), 2);
        assert!(side.edges.iter().all(|e| mesh.edges.iter().any(|m| (m.mid - *e).length() < 1e-6)));
    }

    #[test]
    fn stored_dimensions() {
        let mut s = sketch(vec![rect(0.0, 0.0, 4.0, 3.0), l_shape(), SketchShape::Circle { cx: 50.0, cy: 40.0, r: 2.0 }]);
        // Отрезок — это расстояние между его концами; дуга и окружность — радиус и диаметр.
        let bottom = s.element_dim(DimRef::Segment(1, 0)).unwrap();
        assert_eq!(bottom, Dim::Distance(DimRef::Point(1, 0), DimRef::Point(1, 1), DimMode::Aligned));
        assert!(s.dim_has_modes(&bottom));
        let width = s.element_dim(DimRef::Segment(0, 0)).unwrap();
        let diameter = s.element_dim(DimRef::Circle(2)).unwrap();
        assert_eq!(diameter, Dim::Size(DimRef::Circle(2)));
        assert!(s.element_dim(DimRef::Point(1, 0)).is_none());
        // Размер по концам отрезка ведёт себя как длина: соседние стороны остаются прямыми.
        s.set_dim(&bottom, 40.0).unwrap();
        assert!(close(s.shapes[1].points()[2], 40.0, 10.0));
        s.set_dim(&width, 8.0).unwrap();
        assert_eq!(s.shapes[0], rect(0.0, 0.0, 8.0, 3.0));

        assert_eq!((s.store_dim(width), s.store_dim(bottom), s.store_dim(diameter), s.store_dim(bottom)), (0, 1, 2, 1));
        let to_origin = Dim::Distance(DimRef::Origin, DimRef::Point(2, 0), DimMode::Horizontal);
        s.store_dim(to_origin);
        // Размеры сохраняются в файле; старые эскизы без размеров читаются.
        let saved: Sketch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(saved.dims, s.dims);
        let old = r#"{"plane":{"origin":[0,0,0],"x":[1,0,0],"y":[0,1,0]},"shapes":[]}"#;
        assert!(serde_json::from_str::<Sketch>(old).unwrap().dims.is_empty());
        // Удаление фигуры убирает её размеры и не сбивает остальные.
        s.remove_shape(1);
        assert_eq!(
            s.dims,
            [width, Dim::Size(DimRef::Circle(1)), Dim::Distance(DimRef::Origin, DimRef::Point(1, 0), DimMode::Horizontal)]
        );
        assert_eq!(s.dim_value(&s.dims[1]), Some(4.0));
    }

    #[test]
    fn picking_dimension_references() {
        let s = sketch(vec![l_shape(), SketchShape::Circle { cx: 50.0, cy: 40.0, r: 2.0 }]);
        assert_eq!(s.pick_ref(DVec2::new(30.2, 0.1), 0.5), Some(DimRef::Point(0, 1)));
        assert_eq!(s.pick_ref(DVec2::new(15.0, 0.2), 0.5), Some(DimRef::Segment(0, 0)));
        assert_eq!(s.pick_ref(DVec2::new(-0.2, 12.0), 0.5), Some(DimRef::Segment(0, 5)));
        assert_eq!(s.pick_ref(DVec2::new(52.1, 40.0), 0.5), Some(DimRef::Circle(1)));
        assert_eq!(s.pick_ref(DVec2::new(50.0, 40.1), 0.5), Some(DimRef::Point(1, 0)));
        assert_eq!(s.pick_ref(DVec2::new(20.0, 20.0), 0.5), None);
        let free = sketch(vec![]);
        assert_eq!(free.pick_ref(DVec2::new(0.1, 0.1), 0.5), Some(DimRef::Origin));
    }

    #[test]
    fn bad_contours_are_errors() {
        let _kernel = kernel();
        let open = path(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (10.0, 10.0, 0.0)], false);
        let (part, body) = extruded(open, 5.0);
        assert!(body.is_none() && part.features[1].error.is_some());

        let bow_tie = path(&[(0.0, 0.0, 0.0), (10.0, 10.0, 0.0), (10.0, 0.0, 0.0), (0.0, 10.0, 0.0)], true);
        let (part, body) = extruded(bow_tie, 5.0);
        assert!(body.is_none());
        assert!(part.features[1].error.as_deref().unwrap().contains("пересекает"));
    }

    #[test]
    fn fillet_and_chamfer() {
        let _kernel = kernel();
        let (mut part, body) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        assert_eq!(mesh.edges.len(), 12);
        let vertical: Vec<DVec3> =
            mesh.edges.iter().filter(|e| (e.mid.z - 5.0).abs() < 1e-6).map(|e| e.mid).collect();
        assert_eq!(vertical.len(), 4);

        let fillet = part.add("Скругление", FeatureKind::Fillet { edges: vertical.clone(), radius: 5.0 });
        let body = part.rebuild(None).unwrap();
        assert!(part.feature(fillet).unwrap().error.is_none(), "{:?}", part.feature(fillet).unwrap().error);
        let mesh = RenderMesh::from_shape(&body);
        assert_eq!(mesh.faces.len(), 10, "6 граней + 4 скругления");
        assert!(mesh.pick(DVec3::new(0.5, 0.5, 50.0), -DVec3::Z).is_none(), "угол срезан скруглением");

        // Фаска на верхнем ребре (после скругления оно стало короче, но середина та же).
        // Верхний контур теперь гладкий, поэтому ядро ведёт фаску по всей касательной цепочке:
        // 4 прямых участка + 4 дуги = 8 новых граней.
        let top_edge = mesh
            .edges
            .iter()
            .find(|e| (e.mid - DVec3::new(20.0, 0.0, 10.0)).length() < 1e-6)
            .expect("верхнее переднее ребро")
            .mid;
        let chamfer = part.add("Фаска", FeatureKind::Chamfer { edges: vec![top_edge], distance: 2.0 });
        let body = part.rebuild(None).unwrap();
        assert!(part.feature(chamfer).unwrap().error.is_none(), "{:?}", part.feature(chamfer).unwrap().error);
        assert_eq!(RenderMesh::from_shape(&body).faces.len(), 18);
    }

    #[test]
    fn sample_part_surface_is_closed() {
        let _kernel = kernel();
        // Скругление цепочки рёбер через паз: при исходном порядке рёбер ядро выворачивает две грани.
        let json = std::fs::read_to_string("samples/деталь-пример.ms3d").unwrap();
        let mut part: Part = serde_json::from_str(&json).unwrap();
        let body = part.rebuild(None).expect("тело примера");
        assert!(body.faces().all(|f| f.surface_area() > 0.0));
        // Рёбра «Скругления9» после правок размеров сдвинулись — теперь они находятся заново, и всё строится.
        let failed: Vec<u64> = part.features.iter().filter(|f| f.error.is_some()).map(|f| f.id).collect();
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(RenderMesh::from_shape(&body).torn_edges(), 0);
    }

    /// Брусок с пазом из трёх дуг, стыкующихся под углом (как в `samples/деталь2.ms3d`),
    /// и середины этих дуг на торце.
    fn block_with_round_groove() -> (Part, Vec<DVec3>) {
        let mut part = Part::default();
        let base = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch::new(Plane::FRONT, vec![rect(-1.0, 0.0, 36.0, 36.0)])),
        );
        part.add("Бобышка", FeatureKind::Boss { sketch: base, depth: 30.0, flip: false });
        let plane = Plane { origin: DVec3::new(17.0, -30.0, 18.0), ..Plane::FRONT };
        let groove = path(
            &[
                (-9.0, -18.0, -0.5489458918550271),
                (-9.0, -7.0, -0.44906606431025514),
                (9.0, -7.0, -0.5781474402824104),
                (9.0, -18.0, 0.0),
            ],
            true,
        );
        let cut = part.add("Эскиз", FeatureKind::Sketch(Sketch::new(plane, vec![groove])));
        part.add("Вырез", FeatureKind::Cut { sketch: cut, depth: 10.0, through_all: true, flip: false });
        let body = part.rebuild(None).expect("брусок с пазом");
        let arcs: Vec<DVec3> = RenderMesh::from_shape(&body)
            .edges
            .iter()
            .filter(|e| e.points.len() > 2 && e.points.iter().all(|p| (p.y + 30.0).abs() < 1e-6))
            .map(|e| e.mid)
            .collect();
        assert_eq!(arcs.len(), 3);
        (part, arcs)
    }

    #[test]
    fn fillet_around_groove_is_closed() {
        // При исходном порядке рёбер ядро выворачивает поверхность у средней дуги.
        let _kernel = kernel();
        let (mut part, arcs) = block_with_round_groove();
        let fillet = part.add("Скругление", FeatureKind::Fillet { edges: arcs, radius: 2.0 });
        let body = part.rebuild(None).unwrap();
        assert!(part.feature(fillet).unwrap().error.is_none(), "{:?}", part.feature(fillet).unwrap().error);
        let mesh = RenderMesh::from_shape(&body);
        assert_eq!((mesh.faces.len(), mesh.torn_edges()), (13, 0));
    }

    #[test]
    fn torn_fillet_is_an_error() {
        // Одну среднюю дугу с таким радиусом ядро скругляет с петлёй и разрывами — тело не портим.
        let _kernel = kernel();
        let (mut part, arcs) = block_with_round_groove();
        let middle = *arcs.iter().max_by(|a, b| a.z.total_cmp(&b.z)).unwrap();
        let started = std::time::Instant::now();
        let fillet = part.add("Скругление", FeatureKind::Fillet { edges: vec![middle], radius: 2.0 });
        let body = part.rebuild(None).unwrap();
        println!("torn fillet attempt: {:?}", started.elapsed());
        assert!(part.feature(fillet).unwrap().error.as_deref().unwrap().contains("разрывами"));
        assert_eq!(RenderMesh::from_shape(&body).faces.len(), 10, "тело осталось без скругления");
        // С меньшим радиусом то же ребро скругляется.
        let small = part.add("Скругление", FeatureKind::Fillet { edges: vec![middle], radius: 0.5 });
        let body = part.rebuild(None).unwrap();
        assert!(part.feature(small).unwrap().error.is_none());
        assert_eq!(RenderMesh::from_shape(&body).torn_edges(), 0);
    }

    #[test]
    fn impossible_fillet_is_an_error_not_a_crash() {
        let _kernel = kernel();
        let (mut part, body) = extruded(rect(0.0, 0.0, 40.0, 30.0), 10.0);
        let mesh = RenderMesh::from_shape(&body.unwrap());
        let edges: Vec<DVec3> = mesh.edges.iter().map(|e| e.mid).collect();
        let fillet = part.add("Скругление", FeatureKind::Fillet { edges, radius: 500.0 });
        let body = part.rebuild(None);
        assert!(part.feature(fillet).unwrap().error.is_some());
        assert!(body.is_some(), "тело до скругления сохраняется");

        // Ребро, которого нет в теле.
        let ghost = part.add("Фаска", FeatureKind::Chamfer { edges: vec![DVec3::new(999.0, 0.0, 0.0)], distance: 1.0 });
        part.rebuild(None);
        assert!(part.feature(ghost).unwrap().error.as_deref().unwrap().contains("не найдено"));
    }
}

trait SignumOr {
    fn signum_or(self, zero: f64) -> f64;
}

impl SignumOr for f64 {
    /// Знак числа; для нуля — `zero`.
    fn signum_or(self, zero: f64) -> f64 {
        if self.abs() < 1e-12 { zero } else { self.signum() }
    }
}
