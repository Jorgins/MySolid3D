//! Команды для внешнего управления программой (см. docs/API.md): разбор JSON и выполнение над деталью.
//! Всё выполняется в потоке интерфейса — так ядро OpenCascade работает из одного потока,
//! а результат сразу виден в окне, и пользователь может отменить его обычным Ctrl+Z.

use super::*;
use crate::model::{bulge_through, Material, Rgb};
use serde_json::{json, Value};

type CmdResult = Result<Value, String>;

fn num(v: &Value, key: &str) -> Result<f64, String> {
    v.get(key)
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("нужно число в поле «{key}»"))
}

fn opt_num(v: &Value, key: &str) -> Result<Option<f64>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => num(v, key).map(Some),
    }
}

fn opt_bool(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(Value::as_bool)
}

fn index(v: &Value, key: &str) -> Result<usize, String> {
    v.get(key)
        .and_then(Value::as_u64)
        .map(|x| x as usize)
        .ok_or_else(|| format!("нужно целое число в поле «{key}»"))
}

fn id_of(v: &Value, key: &str) -> Result<u64, String> {
    v.get(key).and_then(Value::as_u64).ok_or_else(|| format!("нужен номер элемента в поле «{key}»"))
}

fn indexes(v: &Value, key: &str) -> Result<Option<Vec<usize>>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_u64().map(|i| i as usize).ok_or_else(|| format!("в «{key}» должны быть целые числа")))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("«{key}» — это список номеров")),
    }
}

fn pt2(v: &Value) -> Result<DVec2, String> {
    match v.as_array().map(Vec::as_slice) {
        Some([x, y, ..]) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Ok(DVec2::new(x, y)),
            _ => Err("координаты точки — числа".into()),
        },
        _ => Err("точка — это список [x, y]".into()),
    }
}

fn pt3(v: &Value) -> Result<DVec3, String> {
    match v.as_array().map(Vec::as_slice) {
        Some([x, y, z, ..]) => match (x.as_f64(), y.as_f64(), z.as_f64()) {
            (Some(x), Some(y), Some(z)) if x.is_finite() && y.is_finite() && z.is_finite() => Ok(DVec3::new(x, y, z)),
            _ => Err("координаты точки — числа".into()),
        },
        _ => Err("точка — это список [x, y, z]".into()),
    }
}

fn pt2_of(v: &Value, key: &str) -> Result<DVec2, String> {
    pt2(v.get(key).ok_or_else(|| format!("нужна точка в поле «{key}»"))?)
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

#[allow(dead_code)]
fn j2(p: DVec2) -> Value {
    json!([round6(p.x), round6(p.y)])
}

fn j3(p: DVec3) -> Value {
    json!([round6(p.x), round6(p.y), round6(p.z)])
}

fn parse_color(v: &Value) -> Result<Option<Rgb>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Array(a) if a.len() == 3 => {
            let mut out = [0u8; 3];
            for (slot, x) in out.iter_mut().zip(a) {
                *slot = x.as_u64().filter(|c| *c <= 255).ok_or("цвет — три числа 0…255")? as u8;
            }
            Ok(Some(out))
        }
        Value::String(s) => {
            let h = s.trim().trim_start_matches('#');
            if h.len() != 6 {
                return Err("цвет — [r, g, b] или строка #RRGGBB".into());
            }
            let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| "цвет — строка #RRGGBB".to_string());
            Ok(Some([c(0)?, c(2)?, c(4)?]))
        }
        _ => Err("цвет — [r, g, b] или строка #RRGGBB".into()),
    }
}

fn parse_material(s: &str) -> Result<Material, String> {
    match s.trim().to_lowercase().as_str() {
        "plastic" | "пластик" => Ok(Material::Plastic),
        "metal" | "металл" => Ok(Material::Metal),
        "glass" | "стекло" => Ok(Material::Glass),
        "stone" | "камень" => Ok(Material::Stone),
        _ => Err("материал: plastic, metal, glass или stone".into()),
    }
}

