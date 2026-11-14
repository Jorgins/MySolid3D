//! Интерфейс: панель инструментов, дерево построения, свойства и 3D-вид.

use crate::camera::Camera;
use crate::model::{
    arc_radius, bulge_through, segment_points, FeatureKind, Part, PathPoint, Plane, RenderMesh, Sketch,
    SketchShape, MIN_SIZE,
};
use crate::render::{DrawParams, Renderer};
use eframe::egui::{
    self, Align2, Color32, FontId, Painter, PointerButton, Pos2, Rect, Response, Stroke, Vec2,
};
use eframe::egui_glow;
use glam::{DVec2, DVec3};
use opencascade::primitives::Shape;
use std::sync::{Arc, Mutex};

const PLANE_NAMES: [&str; 3] = ["Сверху", "Спереди", "Справа"];
const BASE_PLANES: [Plane; 3] = [Plane::TOP, Plane::FRONT, Plane::RIGHT];
const ERROR_COLOR: Color32 = Color32::from_rgb(240, 100, 90);
const SKETCH_COLOR: Color32 = Color32::from_rgb(120, 190, 255);
const SELECT_COLOR: Color32 = Color32::from_rgb(255, 185, 70);

#[derive(Clone, PartialEq)]
enum Selection {
    None,
    Plane(usize),
    Face(usize),
    /// Рёбра тела (индексы в `RenderMesh::edges`).
    Edges(Vec<usize>),
    Feature(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Select,
    Line,
    Arc,
    Rect,
    Circle,
}

#[derive(Clone, Copy)]
enum DimKind {
    Width,
    Height,
    Diameter,
}

/// Состояние редактирования эскиза.
struct SketchEdit {
    id: u64,
    tool: Tool,
    drag_start: Option<DVec2>,
    move_last: Option<DVec2>,
    selected: Option<usize>,
    /// Контур, который сейчас рисуется инструментами «Линия» / «Дуга».
    chain: Option<usize>,
    /// Конечная точка дуги, для которой ещё не указана точка на дуге.
    arc_end: Option<DVec2>,
    /// Перетаскиваемая точка контура: (фигура, точка).
    drag_vertex: Option<(usize, usize)>,
}

impl SketchEdit {
    fn new(id: u64) -> Self {
        Self {
            id,
            tool: Tool::Line,
            drag_start: None,
            move_last: None,
            selected: None,
            chain: None,
            arc_end: None,
            drag_vertex: None,
        }
    }

