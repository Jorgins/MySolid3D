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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SketchShape {
    /// `x`, `y` — левый нижний угол.
    Rect { x: f64, y: f64, w: f64, h: f64 },
    Circle { cx: f64, cy: f64, r: f64 },
}

impl SketchShape {
    /// Полностью ли `self` содержит `other` (для определения отверстий в контуре).
    fn contains(&self, other: &SketchShape) -> bool {
        use SketchShape::*;
        match (*self, *other) {
            (Rect { x, y, w, h }, Rect { x: x2, y: y2, w: w2, h: h2 }) => {
                x2 > x && y2 > y && x2 + w2 < x + w && y2 + h2 < y + h
            }
            (Rect { x, y, w, h }, Circle { cx, cy, r }) => {
                cx - r > x && cy - r > y && cx + r < x + w && cy + r < y + h
            }
            (Circle { cx, cy, r }, Rect { x, y, w, h }) => {
                [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
                    .iter()
                    .all(|&(px, py)| (px - cx).hypot(py - cy) < r)
            }
            (Circle { cx, cy, r }, Circle { cx: cx2, cy: cy2, r: r2 }) => {
                (cx2 - cx).hypot(cy2 - cy) + r2 < r
            }
        }
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
        }
    }

    /// Расстояние от точки до контура фигуры (в единицах эскиза).
    pub fn outline_distance(&self, p: DVec2) -> f64 {
        match *self {
            SketchShape::Rect { x, y, w, h } => {
                let c = DVec2::new(x + w / 2.0, y + h / 2.0);
                let q = (p - c).abs() - DVec2::new(w / 2.0, h / 2.0);
                let outside = q.max(DVec2::ZERO).length();
                let inside = q.x.max(q.y).min(0.0);
                (outside + inside).abs()
            }
            SketchShape::Circle { cx, cy, r } => ((p - DVec2::new(cx, cy)).length() - r).abs(),
        }
    }

    fn wire(&self, plane: &Plane) -> Result<Wire, String> {
        match *self {
            SketchShape::Rect { x, y, w, h } => {
                if w < MIN_SIZE || h < MIN_SIZE {
                    return Err("прямоугольник нулевого размера".into());
                }
                let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
                    .map(|(px, py)| plane.to_world(DVec2::new(px, py)));
                Wire::from_ordered_points(pts).map_err(|e| e.to_string())
            }
            SketchShape::Circle { cx, cy, r } => {
                if r < MIN_SIZE {
                    return Err("окружность нулевого радиуса".into());
                }
                let edge = Edge::circle(plane.to_world(DVec2::new(cx, cy)), plane.normal(), r);
                Ok(Wire::from_edges([&edge]))
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
            FeatureKind::Sketch(_) => None,
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
        }
    }
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

        for edge in shape.edges() {
            let pts: Vec<DVec3> = edge.approximation_segments().collect();
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
    pub fn pick(&self, origin: DVec3, dir: DVec3) -> Option<usize> {
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
        best.map(|(_, face)| face)
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
}
