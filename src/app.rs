//! Интерфейс: панель инструментов, дерево построения, свойства и 3D-вид.

use crate::camera::Camera;
use crate::model::{
    bulge_through, segment_points, Dim, DimMode, DimRef, FeatureKind, Part, PathPoint, Plane, Relation,
    RenderMesh, Sketch, SketchImage, SketchShape, TreeItem, MIN_SIZE,
};
use crate::render::{DrawParams, Renderer};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, Painter, PointerButton, Pos2, Rect, Response, Stroke, Vec2,
};
use eframe::egui_glow;
use glam::{DVec2, DVec3};
use opencascade::primitives::Shape;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const PLANE_NAMES: [&str; 3] = ["Сверху", "Спереди", "Справа"];
const BASE_PLANES: [Plane; 3] = [Plane::TOP, Plane::FRONT, Plane::RIGHT];
const ERROR_COLOR: Color32 = Color32::from_rgb(240, 100, 90);
const SKETCH_COLOR: Color32 = Color32::from_rgb(120, 190, 255);
const SELECT_COLOR: Color32 = Color32::from_rgb(255, 185, 70);
/// Цвета для раскраски детали и граней.
const PALETTE: [[u8; 3]; 10] = [
    [179, 189, 204],
    [240, 240, 240],
    [70, 74, 82],
    [214, 72, 62],
    [236, 142, 52],
    [238, 204, 70],
    [92, 178, 96],
    [64, 170, 180],
    [72, 124, 220],
    [150, 100, 200],
];

#[derive(Clone, PartialEq)]
enum Selection {
    None,
    Plane(usize),
    Face(usize),
    /// Несколько граней (Ctrl+щелчок), в том числе не соприкасающихся.
    Faces(Vec<usize>),
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
    Dimension,
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
    /// Первый элемент, указанный инструментом «Размер».
    dim_first: Option<DimRef>,
    /// Расстояние между двумя элементами, которое сейчас размещается: способ измерения
    /// выбирается положением курсора, щелчок фиксирует его и открывает ввод значения.
    placing: Option<Dim>,
    /// Области подписей размеров на прошлом кадре: щелчок по ним не должен попадать в инструмент.
    labels: Vec<Rect>,
    /// Открытое окно операции над выбранной фигурой и введённые в нём параметры.
    op: Option<OpKind>,
    params: OpParams,
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
            dim_first: None,
            placing: None,
            labels: Vec::new(),
            op: None,
            params: OpParams::default(),
        }
    }

    fn stop_chain(&mut self) {
        self.chain = None;
        self.arc_end = None;
    }
}

struct DimLabel {
    rect: Rect,
    dim: Dim,
    /// Номер размера в `Sketch::dims`.
    index: usize,
}

/// Окно ввода значения размера. Текущее значение показывается подсказкой, пока поле пустое.
struct DimEdit {
    dim: Dim,
    /// Номер уже поставленного размера; `None` — размер ещё только создаётся.
    stored: Option<usize>,
    text: String,
    pos: Pos2,
    focus: bool,
    /// Куда пользователь поставил размерную линию (в координатах эскиза).
    place: Option<DVec2>,
}

/// Все иконки действий (файлы `assets/icons/<имя>.png`, готовятся `tools/make_icons.py`).
macro_rules! icons {
    ($($name:ident),* $(,)?) => {
        #[allow(dead_code)]
        struct Icons {
            $($name: egui::TextureHandle,)*
        }

        impl Icons {
            fn load(ctx: &egui::Context) -> Result<Self, String> {
                let load = |name: &str, png: &[u8]| {
                    let icon =
                        eframe::icon_data::from_png_bytes(png).map_err(|e| format!("иконка «{name}»: {e}"))?;
                    let size = [icon.width as usize, icon.height as usize];
                    let image = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
                    Ok::<_, String>(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
                };
                Ok(Self {
                    $($name: load(
                        stringify!($name),
                        include_bytes!(concat!("../assets/icons/", stringify!($name), ".png")),
                    )?,)*
                })
            }
        }
    };
}

icons!(
    arc, chamfer, circle, copy, corner_chamfer, corner_fillet, delete, dimension, extrude_boss, extrude_cut,
    fillet, fit_view, line, materials, mirror, new_file, offset, open_file, pattern_circle, pattern_grid, rect,
    redo, rotate, save_file, scale, select, sketch, solid_view, undo, view_front, view_iso, view_right, view_top,
    wireframe, zoom_in, zoom_out,
);

/// Кнопка только с иконкой; что она делает — в подсказке при наведении.
fn tool_button(ui: &mut egui::Ui, icon: &egui::TextureHandle, tip: &str, selected: bool) -> bool {
    let image = egui::Image::from_texture(icon).fit_to_exact_size(Vec2::splat(24.0));
    ui.add(egui::Button::image(image).selected(selected)).on_hover_text(tip).clicked()
}

/// Квадратик цвета в палитре; `true` — по нему щёлкнули.
fn swatch(ui: &mut egui::Ui, c: [u8; 3]) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r.expand(2.0), 4.0, Color32::WHITE);
    }
    ui.painter().rect_filled(r, 3.0, Color32::from_rgb(c[0], c[1], c[2]));
    resp.clicked()
}

/// Характерные точки фигуры, к которым можно привязаться: углы, середины сторон, центры.
fn shape_snap_points(s: &SketchShape) -> Vec<DVec2> {
    match s {
        SketchShape::Rect { x, y, w, h } => {
            let (x, y, w, h) = (*x, *y, *w, *h);
            [
                (x, y),
                (x + w, y),
                (x + w, y + h),
                (x, y + h),
                (x + w / 2.0, y),
                (x + w, y + h / 2.0),
                (x + w / 2.0, y + h),
                (x, y + h / 2.0),
                (x + w / 2.0, y + h / 2.0),
            ]
            .iter()
            .map(|&(a, b)| DVec2::new(a, b))
            .collect()
        }
        SketchShape::Circle { cx, cy, r } => {
            let c = DVec2::new(*cx, *cy);
            vec![c, c + DVec2::new(*r, 0.0), c - DVec2::new(*r, 0.0), c + DVec2::new(0.0, *r), c - DVec2::new(0.0, *r)]
        }
        SketchShape::Path { points, closed } => {
            let n = points.len();
            let mut out = Vec::new();
            for i in 0..n {
                out.push(points[i].pos());
                if (i + 1 < n || *closed) && points[i].bulge == 0.0 {
                    out.push((points[i].pos() + points[(i + 1) % n].pos()) * 0.5);
                }
            }
            out
        }
    }
}

/// Точки и линии эскиза для привязки; `skip` — фигура, которую сейчас переносят.
fn sketch_snap_targets(sketch: &Sketch, skip: Option<usize>) -> (Vec<DVec2>, Vec<Vec<DVec2>>) {
    let (mut points, mut lines) = (Vec::new(), Vec::new());
    for (_, shape) in sketch.shapes.iter().enumerate().filter(|(i, _)| Some(*i) != skip) {
        points.extend(shape_snap_points(shape));
        lines.push(shape.outline());
    }
    (points, lines)
}

/// Красный крестик в правом верхнем углу вида: выйти из эскиза.
fn exit_button(ui: &egui::Ui, rect: Rect) -> bool {
    let mut clicked = false;
    egui::Area::new(egui::Id::unique("exit_sketch"))
        .fixed_pos(rect.right_top() + Vec2::new(-48.0, 12.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::click());
            let red = if resp.hovered() { Color32::from_rgb(255, 80, 70) } else { Color32::from_rgb(214, 52, 44) };
            ui.painter().circle_filled(r.center(), 17.0, red);
            let c = r.center();
            let stroke = Stroke::new(3.5, Color32::WHITE);
            ui.painter().line_segment([c + Vec2::new(-7.0, -7.0), c + Vec2::new(7.0, 7.0)], stroke);
            ui.painter().line_segment([c + Vec2::new(-7.0, 7.0), c + Vec2::new(7.0, -7.0)], stroke);
            clicked = resp.on_hover_text("Выйти из эскиза").clicked();
        });
    clicked
}

/// Способ измерения расстояния по положению курсора (как в SolidWorks): между выносными линиями —
/// вдоль линии, соединяющей точки; выше или ниже — по горизонтали; слева или справа — по вертикали.
fn placing_mode(a: DVec2, b: DVec2, p: DVec2) -> DimMode {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 > 1e-12 && (0.0..=1.0).contains(&((p - a).dot(d) / len2)) {
        return DimMode::Aligned;
    }
    let (lo, hi) = (a.min(b), a.max(b));
    let ex = (lo.x - p.x).max(p.x - hi.x).max(0.0);
    let ey = (lo.y - p.y).max(p.y - hi.y).max(0.0);
    if ey > ex { DimMode::Horizontal } else { DimMode::Vertical }
}

/// Поле ввода длины в миллиметрах.
fn mm(v: &mut f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v).speed(0.25).max_decimals(1).suffix(" мм")
}

/// Операции над выбранной фигурой эскиза (открывают окно с параметрами).
#[derive(Clone, Copy, PartialEq)]
enum OpKind {
    Rotate,
    Scale,
    Mirror,
    Copy,
    Offset,
    CornerFillet,
    CornerChamfer,
    PatternGrid,
    PatternCircle,
}

impl OpKind {
    fn title(self) -> &'static str {
        match self {
            OpKind::Rotate => "Поворот",
            OpKind::Scale => "Масштаб",
            OpKind::Mirror => "Зеркало",
            OpKind::Copy => "Копия со сдвигом",
            OpKind::Offset => "Смещение контура",
            OpKind::CornerFillet => "Скругление угла",
            OpKind::CornerChamfer => "Фаска угла",
            OpKind::PatternGrid => "Массив сеткой",
            OpKind::PatternCircle => "Круговой массив",
        }
    }
}

/// Введённые параметры операций; значения запоминаются между применениями.
#[derive(Clone)]
struct OpParams {
    angle: f64,
    factor: f64,
    about_origin: bool,
    mirror_axis: usize,
    keep: bool,
    dx: f64,
    dy: f64,
    dist: f64,
    size: f64,
    vertex: usize,
    cols: usize,
    rows: usize,
    count: usize,
    sweep: f64,
}

impl Default for OpParams {
    fn default() -> Self {
        Self {
            angle: 90.0,
            factor: 2.0,
            about_origin: false,
            mirror_axis: 0,
            keep: true,
            dx: 10.0,
            dy: 10.0,
            dist: 2.0,
            size: 2.0,
            vertex: 0,
            cols: 3,
            rows: 1,
            count: 6,
            sweep: 360.0,
        }
    }
}