    fn stop_chain(&mut self) {
        self.chain = None;
        self.arc_end = None;
    }
}

struct DimLabel {
    rect: Rect,
    shape: usize,
    kind: DimKind,
}

struct DimEdit {
    shape: usize,
    kind: DimKind,
    text: String,
    /// Текущее значение — показывается подсказкой, пока поле пустое.
    hint: String,
    pos: Pos2,
    focus: bool,
}

pub struct App {
    part: Part,
    shape: Option<Shape>,
    mesh: RenderMesh,
    dirty: bool,
    cam: Camera,
    renderer: Arc<Mutex<Renderer>>,
    selection: Selection,
    hover_face: Option<usize>,
    hover_edge: Option<usize>,
    sketch: Option<SketchEdit>,
    dim_edit: Option<DimEdit>,
    snap: bool,
    grid_step: f64,
    status: String,
    need_fit: bool,
}

fn fmt_mm(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn draw_shape(painter: &Painter, cam: &Camera, rect: Rect, plane: &Plane, s: &SketchShape, stroke: Stroke) {
    let pts: Vec<Pos2> = s.outline().iter().map(|p| cam.project(rect, plane.to_world(*p))).collect();
    painter.line(pts, stroke);
}

/// Расстояние в пикселях от точки до отрезка на экране.
fn screen_segment_distance(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    let d = b - a;
    let len2 = d.length_sq();
    let t = if len2 < 1e-6 { 0.0 } else { ((p - a).dot(d) / len2).clamp(0.0, 1.0) };
    (a + d * t - p).length()
}

fn point_in_quad(q: &[Pos2; 4], p: Pos2) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign != 0.0 && cross.signum() != sign {
            return false;
        }
        sign = cross.signum();
    }
    sign != 0.0
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let gl = cc.gl.as_ref().ok_or("не удалось получить контекст OpenGL")?;
        let renderer = Renderer::new(gl)?;
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            part: Part::default(),
            shape: None,
            mesh: RenderMesh::default(),
            dirty: false,
            cam: Camera::default(),
            renderer: Arc::new(Mutex::new(renderer)),
            selection: Selection::None,
            hover_face: None,
            hover_edge: None,
            sketch: None,
            dim_edit: None,
            snap: true,
            grid_step: 1.0,
            status: String::new(),
            need_fit: false,
        };
        // Файл детали можно передать аргументом командной строки (в т.ч. «Открыть с помощью»).
        if let Some(path) = std::env::args_os().nth(1) {
            app.load(std::path::Path::new(&path));
        }
        Ok(app)
    }

    fn rebuild(&mut self, frame: &eframe::Frame) {
        let stop = self.sketch.as_ref().map(|e| e.id);
        self.shape = self.part.rebuild(stop);
        self.mesh = self.shape.as_ref().map(RenderMesh::from_shape).unwrap_or_default();
        if let Some(gl) = frame.gl() {
            self.renderer.lock().unwrap().upload(gl, &self.mesh);
        }
        self.hover_face = None;
        self.hover_edge = None;
        // Номера граней и рёбер после пересчёта другие.
        if matches!(self.selection, Selection::Face(_) | Selection::Edges(_)) {
            self.selection = Selection::None;
        }
        self.dirty = false;
    }

    // ---------- действия ----------

    fn start_sketch(&mut self) {
        let plane = match self.selection {
            Selection::Plane(i) => BASE_PLANES[i],
            Selection::Face(i) => match self.mesh.faces.get(i) {
                Some(f) if f.planar => Plane::from_normal(f.origin, f.normal),
                _ => {
                    self.status = "Эскиз можно создать только на плоской грани".into();
                    return;
                }
            },
            _ => {
                self.status = "Сначала выберите плоскость (в дереве) или плоскую грань детали".into();
                return;
            }
        };
        let id = self.part.add("Эскиз", FeatureKind::Sketch(Sketch { plane, shapes: vec![] }));
        self.enter_sketch(id);
    }

    fn enter_sketch(&mut self, id: u64) {
        let Some(plane) = self.part.sketch(id).map(|s| s.plane) else { return };
        self.cam.look_at_plane(&plane);
        self.sketch = Some(SketchEdit::new(id));
        self.selection = Selection::Feature(id);
        self.status.clear();
        self.dirty = true;
    }

    fn exit_sketch(&mut self) {
        // Контуры из одной точки (начали и бросили) не сохраняем.
        if let Some(sketch) = self.sketch.as_ref().and_then(|e| self.part.sketch_mut(e.id)) {
            sketch.shapes.retain(|s| !matches!(s, SketchShape::Path { points, .. } if points.len() < 2));
            if sketch.shapes.iter().any(|s| !s.is_closed()) {
                self.status = "В эскизе остался незамкнутый контур — вытянуть его не получится".into();
            }
        }
        self.sketch = None;
        self.dim_edit = None;
        self.cam.set_iso();
        self.need_fit = true;
        self.dirty = true;
    }

    fn add_extrude(&mut self, cut: bool) {
        let selected = match self.selection {
            Selection::Feature(id) if self.part.sketch(id).is_some() => Some(id),
            _ => None,
        };
        let sketches = || self.part.features.iter().rev().filter(|f| matches!(f.kind, FeatureKind::Sketch(_)));
        let sketch = selected
            .or_else(|| sketches().find(|f| !self.part.is_sketch_used(f.id)).map(|f| f.id))
            .or_else(|| sketches().next().map(|f| f.id));
        let Some(sketch) = sketch else {
            self.status = "Сначала создайте эскиз".into();
            return;
        };
        let id = if cut {
            self.part.add(
                "Вырез-Вытянуть",
                FeatureKind::Cut { sketch, depth: 10.0, through_all: false, flip: false },
            )
        } else {
            self.part.add("Бобышка-Вытянуть", FeatureKind::Boss { sketch, depth: 10.0, flip: false })
        };
        self.selection = Selection::Feature(id);
        self.status.clear();
        self.dirty = true;
    }

    /// Скругление или фаска на выбранных рёбрах.
    fn add_edge_feature(&mut self, chamfer: bool) {
        let edges: Vec<DVec3> = match &self.selection {
            Selection::Edges(list) => list.iter().filter_map(|&i| self.mesh.edges.get(i)).map(|e| e.mid).collect(),
            _ => vec![],
        };
        if edges.is_empty() {
            self.status = "Сначала выберите рёбра детали (Ctrl+щелчок — добавить ребро к выбранным)".into();
            return;
        }
        let id = if chamfer {
            self.part.add("Фаска", FeatureKind::Chamfer { edges, distance: 1.0 })
        } else {
            self.part.add("Скругление", FeatureKind::Fillet { edges, radius: 2.0 })
        };
        self.selection = Selection::Feature(id);
        self.status.clear();
        self.dirty = true;
    }

    fn new_part(&mut self) {
        self.part = Part::default();
        self.selection = Selection::None;
        self.sketch = None;
        self.dim_edit = None;
        self.cam = Camera::default();
        self.status.clear();
        self.dirty = true;
    }

    fn save(&mut self) {
        let Some(path) =
            rfd::FileDialog::new().add_filter("Деталь MySolid3D", &["ms3d"]).set_file_name("деталь.ms3d").save_file()
        else {
            return;
        };
        let result = serde_json::to_string_pretty(&self.part)
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => format!("Сохранено: {}", path.display()),
            Err(e) => format!("Не удалось сохранить: {e}"),
        };
    }

    fn open(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Деталь MySolid3D", &["ms3d"]).pick_file() {
            self.load(&path);
        }
    }

    fn load(&mut self, path: &std::path::Path) {
        let result = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|json| serde_json::from_str::<Part>(&json).map_err(|e| e.to_string()));
        match result {
            Ok(part) => {
                self.new_part();
                self.part = part;
                self.need_fit = true;
                self.status = format!("Открыто: {}", path.display());
            }
            Err(e) => self.status = format!("Не удалось открыть: {e}"),
        }
    }

    fn export(&mut self, step: bool) {
        let Some(shape) = &self.shape else {
            self.status = "Нет тела для экспорта".into();
            return;
        };
        let (name, ext) = if step { ("STEP", "step") } else { ("STL", "stl") };
        let Some(path) =
            rfd::FileDialog::new().add_filter(name, &[ext]).set_file_name(format!("деталь.{ext}")).save_file()
        else {
            return;
        };
        let result =
            if step { shape.write_step(&path) } else { shape.write_stl_with_tolerance(&path, 0.01) };
        self.status = match result {
            Ok(()) => format!("Экспортировано: {}", path.display()),
            Err(e) => format!("Ошибка экспорта: {e}"),
        };
    }

    // ---------- панели ----------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if self.sketch.is_some() {
                let mut exit = false;
                if let Some(edit) = &mut self.sketch {
                    ui.strong("Эскиз:");
                    ui.selectable_value(&mut edit.tool, Tool::Select, "Выбор / перенос");
                    ui.selectable_value(&mut edit.tool, Tool::Line, "Линия")
                        .on_hover_text("Щелчками задавайте точки контура; щелчок по первой точке замыкает его");
                    ui.selectable_value(&mut edit.tool, Tool::Arc, "Дуга")
                        .on_hover_text("Щелчок — конец дуги, второй щелчок — точка, через которую она проходит");
                    ui.selectable_value(&mut edit.tool, Tool::Rect, "Прямоугольник");
                    ui.selectable_value(&mut edit.tool, Tool::Circle, "Окружность");
                    ui.separator();
                    ui.checkbox(&mut self.snap, "Привязка к сетке");
                    ui.add(
                        egui::DragValue::new(&mut self.grid_step).speed(0.1).range(0.1..=100.0).suffix(" мм"),
                    );
                    ui.separator();
                    exit = ui.button("Выйти из эскиза").clicked();
                }
                if exit {
                    self.exit_sketch();
                }
            } else {
                ui.menu_button("Файл", |ui| {
                    if ui.button("Новая деталь").clicked() {
                        self.new_part();
                        ui.close();
                    }
                    if ui.button("Открыть…").clicked() {
                        self.open();
                        ui.close();
                    }
                    if ui.button("Сохранить…").clicked() {
                        self.save();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Экспорт STL (для печати)…").clicked() {
                        self.export(false);
                        ui.close();
                    }
                    if ui.button("Экспорт STEP…").clicked() {
                        self.export(true);
                        ui.close();
                    }
                });
                ui.separator();
                if ui.button("Эскиз").on_hover_text("Новый эскиз на выбранной плоскости или грани").clicked() {
                    self.start_sketch();
                }
                if ui.button("Бобышка-вытянуть").on_hover_text("Вытянуть эскиз в тело").clicked() {
                    self.add_extrude(false);
                }
                if ui.button("Вырез-вытянуть").on_hover_text("Вырезать эскизом из тела").clicked() {
                    self.add_extrude(true);
                }
                if ui.button("Скругление").on_hover_text("Скруглить выбранные рёбра").clicked() {
                    self.add_edge_feature(false);
                }
                if ui.button("Фаска").on_hover_text("Снять фаску с выбранных рёбер").clicked() {
                    self.add_edge_feature(true);
                }
            }
            ui.separator();
            ui.weak("Вид:");
            if ui.button("Изометрия").clicked() {
                self.cam.set_iso();
            }
            if ui.button("Спереди").clicked() {
                self.cam.look_at_plane(&Plane::FRONT);
            }
            if ui.button("Сверху").clicked() {
                self.cam.look_at_plane(&Plane::TOP);
            }
            if ui.button("Справа").clicked() {
                self.cam.look_at_plane(&Plane::RIGHT);
            }
            if ui.button("Вписать").clicked() {
                self.need_fit = true;
            }
        });
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        ui.heading("Дерево построения");
        ui.add_space(4.0);
        let editing = self.sketch.as_ref().map(|e| e.id);
        let mut edit_request = None;
        ui.add_enabled_ui(editing.is_none(), |ui| {
            ui.weak("Плоскости");
            for (i, name) in PLANE_NAMES.iter().enumerate() {
                if ui.selectable_label(self.selection == Selection::Plane(i), *name).clicked() {
                    self.selection = Selection::Plane(i);
                }
            }
            ui.separator();
            ui.weak("Элементы");
            if self.part.features.is_empty() {
                ui.weak("(пусто)");
            }
            for f in &self.part.features {
                let is_sketch = matches!(f.kind, FeatureKind::Sketch(_));
                let mut text = egui::RichText::new(&f.name);
                if f.error.is_some() {
                    text = text.color(ERROR_COLOR);
                } else if is_sketch {
                    text = text.color(SKETCH_COLOR);
                }
                let mut r = ui.selectable_label(self.selection == Selection::Feature(f.id), text);
                if let Some(e) = &f.error {
                    r = r.on_hover_text(format!("Ошибка: {e}"));
                }
                if r.clicked() {
                    self.selection = Selection::Feature(f.id);
                }
                if r.double_clicked() && is_sketch {
                    edit_request = Some(f.id);
                }
            }
        });
        if let Some(id) = edit_request {
            self.enter_sketch(id);
        }
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        ui.heading("Свойства");
        ui.add_space(4.0);
        match self.selection {
            Selection::None => {
                ui.weak("Ничего не выбрано.");
                ui.add_space(8.0);
                ui.weak(
                    "Порядок работы:\n1. Выберите плоскость в дереве.\n2. «Эскиз» — нарисуйте контур.\n\
                     3. «Выйти из эскиза».\n4. «Бобышка-вытянуть».\n5. Щёлкните по грани, снова «Эскиз», затем «Вырез-вытянуть».",
                );
            }
            Selection::Plane(i) => {
                ui.label(format!("Плоскость «{}»", PLANE_NAMES[i]));
                if ui.button("Создать эскиз").clicked() {
                    self.start_sketch();
                }
            }
            Selection::Face(i) => {
                if self.mesh.faces.get(i).is_some_and(|f| f.planar) {
                    ui.label("Плоская грань");
                    if ui.button("Создать эскиз на грани").clicked() {
                        self.start_sketch();
                    }
                } else {
                    ui.label("Криволинейная грань");
                    ui.weak("Эскиз можно создать только на плоской грани.");
                }
            }
            Selection::Edges(ref list) => {
                ui.label(format!("Выбрано рёбер: {}", list.len()));
                ui.weak("Ctrl+щелчок — добавить или убрать ребро.");
                ui.add_space(4.0);
                let fillet = ui.button("Скругление").clicked();
                let chamfer = ui.button("Фаска").clicked();
                if fillet || chamfer {
                    self.add_edge_feature(chamfer);
                }
            }
            Selection::Feature(id) => self.feature_properties(ui, id),
        }
    }

    fn feature_properties(&mut self, ui: &mut egui::Ui, id: u64) {
        let editing = self.sketch.as_ref().map(|e| e.id);
        let selected_shape = self.sketch.as_ref().and_then(|e| e.selected);
        let Some(feature) = self.part.feature_mut(id) else {
            self.selection = Selection::None;
            return;
        };
        let (mut changed, mut edit, mut delete, mut removed_shape) = (false, false, false, false);

        ui.horizontal(|ui| {
            ui.label("Имя");
            ui.text_edit_singleline(&mut feature.name);
        });
        if let Some(e) = &feature.error {
            ui.colored_label(ERROR_COLOR, format!("Ошибка: {e}"));
        }
        ui.separator();

        let depth_row = |ui: &mut egui::Ui, depth: &mut f64, enabled: bool| {
            ui.horizontal(|ui| {
                ui.label("Глубина");
                ui.add_enabled(
                    enabled,
                    egui::DragValue::new(depth).speed(0.25).range(MIN_SIZE..=1.0e4).max_decimals(2).suffix(" мм"),
                )
                .changed()
            })
            .inner
        };
        match &mut feature.kind {
            FeatureKind::Sketch(sketch) => {
                if editing != Some(id) && ui.button("Редактировать эскиз").clicked() {
                    edit = true;
                }
                ui.add_space(4.0);
                let (c, r) = shapes_ui(ui, sketch, selected_shape);
                changed |= c;
                removed_shape = r;
            }
            FeatureKind::Boss { depth, flip, .. } => {
                changed |= depth_row(ui, depth, true);
                changed |= ui.checkbox(flip, "Обратное направление").changed();
            }
            FeatureKind::Cut { depth, through_all, flip, .. } => {
                changed |= ui.checkbox(through_all, "Насквозь").changed();
                changed |= depth_row(ui, depth, !*through_all);
                changed |= ui.checkbox(flip, "Обратное направление").changed();
            }
            FeatureKind::Fillet { edges, radius: size } | FeatureKind::Chamfer { edges, distance: size } => {
                ui.horizontal(|ui| {
                    ui.label("Размер");
                    let drag =
                        egui::DragValue::new(size).speed(0.1).range(MIN_SIZE..=1.0e3).max_decimals(2).suffix(" мм");
                    changed |= ui.add(drag).changed();
                });
                ui.weak(format!("Рёбер: {}", edges.len()));
            }
        }
        if editing.is_none() {
            ui.separator();
            delete = ui.button("Удалить элемент").clicked();
        }

        if removed_shape {
            if let Some(e) = &mut self.sketch {
                e.selected = None;
            }
            self.dim_edit = None;
        }
        if changed && editing != Some(id) {
            self.dirty = true;
        }
        if edit {
            self.enter_sketch(id);
        }
        if delete {
            self.part.remove(id);
            self.selection = Selection::None;
            self.dirty = true;
        }
    }

    // ---------- 3D-вид ----------

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = resp.rect;
        if self.need_fit {
            self.cam.fit(rect, self.mesh.bounds);
            self.need_fit = false;
        }

        // Навигация: средняя кнопка — вращение, правая (или Shift+средняя) — сдвиг, колесо — масштаб.
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.dragged_by(PointerButton::Middle) && !shift {
            self.cam.orbit(resp.drag_delta());
        } else if resp.dragged_by(PointerButton::Middle) || resp.dragged_by(PointerButton::Secondary) {
            self.cam.pan(resp.drag_delta());
        }
        if let Some(pos) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.cam.zoom(rect, pos, (scroll as f64 * 0.005).exp());
            }
        }

        // Фон — вертикальный градиент.
        let (top, bottom) = (Color32::from_rgb(58, 66, 82), Color32::from_rgb(24, 27, 34));
        let mut bg = egui::Mesh::default();
        bg.colored_vertex(rect.left_top(), top);
        bg.colored_vertex(rect.right_top(), top);
        bg.colored_vertex(rect.right_bottom(), bottom);
        bg.colored_vertex(rect.left_bottom(), bottom);
        bg.add_triangle(0, 1, 2);
        bg.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(bg));

        // Выбор граней (только вне эскиза).
        let navigating = resp.dragged();
        self.hover_face = None;
        self.hover_edge = None;
        if self.sketch.is_none() {
            if let Some(pos) = resp.hover_pos().filter(|_| !navigating) {
                let (o, d) = self.cam.ray(rect, pos);
                let hit = self.mesh.pick_hit(o, d);
                // Ребро рядом с курсором важнее грани под ним.
                self.hover_edge = self.pick_edge(rect, pos, hit.map(|(_, t)| t));
                if self.hover_edge.is_none() {
                    self.hover_face = hit.map(|(face, _)| face);
                }
            }
            if resp.clicked() {
                let ctrl = ui.input(|i| i.modifiers.command);
                self.selection = match (self.hover_edge, self.hover_face) {
                    (Some(e), _) => {
                        let mut list = match &self.selection {
                            Selection::Edges(list) if ctrl => list.clone(),
                            _ => vec![],
                        };
                        match list.iter().position(|&i| i == e) {
                            Some(at) => drop(list.remove(at)),
                            None => list.push(e),
                        }
                        if list.is_empty() { Selection::None } else { Selection::Edges(list) }
                    }
                    (None, Some(f)) => Selection::Face(f),
                    (None, None) => self.pick_base_plane(rect, resp.interact_pointer_pos()),
                };
            }
        }

        // Тело — через OpenGL.
        let depth_range = match self.mesh.bounds {
            Some((min, max)) => ((min + max) * 0.5 - self.cam.target).length() + (max - min).length() + 10.0,
            None => 1000.0,
        };
        let params = DrawParams {
            view_proj: self.cam.view_proj(rect, depth_range),
            normal_mat: self.cam.normal_matrix(),
            hover: self.hover_face,
            selected: match self.selection {
                Selection::Face(i) => Some(i),
                _ => None,
            },
        };
        let renderer = self.renderer.clone();
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
                renderer.lock().unwrap().paint(painter.gl(), &params);
            })),
        });

        self.draw_edge_highlights(&painter, rect);
        self.draw_base_planes(&painter, rect);
        self.draw_sketches(&painter, rect);
        if let Some(mut edit) = self.sketch.take() {
            self.sketch_mode(ui, &resp, &painter, rect, &mut edit);
            self.sketch = Some(edit);
        }
        self.draw_axes(&painter, rect);
        self.dim_edit_popup(ui);
    }

    /// Видимое ребро в пределах нескольких пикселей от курсора.
    /// `face_depth` — расстояние вдоль луча до грани под курсором (рёбра за ней считаем скрытыми).
    fn pick_edge(&self, rect: Rect, pos: Pos2, face_depth: Option<f64>) -> Option<usize> {
        let (origin, dir) = self.cam.ray(rect, pos);
        // Допуск по глубине: курсор стоит не точно на ребре, а грань может быть наклонной.
        let tolerance = 30.0 / self.cam.scale + 0.05;
        let mut best: Option<(f32, usize)> = None;
        for (index, edge) in self.mesh.edges.iter().enumerate() {
            for w in edge.points.windows(2) {
                let (a, b) = (self.cam.project(rect, w[0]), self.cam.project(rect, w[1]));
                let dist = screen_segment_distance(a, b, pos);
                if dist > 6.0 || best.is_some_and(|(d, _)| d <= dist) {
                    continue;
                }
                let depth = ((w[0] + w[1]) * 0.5 - origin).dot(dir);
                if face_depth.is_some_and(|t| depth > t + tolerance + (w[1] - w[0]).length() * 0.5) {
                    continue;
                }
                best = Some((dist, index));
            }
        }
        best.map(|(_, index)| index)
    }

    fn draw_edge_highlights(&self, painter: &Painter, rect: Rect) {
        let draw = |index: usize, stroke: Stroke| {
            if let Some(edge) = self.mesh.edges.get(index) {
                painter.line(edge.points.iter().map(|p| self.cam.project(rect, *p)).collect(), stroke);
            }
        };
        if let Selection::Edges(list) = &self.selection {
            for &i in list {
                draw(i, Stroke::new(3.0, Color32::from_rgb(60, 160, 255)));
            }
        }
        if let Some(i) = self.hover_edge {
            draw(i, Stroke::new(3.0, SELECT_COLOR));
        }
    }

    fn base_plane_size(&self) -> f64 {
        match self.mesh.bounds {
            Some((min, max)) => ((max - min).length() * 0.6).max(20.0),
            None => 40.0,
        }
    }

    fn base_plane_quad(&self, rect: Rect, plane: &Plane) -> [Pos2; 4] {
        let h = self.base_plane_size();
        [(-h, -h), (h, -h), (h, h), (-h, h)]
            .map(|(x, y)| self.cam.project(rect, plane.to_world(DVec2::new(x, y))))
    }

    fn planes_visible(&self) -> bool {
        self.sketch.is_none() && self.mesh.indices.is_empty()
    }

    fn pick_base_plane(&self, rect: Rect, pos: Option<Pos2>) -> Selection {
        let (Some(pos), true) = (pos, self.planes_visible()) else { return Selection::None };
        let f = self.cam.forward();
        (0..3)
            .filter(|&i| point_in_quad(&self.base_plane_quad(rect, &BASE_PLANES[i]), pos))
            .max_by(|&a, &b| {
                let facing = |i: usize| BASE_PLANES[i].normal().dot(f).abs();
                facing(a).total_cmp(&facing(b))
            })
            .map_or(Selection::None, Selection::Plane)
    }

    fn draw_base_planes(&self, painter: &Painter, rect: Rect) {
        if self.sketch.is_some() {
            return;
        }
        for (i, plane) in BASE_PLANES.iter().enumerate() {
            let selected = self.selection == Selection::Plane(i);
            if !selected && !self.planes_visible() {
                continue;
            }
            let quad = self.base_plane_quad(rect, plane);
            let (fill, stroke) = if selected {
                (Color32::from_rgba_unmultiplied(255, 185, 70, 40), Stroke::new(1.5, SELECT_COLOR))
            } else {
                (
                    Color32::from_rgba_unmultiplied(120, 170, 255, 14),
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(140, 180, 255, 110)),
                )
            };
            painter.add(egui::Shape::convex_polygon(quad.to_vec(), fill, stroke));
            painter.text(
                quad[3] + Vec2::new(4.0, 4.0),
                Align2::LEFT_TOP,
                PLANE_NAMES[i],
                FontId::proportional(12.0),
                stroke.color,
            );
        }
    }

    /// Эскизы вне режима редактирования: показываем выбранный и ещё не использованные.
    fn draw_sketches(&self, painter: &Painter, rect: Rect) {
        let editing = self.sketch.as_ref().map(|e| e.id);
        for f in &self.part.features {
            let FeatureKind::Sketch(sketch) = &f.kind else { continue };
            if editing == Some(f.id) {
                continue;
            }
            let selected = self.selection == Selection::Feature(f.id);
            if !selected && (self.part.is_sketch_used(f.id) || editing.is_some()) {
                continue;
            }
            let stroke = if selected {
                Stroke::new(2.0, SELECT_COLOR)
            } else {
                Stroke::new(1.5, SKETCH_COLOR.gamma_multiply(0.8))
            };
            for s in &sketch.shapes {
                draw_shape(painter, &self.cam, rect, &sketch.plane, s, stroke);
            }
        }
    }

    fn draw_axes(&self, painter: &Painter, rect: Rect) {
        let o = self.cam.project(rect, DVec3::ZERO);
        let len = 34.0 / self.cam.scale;
        let axes = [
            (DVec3::X, "X", Color32::from_rgb(235, 90, 80)),
            (DVec3::Y, "Y", Color32::from_rgb(110, 205, 100)),
            (DVec3::Z, "Z", Color32::from_rgb(90, 150, 250)),
        ];
        for (axis, name, color) in axes {
            let end = self.cam.project(rect, axis * len);
            painter.line_segment([o, end], Stroke::new(1.5, color));
            let dir = (end - o).normalized();
            if dir.is_finite() && (end - o).length() > 6.0 {
                painter.text(end + dir * 8.0, Align2::CENTER_CENTER, name, FontId::proportional(11.0), color);
            }
        }
    }

    fn draw_grid(&self, painter: &Painter, rect: Rect, plane: &Plane) {
        let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
        let pts: Vec<DVec2> =
            corners.iter().filter_map(|c| self.cam.screen_to_plane(rect, *c, plane)).collect();
        if pts.len() < 4 {
            return;
        }
        let min = pts.iter().fold(DVec2::splat(f64::INFINITY), |a, b| a.min(*b));
        let max = pts.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, b| a.max(*b));

        let mut step = self.grid_step.max(0.1);
        let mut by_five = true;
        while step * self.cam.scale < 12.0 {
            step *= if by_five { 5.0 } else { 2.0 };
            by_five = !by_five;
        }
        if (max.x - min.x) / step > 400.0 || (max.y - min.y) / step > 400.0 {
            return;
        }
        let line = |a: DVec2, b: DVec2, stroke: Stroke| {
            let (a, b) = (plane.to_world(a), plane.to_world(b));
            painter.line_segment([self.cam.project(rect, a), self.cam.project(rect, b)], stroke);
        };
        let thin = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 12));
        let axis = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 70));
        for i in (min.x / step).ceil() as i64..=(max.x / step).floor() as i64 {
            let x = i as f64 * step;
            line(DVec2::new(x, min.y), DVec2::new(x, max.y), if i == 0 { axis } else { thin });
        }
        for i in (min.y / step).ceil() as i64..=(max.y / step).floor() as i64 {
            let y = i as f64 * step;
            line(DVec2::new(min.x, y), DVec2::new(max.x, y), if i == 0 { axis } else { thin });
        }
    }

    /// Режим эскиза: рисование фигур, перенос, размеры.
    fn sketch_mode(&mut self, ui: &egui::Ui, resp: &Response, painter: &Painter, rect: Rect, edit: &mut SketchEdit) {
        let Some(plane) = self.part.sketch(edit.id).map(|s| s.plane) else { return };
        self.draw_grid(painter, rect, &plane);

        let cam = self.cam.clone();
        let (snap, step) = (self.snap, self.grid_step.max(0.1));
        let snap_pt = |p: DVec2| if snap { (p / step).round() * step } else { p };
        let raw_at = |pos: Pos2| cam.screen_to_plane(rect, pos, &plane);
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let press = ui.input(|i| i.pointer.press_origin());
        let primary = PointerButton::Primary;
        let px = 1.0 / cam.scale; // миллиметров в одном пикселе
        let no_focus = ui.memory(|m| m.focused().is_none());

        let Some(sketch) = self.part.sketch_mut(edit.id) else { return };
        let mut preview = None;
        // Предпросмотр следующего сегмента контура (в координатах эскиза).
        let mut chain_preview: Option<Vec<DVec2>> = None;

        // Завершить рисование контура; контур из одной точки удаляется.
        let finish_chain = |sketch: &mut Sketch, edit: &mut SketchEdit| {
            if let Some(c) = edit.chain {
                if matches!(sketch.shapes.get(c), Some(SketchShape::Path { points, .. }) if points.len() < 2) {
                    sketch.shapes.remove(c);
                    edit.selected = None;
                }
            }
            edit.stop_chain();
        };

        let chain_tool = matches!(edit.tool, Tool::Line | Tool::Arc);
        if !chain_tool && edit.chain.is_some() {
            finish_chain(sketch, edit);
        }
        if edit.tool != Tool::Arc {
            edit.arc_end = None;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            edit.drag_start = None;
            if edit.chain.is_some() {
                finish_chain(sketch, edit);
            } else {
                edit.tool = Tool::Select;
            }
        }
        if no_focus && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            edit.stop_chain();
            if let Some(i) = edit.selected.take().filter(|&i| i < sketch.shapes.len()) {
                sketch.shapes.remove(i);
                self.dim_edit = None;
            }
        }

        match edit.tool {
            Tool::Line | Tool::Arc => {
                let ends = |sketch: &Sketch, chain: usize| match sketch.shapes.get(chain) {
                    Some(SketchShape::Path { points, .. }) if !points.is_empty() => {
                        Some((points[0].pos(), points[points.len() - 1].pos(), points.len()))
                    }
                    _ => None,
                };
                // Рядом с началом контура курсор «прилипает» к нему — так контур замыкается.
                let target = pointer.and_then(raw_at).map(|raw| match edit.chain.and_then(|c| ends(sketch, c)) {
                    Some((first, _, n)) if n >= 2 && (first - raw).length() < 10.0 * px => first,
                    _ => snap_pt(raw),
                });

                if let (true, Some(p)) = (resp.clicked(), target) {
                    match edit.chain.and_then(|c| ends(sketch, c).map(|e| (c, e))) {
                        None => {
                            sketch.shapes.push(SketchShape::Path { points: vec![PathPoint::new(p)], closed: false });
                            edit.chain = Some(sketch.shapes.len() - 1);
                            edit.selected = edit.chain;
                            edit.arc_end = None;
                        }
                        Some((c, (first, last, n))) => {
                            let SketchShape::Path { points, closed } = &mut sketch.shapes[c] else { unreachable!() };
                            let closes = |end: DVec2| n >= 2 && (end - first).length() < 1e-9;
                            if edit.tool == Tool::Line {
                                if closes(p) {
                                    *closed = true;
                                    edit.stop_chain();
                                } else if (p - last).length() > MIN_SIZE {
                                    points.push(PathPoint::new(p));
                                }
                            } else if let Some(end) = edit.arc_end.take() {
                                points[n - 1].bulge = bulge_through(last, end, p);
                                if closes(end) {
                                    *closed = true;
                                    edit.stop_chain();
                                } else {
                                    points.push(PathPoint::new(end));
                                }
                            } else if (p - last).length() > MIN_SIZE {
                                edit.arc_end = Some(p);
                            }
                        }
                    }
                }
                if resp.double_clicked() || (no_focus && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    finish_chain(sketch, edit);
                }

                if let (Some((_, last, _)), Some(t)) = (edit.chain.and_then(|c| ends(sketch, c)), target) {
                    let mut pts = vec![last];
                    match edit.arc_end {
                        Some(end) => segment_points(last, end, bulge_through(last, end, t), &mut pts),
                        None => pts.push(t),
                    }
                    chain_preview = Some(pts);
                }
            }
            Tool::Select => {
                // Сначала пробуем схватить точку контура, иначе — фигуру целиком.
                if resp.drag_started_by(primary) {
                    edit.drag_vertex = press.and_then(raw_at).and_then(|p| {
                        sketch.shapes.iter().enumerate().find_map(|(si, s)| match s {
                            SketchShape::Path { points, .. } => {
                                points.iter().position(|q| (q.pos() - p).length() < 8.0 * px).map(|pi| (si, pi))
                            }
                            _ => None,
                        })
                    });
                }
                if let Some((si, pi)) = edit.drag_vertex {
                    edit.selected = Some(si);
                    if let (Some(cur), Some(SketchShape::Path { points, .. })) =
                        (pointer.and_then(raw_at).map(snap_pt), sketch.shapes.get_mut(si))
                    {
                        if let Some(point) = points.get_mut(pi) {
                            (point.x, point.y) = (cur.x, cur.y);
                        }
                    }
                    if resp.drag_stopped_by(primary) || !resp.dragged_by(primary) {
                        edit.drag_vertex = None;
                    }
                }
                let dragging_vertex = edit.drag_vertex.is_some();
                let pick = |sketch: &Sketch, p: DVec2| {
                    sketch
                        .shapes
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i, s.outline_distance(p)))
                        .filter(|(_, d)| *d < 8.0 * px)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(i, _)| i)
                };
                if dragging_vertex {
                    edit.move_last = None;
                } else if resp.drag_started_by(primary) {
                    if let Some(p) = press.and_then(raw_at) {
                        edit.selected = pick(sketch, p);
                        edit.move_last = edit.selected.map(|_| snap_pt(p));
                    }
                } else if resp.clicked() {
                    edit.selected = pointer.and_then(raw_at).and_then(|p| pick(sketch, p));
                }
                if resp.dragged_by(primary) {
                    let cur = pointer.and_then(raw_at).map(snap_pt);
                    if let (Some(last), Some(i), Some(cur)) = (edit.move_last, edit.selected, cur) {
                        sketch.shapes[i].translate(cur - last);
                        edit.move_last = Some(cur);
                    }
                }
                if resp.drag_stopped_by(primary) {
                    edit.move_last = None;
                }
            }
            Tool::Rect | Tool::Circle => {
                if resp.drag_started_by(primary) {
                    edit.drag_start = press.and_then(raw_at).map(snap_pt);
                }
                let cur = pointer.and_then(raw_at).map(snap_pt);
                if let (Some(a), Some(b)) = (edit.drag_start, cur) {
                    let (shape, valid) = if edit.tool == Tool::Rect {
                        let (lo, size) = (a.min(b), (b - a).abs());
                        (
                            SketchShape::Rect { x: lo.x, y: lo.y, w: size.x, h: size.y },
                            size.x >= MIN_SIZE && size.y >= MIN_SIZE,
                        )
                    } else {
                        let mut r = (b - a).length();
                        if snap {
                            r = (r / step).round() * step;
                        }
                        (SketchShape::Circle { cx: a.x, cy: a.y, r }, r >= MIN_SIZE)
                    };
                    if resp.drag_stopped_by(primary) {
                        edit.drag_start = None;
                        if valid {
                            sketch.shapes.push(shape);
                            edit.selected = Some(sketch.shapes.len() - 1);
                        }
                    } else if valid {
                        preview = Some(shape);
                    }
                } else if resp.drag_stopped_by(primary) {
                    edit.drag_start = None;
                }
            }
        }

        // Отрисовка фигур и размеров.
        let mut labels = Vec::new();
        for (i, s) in sketch.shapes.iter().enumerate() {
            let color = if edit.selected == Some(i) { SELECT_COLOR } else { SKETCH_COLOR };
            draw_shape(painter, &cam, rect, &plane, s, Stroke::new(2.0, color));
            draw_dimensions(painter, &cam, rect, &plane, s, Some(i), &mut labels);
            if let SketchShape::Path { points, closed } = s {
                for (pi, p) in points.iter().enumerate() {
                    let pos = cam.project(rect, plane.to_world(p.pos()));
                    painter.circle_filled(pos, 3.0, color);
                    // Кольцо на первой точке незамкнутого контура — подсказка, куда щёлкнуть для замыкания.
                    if pi == 0 && !closed {
                        painter.circle_stroke(pos, 7.0, Stroke::new(1.0, color));
                    }
                }
            }
        }
        if let Some(pts) = chain_preview {
            let line: Vec<Pos2> = pts.iter().map(|p| cam.project(rect, plane.to_world(*p))).collect();
            painter.line(line, Stroke::new(1.5, Color32::WHITE));
        }
        if let Some(s) = &preview {
            draw_shape(painter, &cam, rect, &plane, s, Stroke::new(1.5, Color32::WHITE));
            draw_dimensions(painter, &cam, rect, &plane, s, None, &mut labels);
        }

        // Двойной щелчок по размеру — ввод значения.
        if resp.double_clicked() {
            if let Some(l) = pointer.and_then(|p| labels.iter().find(|l| l.rect.expand(4.0).contains(p))) {
                let value = match (&sketch.shapes[l.shape], l.kind) {
                    (SketchShape::Rect { w, .. }, DimKind::Width) => *w,
                    (SketchShape::Rect { h, .. }, DimKind::Height) => *h,
                    (SketchShape::Circle { r, .. }, _) => r * 2.0,
                    _ => 0.0,
                };
                self.dim_edit = Some(DimEdit {
                    shape: l.shape,
                    kind: l.kind,
                    text: String::new(),
                    hint: fmt_mm(value),
                    pos: l.rect.left_top() - Vec2::new(8.0, 6.0),
                    focus: true,
                });
            }
        }
    }

    fn dim_edit_popup(&mut self, ui: &egui::Ui) {
        let Some(de) = &mut self.dim_edit else { return };
        let (mut apply, mut close) = (false, false);
        egui::Area::new(egui::Id::unique("dim_edit")).fixed_pos(de.pos).order(egui::Order::Foreground).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut de.text).hint_text(de.hint.as_str()).desired_width(70.0));
                    if de.focus {
                        r.request_focus();
                        de.focus = false;
                    } else if r.lost_focus() {
                        apply = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        close = true;
                    }
                });
            },
        );
        if apply && !de.text.trim().is_empty() {
            let value = de.text.trim().replace(',', ".").parse::<f64>().ok().filter(|v| *v >= MIN_SIZE);
            let (index, kind) = (de.shape, de.kind);
            let shape = self
                .sketch
                .as_ref()
                .and_then(|e| self.part.sketch_mut(e.id))
                .and_then(|s| s.shapes.get_mut(index));
            match (value, shape) {
                (Some(v), Some(shape)) => match (shape, kind) {
                    (SketchShape::Rect { w, .. }, DimKind::Width) => *w = v,
                    (SketchShape::Rect { h, .. }, DimKind::Height) => *h = v,
                    (SketchShape::Circle { r, .. }, DimKind::Diameter) => *r = v / 2.0,
                    _ => {}
                },
                _ => self.status = "Размер должен быть положительным числом".into(),
            }
        }
        if close {
            self.dim_edit = None;
        }
    }
}

