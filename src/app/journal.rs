//! Журнал действий: что делал пользователь и что из этого получилось.
//!
//! Каждый запуск программы пишет свою папку `%APPDATA%\MySolid3D\journal\<дата_время>\`:
//! - `journal.md` — записи по порядку: время, что изменилось в детали, сообщения программы, ошибки
//!   построения, размеры тела, заметки пользователя («что я хотел сделать»);
//! - `step-NNN.ms3d` — деталь после шага (открывается в программе, можно вернуться к любому шагу);
//! - `step-NNN.jpg` — снимок окна после шага.
//!
//! Хранятся последние `KEPT_SESSIONS` сеансов.
//!
//! Записи о правках составляются сравнением детали до и после шага истории (тот же момент,
//! когда появляется шаг для Ctrl+Z), поэтому журнал видит любые правки — мышью, вводом чисел или через API.

use super::*;
use crate::model::{Feature, FeatureKind};

/// Сколько последних записей показывать в окне журнала.
const SHOWN_ENTRIES: usize = 200;
/// Сколько последних сеансов хранить: папки более старых удаляются при начале нового.
const KEPT_SESSIONS: usize = 30;
/// Снимки окна уменьшаются до этой ширины и пишутся в JPEG — так шаг занимает около 100 КБ.
const SHOT_WIDTH: u32 = 1280;

pub(super) struct Journal {
    pub enabled: bool,
    /// Папка текущего сеанса; создаётся при первой записи.
    dir: Option<PathBuf>,
    step: u32,
    /// Записи для окна журнала (последние сверху не храним — разворачиваем при показе).
    entries: Vec<(String, String)>,
    pub window: bool,
    note: String,
    /// Последнее записанное сообщение строки состояния (чтобы не повторять его каждый кадр).
    last_status: String,
    /// Шаг, который нужно записать, когда деталь перестроится (после открытия файла, отмены, повтора),
    /// и состояние детали до него — чтобы описать, что изменилось.
    pending: Option<(String, Option<Part>)>,
    /// Снимок окна, который нужно сделать в конце кадра.
    shot: Option<PathBuf>,
    /// С прошлого шага истории приходили команды API (правка сделана программой-помощником).
    api_recent: bool,
}

/// Признак «журнал включён»: пустой файл рядом со списком последних файлов. По умолчанию журнал выключен.
fn on_flag_path() -> Option<PathBuf> {
    recent_list_path().map(|p| p.with_file_name("journal_on"))
}

fn journal_root() -> Option<PathBuf> {
    recent_list_path().map(|p| p.with_file_name("journal"))
}

/// Местное время: (дата для имени папки, время для записи).
fn now() -> (String, String) {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct SystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            ms: u16,
        }
        extern "system" {
            fn GetLocalTime(st: *mut SystemTime);
        }
        let mut t = SystemTime::default();
        // SAFETY: GetLocalTime только заполняет переданную структуру.
        unsafe { GetLocalTime(&mut t) };
        (
            format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second),
            format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second),
        )
    }
    #[cfg(not(windows))]
    {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let (h, m, sec) = (s / 3600 % 24, s / 60 % 60, s % 60);
        (format!("{s}"), format!("{h:02}:{m:02}:{sec:02} UTC"))
    }
}

impl Journal {
    pub fn new() -> Self {
        Self {
            enabled: on_flag_path().is_some_and(|p| p.exists()),
            dir: None,
            step: 0,
            entries: Vec::new(),
            window: false,
            note: String::new(),
            last_status: String::new(),
            pending: None,
            shot: None,
            api_recent: false,
        }
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if let Some(flag) = on_flag_path() {
            if on {
                if let Some(dir) = flag.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(flag, "");
            } else {
                let _ = std::fs::remove_file(flag);
            }
        }
    }