/// Фигура эскиза из описания в JSON.
fn parse_shape(v: &Value) -> Result<SketchShape, String> {
    let kind = v.get("type").and_then(Value::as_str).ok_or("у фигуры нужно поле «type»")?;
    let positive = |x: f64, what: &str| if x > MIN_SIZE { Ok(x) } else { Err(format!("{what} должен быть больше нуля")) };
    // Центр: либо «center»: [x, y], либо «cx», «cy».
    let center = |v: &Value| -> Result<DVec2, String> {
        match v.get("center") {
            Some(c) => pt2(c),
            None => Ok(DVec2::new(num(v, "cx")?, num(v, "cy")?)),
        }
    };
    match kind {
        "rect" => {
            let (w, h) = (positive(num(v, "w")?, "w")?, positive(num(v, "h")?, "h")?);
            let (x, y) = match v.get("center") {
                Some(c) => {
                    let c = pt2(c)?;
                    (c.x - w / 2.0, c.y - h / 2.0)
                }
                None => (num(v, "x")?, num(v, "y")?),
            };
            Ok(SketchShape::Rect { x, y, w, h })
        }
        "circle" => {
            let c = center(v)?;
            let r = match opt_num(v, "r")? {
                Some(r) => r,
                None => num(v, "d")? / 2.0,
            };
            Ok(SketchShape::Circle { cx: c.x, cy: c.y, r: positive(r, "радиус")? })
        }
        "path" | "polygon" | "polyline" => {
            let list = v.get("points").and_then(Value::as_array).ok_or("нужен список точек «points»")?;
            let mut points = Vec::new();
            for p in list {
                let pos = pt2(p)?;
                // Третье число — выпуклость дуги от этой точки к следующей.
                let bulge = p.as_array().and_then(|a| a.get(2)).and_then(Value::as_f64).unwrap_or(0.0);
                points.push(PathPoint { x: pos.x, y: pos.y, bulge, construction: false });
            }
            let closed = opt_bool(v, "closed").unwrap_or(kind != "polyline");
            if points.len() < 2 || (closed && points.len() < 3 && points.iter().all(|p| p.bulge == 0.0)) {
                return Err("в контуре слишком мало точек".into());
            }
            Ok(SketchShape::Path { points, closed })
        }
        "line" => {
            let (a, b) = (pt2_of(v, "from")?, pt2_of(v, "to")?);
            if (a - b).length() < MIN_SIZE {
                return Err("концы линии совпадают".into());
            }
            Ok(SketchShape::Path { points: vec![PathPoint::new(a), PathPoint::new(b)], closed: false })
        }
        "arc" => {
            let (a, b, through) = (pt2_of(v, "from")?, pt2_of(v, "to")?, pt2_of(v, "through")?);
            let bulge = bulge_through(a, b, through);
            Ok(SketchShape::Path {
                points: vec![PathPoint { x: a.x, y: a.y, bulge, construction: false }, PathPoint::new(b)],
                closed: false,
            })
        }
        "regular_polygon" => {
            let c = center(v)?;
            let r = positive(num(v, "r")?, "радиус описанной окружности")?;
            let n = v.get("n").and_then(Value::as_u64).filter(|n| (3..=360).contains(n)).ok_or("число сторон «n» — от 3 до 360")?;
            let start = opt_num(v, "rotation")?.unwrap_or(0.0).to_radians();
            let points = (0..n)
                .map(|i| {
                    let a = start + std::f64::consts::TAU * i as f64 / n as f64;
                    PathPoint::new(c + DVec2::new(a.cos(), a.sin()) * r)
                })
                .collect();
            Ok(SketchShape::Path { points, closed: true })
        }
        "ellipse" => {
            let c = center(v)?;
            let (rx, ry) = (positive(num(v, "rx")?, "rx")?, positive(num(v, "ry")?, "ry")?);
            let points = (0..96)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / 96.0;
                    PathPoint::new(c + DVec2::new(rx * a.cos(), ry * a.sin()))
                })
                .collect();
            Ok(SketchShape::Path { points, closed: true })
        }
        other => Err(format!("неизвестный тип фигуры «{other}» (rect, circle, path, polygon, polyline, line, arc, regular_polygon, ellipse)")),
    }
}