/// Выполняет операцию над фигурой `index`. Возвращает номер новой фигуры для выбора и сообщение.
fn apply_op(part: &mut Part, id: u64, index: usize, kind: OpKind, p: &OpParams) -> Result<(Option<usize>, String), String> {
    let sketch = part.sketch_mut(id).ok_or("эскиз не найден")?;
    let center = sketch.shapes.get(index).ok_or("фигура не найдена")?.center();
    let pivot = if p.about_origin { DVec2::ZERO } else { center };
    let added = |sketch: &Sketch| Some(sketch.shapes.len() - 1);
    let done = |sel: Option<usize>| Ok((sel, "Готово".to_string()));
    match kind {
        OpKind::Rotate => {
            sketch.shapes[index].rotate(pivot, p.angle.to_radians());
            done(None)
        }
        OpKind::Scale => {
            sketch.shapes[index].scale(pivot, p.factor)?;
            done(None)
        }
        OpKind::Mirror => {
            let (origin, dir) = match p.mirror_axis {
                0 => (center, DVec2::Y),
                1 => (center, DVec2::X),
                2 => (DVec2::ZERO, DVec2::Y),
                _ => (DVec2::ZERO, DVec2::X),
            };
            if p.keep {
                let mut copy = sketch.shapes[index].clone();
                copy.mirror(origin, dir);
                sketch.shapes.push(copy);
                done(added(sketch))
            } else {
                sketch.shapes[index].mirror(origin, dir);
                done(None)
            }
        }
        OpKind::Copy => {
            let mut copy = sketch.shapes[index].clone();
            copy.translate(DVec2::new(p.dx, p.dy));
            sketch.shapes.push(copy);
            done(added(sketch))
        }
        OpKind::Offset => {
            let shape = sketch.shapes[index].offset(p.dist)?;
            sketch.shapes.push(shape);
            done(added(sketch))
        }
        OpKind::CornerFillet | OpKind::CornerChamfer => {
            let vertex = p.vertex.checked_sub(1);
            let n = sketch.round_corners(index, vertex, p.size, kind == OpKind::CornerChamfer)?;
            Ok((None, format!("Обработано углов: {n}")))
        }
        OpKind::PatternGrid => {
            let n = sketch.pattern_grid(index, p.cols, p.rows, DVec2::new(p.dx, p.dy))?;
            Ok((None, format!("Добавлено копий: {n}")))
        }
        OpKind::PatternCircle => {
            let n = sketch.pattern_circle(index, p.count, pivot, p.sweep)?;
            Ok((None, format!("Добавлено копий: {n}")))
        }
    }
}

/// Сколько последних файлов показывать в меню «Файл».
const RECENT_COUNT: usize = 5;

/// Файл со списком последних открывавшихся деталей (по одному пути в строке).
fn recent_list_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("MySolid3D").join("recent.txt"))
}

fn load_recent() -> Vec<PathBuf> {
    let text = recent_list_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).take(RECENT_COUNT).collect()
}

/// Переименование строки дерева прямо в списке.
struct Rename {
    target: TreeItem,
    text: String,
    focus: bool,
}

/// Имя строки дерева: подпись либо поле переименования (открывается правой кнопкой мыши).
/// Возвращает (щелчок по подписи, двойной щелчок, новое имя).
fn name_cell(
    ui: &mut egui::Ui,
    rename: &mut Option<Rename>,
    target: TreeItem,
    name: &str,
    color: Option<Color32>,
    selected: bool,
    hover: Option<String>,
) -> (bool, bool, Option<String>) {
    if let Some(r) = rename.as_mut().filter(|r| r.target == target) {
        let resp = ui.add(egui::TextEdit::singleline(&mut r.text).desired_width(f32::INFINITY));
        let mut renamed = None;
        if r.focus {
            resp.request_focus();
            r.focus = false;
        } else if resp.lost_focus() {
            // Enter или щелчок в стороне — принять, Esc — отменить.
            let text = r.text.trim();
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !text.is_empty() {
                renamed = Some(text.to_string());
            }
            *rename = None;
        }
        return (false, false, renamed);
    }
    let mut text = egui::RichText::new(name);
    if let Some(color) = color {
        text = text.color(color);
    }
    let mut resp = ui.selectable_label(selected, text);
    if let Some(hover) = hover {
        resp = resp.on_hover_text(hover);
    }
    if resp.secondary_clicked() {
        *rename = Some(Rename { target, text: name.to_string(), focus: true });
    }
    (resp.clicked(), resp.double_clicked(), None)
}

pub struct App {
    icons: Icons,
    part: Part,
    shape: Option<Rc<Shape>>,
    mesh: Rc<RenderMesh>,
    dirty: bool,
    cam: Camera,
    renderer: Arc<Mutex<Renderer>>,
    selection: Selection,
    hover_face: Option<usize>,
    hover_edge: Option<usize>,
    sketch: Option<SketchEdit>,
    dim_edit: Option<DimEdit>,
    /// Элементы, отмеченные в дереве галочками (для объединения в группу).
    checked: Vec<u64>,
    rename: Option<Rename>,
    /// Последние открывавшиеся файлы, самый свежий — первый.
    recent: Vec<PathBuf>,
    /// Цвета изменились — их нужно заново передать в видеокарту.
    colors_dirty: bool,
    snap: bool,
    /// Привязка к точкам и линиям: текущего эскиза, более ранних эскизов и самого тела.
    snap_objects: bool,
    grid_step: f64,
    status: String,
    need_fit: bool,
    /// Каркасный режим показа.
    wireframe: bool,
    /// Область 3D-вида на прошлом кадре (для кнопок масштаба).
    view_rect: Rect,
    /// Загруженная картинка-подложка: (путь, текстура и отношение высоты к ширине).
    image_tex: Option<(String, Option<(egui::TextureHandle, f32)>)>,
    /// История для отмены и повтора: состояния детали до каждого изменения.
    undo: Vec<Part>,
    redo: Vec<Part>,
    /// Последнее зафиксированное в истории состояние детали и его запись в JSON.
    base: Part,
    base_json: String,
    /// Идёт пошаговое построение: показывается, как деталь строится.
    building: bool,
    /// Деталь только что загружена: пока строится, вид подгоняется под неё.
    fit_while_building: bool,
    /// Перед пошаговым построением деталь совпадала с историей (перестановки рёбер — не правка).
    build_clean: bool,
    /// Подсвеченные (выбранные) грани, как они переданы в видеокарту.
    face_hl: Vec<usize>,
    /// Что красит палитра: всю деталь (иначе выбранные грани).
    paint_part: bool,
    had_faces: bool,
    /// Открыт вопрос «сохранить под текущим названием или как…».
    ask_save: bool,
    /// Раскрыта ли раскладушка с переходными цветами.
    palette_open: bool,
    /// Файл, в который сохраняется деталь (Ctrl+S), и её состояние в момент сохранения.
    file: Option<PathBuf>,
    saved_json: String,
    title: String,
    /// Последние введённые размеры — подставляются в новые элементы.
    last_fillet: f64,
    last_chamfer: f64,
    last_depth: f64,
}