/// Параметры фигур эскиза в панели свойств. Возвращает (изменено, удалена фигура).
fn shapes_ui(ui: &mut egui::Ui, sketch: &mut Sketch, selected: Option<usize>) -> (bool, bool) {
    let mut changed = false;
    let mut remove = None;
    if sketch.shapes.is_empty() {
        ui.weak("В эскизе пока нет фигур.");
    }
    for (i, shape) in sketch.shapes.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                let title = match shape {
                    SketchShape::Rect { .. } => "Прямоугольник",
                    SketchShape::Circle { .. } => "Окружность",
                    SketchShape::Path { closed: true, .. } => "Контур",
                    SketchShape::Path { closed: false, .. } => "Контур (не замкнут)",
                };
                if selected == Some(i) {
                    ui.colored_label(SELECT_COLOR, title);
                } else {
                    ui.strong(title);
                }
                if ui.small_button("Удалить").clicked() {
                    remove = Some(i);
                }
            });
            egui::Grid::new(("shape", i)).num_columns(2).show(ui, |ui| {
                let mut row = |ui: &mut egui::Ui, label: &str, v: &mut f64, min: f64| {
                    ui.label(label);
                    let drag = egui::DragValue::new(v).speed(0.25).range(min..=1.0e5).max_decimals(2).suffix(" мм");
                    changed |= ui.add(drag).changed();
                    ui.end_row();
                };
                match shape {
                    SketchShape::Rect { x, y, w, h } => {
                        row(ui, "Ширина", w, MIN_SIZE);
                        row(ui, "Высота", h, MIN_SIZE);
                        row(ui, "X угла", x, -1.0e5);
                        row(ui, "Y угла", y, -1.0e5);
                    }
                    SketchShape::Circle { cx, cy, r } => {
                        let mut d = *r * 2.0;
                        row(ui, "Диаметр", &mut d, MIN_SIZE);
                        *r = d / 2.0;
                        row(ui, "X центра", cx, -1.0e5);
                        row(ui, "Y центра", cy, -1.0e5);
                    }
                    SketchShape::Path { points, .. } => {
                        for (pi, p) in points.iter_mut().enumerate() {
                            row(ui, &format!("Точка {} — X", pi + 1), &mut p.x, -1.0e5);
                            row(ui, &format!("Точка {} — Y", pi + 1), &mut p.y, -1.0e5);
                        }
                    }
                }
            });
        });
    }
    if let Some(i) = remove {
        sketch.shapes.remove(i);
        changed = true;
    }
    (changed, remove.is_some())
}