fn shape_json(i: usize, s: &SketchShape) -> Value {
    match s {
        SketchShape::Rect { x, y, w, h } => {
            json!({"index": i, "type": "rect", "x": round6(*x), "y": round6(*y), "w": round6(*w), "h": round6(*h)})
        }
        SketchShape::Circle { cx, cy, r } => {
            json!({"index": i, "type": "circle", "cx": round6(*cx), "cy": round6(*cy), "r": round6(*r)})
        }
        SketchShape::Path { points, closed } => json!({
            "index": i, "type": "path", "closed": closed, "construction": s.is_construction(),
            "points": points.iter().map(|p| json!([round6(p.x), round6(p.y), round6(p.bulge)])).collect::<Vec<_>>(),
        }),
    }
}

fn feature_json(f: &crate::model::Feature) -> Value {
    let mut out = match &f.kind {
        FeatureKind::Sketch(s) => json!({
            "type": "sketch",
            "plane": {"origin": j3(s.plane.origin), "x": j3(s.plane.x), "y": j3(s.plane.y), "normal": j3(s.plane.normal())},
            "shapes": s.shapes.iter().enumerate().map(|(i, sh)| shape_json(i, sh)).collect::<Vec<_>>(),
            "has_image": s.image.is_some(),
        }),
        FeatureKind::Boss { sketch, depth, flip } => json!({"type": "boss", "sketch": sketch, "depth": depth, "flip": flip}),
        FeatureKind::Cut { sketch, depth, through_all, flip } => {
            json!({"type": "cut", "sketch": sketch, "depth": depth, "through_all": through_all, "flip": flip})
        }
        FeatureKind::Fillet { edges, radius } => json!({"type": "fillet", "edges": edges.len(), "radius": radius}),
        FeatureKind::Chamfer { edges, distance } => json!({"type": "chamfer", "edges": edges.len(), "distance": distance}),
    };
    out["id"] = json!(f.id);
    out["name"] = json!(f.name);
    out["error"] = json!(f.error);
    out
}

impl App {
    /// Забирает пришедшие по сети запросы и отвечает на них.
    pub(super) fn api_poll(&mut self, frame: &eframe::Frame) {
        let Some(rx) = &self.api_rx else { return };
        let requests: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for request in requests {
            let answer = self.api_request(frame, &request.body);
            let _ = request.reply.send(answer);
        }
    }

    /// Запуск или остановка сервера (меню «Файл» и ключ командной строки `--api`).
    pub(super) fn api_set_enabled(&mut self, ctx: &egui::Context, on: bool) {
        if !on {
            self.api = None;
            self.api_rx = None;
            self.status = "API выключен".into();
            return;
        }
        if self.api.is_some() {
            return;
        }
        match crate::server::start(self.api_port, ctx.clone()) {
            Ok((server, rx)) => {
                self.status = format!("API включён: http://127.0.0.1:{}/ (инструкция по этому адресу)", server.port);
                self.api = Some(server);
                self.api_rx = Some(rx);
            }
            Err(e) => self.status = format!("API не запущен: {e}"),
        }
    }