    /// Папка сеанса; при первом обращении создаётся вместе с заголовком журнала.
    fn session_dir(&mut self) -> Option<PathBuf> {
        if self.dir.is_none() {
            let (stamp, _) = now();
            let root = journal_root()?;
            let dir = root.join(stamp);
            std::fs::create_dir_all(&dir).ok()?;
            // Старые сеансы удаляем: имена папок — дата и время, поэтому сортировка по имени = по времени.
            let mut old: Vec<PathBuf> = std::fs::read_dir(&root)
                .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && *p != dir).collect())
                .unwrap_or_default();
            old.sort();
            let extra = (old.len() + 1).saturating_sub(KEPT_SESSIONS);
            for path in old.into_iter().take(extra) {
                let _ = std::fs::remove_dir_all(path);
            }
            let head = format!(
                "# Журнал MySolid3D {}\n\nСеанс {}. Каждая запись — время и что произошло; после правок — что получилось \
                 (размеры тела, ошибки), деталь на этом шаге (`step-NNN.ms3d`) и снимок окна.\n\n",
                env!("CARGO_PKG_VERSION"),
                dir.file_name().map_or(String::new(), |n| n.to_string_lossy().replace('_', " ")),
            );
            std::fs::write(dir.join("journal.md"), head).ok()?;
            self.dir = Some(dir);
        }
        self.dir.clone()
    }

    fn append(&mut self, text: &str) {
        let Some(dir) = self.session_dir() else { return };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(dir.join("journal.md")) {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

// ---------- описание правок ----------

fn feature_name(part: &Part, id: u64) -> String {
    part.feature(id).map_or_else(|| format!("#{id}"), |f| f.name.clone())
}

fn summary(part: &Part, f: &Feature) -> String {
    match &f.kind {
        FeatureKind::Sketch(s) => format!("эскиз: фигур {}, размеров {}", s.shapes.len(), s.dims.len()),
        FeatureKind::Boss { sketch, depth, flip } => format!(
            "бобышка из «{}», глубина {} мм{}",
            feature_name(part, *sketch),
            fmt_mm(*depth),
            if *flip { ", в обратную сторону" } else { "" }
        ),
        FeatureKind::Cut { sketch, depth, through_all, flip } => format!(
            "вырез из «{}», {}{}",
            feature_name(part, *sketch),
            if *through_all { "насквозь".to_string() } else { format!("глубина {} мм", fmt_mm(*depth)) },
            if *flip { ", в обратную сторону" } else { "" }
        ),
        FeatureKind::Fillet { edges, radius } => format!("скругление R{} мм, рёбер {}", fmt_mm(*radius), edges.len()),
        FeatureKind::Chamfer { edges, distance } => format!("фаска {} мм, рёбер {}", fmt_mm(*distance), edges.len()),
    }
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Что поменялось в одном элементе (одинаковый номер до и после).
fn feature_changes(before: &Feature, after: &Feature) -> Vec<String> {
    let mut out = Vec::new();
    let was = |a: f64, b: f64, what: &str, out: &mut Vec<String>| {
        if a != b {
            out.push(format!("{what} {} → {} мм", fmt_mm(a), fmt_mm(b)));
        }
    };
    match (&before.kind, &after.kind) {
        (FeatureKind::Sketch(a), FeatureKind::Sketch(b)) => {
            if a.shapes.len() != b.shapes.len() {
                out.push(format!("фигур {} → {}", a.shapes.len(), b.shapes.len()));
            } else {
                let moved = a.shapes.iter().zip(&b.shapes).filter(|(x, y)| x != y).count();
                if moved > 0 {
                    out.push(format!("изменена геометрия фигур: {moved}"));
                }
            }
            if a.dims.len() != b.dims.len() {
                out.push(format!("размеров {} → {}", a.dims.len(), b.dims.len()));
            }
            if json(&a.plane) != json(&b.plane) {
                out.push("другая плоскость".into());
            }
            match (&a.image, &b.image) {
                (None, Some(i)) => out.push(format!("добавлена картинка-подложка {}", i.path)),
                (Some(_), None) => out.push("убрана картинка-подложка".into()),
                (Some(x), Some(y)) if x != y => out.push("сдвинута или изменена картинка-подложка".into()),
                _ => {}
            }
            if json(&a.region_sel) != json(&b.region_sel) {
                out.push("изменён выбор областей для вытягивания".into());
            }
            if out.is_empty() && json(a) != json(b) {
                out.push("изменено положение размеров".into());
            }
        }
        (
            FeatureKind::Boss { depth: d1, flip: f1, sketch: s1 },
            FeatureKind::Boss { depth: d2, flip: f2, sketch: s2 },
        ) => {
            was(*d1, *d2, "глубина", &mut out);
            if f1 != f2 {
                out.push(if *f2 { "направление: обратное" } else { "направление: прямое" }.into());
            }
            if s1 != s2 {
                out.push("другой эскиз".into());
            }
        }
        (
            FeatureKind::Cut { depth: d1, through_all: t1, flip: f1, sketch: s1 },
            FeatureKind::Cut { depth: d2, through_all: t2, flip: f2, sketch: s2 },
        ) => {
            was(*d1, *d2, "глубина", &mut out);
            if t1 != t2 {
                out.push(if *t2 { "насквозь" } else { "на глубину" }.into());
            }
            if f1 != f2 {
                out.push(if *f2 { "направление: обратное" } else { "направление: прямое" }.into());
            }
            if s1 != s2 {
                out.push("другой эскиз".into());
            }
        }
        (FeatureKind::Fillet { edges: e1, radius: r1 }, FeatureKind::Fillet { edges: e2, radius: r2 })
        | (FeatureKind::Chamfer { edges: e1, distance: r1 }, FeatureKind::Chamfer { edges: e2, distance: r2 }) => {
            was(*r1, *r2, "размер", &mut out);
            if e1.len() != e2.len() {
                out.push(format!("рёбер {} → {}", e1.len(), e2.len()));
            } else if json(e1) != json(e2) {
                out.push("рёбра уточнены".into());
            }
        }
        _ => {
            if json(&before.kind) != json(&after.kind) {
                out.push("изменены параметры".into());
            }
        }
    }
    out
}

/// Список правок между двумя состояниями детали, по строке на правку.
fn describe_changes(before: &Part, after: &Part) -> Vec<String> {
    let mut out = Vec::new();
    for f in &after.features {
        match before.feature(f.id) {
            None => out.push(format!("Добавлен «{}» — {}", f.name, summary(after, f))),
            Some(old) => {
                if old.name != f.name {
                    out.push(format!("Переименован «{}» → «{}»", old.name, f.name));
                }
                let changes = feature_changes(old, f);
                if !changes.is_empty() {
                    out.push(format!("Изменён «{}»: {} (теперь {})", f.name, changes.join(", "), summary(after, f)));
                }
            }
        }
    }
    for f in &before.features {
        if after.feature(f.id).is_none() {
            out.push(format!("Удалён «{}» ({})", f.name, summary(before, f)));
        }
    }
    let order = |p: &Part, other: &Part| -> Vec<u64> {
        p.features.iter().map(|f| f.id).filter(|id| other.feature(*id).is_some()).collect()
    };
    if order(before, after) != order(after, before) {
        out.push("Изменён порядок элементов в дереве".into());
    }
    if before.color != after.color {
        out.push(match after.color {
            Some([r, g, b]) => format!("Цвет детали: #{r:02x}{g:02x}{b:02x}"),
            None => "Цвет детали сброшен".into(),
        });
    }
    if before.material != after.material {
        out.push(format!("Материал детали: {}", after.material.name()));
    }
    if json(&before.face_colors) != json(&after.face_colors) {
        out.push(format!(
            "Покраска граней: окрашено {} → {}",
            before.face_colors.len(),
            after.face_colors.len()
        ));
    }
    if json(&before.groups) != json(&after.groups) {
        out.push("Изменены группы в дереве".into());
    }
    if out.is_empty() {
        out.push("Мелкая правка (служебные данные детали)".into());
    }
    out
}

impl App {
    /// Итог построения: размеры и объём тела, ошибки элементов.
    fn journal_result(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.mesh.bounds {
            Some((lo, hi)) => {
                let s = hi - lo;
                out.push(format!(
                    "Тело {} × {} × {} мм, объём {:.1} мм³, граней {}, рёбер {}",
                    fmt_mm(s.x),
                    fmt_mm(s.y),
                    fmt_mm(s.z),
                    self.mesh_volume(),
                    self.mesh.faces.len(),
                    self.mesh.edges.len()
                ));
            }
            None => out.push("Тела нет".into()),
        }
        for f in &self.part.features {
            if let Some(e) = &f.error {
                out.push(format!("**Ошибка** в «{}»: {e}", f.name));
            }
        }
        out
    }

    /// Запись в журнал. `snapshot` — сохранить деталь, итог построения и снимок окна на этом шаге.
    pub(super) fn journal_log(&mut self, title: &str, lines: &[String], snapshot: bool) {
        if !self.journal.enabled {
            return;
        }
        let (_, time) = now();
        let mut text = format!("### {time} — {title}\n\n");
        for line in lines {
            text += &format!("- {line}\n");
        }
        if snapshot {
            if let Some(dir) = self.journal.session_dir() {
                self.journal.step += 1;
                let name = format!("step-{:03}", self.journal.step);
                let saved = serde_json::to_string_pretty(&self.part)
                    .ok()
                    .and_then(|j| std::fs::write(dir.join(format!("{name}.ms3d")), j).ok())
                    .is_some();
                text += "\nЧто получилось:\n";
                for line in self.journal_result() {
                    text += &format!("- {line}\n");
                }
                if saved {
                    text += &format!("- Деталь на этом шаге: [{name}.ms3d]({name}.ms3d)\n");
                }
                text += &format!("\n![{name}]({name}.jpg)\n");
                self.journal.shot = Some(dir.join(format!("{name}.jpg")));
            }
        }
        text.push('\n');
        self.journal.append(&text);
        let mut shown = title.to_string();
        if !lines.is_empty() {
            shown += &format!(": {}", lines.join("; "));
        }
        self.journal.entries.push((time, shown));
        if self.journal.entries.len() > SHOWN_ENTRIES {
            self.journal.entries.remove(0);
        }
    }

    /// Записать шаг, когда деталь перестроится (после открытия файла, отмены, повтора).
    /// `before` — деталь до действия (для отмены и повтора: что именно вернулось).
    pub(super) fn journal_after_rebuild(&mut self, title: &str, before: Option<Part>) {
        if self.journal.enabled {
            self.journal.pending = Some((title.to_string(), before));
        }
    }

    /// Новый шаг истории: что поменялось между прошлым и текущим состоянием детали.
    pub(super) fn journal_change(&mut self, before: &Part) {
        if self.journal.enabled {
            let lines = describe_changes(before, &self.part);
            let title = if self.journal.api_recent { "Правка (через API)" } else { "Правка" };
            self.journal_log(title, &lines, true);
        }
        self.journal.api_recent = false;
    }

    /// Вызывается в конце каждого кадра: отложенные записи, новые сообщения, снимок окна.
    pub(super) fn journal_frame(&mut self, ctx: &egui::Context) {
        if !self.journal.enabled {
            return;
        }
        if !self.dirty && !self.building {
            if let Some((title, before)) = self.journal.pending.take() {
                let lines = before.map(|b| describe_changes(&b, &self.part)).unwrap_or_default();
                self.journal_log(&title, &lines, true);
            }
        }
        if self.status != self.journal.last_status {
            self.journal.last_status = self.status.clone();
            if !self.status.is_empty() && !self.status.starts_with("Построение:") {
                let status = self.status.clone();
                self.journal_log("Сообщение программы", &[status], false);
            }
        }
        if let Some(path) = self.journal.shot.take() {
            ctx.request_screenshot(move |image| {
                std::thread::spawn(move || {
                    let [w, h] = image.size;
                    let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec()) else {
                        return;
                    };
                    let mut rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
                    if rgb.width() > SHOT_WIDTH {
                        let height = (rgb.height() as f64 * SHOT_WIDTH as f64 / rgb.width() as f64).round() as u32;
                        let filter = image::imageops::FilterType::Triangle;
                        rgb = image::imageops::resize(&rgb, SHOT_WIDTH, height.max(1), filter);
                    }
                    if let Ok(file) = std::fs::File::create(path) {
                        let mut out = std::io::BufWriter::new(file);
                        let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&rgb);
                    }
                });
            });
        }
    }

    /// Запрос через API: что именно попросила программа-помощник (сокращённо).
    pub(super) fn journal_api(&mut self, body: &str) {
        self.journal.api_recent = true;
        if self.journal.enabled {
            // Разбор и обратная запись: русские буквы вместо escape-кодов, без лишних пробелов.
            let body =
                serde_json::from_str::<serde_json::Value>(body).map_or_else(|_| body.to_string(), |v| v.to_string());
            let mut text: String = body.chars().take(400).collect();
            if text.len() < body.len() {
                text += " …";
            }
            self.journal_log("Команда API", &[format!("`{}`", text.replace('`', "'"))], false);
        }
    }

    /// Окно «Журнал действий»: включение, заметки о намерениях, последние записи.
    pub(super) fn journal_window(&mut self, ctx: &egui::Context) {
        if !self.journal.window {
            return;
        }
        let mut open = true;
        let mut note = None;
        let mut enabled = self.journal.enabled;
        egui::Window::new("Журнал действий")
            .open(&mut open)
            .default_size([460.0, 420.0])
            .show(ctx, |ui| {
                ui.checkbox(&mut enabled, "Записывать действия")
                    .on_hover_text("Правки детали, сообщения, ошибки, снимки окна и деталь на каждом шаге");
                if let Some(dir) = &self.journal.dir {
                    ui.horizontal(|ui| {
                        ui.weak(format!("Папка сеанса: {}", dir.display()));
                    });
                }
                ui.horizontal(|ui| {
                    if ui.button("Открыть папку журнала").clicked() {
                        let dir = self.journal.dir.clone().or_else(journal_root);
                        if let Some(dir) = dir {
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = std::process::Command::new("explorer").arg(dir).spawn();
                        }
                    }
                });
                ui.separator();
                ui.label("Заметка — что вы хотите сделать или что пошло не так:");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.journal.note)
                        .hint_text("например: хочу вырез под разъём USB на торце")
                        .desired_width(f32::INFINITY),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Записать заметку").clicked() || enter) && !self.journal.note.trim().is_empty() {
                    note = Some(std::mem::take(&mut self.journal.note));
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.journal.entries.is_empty() {
                        ui.weak("Пока записей нет.");
                    }
                    for (time, text) in self.journal.entries.iter().rev() {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(time);
                            ui.label(text);
                        });
                    }
                });
            });
        if enabled != self.journal.enabled {
            if !enabled {
                self.journal_log("Запись выключена", &[], false);
            }
            self.journal.set_enabled(enabled);
            if enabled {
                self.journal_log("Запись включена", &[], true);
            }
        }
        if let Some(note) = note {
            self.journal_log("Заметка", &[note.trim().to_string()], true);
        }
        self.journal.window = open;
    }
}
