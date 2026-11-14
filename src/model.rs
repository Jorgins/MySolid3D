//! Параметрическая модель детали: дерево построения и его пересчёт в тело.

use glam::{DVec2, DVec3};
use opencascade::primitives::{Edge, Face, Shape, Wire};
use serde::{Deserialize, Serialize};

pub const MIN_SIZE: f64 = 0.01;
const THROUGH_ALL: f64 = 10_000.0;
const MESH_TOLERANCE: f64 = 0.02;

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
}

impl PathPoint {
    pub fn new(p: DVec2) -> Self {
        Self { x: p.x, y: p.y, bulge: 0.0 }
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
}

impl Sketch {
    /// Тела-призмы из контуров эскиза; вложенные контуры становятся отверстиями.
    fn prisms(&self, dir: DVec3) -> Result<Vec<Shape>, String> {
        if self.shapes.is_empty() {
            return Err("эскиз пуст".into());
        }
        if self.shapes.iter().any(|s| !s.is_closed()) {
            return Err("в эскизе есть незамкнутый контур — завершите его щелчком по первой точке".into());
        }
        let n = self.shapes.len();
        let depth: Vec<usize> = (0..n)
            .map(|i| (0..n).filter(|&j| j != i && self.shapes[j].contains(&self.shapes[i])).count())
            .collect();
        let dir_unit = dir.normalize();
        // Отверстия вытягиваем с запасом в обе стороны, чтобы булева операция была устойчивой.
        let hole_plane = self.plane.shifted(-dir_unit.dot(self.plane.normal()));
        let hole_dir = dir + dir_unit * 2.0;

        let mut out = Vec::new();
        for i in (0..n).filter(|&i| depth[i] % 2 == 0) {
            let face = Face::from_wire(&self.shapes[i].wire(&self.plane)?);
            let mut solid: Shape = face.extrude(dir).into();
            for k in 0..n {
                if depth[k] == depth[i] + 1 && self.shapes[i].contains(&self.shapes[k]) {
                    let hole_face = Face::from_wire(&self.shapes[k].wire(&hole_plane)?);
                    let hole: Shape = hole_face.extrude(hole_dir).into();
                    solid = solid.subtract(&hole).shape;
                }
            }
            out.push(solid);
        }
        Ok(out)
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

impl Feature {
    pub fn sketch_ref(&self) -> Option<u64> {
        match self.kind {
            FeatureKind::Boss { sketch, .. } | FeatureKind::Cut { sketch, .. } => Some(sketch),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Part {
    pub features: Vec<Feature>,
    next_id: u64,
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
    }

    /// Пересчёт дерева сверху вниз. `stop_before` — «откат» перед указанным элементом
    /// (используется при редактировании эскиза).
    pub fn rebuild(&mut self, stop_before: Option<u64>) -> Option<Shape> {
        let mut body: Option<Shape> = None;
        let end = stop_before
            .and_then(|id| self.features.iter().position(|f| f.id == id))
            .unwrap_or(self.features.len());

        for i in 0..end {
            let result = self.apply(i, body.as_ref());
            let feature = &mut self.features[i];
            match result {
                Ok(Some(new_body)) => {
                    body = Some(new_body);
                    feature.error = None;
                }
                Ok(None) => feature.error = None,
                Err(e) => feature.error = Some(e),
            }
        }
        body
    }

    fn apply(&self, index: usize, body: Option<&Shape>) -> Result<Option<Shape>, String> {
        let find_sketch = |id: u64| {
            self.features[..index]
                .iter()
                .find_map(|f| match &f.kind {
                    FeatureKind::Sketch(s) if f.id == id => Some(s),
                    _ => None,
                })
                .ok_or_else(|| "эскиз не найден".to_string())
        };
        match &self.features[index].kind {
            FeatureKind::Sketch(_) => Ok(None),
            FeatureKind::Boss { sketch, depth, flip } => {
                let sketch = find_sketch(*sketch)?;
                if *depth < MIN_SIZE {
                    return Err("глубина должна быть больше нуля".into());
                }
                let sign = if *flip { -1.0 } else { 1.0 };
                let mut result: Option<Shape> = None;
                for prism in sketch.prisms(sketch.plane.normal() * *depth * sign)? {
                    result = Some(match result.as_ref().or(body) {
                        Some(b) => b.union(&prism).shape,
                        None => prism,
                    });
                }
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
                let mut result: Option<Shape> = None;
                for prism in sketch.prisms(sketch.plane.normal() * depth * sign)? {
                    result = Some(result.as_ref().unwrap_or(body).subtract(&prism).shape);
                }
                Ok(result.map(|s| s.clean()))
            }
            FeatureKind::Fillet { edges, radius } => {
                let body = body.ok_or("нет тела для скругления")?;
                if *radius < MIN_SIZE {
                    return Err("радиус должен быть больше нуля".into());
                }
                let found = find_edges(body, edges)?;
                body.try_fillet_edges(*radius, found.iter())
                    .map(Some)
                    .ok_or_else(|| "не удалось построить скругление — попробуйте меньший радиус".to_string())
            }
            FeatureKind::Chamfer { edges, distance } => {
                let body = body.ok_or("нет тела для фаски")?;
                if *distance < MIN_SIZE {
                    return Err("размер фаски должен быть больше нуля".into());
                }
                let found = find_edges(body, edges)?;
                body.try_chamfer_edges(*distance, found.iter())
                    .map(Some)
                    .ok_or_else(|| "не удалось построить фаску — попробуйте меньший размер".to_string())
            }
        }
    }
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

/// Находит рёбра тела по запомненным точкам на них.
fn find_edges(body: &Shape, refs: &[DVec3]) -> Result<Vec<Edge>, String> {
    if refs.is_empty() {
        return Err("не выбрано ни одного ребра".into());
    }
    let mut edges: Vec<(Edge, Vec<DVec3>)> =
        body.edges().map(|e| { let pts = e.approximation_segments().collect(); (e, pts) }).collect();
    let mut found = Vec::new();
    for r in refs {
        let best = edges
            .iter()
            .enumerate()
            .map(|(i, (_, pts))| (i, polyline_distance(pts, *r)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, d)| *d < 0.01);
        match best {
            Some((i, _)) => found.push(edges.swap_remove(i).0),
            None => return Err("ребро не найдено — геометрия изменилась, выберите рёбра заново".into()),
        }
    }
    Ok(found)
}

pub struct EdgeInfo {
    pub points: Vec<DVec3>,
    /// Точка на середине ребра — по ней ребро запоминается в операциях.
    pub mid: DVec3,
}

#[derive(Clone, Copy)]
pub struct FaceInfo {
    pub planar: bool,
    pub origin: DVec3,
    pub normal: DVec3,
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
            out.faces.push(FaceInfo { planar, origin: face.center_of_mass(), normal: first });

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

    fn rect(x: f64, y: f64, w: f64, h: f64) -> SketchShape {
        SketchShape::Rect { x, y, w, h }
    }

    #[test]
    fn boss_then_cut() {
        let mut part = Part::default();
        let base = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch { plane: Plane::TOP, shapes: vec![rect(0.0, 0.0, 40.0, 30.0)] }),
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
            FeatureKind::Sketch(Sketch { plane, shapes: vec![SketchShape::Circle { cx: 0.0, cy: 0.0, r: 5.0 }] }),
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
        let mut part = Part::default();
        let shapes = vec![rect(0.0, 0.0, 20.0, 20.0), SketchShape::Circle { cx: 10.0, cy: 10.0, r: 4.0 }];
        let s = part.add("Эскиз", FeatureKind::Sketch(Sketch { plane: Plane::FRONT, shapes }));
        part.add("Бобышка", FeatureKind::Boss { sketch: s, depth: 5.0, flip: false });
        let mesh = RenderMesh::from_shape(&part.rebuild(None).unwrap());
        assert_eq!(mesh.faces.len(), 7);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let mut part = Part::default();
        let s = part.add(
            "Эскиз",
            FeatureKind::Sketch(Sketch { plane: Plane::TOP, shapes: vec![rect(0.0, 0.0, 5.0, 5.0)] }),
        );
        part.add("Вырез", FeatureKind::Cut { sketch: s, depth: 5.0, through_all: false, flip: false });
        assert!(part.rebuild(None).is_none());
        assert!(part.features[1].error.is_some());
    }

    fn path(points: &[(f64, f64, f64)], closed: bool) -> SketchShape {
        SketchShape::Path {
            points: points.iter().map(|&(x, y, bulge)| PathPoint { x, y, bulge }).collect(),
            closed,
        }
    }

    fn extruded(shape: SketchShape, depth: f64) -> (Part, Option<Shape>) {
        let mut part = Part::default();
        let s = part.add("Эскиз", FeatureKind::Sketch(Sketch { plane: Plane::TOP, shapes: vec![shape] }));
        part.add("Бобышка", FeatureKind::Boss { sketch: s, depth, flip: false });
        let body = part.rebuild(None);
        (part, body)
    }

    #[test]
    fn l_shaped_contour() {
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

    #[test]
    fn bad_contours_are_errors() {
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
    fn impossible_fillet_is_an_error_not_a_crash() {
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