    fn api_request(&mut self, frame: &eframe::Frame, body: &str) -> String {
        let value: Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return json!({"ok": false, "error": format!("тело запроса — не JSON: {e}")}).to_string(),
        };
        // Одна команда, список команд или {"commands": [...]}.
        let (commands, batch) = match &value {
            Value::Array(list) => (list.clone(), true),
            Value::Object(o) if o.contains_key("commands") => {
                (o["commands"].as_array().cloned().unwrap_or_default(), true)
            }
            other => (vec![other.clone()], false),
        };
        // API берёт управление на себя: открытый эскиз закрывается, остальное остаётся как было.
        if self.sketch.is_some() {
            self.exit_sketch();
        }
        let before = self.mesh.bounds;
        let mut results = Vec::new();
        let mut all_ok = true;
        for cmd in &commands {
            match self.api_command(frame, cmd) {
                Ok(mut v) => {
                    v["ok"] = json!(true);
                    results.push(v);
                }
                Err(e) => {
                    all_ok = false;
                    results.push(json!({"ok": false, "error": e}));
                    // Дальше по пачке не идём: следующие команды обычно опираются на эту.
                    break;
                }
            }
        }
        if self.mesh.bounds != before {
            self.need_fit = true;
        }
        if !batch {
            return results.into_iter().next().unwrap_or(json!({"ok": false, "error": "пустая команда"})).to_string();
        }
        let done = results.len();
        json!({"ok": all_ok, "executed": done, "total": commands.len(), "results": results}).to_string()
    }

    /// Строит деталь заново целиком, чтобы сразу ответить про ошибки, границы, грани и рёбра.
    fn api_sync(&mut self, frame: &eframe::Frame) {
        self.shape = self.part.rebuild(None);
        self.mesh = self.part.mesh(None);
        if let Some(gl) = frame.gl() {
            self.renderer.lock().unwrap().upload(gl, &self.mesh);
        }
        self.colors_dirty = true;
        self.hover_face = None;
        self.hover_edge = None;
        if matches!(self.selection, Selection::Face(_) | Selection::Faces(_) | Selection::Edges(_)) {
            self.selection = Selection::None;
        }
        self.building = false;
        self.dirty = false;
    }

    fn body_json(&self) -> Value {
        match self.mesh.bounds {
            Some((lo, hi)) => json!({
                "has_body": true, "min": j3(lo), "max": j3(hi), "size": j3(hi - lo),
                "faces": self.mesh.faces.len(), "edges": self.mesh.edges.len(),
            }),
            None => json!({"has_body": false}),
        }
    }

    fn api_sketch_mut(&mut self, v: &Value) -> Result<&mut crate::model::Sketch, String> {
        let id = id_of(v, "sketch")?;
        self.part.sketch_mut(id).ok_or_else(|| format!("эскиз {id} не найден"))
    }

    /// Номера рёбер для скругления или фаски: «edges» (номера рёбер), «faces» (все рёбра граней)
    /// или «all»; ребро запоминается точкой на нём.
    fn api_edge_mids(&self, v: &Value) -> Result<Vec<DVec3>, String> {
        let mut mids: Vec<DVec3> = Vec::new();
        let mut add = |p: DVec3| {
            if !mids.iter().any(|m| (*m - p).length() < 1e-4) {
                mids.push(p);
            }
        };
        if opt_bool(v, "all") == Some(true) {
            self.mesh.edges.iter().for_each(|e| add(e.mid));
        }
        for i in indexes(v, "edges")?.unwrap_or_default() {
            add(self.mesh.edges.get(i).ok_or_else(|| format!("ребра {i} нет (всего {})", self.mesh.edges.len()))?.mid);
        }
        for i in indexes(v, "faces")?.unwrap_or_default() {
            let face = self.mesh.faces.get(i).ok_or_else(|| format!("грани {i} нет (всего {})", self.mesh.faces.len()))?;
            face.edges.iter().for_each(|e| add(*e));
        }
        if let Some(Value::Array(list)) = v.get("edge_mids") {
            for p in list {
                add(pt3(p)?);
            }
        }
        if mids.is_empty() {
            return Err("не указаны рёбра: передайте «edges» (номера из команды edges), «faces» или «all»".into());
        }
        Ok(mids)
    }

    fn api_feature_result(&self, id: u64) -> Value {
        let f = self.part.feature(id);
        json!({"id": id, "error": f.and_then(|f| f.error.clone()), "body": self.body_json()})
    }

    fn api_command(&mut self, frame: &eframe::Frame, v: &Value) -> CmdResult {
        let cmd = v.get("cmd").and_then(Value::as_str).ok_or("у команды нужно поле «cmd»")?;
        match cmd {
            "help" => Ok(json!({"docs": "GET http://127.0.0.1:<порт>/ — полная инструкция", "commands": [
                "state", "new", "open", "save", "export",
                "add_sketch", "sketch_add", "sketch_remove_shape", "sketch_transform", "sketch_import_svg", "sketch_construction",
                "boss", "cut", "fillet", "chamfer", "set_feature", "delete_feature",
                "faces", "edges", "color", "material", "undo", "redo", "view"]})),
            "state" => Ok(json!({
                "file": self.file.as_ref().map(|p| p.display().to_string()),
                "features": self.part.features.iter().map(feature_json).collect::<Vec<_>>(),
                "body": self.body_json(),
                "color": self.part.color,
                "material": self.part.material.name(),
            })),
            "new" => {
                self.new_part();
                self.api_sync(frame);
                Ok(json!({}))
            }
            "open" => {
                let path = v.get("path").and_then(Value::as_str).ok_or("нужен путь «path»")?;
                self.load(Path::new(path));
                if self.status.starts_with("Не удалось") {
                    return Err(self.status.clone());
                }
                self.api_sync(frame);
                Ok(json!({"features": self.part.features.len(), "body": self.body_json()}))
            }
            "save" => {
                match v.get("path").and_then(Value::as_str) {
                    Some(p) => self.write_to(Path::new(p)),
                    None if self.file.is_some() => self.save(),
                    None => return Err("файл ещё не задан: передайте «path»".into()),
                }
                if self.status.starts_with("Не удалось") {
                    return Err(self.status.clone());
                }
                Ok(json!({"file": self.file.as_ref().map(|p| p.display().to_string())}))
            }
            "export" => {
                let path = v.get("path").and_then(Value::as_str).ok_or("нужен путь «path» (.stl или .step)")?;
                let lower = path.to_lowercase();
                let step = match v.get("format").and_then(Value::as_str) {
                    Some("step") => true,
                    Some("stl") => false,
                    Some(other) => return Err(format!("формат «{other}» не поддерживается: stl или step")),
                    None if lower.ends_with(".step") || lower.ends_with(".stp") => true,
                    None if lower.ends_with(".stl") => false,
                    None => return Err("не удалось понять формат: укажите расширение .stl/.step или «format»".into()),
                };
                let shape = self.shape.clone().ok_or("нет тела для экспорта")?;
                let result = if step { shape.write_step(path) } else { shape.write_stl_with_tolerance(path, 0.01) };
                result.map_err(|e| format!("ошибка экспорта: {e}"))?;
                Ok(json!({"path": path}))
            }

            // ----- эскизы -----
            "add_sketch" => {
                let plane = match (v.get("face"), v.get("plane")) {
                    (Some(face), _) => {
                        let i = face.as_u64().ok_or("«face» — номер грани")? as usize;
                        let f = self.mesh.faces.get(i).ok_or_else(|| format!("грани {i} нет"))?;
                        if !f.planar {
                            return Err("эскиз можно поставить только на плоскую грань".into());
                        }
                        Plane::from_normal(f.origin, f.normal)
                    }
                    (None, Some(Value::String(name))) => match name.to_lowercase().as_str() {
                        "top" | "xy" | "сверху" => Plane::TOP,
                        "front" | "xz" | "спереди" => Plane::FRONT,
                        "right" | "yz" | "справа" => Plane::RIGHT,
                        other => return Err(format!("плоскость «{other}»: top, front, right или объект {{origin, normal}}")),
                    },
                    (None, Some(obj @ Value::Object(_))) => {
                        let origin = pt3(obj.get("origin").ok_or("у плоскости нужен «origin»")?)?;
                        let normal = pt3(obj.get("normal").ok_or("у плоскости нужна «normal»")?)?;
                        if normal.length() < 1e-9 {
                            return Err("нормаль плоскости нулевая".into());
                        }
                        Plane::from_normal(origin, normal)
                    }
                    _ => return Err("укажите «plane» (top, front, right или объект) либо «face»".into()),
                };
                let plane = match opt_num(v, "offset")? {
                    Some(d) => Plane { origin: plane.origin + plane.normal() * d, ..plane },
                    None => plane,
                };
                let id = self.part.add("Эскиз", FeatureKind::Sketch(crate::model::Sketch::new(plane, vec![])));
                if let Some(name) = v.get("name").and_then(Value::as_str) {
                    if let Some(f) = self.part.feature_mut(id) {
                        f.name = name.to_string();
                    }
                }
                self.api_sync(frame);
                Ok(json!({"id": id}))
            }
            "sketch_add" => {
                let shapes: Vec<SketchShape> = match (v.get("shape"), v.get("shapes")) {
                    (Some(s), _) => vec![parse_shape(s)?],
                    (None, Some(Value::Array(list))) => list.iter().map(parse_shape).collect::<Result<_, _>>()?,
                    _ => return Err("нужна фигура «shape» или список «shapes»".into()),
                };
                let sketch = self.api_sketch_mut(v)?;
                let first = sketch.shapes.len();
                sketch.shapes.extend(shapes);
                let added: Vec<usize> = (first..sketch.shapes.len()).collect();
                self.api_sync(frame);
                Ok(json!({"indexes": added}))
            }
            "sketch_remove_shape" => {
                let i = index(v, "index")?;
                let sketch = self.api_sketch_mut(v)?;
                if i >= sketch.shapes.len() {
                    return Err(format!("фигуры {i} нет (всего {})", sketch.shapes.len()));
                }
                sketch.remove_shape(i);
                self.api_sync(frame);
                Ok(json!({}))
            }
            "sketch_transform" => {
                let i = index(v, "index")?;
                let op = v.get("op").and_then(Value::as_str).ok_or("нужно действие «op»: move, rotate, scale")?;
                let pivot = match v.get("about") {
                    Some(p) => Some(pt2(p)?),
                    None => None,
                };
                let sketch = self.api_sketch_mut(v)?;
                let shape = sketch.shapes.get_mut(i).ok_or_else(|| format!("фигуры {i} нет"))?;
                let pivot = pivot.unwrap_or_else(|| shape.center());
                match op {
                    "move" => shape.translate(DVec2::new(opt_num(v, "dx")?.unwrap_or(0.0), opt_num(v, "dy")?.unwrap_or(0.0))),
                    "rotate" => shape.rotate(pivot, num(v, "angle")?.to_radians()),
                    "scale" => shape.scale(pivot, num(v, "factor")?)?,
                    other => return Err(format!("действие «{other}»: move, rotate или scale")),
                }
                self.api_sync(frame);
                Ok(json!({}))
            }
            "sketch_construction" => {
                let (shape, seg) = (index(v, "shape")?, index(v, "segment")?);
                let on = opt_bool(v, "on").unwrap_or(true);
                self.api_sketch_mut(v)?.set_construction(shape, seg, on)?;
                self.api_sync(frame);
                Ok(json!({}))
            }
            "sketch_import_svg" => {
                let text = match (v.get("svg").and_then(Value::as_str), v.get("path").and_then(Value::as_str)) {
                    (Some(t), _) => t.to_string(),
                    (None, Some(p)) => std::fs::read_to_string(p).map_err(|e| format!("не удалось прочитать файл: {e}"))?,
                    _ => return Err("нужен текст «svg» или путь «path»".into()),
                };
                let shapes = crate::svg::parse(&text)?;
                let sketch = self.api_sketch_mut(v)?;
                let first = sketch.shapes.len();
                sketch.shapes.extend(shapes);
                let added: Vec<usize> = (first..sketch.shapes.len()).collect();
                self.api_sync(frame);
                Ok(json!({"indexes": added}))
            }

            // ----- операции -----
            "boss" | "cut" => {
                let sketch = id_of(v, "sketch")?;
                if self.part.sketch(sketch).is_none() {
                    return Err(format!("эскиз {sketch} не найден"));
                }
                let flip = opt_bool(v, "flip").unwrap_or(false);
                let through_all = opt_bool(v, "through_all").unwrap_or(false);
                let depth = match opt_num(v, "depth")? {
                    Some(d) => d,
                    None if cmd == "cut" && through_all => self.last_depth,
                    None => return Err("нужна глубина «depth» (мм)".into()),
                };
                if depth < MIN_SIZE {
                    return Err("глубина должна быть больше нуля".into());
                }
                let id = if cmd == "boss" {
                    self.part.add("Бобышка-Вытянуть", FeatureKind::Boss { sketch, depth, flip })
                } else {
                    self.part.add("Вырез-Вытянуть", FeatureKind::Cut { sketch, depth, through_all, flip })
                };
                if let Some(list) = indexes(v, "regions")? {
                    if let Some(sk) = self.part.sketch_mut(sketch) {
                        sk.region_sel.push((id, list));
                    }
                }
                if let Some(name) = v.get("name").and_then(Value::as_str) {
                    if let Some(f) = self.part.feature_mut(id) {
                        f.name = name.to_string();
                    }
                }
                self.last_depth = depth;
                self.api_sync(frame);
                Ok(self.api_feature_result(id))
            }
            "fillet" | "chamfer" => {
                let mids = self.api_edge_mids(v)?;
                let id = if cmd == "fillet" {
                    let radius = num(v, "radius")?;
                    self.part.add("Скругление", FeatureKind::Fillet { edges: mids, radius })
                } else {
                    let distance = match opt_num(v, "distance")? {
                        Some(d) => d,
                        None => num(v, "size")?,
                    };
                    self.part.add("Фаска", FeatureKind::Chamfer { edges: mids, distance })
                };
                self.api_sync(frame);
                Ok(self.api_feature_result(id))
            }
            "set_feature" => {
                let id = id_of(v, "id")?;
                let sketch_of = self.part.feature(id).and_then(|f| f.sketch_ref());
                if let (Some(list), Some(sk)) = (indexes(v, "regions")?, sketch_of) {
                    if let Some(s) = self.part.sketch_mut(sk) {
                        s.region_sel.retain(|(f, _)| *f != id);
                        s.region_sel.push((id, list));
                    }
                }
                let depth = opt_num(v, "depth")?;
                let (radius, distance) = (opt_num(v, "radius")?, opt_num(v, "distance")?);
                let flip_new = opt_bool(v, "flip");
                let through = opt_bool(v, "through_all");
                let feature = self.part.feature_mut(id).ok_or_else(|| format!("элемент {id} не найден"))?;
                if let Some(name) = v.get("name").and_then(Value::as_str) {
                    feature.name = name.to_string();
                }
                match &mut feature.kind {
                    FeatureKind::Boss { depth: d, flip, .. } => {
                        *d = depth.unwrap_or(*d);
                        *flip = flip_new.unwrap_or(*flip);
                    }
                    FeatureKind::Cut { depth: d, flip, through_all, .. } => {
                        *d = depth.unwrap_or(*d);
                        *flip = flip_new.unwrap_or(*flip);
                        *through_all = through.unwrap_or(*through_all);
                    }
                    FeatureKind::Fillet { radius: r, .. } => *r = radius.unwrap_or(*r),
                    FeatureKind::Chamfer { distance: d, .. } => *d = distance.or(radius).unwrap_or(*d),
                    FeatureKind::Sketch(_) => {}
                }
                self.api_sync(frame);
                Ok(self.api_feature_result(id))
            }
            "delete_feature" => {
                let id = id_of(v, "id")?;
                if self.part.feature(id).is_none() {
                    return Err(format!("элемент {id} не найден"));
                }
                // Вместе с эскизом удаляются построенные на нём операции.
                self.part.remove(id);
                self.selection = Selection::None;
                self.api_sync(frame);
                Ok(json!({"body": self.body_json()}))
            }

            // ----- осмотр тела -----
            "faces" => {
                let mut area = vec![0.0f64; self.mesh.faces.len()];
                for tri in self.mesh.indices.chunks_exact(3) {
                    let at = |k: u32| {
                        let o = k as usize * 7;
                        (DVec3::new(self.mesh.vertices[o] as f64, self.mesh.vertices[o + 1] as f64, self.mesh.vertices[o + 2] as f64), self.mesh.vertices[o + 6] as usize)
                    };
                    let ((a, face), (b, _), (c, _)) = (at(tri[0]), at(tri[1]), at(tri[2]));
                    if let Some(slot) = area.get_mut(face) {
                        *slot += (b - a).cross(c - a).length() / 2.0;
                    }
                }
                let faces: Vec<Value> = self
                    .mesh
                    .faces
                    .iter()
                    .enumerate()
                    .map(|(i, f)| json!({"index": i, "planar": f.planar, "center": j3(f.origin), "normal": j3(f.normal), "area": round6(area[i]), "edges": f.edges.len()}))
                    .collect();
                Ok(json!({"faces": faces}))
            }
            "edges" => {
                let edges: Vec<Value> = self
                    .mesh
                    .edges
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let length: f64 = e.points.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                        let (a, b) = (e.points.first().copied().unwrap_or(DVec3::ZERO), e.points.last().copied().unwrap_or(DVec3::ZERO));
                        let chord = (b - a).length();
                        json!({
                            "index": i, "mid": j3(e.mid), "start": j3(a), "end": j3(b),
                            "length": round6(length), "straight": (length - chord).abs() < 1e-6 * length.max(1.0),
                            "closed": chord < 1e-6,
                        })
                    })
                    .collect();
                Ok(json!({"edges": edges}))
            }

            // ----- внешний вид -----
            "color" => {
                let color = parse_color(v.get("color").unwrap_or(&Value::Null))?;
                match indexes(v, "faces")? {
                    Some(list) => {
                        for i in list {
                            let f = self.mesh.faces.get(i).ok_or_else(|| format!("грани {i} нет"))?;
                            self.part.set_face_color(f.origin, f.normal, color);
                        }
                    }
                    None => self.part.paint_part_color(color),
                }
                self.colors_dirty = true;
                Ok(json!({}))
            }
            "material" => {
                let m = parse_material(v.get("material").and_then(Value::as_str).ok_or("нужен «material»")?)?;
                match indexes(v, "faces")? {
                    Some(list) => {
                        let own = (m != self.part.material).then_some(m);
                        for i in list {
                            let f = self.mesh.faces.get(i).ok_or_else(|| format!("грани {i} нет"))?;
                            self.part.set_face_material(f.origin, f.normal, own);
                        }
                    }
                    None => self.part.paint_part_material(m),
                }
                self.colors_dirty = true;
                Ok(json!({}))
            }

            // ----- прочее -----
            "undo" | "redo" => {
                if cmd == "undo" { self.undo() } else { self.redo() }
                self.api_sync(frame);
                Ok(json!({"body": self.body_json()}))
            }
            "view" => {
                match v.get("name").and_then(Value::as_str).unwrap_or("iso") {
                    "iso" => self.cam.set_iso(),
                    "front" => self.cam.look_at_plane(&Plane::FRONT),
                    "top" => self.cam.look_at_plane(&Plane::TOP),
                    "right" => self.cam.look_at_plane(&Plane::RIGHT),
                    other => return Err(format!("вид «{other}»: iso, front, top, right")),
                }
                self.need_fit = true;
                Ok(json!({}))
            }
            other => Err(format!("неизвестная команда «{other}» (список — команда help)")),
        }
    }
}