/// Размеры показываются и вводятся в миллиметрах с точностью до 0,1.
fn fmt_mm(v: f64) -> String {
    let s = format!("{v:.1}");
    let s = s.strip_suffix(".0").unwrap_or(&s);
    if s == "-0" { "0".into() } else { s.to_string() }
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
            icons: Icons::load(&cc.egui_ctx)?,
            part: Part::default(),
            shape: None,
            mesh: Rc::default(),
            dirty: false,
            cam: Camera::default(),
            renderer: Arc::new(Mutex::new(renderer)),
            selection: Selection::None,
            hover_face: None,
            hover_edge: None,
            sketch: None,
            dim_edit: None,
            checked: Vec::new(),
            rename: None,
            recent: load_recent(),
            colors_dirty: false,
            snap: true,
            snap_objects: true,
            grid_step: 1.0,
            status: String::new(),
            need_fit: false,
            wireframe: false,
            view_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0)),
            image_tex: None,
            undo: Vec::new(),
            redo: Vec::new(),
            base: Part::default(),
            base_json: String::new(),
            building: false,
            fit_while_building: false,
            build_clean: false,
            face_hl: Vec::new(),
            paint_part: true,
            had_faces: false,
            ask_save: false,
            palette_open: false,
            file: None,
            saved_json: String::new(),
            title: String::new(),
            last_fillet: 2.0,
            last_chamfer: 1.0,
            last_depth: 10.0,
        };
        app.reset_history();
        // Файл детали можно передать аргументом командной строки (в т.ч. «Открыть с помощью»).
        if let Some(path) = std::env::args_os().nth(1) {
            app.load(Path::new(&path));
        }
        Ok(app)
    }

    /// Построение идёт по одному элементу за кадр, чтобы было видно, как строится деталь,
    /// и окно не замирало на время долгого пересчёта.
    fn rebuild(&mut self, frame: &eframe::Frame) {
        let stop = self.sketch.as_ref().map(|e| e.id);
        if !self.building {
            self.build_clean = self.history_clean();
        }
        let (shape, pending) = self.part.rebuild_step(stop, 1);
        self.shape = shape;
        self.mesh = self.part.mesh(stop);
        if let Some(gl) = frame.gl() {
            self.renderer.lock().unwrap().upload(gl, &self.mesh);
        }
        self.colors_dirty = true;
        self.hover_face = None;
        self.hover_edge = None;
        // Номера граней и рёбер после пересчёта другие.
        if matches!(self.selection, Selection::Face(_) | Selection::Faces(_) | Selection::Edges(_)) {
            self.selection = Selection::None;
        }
        match pending {
            Some(i) => {
                let name = self.part.features.get(i).map_or("", |f| f.name.as_str());
                self.status = format!("Построение: {name} ({} из {})", i + 1, self.part.features.len());
                self.building = true;
                self.dirty = true;
                if self.fit_while_building {
                    self.need_fit = true;
                }
            }
            None => {
                if self.building {
                    self.status.clear();
                    self.building = false;
                }
                self.fit_while_building = false;
                self.dirty = false;
                // Скругление могло переставить свои рёбра — это не правка пользователя.
                if self.build_clean {
                    self.commit_base();
                }
            }
        }
    }

    fn upload_colors(&mut self, frame: &eframe::Frame) {
        if let Some(gl) = frame.gl() {
            self.renderer.lock().unwrap().upload_colors(gl, &self.part.vertex_colors(&self.mesh, &self.face_hl));
        }
        self.colors_dirty = false;
    }

    // ---------- отмена и повтор ----------

    fn history_clean(&self) -> bool {
        serde_json::to_string(&self.part).is_ok_and(|json| json == self.base_json)
    }

    /// Текущее состояние детали становится «зафиксированным» без нового шага истории.
    fn commit_base(&mut self) {
        self.base = self.part.clone();
        self.base_json = serde_json::to_string(&self.part).unwrap_or_default();
    }

    fn reset_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.commit_base();
        self.saved_json = self.base_json.clone();
    }

    /// В конце кадра, когда кнопка мыши отпущена и ничего не вводится, запоминает сделанное изменение.
    fn track_history(&mut self, ctx: &egui::Context) {
        if self.dirty || self.building {
            return;
        }
        if ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let Ok(json) = serde_json::to_string(&self.part) else { return };
        if json != self.base_json {
            let previous = std::mem::replace(&mut self.base, self.part.clone());
            self.undo.push(previous);
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.base_json = json;
        }
    }

    fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            let current = std::mem::replace(&mut self.base, previous.clone());
            self.redo.push(current);
            self.restore(previous);
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let current = std::mem::replace(&mut self.base, next.clone());
            self.undo.push(current);
            self.restore(next);
        }
    }

    fn restore(&mut self, part: Part) {
        self.part = part;
        self.base_json = serde_json::to_string(&self.part).unwrap_or_default();
        self.dirty = true;
        self.rename = None;
        self.dim_edit = None;
        let part = &self.part;
        self.checked.retain(|c| part.feature(*c).is_some());
        if let Some(edit) = &mut self.sketch {
            if part.sketch(edit.id).is_some() {
                edit.selected = None;
                edit.dim_first = None;
                edit.op = None;
                edit.stop_chain();
            } else {
                self.sketch = None;
                self.cam.set_iso();
            }
        }
        self.selection = match self.selection {
            Selection::Feature(id) if self.part.feature(id).is_some() => Selection::Feature(id),
            Selection::Plane(i) => Selection::Plane(i),
            _ => Selection::None,
        };
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
        let id = self.part.add("Эскиз", FeatureKind::Sketch(Sketch::new(plane, vec![])));
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
            let stub = |s: &SketchShape| matches!(s, SketchShape::Path { points, .. } if points.len() < 2);
            while let Some(i) = sketch.shapes.iter().position(stub) {
                sketch.remove_shape(i);
            }
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
                FeatureKind::Cut { sketch, depth: self.last_depth, through_all: false, flip: false },
            )
        } else {
            self.part.add("Бобышка-Вытянуть", FeatureKind::Boss { sketch, depth: self.last_depth, flip: false })
        };
        self.selection = Selection::Feature(id);
        self.status.clear();
        self.dirty = true;
    }

    /// Скругление или фаска на выбранных рёбрах; для выбранных граней — на всех их рёбрах.
    fn add_edge_feature(&mut self, chamfer: bool) {
        let face_edges = |list: &[usize]| {
            let mut out: Vec<DVec3> = Vec::new();
            for face in list.iter().filter_map(|&i| self.mesh.faces.get(i)) {
                for e in &face.edges {
                    if !out.iter().any(|o| (*o - *e).length() < 1e-4) {
                        out.push(*e);
                    }
                }
            }
            out
        };
        let edges: Vec<DVec3> = match &self.selection {
            Selection::Edges(list) => list.iter().filter_map(|&i| self.mesh.edges.get(i)).map(|e| e.mid).collect(),
            Selection::Face(i) => face_edges(&[*i]),
            Selection::Faces(list) => face_edges(list),
            _ => vec![],
        };
        if edges.is_empty() {
            self.status = "Сначала выберите рёбра или грани детали (Ctrl+щелчок — добавить к выбранным)".into();
            return;
        }
        let id = if chamfer {
            self.part.add("Фаска", FeatureKind::Chamfer { edges, distance: self.last_chamfer })
        } else {
            self.part.add("Скругление", FeatureKind::Fillet { edges, radius: self.last_fillet })
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
        self.checked.clear();
        self.rename = None;
        self.cam = Camera::default();
        self.status.clear();
        self.dirty = true;
        self.file = None;
        self.reset_history();
    }

    /// Кнопка «Сохранить» и Ctrl+S: у открытого файла сначала спрашиваем, куда сохранять.
    fn save(&mut self) {
        if self.file.is_some() {
            self.ask_save = true;
        } else {
            self.save_as();
        }
    }

    fn save_question(&mut self, ctx: &egui::Context) {
        let Some(path) = self.file.clone().filter(|_| self.ask_save) else {
            self.ask_save = false;
            return;
        };
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let (mut here, mut other, mut cancel) = (false, false, false);
        egui::Window::new("Сохранение")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("Сохранить под текущим названием «{name}» или сохранить как новый файл?"));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    here = ui.button("Сохранить под текущим").clicked();
                    other = ui.button("Сохранить как…").clicked();
                    cancel = ui.button("Отмена").clicked();
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if here || other || cancel {
            self.ask_save = false;
        }
        if here {
            self.write_to(&path);
        } else if other {
            self.save_as();
        }
    }

    fn save_as(&mut self) {
        let name = self
            .file
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("деталь.ms3d".into(), |n| n.to_string_lossy().into_owned());
        if let Some(path) =
            rfd::FileDialog::new().add_filter("Деталь MySolid3D", &["ms3d"]).set_file_name(name).save_file()
        {
            self.write_to(&path);
        }
    }

    fn write_to(&mut self, path: &Path) {
        let result = serde_json::to_string_pretty(&self.part)
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(path, json).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => {
                self.remember_file(path, true);
                self.file = Some(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
                self.saved_json = serde_json::to_string(&self.part).unwrap_or_default();
                format!("Сохранено: {}", path.display())
            }
            Err(e) => format!("Не удалось сохранить: {e}"),
        };
    }

    /// Ставит файл первым в списке последних (или убирает его оттуда) и сохраняет список.
    fn remember_file(&mut self, path: &Path, keep: bool) {
        // Полный путь — чтобы файл находился при запуске из другой папки.
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| *p != path);
        if keep {
            self.recent.insert(0, path);
            self.recent.truncate(RECENT_COUNT);
        }
        if let Some(list) = recent_list_path() {
            let text: String = self.recent.iter().map(|p| format!("{}\n", p.display())).collect();
            // Список — удобство, а не данные: если записать не удалось, работу это не останавливает.
            let _ = list.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(list, text);
        }
    }

    fn open(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Деталь MySolid3D", &["ms3d"]).pick_file() {
            self.load(&path);
        }
    }

    fn load(&mut self, path: &Path) {
        let result = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|json| serde_json::from_str::<Part>(&json).map_err(|e| e.to_string()));
        match result {
            Ok(part) => {
                self.new_part();
                self.part = part;
                self.reset_history();
                self.file = Some(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
                self.need_fit = true;
                self.fit_while_building = true;
                self.remember_file(path, true);
                self.status = format!("Открыто: {}", path.display());
            }
            Err(e) => {
                // Файл пропал или испорчен — в списке последних ему не место.
                self.remember_file(path, false);
                self.status = format!("Не удалось открыть {}: {e}", path.display());
            }
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
            let (mut undo, mut redo) = (false, false);
            ui.add_enabled_ui(!self.undo.is_empty(), |ui| {
                undo = tool_button(ui, &self.icons.undo, "Отменить (Ctrl+Z)", false);
            });
            ui.add_enabled_ui(!self.redo.is_empty(), |ui| {
                redo = tool_button(ui, &self.icons.redo, "Повторить (Ctrl+Y)", false);
            });
            if undo {
                self.undo();
            }
            if redo {
                self.redo();
            }
            ui.separator();
            if self.sketch.is_some() {
                self.sketch_tools(ui);
            } else {
                self.part_tools(ui);
            }
            ui.separator();
            self.view_tools(ui);
        });
    }

    fn part_tools(&mut self, ui: &mut egui::Ui) {
        if tool_button(ui, &self.icons.new_file, "Новая деталь", false) {
            self.new_part();
        }
        if tool_button(ui, &self.icons.open_file, "Открыть деталь…", false) {
            self.open();
        }
        if tool_button(ui, &self.icons.save_file, "Сохранить деталь (Ctrl+S)", false) {
            self.save();
        }
        ui.menu_button("Файл", |ui| {
            if ui.button("Сохранить как…").clicked() {
                self.save_as();
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
            if !self.recent.is_empty() {
                ui.separator();
                ui.weak("Последние файлы");
                for path in self.recent.clone() {
                    let name = path
                        .file_name()
                        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    if ui.button(name).on_hover_text(path.display().to_string()).clicked() {
                        self.load(&path);
                        ui.close();
                    }
                }
            }
        });
        ui.separator();
        if tool_button(ui, &self.icons.sketch, "Эскиз: новый эскиз на выбранной плоскости или плоской грани", false) {
            self.start_sketch();
        }
        if tool_button(ui, &self.icons.extrude_boss, "Бобышка-вытянуть: вытянуть эскиз в тело", false) {
            self.add_extrude(false);
        }
        if tool_button(ui, &self.icons.extrude_cut, "Вырез-вытянуть: вырезать эскизом из тела", false) {
            self.add_extrude(true);
        }
        if tool_button(ui, &self.icons.fillet, "Скругление выбранных рёбер", false) {
            self.add_edge_feature(false);
        }
        if tool_button(ui, &self.icons.chamfer, "Фаска на выбранных рёбрах", false) {
            self.add_edge_feature(true);
        }
    }

    fn sketch_tools(&mut self, ui: &mut egui::Ui) {
        let icons = &self.icons;
        let Some(edit) = self.sketch.as_mut() else { return };
        let mut add_image = false;
        ui.strong("Эскиз");

        macro_rules! tool {
            ($tool:expr, $icon:ident, $tip:expr) => {
                if tool_button(ui, &icons.$icon, $tip, edit.tool == $tool) {
                    edit.tool = $tool;
                }
            };
        }
        tool!(Tool::Select, select, "Выбор / перенос: щелчок выбирает фигуру, протяжка переносит её или её точку");
        tool!(Tool::Line, line, "Линия: щелчками задавайте точки контура; щелчок по первой точке замыкает его");
        tool!(Tool::Arc, arc, "Дуга: щелчок — конец дуги, второй щелчок — точка, через которую она проходит");
        tool!(Tool::Rect, rect, "Прямоугольник: протяжкой от угла до угла");
        tool!(Tool::Circle, circle, "Окружность: протяжкой от центра");
        tool!(
            Tool::Dimension,
            dimension,
            "Размер: укажите элемент (или два), выберите способ измерения и нажмите Enter.\n\
             Щелчок по отрезку, дуге или окружности — длина, радиус или диаметр.\n\
             Щелчок по двум элементам (точки, отрезки, центр окружности, начало координат) — расстояние \
             или угол и взаимосвязи: параллельность, перпендикулярность, коллинеарность, совпадение.\n\
             Первый элемент остаётся на месте, подгоняется второй."
        );
        ui.separator();

        macro_rules! op {
            ($kind:expr, $icon:ident, $tip:expr) => {
                if tool_button(ui, &icons.$icon, concat!($tip, "\n(действует на выбранную фигуру)"), edit.op == Some($kind)) {
                    if edit.selected.is_none() {
                        self.status = "Сначала выберите фигуру инструментом «Выбор»".into();
                    } else {
                        edit.op = if edit.op == Some($kind) { None } else { Some($kind) };
                    }
                }
            };
        }
        op!(OpKind::Rotate, rotate, "Поворот");
        op!(OpKind::Scale, scale, "Масштаб");
        op!(OpKind::Mirror, mirror, "Зеркальное отражение");
        op!(OpKind::Copy, copy, "Копия со сдвигом");
        op!(OpKind::Offset, offset, "Смещение контура наружу или внутрь");
        op!(OpKind::CornerFillet, corner_fillet, "Скругление углов контура");
        op!(OpKind::CornerChamfer, corner_chamfer, "Фаска на углах контура");
        op!(OpKind::PatternGrid, pattern_grid, "Массив копий сеткой");
        op!(OpKind::PatternCircle, pattern_circle, "Массив копий по кругу");
        if tool_button(ui, &icons.delete, "Удалить выбранную фигуру (Del)", false) {
            if let Some(i) = edit.selected.take() {
                if let Some(sketch) = self.part.sketch_mut(edit.id) {
                    sketch.remove_shape(i);
                }
                edit.dim_first = None;
                edit.op = None;
                self.dim_edit = None;
            }
        }
        ui.separator();
        add_image = ui.button("Картинка").on_hover_text(
            "Вставить картинку-подложку: по ней удобно обводить контуры. Размер и положение — в свойствах эскиза.",
        ).clicked() || add_image;
        ui.separator();
        ui.checkbox(&mut self.snap, "Привязка к сетке");
        ui.checkbox(&mut self.snap_objects, "Привязка к объектам")
            .on_hover_text("К точкам и линиям этого и более ранних эскизов, к рёбрам и вершинам тела");
        ui.add(egui::DragValue::new(&mut self.grid_step).speed(0.1).range(0.1..=100.0).suffix(" мм"))
            .on_hover_text("Шаг сетки");
        if add_image {
            self.add_image();
        }
    }

    fn view_tools(&mut self, ui: &mut egui::Ui) {
        if tool_button(ui, &self.icons.view_iso, "Изометрия", false) {
            self.cam.set_iso();
        }
        if tool_button(ui, &self.icons.view_front, "Вид спереди", false) {
            self.cam.look_at_plane(&Plane::FRONT);
        }
        if tool_button(ui, &self.icons.view_top, "Вид сверху", false) {
            self.cam.look_at_plane(&Plane::TOP);
        }
        if tool_button(ui, &self.icons.view_right, "Вид справа", false) {
            self.cam.look_at_plane(&Plane::RIGHT);
        }
        if tool_button(ui, &self.icons.fit_view, "Вписать деталь в окно", false) {
            self.need_fit = true;
        }
        let (rect, center) = (self.view_rect, self.view_rect.center());
        if tool_button(ui, &self.icons.zoom_in, "Приблизить", false) {
            self.cam.zoom(rect, center, 1.25);
        }
        if tool_button(ui, &self.icons.zoom_out, "Отдалить", false) {
            self.cam.zoom(rect, center, 0.8);
        }
        if tool_button(ui, &self.icons.solid_view, "Показывать грани (тело)", !self.wireframe) {
            self.wireframe = false;
        }
        if tool_button(ui, &self.icons.wireframe, "Каркас: только рёбра", self.wireframe) {
            self.wireframe = true;
        }
    }

    // ---------- картинка-подложка и операции эскиза ----------

    fn add_image(&mut self) {
        let Some(id) = self.sketch.as_ref().map(|e| e.id) else { return };
        let Some(path) = rfd::FileDialog::new().add_filter("Картинка", &["png", "jpg", "jpeg"]).pick_file() else {
            return;
        };
        match image::image_dimensions(&path) {
            Ok((w, h)) if w > 0 && h > 0 => {
                // Полный путь — чтобы картинка находилась при открытии детали из другой папки.
                let path = std::path::absolute(&path).unwrap_or(path);
                let width = 100.0;
                let height = width * h as f64 / w as f64;
                if let Some(sketch) = self.part.sketch_mut(id) {
                    sketch.image = Some(SketchImage {
                        path: path.display().to_string(),
                        x: -width / 2.0,
                        y: -height / 2.0,
                        width,
                        opacity: 0.5,
                    });
                }
                self.status = "Картинка добавлена: размер и положение — в свойствах эскиза".into();
            }
            Ok(_) => self.status = "В картинке нет пикселей".into(),
            Err(e) => self.status = format!("Не удалось прочитать картинку: {e}"),
        }
    }

    /// Рисует подложку эскиза на его плоскости (до сетки и фигур).
    fn draw_image(&mut self, painter: &Painter, rect: Rect, id: u64, plane: &Plane) {
        let Some(img) = self.part.sketch(id).and_then(|s| s.image.clone()) else { return };
        if self.image_tex.as_ref().is_none_or(|(path, _)| *path != img.path) {
            let loaded = match image::open(&img.path) {
                Ok(pic) => {
                    // Огромные картинки уменьшаем: текстуре хватает 2048 пикселей.
                    let pic = if pic.width().max(pic.height()) > 2048 {
                        pic.resize(2048, 2048, image::imageops::FilterType::Triangle)
                    } else {
                        pic
                    };
                    let rgba = pic.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                    let tex = painter.ctx().load_texture("sketch_image", color, egui::TextureOptions::LINEAR);
                    Some((tex, rgba.height() as f32 / rgba.width() as f32))
                }
                Err(e) => {
                    self.status = format!("Не удалось открыть картинку {}: {e}", img.path);
                    None
                }
            };
            self.image_tex = Some((img.path.clone(), loaded));
        }
        let Some((tex, aspect)) = self.image_tex.as_ref().and_then(|(_, t)| t.as_ref()) else { return };
        let height = img.width * *aspect as f64;
        let corner = |dx: f64, dy: f64| self.cam.project(rect, plane.to_world(DVec2::new(img.x + dx, img.y + dy)));
        let color = Color32::from_white_alpha((img.opacity.clamp(0.0, 1.0) * 255.0) as u8);
        let vertex = |pos: Pos2, u: f32, v: f32| egui::epaint::Vertex { pos, uv: Pos2::new(u, v), color };
        let mut mesh = egui::Mesh::with_texture(tex.id());
        mesh.vertices.push(vertex(corner(0.0, height), 0.0, 0.0));
        mesh.vertices.push(vertex(corner(img.width, height), 1.0, 0.0));
        mesh.vertices.push(vertex(corner(img.width, 0.0), 1.0, 1.0));
        mesh.vertices.push(vertex(corner(0.0, 0.0), 0.0, 1.0));
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }

    /// Окно параметров операции над выбранной фигурой эскиза.
    fn sketch_op_window(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.sketch.as_mut() else { return };
        let Some(kind) = edit.op else { return };
        let id = edit.id;
        let Some(index) = edit.selected else {
            edit.op = None;
            return;
        };
        let (mut apply, mut close) = (false, false);
        egui::Window::new(kind.title())
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::LEFT_TOP, Vec2::new(250.0, 56.0))
            .show(ctx, |ui| {
                let p = &mut edit.params;
                let pivot_choice = |ui: &mut egui::Ui, p: &mut OpParams| {
                    ui.checkbox(&mut p.about_origin, "Вокруг начала координат")
                        .on_hover_text("Иначе — вокруг центра фигуры");
                };
                match kind {
                    OpKind::Rotate => {
                        ui.horizontal(|ui| {
                            ui.label("Угол");
                            ui.add(egui::DragValue::new(&mut p.angle).speed(1.0).max_decimals(1).suffix("°"));
                        });
                        pivot_choice(ui, p);
                    }
                    OpKind::Scale => {
                        ui.horizontal(|ui| {
                            ui.label("Коэффициент");
                            ui.add(egui::DragValue::new(&mut p.factor).speed(0.02).range(0.01..=100.0));
                        });
                        pivot_choice(ui, p);
                    }
                    OpKind::Mirror => {
                        let names = [
                            "Вертикаль через центр фигуры",
                            "Горизонталь через центр фигуры",
                            "Ось Y (x = 0)",
                            "Ось X (y = 0)",
                        ];
                        egui::ComboBox::from_label("Ось").selected_text(names[p.mirror_axis.min(3)]).show_ui(
                            ui,
                            |ui| {
                                for (i, name) in names.iter().enumerate() {
                                    ui.selectable_value(&mut p.mirror_axis, i, *name);
                                }
                            },
                        );
                        ui.checkbox(&mut p.keep, "Оставить исходную фигуру");
                    }
                    OpKind::Copy => {
                        ui.horizontal(|ui| {
                            ui.label("Сдвиг X");
                            ui.add(mm(&mut p.dx));
                            ui.label("Y");
                            ui.add(mm(&mut p.dy));
                        });
                    }
                    OpKind::Offset => {
                        ui.horizontal(|ui| {
                            ui.label("Расстояние");
                            ui.add(mm(&mut p.dist));
                        });
                        ui.weak("Плюс — наружу, минус — внутрь. Получится новая фигура.");
                    }
                    OpKind::CornerFillet | OpKind::CornerChamfer => {
                        ui.horizontal(|ui| {
                            ui.label(if kind == OpKind::CornerFillet { "Радиус" } else { "Размер фаски" });
                            ui.add(mm(&mut p.size).range(MIN_SIZE..=1.0e4));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Точка №");
                            ui.add(egui::DragValue::new(&mut p.vertex).range(0..=1000));
                        });
                        ui.weak("0 — все углы контура; номера точек видны в свойствах эскиза.");
                    }
                    OpKind::PatternGrid => {
                        ui.horizontal(|ui| {
                            ui.label("Столбцов");
                            ui.add(egui::DragValue::new(&mut p.cols).range(1..=100));
                            ui.label("Рядов");
                            ui.add(egui::DragValue::new(&mut p.rows).range(1..=100));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Шаг X");
                            ui.add(mm(&mut p.dx));
                            ui.label("Y");
                            ui.add(mm(&mut p.dy));
                        });
                    }
                    OpKind::PatternCircle => {
                        ui.horizontal(|ui| {
                            ui.label("Фигур всего");
                            ui.add(egui::DragValue::new(&mut p.count).range(2..=1000));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Сектор");
                            ui.add(egui::DragValue::new(&mut p.sweep).speed(1.0).range(1.0..=360.0).suffix("°"));
                        });
                        pivot_choice(ui, p);
                    }
                }
                ui.horizontal(|ui| {
                    apply = ui.button("Применить").clicked();
                    close = ui.button("Закрыть").clicked();
                });
            });
        if close {
            edit.op = None;
        }
        if apply {
            let params = edit.params.clone();
            match apply_op(&mut self.part, id, index, kind, &params) {
                Ok((select, message)) => {
                    if let Some(i) = select {
                        edit.selected = Some(i);
                    }
                    edit.op = None;
                    edit.dim_first = None;
                    self.status = message;
                }
                Err(e) => self.status = e,
            }
        }
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        ui.heading("Дерево построения");
        ui.add_space(4.0);
        let editing = self.sketch.as_ref().map(|e| e.id);
        ui.add_enabled_ui(editing.is_none(), |ui| {
            ui.weak("Плоскости");
            for (i, name) in PLANE_NAMES.iter().enumerate() {
                if ui.selectable_label(self.selection == Selection::Plane(i), *name).clicked() {
                    self.selection = Selection::Plane(i);
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.weak("Элементы");
                let any = !self.checked.is_empty();
                let group = ui
                    .add_enabled(any, egui::Button::new("В группу").small())
                    .on_hover_text("Собрать отмеченные галочками элементы в группу");
                let ungroup = ui
                    .add_enabled(any, egui::Button::new("Из группы").small())
                    .on_hover_text("Вынуть отмеченные элементы из групп");
                if group.clicked() {
                    self.part.add_group(&self.checked);
                    self.checked.clear();
                } else if ungroup.clicked() {
                    self.part.ungroup(&self.checked);
                    self.checked.clear();
                }
            });
            if self.part.features.is_empty() {
                ui.weak("(пусто)");
            }
            let (mut delete, mut disband, mut open_sketch) = (None, None, None);
            for (target, nested) in self.part.tree_rows() {
                ui.horizontal(|ui| {
                    if nested {
                        ui.add_space(14.0);
                    }
                    match target {
                        TreeItem::Group(id) => {
                            let Some(g) = self.part.groups.iter_mut().find(|g| g.id == id) else { return };
                            let (sign, tip) = if g.collapsed { ("+", "Развернуть группу") } else { ("−", "Свернуть группу") };
                            if ui.add(egui::Button::new(sign).small().min_size(Vec2::splat(18.0))).on_hover_text(tip).clicked() {
                                g.collapsed = !g.collapsed;
                            }
                            let name = format!("{} ({})", g.name, g.members.len());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text("Расформировать группу (элементы остаются)").clicked() {
                                    disband = Some(id);
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    let editing_name = self.rename.as_ref().is_some_and(|r| r.target == target);
                                    let shown = if editing_name { g.name.as_str() } else { name.as_str() };
                                    let hint = Some("Правая кнопка — переименовать".to_string());
                                    let (_, _, renamed) = name_cell(ui, &mut self.rename, target, shown, None, false, hint);
                                    // Правка только что открылась — править нужно само имя, без счётчика.
                                    if let Some(r) = self.rename.as_mut().filter(|r| r.target == target && !editing_name) {
                                        r.text = g.name.clone();
                                    }
                                    if let Some(new_name) = renamed {
                                        g.name = new_name;
                                    }
                                });
                            });
                        }
                        TreeItem::Feature(id) => {
                            let Some(f) = self.part.feature_mut(id) else { return };
                            let mut on = self.checked.contains(&id);
                            if ui.checkbox(&mut on, "").changed() {
                                self.checked.retain(|c| *c != id);
                                if on {
                                    self.checked.push(id);
                                }
                            }
                            let color = match (&f.error, &f.kind) {
                                (Some(_), _) => Some(ERROR_COLOR),
                                (None, FeatureKind::Sketch(_)) => Some(SKETCH_COLOR),
                                _ => None,
                            };
                            let is_sketch = matches!(f.kind, FeatureKind::Sketch(_));
                            let hint = if is_sketch {
                                "Двойной щелчок — открыть эскиз · правая кнопка — переименовать"
                            } else {
                                "Правая кнопка — переименовать"
                            };
                            let hover = Some(match &f.error {
                                Some(e) => format!("Ошибка: {e}\n{hint}"),
                                None => hint.to_string(),
                            });
                            let name = f.name.clone();
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text("Удалить элемент").clicked() {
                                    delete = Some(id);
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    let selected = self.selection == Selection::Feature(id);
                                    let (clicked, double, renamed) =
                                        name_cell(ui, &mut self.rename, target, &name, color, selected, hover);
                                    if clicked {
                                        self.selection = Selection::Feature(id);
                                    }
                                    if double && is_sketch {
                                        open_sketch = Some(id);
                                    }
                                    if let Some(new_name) = renamed {
                                        f.name = new_name;
                                    }
                                });
                            });
                        }
                    }
                });
            }
            if let Some(id) = disband {
                self.part.remove_group(id);
            }
            if let Some(id) = open_sketch.filter(|id| delete != Some(*id)) {
                self.rename = None;
                self.enter_sketch(id);
            }
            if let Some(id) = delete {
                self.part.remove(id);
                let part = &self.part;
                self.checked.retain(|c| part.feature(*c).is_some());
                self.rename = None;
                if matches!(self.selection, Selection::Feature(s) if part.feature(s).is_none()) {
                    self.selection = Selection::None;
                }
                self.dirty = true;
            }
        });
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
                    ui.weak("Ctrl+щелчок — выбрать несколько граней для скругления или фаски.");
                } else {
                    ui.label("Криволинейная грань");
                    ui.weak("Эскиз можно создать только на плоской грани.");
                }
            }
            Selection::Faces(ref list) => {
                ui.label(format!("Выбрано граней: {}", list.len()));
                ui.weak("Ctrl+щелчок — добавить или убрать грань. Скругление и фаска берут все рёбра выбранных граней.");
                ui.add_space(4.0);
                let fillet = ui.button("Скругление").clicked();
                let chamfer = ui.button("Фаска").clicked();
                if fillet || chamfer {
                    self.add_edge_feature(chamfer);
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
                    egui::DragValue::new(depth).speed(0.25).range(MIN_SIZE..=1.0e4).max_decimals(1).suffix(" мм"),
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
                let mut drop_image = false;
                if let Some(img) = &mut sketch.image {
                    ui.add_space(6.0);
                    ui.strong("Картинка-подложка");
                    let name = Path::new(&img.path).file_name().map(|n| n.to_string_lossy().into_owned());
                    ui.weak(name.unwrap_or_default()).on_hover_text(&img.path);
                    egui::Grid::new("sketch_image").num_columns(2).show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, label: &str, v: &mut f64, min: f64| {
                            ui.label(label);
                            let drag = egui::DragValue::new(v).speed(0.5).range(min..=1.0e5).max_decimals(1).suffix(" мм");
                            ui.add(drag);
                            ui.end_row();
                        };
                        row(ui, "Ширина", &mut img.width, MIN_SIZE);
                        row(ui, "X левого края", &mut img.x, -1.0e5);
                        row(ui, "Y нижнего края", &mut img.y, -1.0e5);
                        ui.label("Яркость");
                        ui.add(egui::Slider::new(&mut img.opacity, 0.05..=1.0));
                        ui.end_row();
                    });
                    drop_image = ui.button("Убрать картинку").clicked();
                }
                if drop_image {
                    sketch.image = None;
                }
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
                        egui::DragValue::new(size).speed(0.1).range(MIN_SIZE..=1.0e3).max_decimals(1).suffix(" мм");
                    changed |= ui.add(drag).changed();
                });
                ui.weak(format!("Рёбер: {}", edges.len()));
            }
        }
        if changed {
            match &feature.kind {
                FeatureKind::Boss { depth, .. } | FeatureKind::Cut { depth, .. } => self.last_depth = *depth,
                FeatureKind::Fillet { radius, .. } => self.last_fillet = *radius,
                FeatureKind::Chamfer { distance, .. } => self.last_chamfer = *distance,
                FeatureKind::Sketch(_) => {}
            }
        }
        if editing.is_none() {
            ui.separator();
            delete = ui.button("Удалить элемент").clicked();
        }

        if removed_shape {
            if let Some(e) = &mut self.sketch {
                e.selected = None;
                e.dim_first = None;
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
        self.view_rect = rect;
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
                    (None, Some(f)) if ctrl => {
                        let mut list = match &self.selection {
                            Selection::Face(i) => vec![*i],
                            Selection::Faces(list) => list.clone(),
                            _ => vec![],
                        };
                        match list.iter().position(|&i| i == f) {
                            Some(at) => drop(list.remove(at)),
                            None => list.push(f),
                        }
                        match list.len() {
                            0 => Selection::None,
                            1 => Selection::Face(list[0]),
                            _ => Selection::Faces(list),
                        }
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
            wireframe: self.wireframe,
        };
        let renderer = self.renderer.clone();
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
                renderer.lock().unwrap().paint(painter.gl(), &params);
            })),
        });

        self.draw_edge_highlights(&painter, rect);
        self.draw_errors(&painter, rect);
        self.draw_base_planes(&painter, rect);
        self.draw_sketches(&painter, rect);
        if let Some(mut edit) = self.sketch.take() {
            self.sketch_mode(ui, &resp, &painter, rect, &mut edit);
            self.sketch = Some(edit);
        }
        self.draw_axes(&painter, rect);
        self.dim_edit_popup(ui);
        if self.sketch.is_some() && exit_button(ui, rect) {
            self.exit_sketch();
        }
        self.color_palette(ui, rect);
    }

    /// Панель цветов в углу вида: красит выбранную грань, а если грань не выбрана — деталь целиком.
    fn color_palette(&mut self, ui: &egui::Ui, rect: Rect) {
        if self.sketch.is_some() || self.mesh.indices.is_empty() {
            return;
        }
        let faces: Vec<DVec3> = match &self.selection {
            Selection::Face(i) => self.mesh.faces.get(*i).map(|f| f.origin).into_iter().collect(),
            Selection::Faces(list) => list.iter().filter_map(|&i| self.mesh.faces.get(i)).map(|f| f.origin).collect(),
            _ => vec![],
        };
        // Выбрали грань — по умолчанию красится она; сняли выбор — вся деталь. Переключатель решает вручную.
        if faces.is_empty() == self.had_faces {
            self.paint_part = faces.is_empty();
            self.had_faces = !faces.is_empty();
        }
        let part_mode = self.paint_part;
        let mut pick: Option<Option<[u8; 3]>> = None;
        let mut mode = part_mode;
        egui::Area::new(egui::Id::unique("color_palette")).fixed_pos(rect.left_top() + Vec2::splat(8.0)).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.weak("Красить:");
                        if ui
                            .selectable_label(!mode, "Грань")
                            .on_hover_text("Выберите грань (Ctrl+щелчок — несколько) и цвет")
                            .clicked()
                        {
                            mode = false;
                        }
                        if ui.selectable_label(mode, "Деталь").on_hover_text("Покрасить весь объект").clicked() {
                            mode = true;
                        }
                        ui.separator();
                        for c in PALETTE {
                            if swatch(ui, c) {
                                pick = Some(Some(c));
                            }
                        }
                        let tip = if part_mode {
                            "Вернуть стандартный цвет детали и всех граней"
                        } else {
                            "Вернуть выбранным граням цвет детали"
                        };
                        if ui.small_button("Сброс").on_hover_text(tip).clicked() {
                            pick = Some(None);
                        }
                        let arrow = if self.palette_open { "▴" } else { "▾" };
                        if ui.small_button(arrow).on_hover_text("Переходные цвета между основными").clicked() {
                            self.palette_open = !self.palette_open;
                        }
                    });
                    if self.palette_open {
                        // Раскладушка: между каждой парой соседних цветов — три промежуточных оттенка.
                        for t in [0.25f32, 0.5, 0.75] {
                            ui.horizontal(|ui| {
                                for pair in PALETTE.windows(2) {
                                    let mix = |k: usize| (pair[0][k] as f32 * (1.0 - t) + pair[1][k] as f32 * t).round() as u8;
                                    let c = [mix(0), mix(1), mix(2)];
                                    if swatch(ui, c) {
                                        pick = Some(Some(c));
                                    }
                                }
                            });
                        }
                    }
                    if !part_mode && faces.is_empty() {
                        ui.colored_label(SELECT_COLOR, "Щёлкните по грани детали, которую нужно покрасить");
                    }
                });
            },
        );
        self.paint_part = mode;
        if let Some(color) = pick {
            if part_mode {
                self.part.color = color;
                // Красим весь объект — прежние цвета отдельных граней больше не нужны.
                self.part.face_colors.clear();
            } else if faces.is_empty() {
                self.status = "Сначала выберите грань (Ctrl+щелчок — несколько) или переключитесь на «Деталь»".into();
            } else {
                for point in faces {
                    self.part.set_face_color(point, color);
                }
                // Снимаем выделение — иначе подсветка выбора закрывает новый цвет.
                self.selection = Selection::None;
            }
            self.colors_dirty = true;
        }
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

    /// Что не удалось построить, показывается красным прямо на детали: рёбра и точки несработавших
    /// скруглений и фасок, контуры эскизов несработавших бобышек и вырезов.
    fn draw_errors(&self, painter: &Painter, rect: Rect) {
        if self.sketch.is_some() || self.building {
            return;
        }
        let stroke = Stroke::new(3.0, ERROR_COLOR);
        for feature in self.part.features.iter().filter(|f| f.error.is_some()) {
            match &feature.kind {
                FeatureKind::Fillet { edges, .. } | FeatureKind::Chamfer { edges, .. } => {
                    for mid in edges {
                        if let Some(edge) = self.mesh.edge_at(*mid).and_then(|i| self.mesh.edges.get(i)) {
                            painter.line(edge.points.iter().map(|p| self.cam.project(rect, *p)).collect(), stroke);
                        }
                        painter.circle_filled(self.cam.project(rect, *mid), 5.0, ERROR_COLOR);
                    }
                }
                FeatureKind::Boss { sketch, .. } | FeatureKind::Cut { sketch, .. } => {
                    if let Some(sketch) = self.part.sketch(*sketch) {
                        for shape in &sketch.shapes {
                            draw_shape(painter, &self.cam, rect, &sketch.plane, shape, stroke);
                        }
                    }
                }
                FeatureKind::Sketch(_) => {}
            }
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
        // Линии пересечения базовых плоскостей — едва заметным пунктиром в обе стороны от начала координат.
        let reach = self.cam.target.length() + 2.0 * rect.width().max(rect.height()) as f64 / self.cam.scale;
        for (axis, _, color) in axes {
            let ends = [self.cam.project(rect, axis * -reach), self.cam.project(rect, axis * reach)];
            if ends.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
                painter.extend(egui::Shape::dashed_line(&ends, Stroke::new(1.0, color.gamma_multiply(0.3)), 7.0, 6.0));
            }
        }
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

    /// Что видно в плоскости эскиза `id` и подходит для привязки, кроме него самого: начало координат,
    /// более ранние эскизы и рёбра тела (всё проецируется на плоскость эскиза, как и на экране).
    fn snap_targets(&self, id: u64, plane: &Plane) -> (Vec<DVec2>, Vec<Vec<DVec2>>) {
        let (mut points, mut lines) = (vec![DVec2::ZERO], Vec::new());
        for feature in &self.part.features {
            if feature.id == id {
                break;
            }
            let FeatureKind::Sketch(other) = &feature.kind else { continue };
            let local = |p: DVec2| plane.to_local(other.plane.to_world(p));
            for shape in &other.shapes {
                points.extend(shape_snap_points(shape).into_iter().map(local));
                lines.push(shape.outline().into_iter().map(local).collect());
            }
        }
        for edge in &self.mesh.edges {
            if let (Some(a), Some(b)) = (edge.points.first(), edge.points.last()) {
                points.extend([plane.to_local(*a), plane.to_local(*b), plane.to_local(edge.mid)]);
            }
            lines.push(edge.points.iter().map(|p| plane.to_local(*p)).collect());
        }
        (points, lines)
    }

    /// Режим эскиза: рисование фигур, перенос, размеры.
    fn sketch_mode(&mut self, ui: &egui::Ui, resp: &Response, painter: &Painter, rect: Rect, edit: &mut SketchEdit) {
        let Some(plane) = self.part.sketch(edit.id).map(|s| s.plane) else { return };
        self.draw_image(painter, rect, edit.id, &plane);
        self.draw_grid(painter, rect, &plane);

        let cam = self.cam.clone();
        let (snap, step) = (self.snap, self.grid_step.max(0.1));
        // Привязка к объектам: сначала к точкам, потом к линиям; без неё — к сетке.
        let (extra_points, extra_lines) =
            if self.snap_objects { self.snap_targets(edit.id, &plane) } else { Default::default() };
        let skip = if edit.tool == Tool::Select { edit.selected } else { None };
        let (own_points, own_lines) = match (self.snap_objects, self.part.sketch(edit.id)) {
            (true, Some(sketch)) => sketch_snap_targets(sketch, skip),
            _ => Default::default(),
        };
        let reach = 10.0 / cam.scale;
        let nearest_object = |raw: DVec2| -> Option<DVec2> {
            let mut best: Option<(f64, DVec2)> = None;
            for q in extra_points.iter().chain(&own_points) {
                let d = (*q - raw).length();
                if d < reach && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, *q));
                }
            }
            if best.is_none() {
                for line in extra_lines.iter().chain(&own_lines) {
                    for w in line.windows(2) {
                        let ab = w[1] - w[0];
                        let len2 = ab.length_squared();
                        let t = if len2 < 1e-12 { 0.0 } else { ((raw - w[0]).dot(ab) / len2).clamp(0.0, 1.0) };
                        let q = w[0] + ab * t;
                        let d = (q - raw).length();
                        if d < reach * 0.7 && best.is_none_or(|(b, _)| d < b) {
                            best = Some((d, q));
                        }
                    }
                }
            }
            best.map(|(_, q)| q)
        };
        let snap_pt = |p: DVec2| {
            nearest_object(p).unwrap_or_else(|| if snap { (p / step).round() * step } else { p })
        };
        let raw_at = |pos: Pos2| cam.screen_to_plane(rect, pos, &plane);
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let press = ui.input(|i| i.pointer.press_origin());
        let primary = PointerButton::Primary;
        let px = 1.0 / cam.scale; // миллиметров в одном пикселе
        let no_focus = ui.memory(|m| m.focused().is_none());
        // Щелчок по подписи размера открывает ввод значения и инструменту не передаётся.
        let on_label = resp.hover_pos().is_some_and(|p| edit.labels.iter().any(|r| r.expand(4.0).contains(p)));
        let clicked = resp.clicked() && !on_label;
        if on_label {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let Some(sketch) = self.part.sketch_mut(edit.id) else { return };
        let mut preview = None;
        // Предпросмотр следующего сегмента контура (в координатах эскиза).
        let mut chain_preview: Option<Vec<DVec2>> = None;

        // Завершить рисование контура; контур из одной точки удаляется.
        let finish_chain = |sketch: &mut Sketch, edit: &mut SketchEdit| {
            if let Some(c) = edit.chain {
                if matches!(sketch.shapes.get(c), Some(SketchShape::Path { points, .. }) if points.len() < 2) {
                    sketch.remove_shape(c);
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
        if edit.tool != Tool::Dimension {
            edit.dim_first = None;
            edit.placing = None;
        }
        // Пока открыто окно размера, Esc закрывает только его.
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.dim_edit.is_none() {
            edit.drag_start = None;
            if edit.chain.is_some() {
                finish_chain(sketch, edit);
            } else if !(edit.placing.take().is_some() | edit.dim_first.take().is_some()) {
                edit.tool = Tool::Select;
            }
        }
        if no_focus && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            edit.stop_chain();
            if let Some(i) = edit.selected.take().filter(|&i| i < sketch.shapes.len()) {
                sketch.remove_shape(i);
                self.dim_edit = None;
                edit.dim_first = None;
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

                if let (true, Some(p)) = (clicked, target) {
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
                } else if clicked {
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
            // Щелчки инструмента «Размер» разбираются ниже — им нужны области подписей.
            Tool::Dimension => {}
        }

        // Отрисовка фигур и поставленных пользователем размеров.
        for (i, s) in sketch.shapes.iter().enumerate() {
            let color = if edit.selected == Some(i) { SELECT_COLOR } else { SKETCH_COLOR };
            draw_shape(painter, &cam, rect, &plane, s, Stroke::new(2.0, color));
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
        sketch.prune_dim_places();
        let mut labels = Vec::new();
        for (i, dim) in sketch.dims.iter().enumerate() {
            draw_dim(painter, &cam, rect, &plane, sketch, dim, sketch.dim_place(dim), i, &mut labels);
        }
        // Пока фигура рисуется, её величина показывается рядом с курсором (это не размер).
        let mut readout = None;
        if let Some(pts) = chain_preview {
            if let [a, b] = pts[..] {
                readout = Some(fmt_mm((b - a).length()));
            }
            let line: Vec<Pos2> = pts.iter().map(|p| cam.project(rect, plane.to_world(*p))).collect();
            painter.line(line, Stroke::new(1.5, Color32::WHITE));
        }
        if let Some(s) = &preview {
            draw_shape(painter, &cam, rect, &plane, s, Stroke::new(1.5, Color32::WHITE));
            readout = match s {
                SketchShape::Rect { w, h, .. } => Some(format!("{} × {}", fmt_mm(*w), fmt_mm(*h))),
                SketchShape::Circle { r, .. } => Some(format!("Ø{}", fmt_mm(r * 2.0))),
                SketchShape::Path { .. } => None,
            };
        }
        if let (Some(text), Some(p)) = (readout, pointer) {
            let at = p + Vec2::new(16.0, 16.0);
            painter.text(at, Align2::LEFT_TOP, text, FontId::proportional(13.0), Color32::from_gray(200));
        }

        // Маркер привязки к объекту под курсором.
        if matches!(edit.tool, Tool::Line | Tool::Arc | Tool::Rect | Tool::Circle) {
            if let Some(q) = pointer.and_then(raw_at).and_then(nearest_object) {
                let c = cam.project(rect, plane.to_world(q));
                let stroke = Stroke::new(2.0, Color32::from_rgb(255, 210, 60));
                painter.rect_stroke(Rect::from_center_size(c, Vec2::splat(10.0)), 0.0, stroke, egui::StrokeKind::Middle);
            }
        }
        // Щелчок по подписи размера — правка значения.
        let dim_tool = edit.tool == Tool::Dimension;
        let mut request: Option<(Dim, Option<usize>, Pos2)> = None;
        edit.labels = labels.iter().map(|l| l.rect).collect();
        if resp.clicked() {
            if let Some(l) = pointer.and_then(|p| labels.iter().find(|l| l.rect.expand(4.0).contains(p))) {
                request = Some((l.dim, Some(l.index), l.rect.left_top() - Vec2::new(8.0, 6.0)));
                edit.dim_first = None;
            }
        }
        // Размер ставится как в SolidWorks: выбрали элемент (или два), ведёте курсор — размерная линия
        // (а у расстояния и способ измерения) следует за ним, щелчок фиксирует место и открывает ввод значения.
        let mut req_place: Option<DVec2> = None;
        if let Some(mut dim) = edit.placing {
            let cursor = pointer.and_then(raw_at);
            if let (Dim::Distance(a, b, _), Some(p)) = (dim, cursor) {
                if sketch.dim_has_modes(&dim) {
                    if let Some((from, to)) = sketch.dim_span(&Dim::Distance(a, b, DimMode::Aligned)) {
                        dim = Dim::Distance(a, b, placing_mode(from, to, p));
                        edit.placing = Some(dim);
                    }
                }
            }
            if resp.clicked() && request.is_none() {
                // Щелчок по другому элементу, с которым возможен размер, начинает размер между двумя.
                let hit = cursor.and_then(|p| sketch.pick_ref(p, 8.0 * px));
                let pairs = edit.dim_first.zip(hit).is_some_and(|(f, h)| f != h && sketch.pair_dim(f, h).is_some());
                if !pairs {
                    let pos = pointer.unwrap_or(rect.center()) + Vec2::new(14.0, 14.0);
                    request = Some((dim, None, pos));
                    req_place = cursor;
                    edit.placing = None;
                    edit.dim_first = None;
                }
            }
        }
        // Инструмент «Размер»: щелчок по элементу — его размер, по второму элементу — размер между двумя.
        if dim_tool && resp.clicked() && request.is_none() {
            let hit = pointer.and_then(raw_at).and_then(|p| sketch.pick_ref(p, 8.0 * px));
            let pair = edit.dim_first.zip(hit).filter(|(a, b)| a != b).and_then(|(a, b)| sketch.pair_dim(a, b));
            edit.dim_first = None;
            edit.placing = None;
            self.dim_edit = None;
            self.status.clear();
            match (pair, hit) {
                (Some(dim), _) => {
                    edit.placing = Some(dim);
                    self.status = if sketch.dim_has_modes(&dim) {
                        "Ведите курсор: по горизонтали, по вертикали или вдоль линии между точками — щелчок ставит размер".into()
                    } else {
                        "Щёлкните там, где должна стоять размерная линия".into()
                    };
                }
                (None, Some(r)) => {
                    edit.dim_first = Some(r);
                    match sketch.element_dim(r) {
                        Some(dim) => {
                            edit.placing = Some(dim);
                            self.status = "Щёлкните там, где поставить размер, или укажите второй элемент".into();
                        }
                        None => self.status = "Укажите второй элемент — точку или отрезок".into(),
                    }
                }
                (None, None) => {}
            }
        }
        if let Some((dim, stored, pos)) = request {
            self.dim_edit = Some(DimEdit { dim, stored, text: String::new(), pos, focus: true, place: req_place });
        }

        // Подсветка элементов, к которым относится размер.
        let to_screen = |p: DVec2| cam.project(rect, plane.to_world(p));
        let mark = |r: DimRef, stroke: Stroke| {
            let pts: Vec<Pos2> = sketch.ref_outline(r).into_iter().map(to_screen).collect();
            if pts.len() == 1 {
                painter.circle_stroke(pts[0], 6.0, stroke);
            } else {
                painter.line(pts, stroke);
            }
        };
        if dim_tool {
            if let Some(r) = resp.hover_pos().and_then(raw_at).and_then(|p| sketch.pick_ref(p, 8.0 * px)) {
                mark(r, Stroke::new(3.0, Color32::WHITE));
            }
            if let Some(r) = edit.dim_first {
                mark(r, Stroke::new(3.0, SELECT_COLOR));
            }
            if let Some(dim) = edit.placing {
                match dim {
                    Dim::Size(r) => mark(r, Stroke::new(3.0, SELECT_COLOR)),
                    Dim::Distance(a, b, _) | Dim::Angle(a, b) => {
                        mark(a, Stroke::new(3.0, SELECT_COLOR));
                        mark(b, Stroke::new(3.0, SELECT_COLOR));
                    }
                }
                if let Some(p) = pointer.and_then(raw_at) {
                    draw_dim(painter, &cam, rect, &plane, sketch, &dim, Some(p), usize::MAX, &mut Vec::new());
                }
            }
        }
        if let Some(de) = &self.dim_edit {
            let stroke = Stroke::new(3.0, SELECT_COLOR);
            match de.dim {
                Dim::Size(r) => mark(r, stroke),
                Dim::Distance(a, b, _) | Dim::Angle(a, b) => {
                    mark(a, stroke);
                    mark(b, stroke);
                }
            }
            // Размер ещё не поставлен — показываем его там, где его разместили.
            if de.stored.is_none() {
                draw_dim(painter, &cam, rect, &plane, sketch, &de.dim, de.place, usize::MAX, &mut Vec::new());
            }
        }
    }

    fn dim_edit_popup(&mut self, ui: &egui::Ui) {
        let Some(de) = &mut self.dim_edit else { return };
        let sketch = self.sketch.as_ref().and_then(|e| self.part.sketch(e.id));
        let Some((sketch, value)) = sketch.and_then(|s| Some((s, s.dim_value(&de.dim)?))) else {
            // Элементы размера исчезли (фигуру удалили).
            self.dim_edit = None;
            return;
        };
        let modes = sketch.dim_has_modes(&de.dim);
        let hint = fmt_mm(value);
        let (pair, unit) = match de.dim {
            Dim::Size(_) => (None, "мм"),
            Dim::Distance(a, b, _) => (Some((a, b)), "мм"),
            Dim::Angle(a, b) => (Some((a, b)), "°"),
        };
        let relations = pair.map(|(a, b)| sketch.relations(a, b)).unwrap_or_default();
        let stored = de.stored.filter(|i| *i < sketch.dims.len());
        let (mut relation, mut remove) = (None, false);
        let area = egui::Area::new(egui::Id::unique("dim_edit")).fixed_pos(de.pos).order(egui::Order::Foreground).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style())
                    .show(ui, |ui| {
                        let field = ui
                            .horizontal(|ui| {
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut de.text).hint_text(hint).desired_width(70.0),
                                );
                                ui.weak(unit);
                                if let (true, Dim::Distance(_, _, mode)) = (modes, &mut de.dim) {
                                    for (m, name) in [
                                        (DimMode::Horizontal, "По X"),
                                        (DimMode::Vertical, "По Y"),
                                        (DimMode::Aligned, "Напрямую"),
                                    ] {
                                        ui.selectable_value(mode, m, name);
                                    }
                                }
                                r
                            })
                            .inner;
                        if stored.is_some() {
                            remove = ui.button("Удалить размер").clicked();
                        } else {
                            ui.weak("Enter — поставить размер · Esc — отмена");
                        }
                        if !relations.is_empty() {
                            ui.horizontal(|ui| {
                                for &r in &relations {
                                    let name = match r {
                                        Relation::Parallel => "Параллельно",
                                        Relation::Perpendicular => "Перпендикулярно",
                                        Relation::Collinear => "Коллинеарно",
                                        Relation::Coincident => "Совпадение",
                                    };
                                    if ui.button(name).clicked() {
                                        relation = Some(r);
                                    }
                                }
                            });
                        }
                        field
                    })
                    .inner
            },
        );
        let field = area.inner;
        let inside = ui.ctx().pointer_latest_pos().is_some_and(|p| area.response.rect.contains(p));
        let (mut apply, mut close) = (false, relation.is_some() || remove);
        if de.focus {
            field.request_focus();
            de.focus = false;
        } else if field.lost_focus() {
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            apply = enter;
            // Щелчок по кнопке внутри окна его не закрывает — возвращаем фокус полю ввода.
            close |= enter || esc || !inside;
            de.focus = !close;
        }
        let dim = de.dim;
        let place = de.place;
        let text = de.text.trim().replace(',', ".");
        let sketch = self.sketch.as_ref().and_then(|e| self.part.sketch_mut(e.id));
        let mut result = None;
        if let Some(sketch) = sketch {
            if remove {
                if let Some(i) = stored {
                    sketch.dims.remove(i);
                }
            } else if let (Some(relation), Some((a, b))) = (relation, pair) {
                result = Some(sketch.apply_relation(a, b, relation));
            } else if apply {
                // Enter ставит размер в эскиз (или обновляет способ измерения у поставленного).
                match stored {
                    Some(i) => {
                        let old = std::mem::replace(&mut sketch.dims[i], dim);
                        sketch.rekey_dim_place(old, dim);
                    }
                    None => {
                        let _ = sketch.store_dim(dim);
                        if let Some(place) = place {
                            sketch.set_dim_place(dim, place);
                        }
                    }
                }
                if !text.is_empty() {
                    result = Some(match text.parse::<f64>() {
                        Ok(v) if v.is_finite() => sketch.set_dim(&dim, (v * 10.0).round() / 10.0),
                        _ => Err("введите число".into()),
                    });
                }
            }
        }
        match result {
            Some(Ok(())) => self.status.clear(),
            Some(Err(e)) => self.status = format!("Размер не изменён: {e}"),
            None => {}
        }
        if close {
            self.dim_edit = None;
            if let Some(e) = &mut self.sketch {
                e.dim_first = None;
            }
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
                    let drag = egui::DragValue::new(v).speed(0.25).range(min..=1.0e5).max_decimals(1).suffix(" мм");
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
        sketch.remove_shape(i);
        changed = true;
    }
    (changed, remove.is_some())
}