/// Размерные линии и подписи фигуры. Для реальных фигур (`index`) запоминает области подписей.
fn draw_dimensions(
    painter: &Painter,
    cam: &Camera,
    rect: Rect,
    plane: &Plane,
    shape: &SketchShape,
    index: Option<usize>,
    labels: &mut Vec<DimLabel>,
) {
    let px = 1.0 / cam.scale;
    let to_screen = |x: f64, y: f64| cam.project(rect, plane.to_world(DVec2::new(x, y)));
    let stroke = Stroke::new(1.0, Color32::from_gray(170));
    // `kind` задан только у размеров, которые можно править двойным щелчком.
    let mut label = |pos: Pos2, text: String, kind: Option<DimKind>| {
        let color = if kind.is_some() { Color32::WHITE } else { Color32::from_gray(190) };
        let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), color);
        let r = Align2::CENTER_CENTER.anchor_size(pos, galley.size());
        painter.rect_filled(r.expand2(Vec2::new(4.0, 2.0)), 3.0, Color32::from_rgba_unmultiplied(20, 24, 32, 230));
        painter.galley(r.min, galley, color);
        if let (Some(shape), Some(kind)) = (index, kind) {
            labels.push(DimLabel { rect: r, shape, kind });
        }
    };
    match *shape {
        SketchShape::Path { ref points, closed } => {
            // Справочные размеры сегментов: длина отрезка или радиус дуги.
            let n = points.len();
            let count = if closed { n } else { n.saturating_sub(1) };
            for i in 0..count {
                let (a, b) = (points[i].pos(), points[(i + 1) % n].pos());
                let chord = b - a;
                if chord.length() < 1e-9 {
                    continue;
                }
                let normal = DVec2::new(chord.y, -chord.x) / chord.length();
                let bulge = points[i].bulge;
                let (anchor, text) = if bulge == 0.0 {
                    ((a + b) * 0.5 + normal * (14.0 * px), fmt_mm(chord.length()))
                } else {
                    let mid = (a + b) * 0.5 + normal * (bulge * chord.length() * 0.5);
                    (mid + normal * (16.0 * px * bulge.signum()), format!("R{}", fmt_mm(arc_radius(a, b, bulge))))
                };
                label(to_screen(anchor.x, anchor.y), text, None);
            }
        }
        SketchShape::Rect { x, y, w, h } => {
            let (off, ext) = (24.0 * px, 30.0 * px);
            // Ширина — под нижней стороной.
            painter.line_segment([to_screen(x, y), to_screen(x, y - ext)], stroke);
            painter.line_segment([to_screen(x + w, y), to_screen(x + w, y - ext)], stroke);
            painter.line_segment([to_screen(x, y - off), to_screen(x + w, y - off)], stroke);
            label(to_screen(x + w / 2.0, y - off), fmt_mm(w), Some(DimKind::Width));
            // Высота — слева.
            painter.line_segment([to_screen(x, y), to_screen(x - ext, y)], stroke);
            painter.line_segment([to_screen(x, y + h), to_screen(x - ext, y + h)], stroke);
            painter.line_segment([to_screen(x - off, y), to_screen(x - off, y + h)], stroke);
            label(to_screen(x - off, y + h / 2.0), fmt_mm(h), Some(DimKind::Height));
        }
        SketchShape::Circle { cx, cy, r } => {
            let k = std::f64::consts::FRAC_1_SQRT_2;
            let far = r + 30.0 * px;
            painter.line_segment([to_screen(cx + r * k, cy + r * k), to_screen(cx + far * k, cy + far * k)], stroke);
            label(to_screen(cx + far * k, cy + far * k), format!("Ø{}", fmt_mm(r * 2.0)), Some(DimKind::Diameter));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.dirty {
            self.rebuild(frame);
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.sketch.is_some() {
                    ui.weak("Эскиз: «Линия» и «Дуга» — щелчками, щелчок по первой точке замыкает контур · прямоугольник и окружность — протяжкой · двойной щелчок по размеру — изменить · Esc — закончить · Del — удалить");
                } else {
                    ui.weak("Колесо — масштаб · средняя кнопка — вращение · правая — сдвиг · щелчок — выбор грани или ребра (Ctrl — несколько рёбер)");
                }
                if !self.status.is_empty() {
                    ui.separator();
                    ui.colored_label(SELECT_COLOR, &self.status);
                }
            });
        });
        egui::Panel::left("tree").exact_size(210.0).resizable(false).show(ui, |ui| self.tree(ui));
        egui::Panel::right("properties").exact_size(270.0).resizable(false).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.properties(ui));
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui));

        if self.dirty {
            ui.ctx().request_repaint();
        }
    }
}