/// Стрелка с остриём в `tip`; `dir` — единичное направление движения к острию.
fn arrow(painter: &Painter, tip: Pos2, dir: Vec2, color: Color32) {
    let back = tip - dir * 9.0;
    let side = Vec2::new(-dir.y, dir.x) * 3.2;
    painter.add(egui::Shape::convex_polygon(vec![tip, back + side, back - side], color, Stroke::NONE));
}

fn unit(v: Vec2, fallback: Vec2) -> Vec2 {
    if v.length() > 1e-3 { v.normalized() } else { fallback }
}

/// Рисует размер, как в SolidWorks: выносные линии, размерная линия со стрелками там, куда её поставил
/// пользователь (`place`), выноска для радиуса и диаметра, дуга для угла. Запоминает область подписи.
#[allow(clippy::too_many_arguments)]
fn draw_dim(
    painter: &Painter,
    cam: &Camera,
    rect: Rect,
    plane: &Plane,
    sketch: &Sketch,
    dim: &Dim,
    place: Option<DVec2>,
    index: usize,
    labels: &mut Vec<DimLabel>,
) {
    let Some(value) = sketch.dim_value(dim) else { return };
    let to_screen = |p: DVec2| cam.project(rect, plane.to_world(p));
    let px = 1.0 / cam.scale;
    let color = Color32::from_gray(200);
    let stroke = Stroke::new(1.0, color);
    let anchor = |r: DimRef| -> Option<DVec2> {
        let o = sketch.ref_outline(r);
        match o.len() {
            0 => None,
            1 => Some(o[0]),
            2 => Some((o[0] + o[1]) * 0.5),
            n => Some(o[n / 2]),
        }
    };
    // Линейный размер: выносные линии от `a` и `b` к размерной линии, идущей вдоль `dir` через `at`.
    let linear = |a: DVec2, b: DVec2, dir: DVec2, at: DVec2| -> Pos2 {
        let perp = DVec2::new(-dir.y, dir.x);
        let p1 = a + perp * (at - a).dot(perp);
        let p2 = b + perp * (at - b).dot(perp);
        let (sa, sb, s1, s2) = (to_screen(a), to_screen(b), to_screen(p1), to_screen(p2));
        for (from, to) in [(sa, s1), (sb, s2)] {
            let d = to - from;
            if d.length() > 1.0 {
                let u = d.normalized();
                painter.line_segment([from + u * 3.0, to + u * 4.0], stroke);
            }
        }
        let l = s2 - s1;
        let u = unit(l, Vec2::X);
        if l.length() >= 26.0 {
            painter.line_segment([s1, s2], stroke);
            arrow(painter, s1, -u, color);
            arrow(painter, s2, u, color);
        } else {
            // Места мало — стрелки снаружи, направленные внутрь.
            painter.line_segment([s1 - u * 16.0, s1], stroke);
            painter.line_segment([s2, s2 + u * 16.0], stroke);
            arrow(painter, s1, u, color);
            arrow(painter, s2, -u, color);
        }
        s1 + l * 0.5
    };
    let side = |dir: DVec2| DVec2::new(-dir.y, dir.x) * 20.0 * px;
    let (pos, text) = match *dim {
        Dim::Distance(a, b, mode) => {
            let (Some(pa), Some(pb)) = (anchor(a), anchor(b)) else { return };
            let fallback = if mode == DimMode::Vertical { DVec2::Y } else { DVec2::X };
            let dir = sketch
                .dim_span(dim)
                .map(|(from, to)| to - from)
                .filter(|d| d.length() > 1e-9)
                .map_or(fallback, |d| d.normalize());
            let at = place.unwrap_or_else(|| (pa + pb) * 0.5 + side(dir));
            (linear(pa, pb, dir, at), fmt_mm(value))
        }
        Dim::Angle(a, b) => {
            let (oa, ob) = (sketch.ref_outline(a), sketch.ref_outline(b));
            let ends = |o: &[DVec2]| (o.len() >= 2).then(|| (o[0], o[o.len() - 1]));
            // Дуга между двумя прямыми с центром в их пересечении.
            let arc = (|| -> Option<Pos2> {
                let ((a0, a1), (b0, b1)) = (ends(&oa)?, ends(&ob)?);
                let (d1, d2) = (a1 - a0, b1 - b0);
                let cross = d1.perp_dot(d2);
                if cross.abs() < 1e-9 {
                    return None;
                }
                let v = a0 + d1 * ((b0 - a0).perp_dot(d2) / cross);
                let (da, db) = (d1.normalize(), d2.normalize());
                let bisector = ((a0 + a1) * 0.5 - v).normalize_or_zero() + ((b0 + b1) * 0.5 - v).normalize_or_zero();
                let guess = if bisector.length() > 1e-9 { bisector.normalize() } else { da };
                let u = place.map(|p| p - v).filter(|d| d.length() > 1e-9).map_or(guess, |d| d.normalize());
                let wanted = value.to_radians();
                let pairs: Vec<(DVec2, DVec2)> = [(da, db), (-da, -db), (da, -db), (-da, db)]
                    .into_iter()
                    .filter(|(s1, s2)| (s1.dot(*s2).clamp(-1.0, 1.0).acos() - wanted).abs() < 0.01)
                    .collect();
                let inside = |&&(s1, s2): &&(DVec2, DVec2)| {
                    let c = s1.perp_dot(s2);
                    s1.perp_dot(u) * c > 0.0 && u.perp_dot(s2) * c > 0.0
                };
                let (s1, s2) = *pairs.iter().find(inside).or(pairs.first())?;
                let radius = place.map_or(40.0 * px, |p| (p - v).length()).max(12.0 * px);
                let delta = s1.perp_dot(s2).atan2(s1.dot(s2));
                let pts: Vec<Pos2> = (0..=24)
                    .map(|k| {
                        let (sin, cos) = (delta * k as f64 / 24.0).sin_cos();
                        let dir = DVec2::new(s1.x * cos - s1.y * sin, s1.x * sin + s1.y * cos);
                        to_screen(v + dir * radius)
                    })
                    .collect();
                painter.line(pts.clone(), stroke);
                arrow(painter, pts[0], unit(pts[0] - pts[1], Vec2::X), color);
                arrow(painter, pts[24], unit(pts[24] - pts[23], Vec2::X), color);
                Some(pts[12])
            })();
            let pos = arc.unwrap_or_else(|| {
                let pts: Vec<Pos2> = oa.iter().chain(&ob).map(|p| to_screen(*p)).collect();
                let sum = pts.iter().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2());
                let center = (sum / pts.len().max(1) as f32).to_pos2();
                match place.map(to_screen) {
                    Some(at) => {
                        painter.line_segment([center, at], stroke);
                        at
                    }
                    None => center,
                }
            });
            (pos, format!("{}°", fmt_mm(value)))
        }
        Dim::Size(r) => {
            let o = sketch.ref_outline(r);
            if o.len() == 2 {
                let (a, b) = (o[0], o[1]);
                let dir = if (b - a).length() > 1e-9 { (b - a).normalize() } else { DVec2::X };
                let at = place.unwrap_or_else(|| (a + b) * 0.5 + side(dir));
                (linear(a, b, dir, at), fmt_mm(value))
            } else if o.len() > 2 {
                // Радиус и диаметр: выноска со стрелкой на ближайшую к курсору точку дуги.
                let centroid = o.iter().fold(DVec2::ZERO, |sum, p| sum + *p) / o.len() as f64;
                let is_circle = matches!(r, DimRef::Circle(_));
                let tip = match place {
                    Some(at) => *o.iter().min_by(|x, y| (**x - at).length().total_cmp(&(**y - at).length())).unwrap(),
                    None => o[if is_circle { o.len() / 8 } else { o.len() / 2 }],
                };
                let outward = (tip - centroid).try_normalize().unwrap_or(DVec2::X);
                let at = place.unwrap_or(tip + outward * 28.0 * px);
                let (st, sa) = (to_screen(tip), to_screen(at));
                painter.line_segment([st, sa], stroke);
                arrow(painter, st, unit(st - sa, Vec2::X), color);
                let prefix = if is_circle { "Ø" } else { "R" };
                (sa, format!("{prefix}{}", fmt_mm(value)))
            } else {
                return;
            }
        }
    };
    let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::WHITE);
    let r = Align2::CENTER_CENTER.anchor_size(pos, galley.size());
    painter.rect_filled(r.expand2(Vec2::new(4.0, 2.0)), 3.0, Color32::from_rgba_unmultiplied(20, 24, 32, 230));
    painter.galley(r.min, galley, Color32::WHITE);
    labels.push(DimLabel { rect: r, dim: *dim, index });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Ctrl+Z — отмена, Ctrl+Y или Ctrl+Shift+Z — повтор (когда не вводится текст).
        if ui.memory(|m| m.focused().is_none()) {
            use egui::{Key, Modifiers};
            if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)) {
                self.redo();
            } else if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Y)) {
                self.redo();
            } else if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
                self.undo();
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save();
        }
        let modified = self.dirty || self.base_json != self.saved_json;
        let name = self
            .file
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("новая деталь".into(), |n| n.to_string_lossy().into_owned());
        let title = format!("MySolid3D — {name}{}", if modified { " *" } else { "" });
        if title != self.title {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        let highlighted = match &self.selection {
            Selection::Faces(list) => list.clone(),
            _ => vec![],
        };
        if highlighted != self.face_hl {
            self.face_hl = highlighted;
            self.colors_dirty = true;
        }
        if self.dirty {
            self.rebuild(frame);
        }
        if self.colors_dirty {
            self.upload_colors(frame);
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.sketch.is_some() {
                    ui.weak("Эскиз: «Линия» и «Дуга» — щелчками, щелчок по первой точке замыкает контур · прямоугольник и окружность — протяжкой · «Размер» — щелчок по элементу или по двум элементам, Enter — поставить размер · щелчок по размеру — изменить · Esc — закончить · Del — удалить");
                } else {
                    ui.weak("Колесо — масштаб · средняя кнопка — вращение · правая — сдвиг · щелчок — выбор грани или ребра (Ctrl — несколько рёбер)");
                }
                if !self.status.is_empty() {
                    ui.separator();
                    ui.colored_label(SELECT_COLOR, &self.status);
                }
            });
        });
        egui::Panel::left("tree").exact_size(240.0).resizable(false).show(ui, |ui| self.tree(ui));
        egui::Panel::right("properties").exact_size(270.0).resizable(false).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.properties(ui));
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui));
        self.sketch_op_window(ui.ctx());
        self.save_question(ui.ctx());
        self.track_history(ui.ctx());

        if self.dirty {
            ui.ctx().request_repaint();
        }
    }
}
