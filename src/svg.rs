//! Импорт контуров из SVG в фигуры эскиза. Читаются path (все команды, включая дуги и кривые Безье),
//! rect, circle, ellipse, line, polyline, polygon и преобразования transform. Кривые заменяются ломаными.
//! Текст, картинки и заливки не читаются. Единицы переводятся в миллиметры, ось Y разворачивается вверх,
//! а центр всех контуров ставится в начало координат эскиза.

use crate::model::{PathPoint, SketchShape, MIN_SIZE};
use glam::DVec2;
use std::collections::HashMap;

/// Аффинное преобразование: x' = a·x + c·y + e, y' = b·x + d·y + f.
#[derive(Clone, Copy, Debug)]
struct Mat {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Mat {
    const IDENT: Mat = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    /// Сначала `o`, потом `self`.
    fn mul(self, o: Mat) -> Mat {
        Mat {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    fn apply(&self, p: DVec2) -> DVec2 {
        DVec2::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f)
    }

    /// Во сколько раз преобразование в среднем меняет длины.
    fn scale_factor(&self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }

    fn axis_aligned(&self) -> bool {
        self.b.abs() < 1e-9 && self.c.abs() < 1e-9
    }
}

/// Числа в строке: через пробелы, запятые и знаки (`1.5.5` — это 1.5 и .5).
fn numbers(s: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut r = Reader { s: s.as_bytes(), i: 0 };
    while let Some(v) = r.number() {
        out.push(v);
    }
    out
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn skip_sep(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_sep();
        self.s.get(self.i).copied()
    }

    fn number(&mut self) -> Option<f64> {
        self.skip_sep();
        let start = self.i;
        let at = |i: usize| self.s.get(i).copied();
        let mut i = self.i;
        if matches!(at(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let mut digits = false;
        while at(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
            digits = true;
        }
        if at(i) == Some(b'.') {
            i += 1;
            while at(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
                digits = true;
            }
        }
        if !digits {
            return None;
        }
        if matches!(at(i), Some(b'e' | b'E')) {
            let mut j = i + 1;
            if matches!(at(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if at(j).is_some_and(|c| c.is_ascii_digit()) {
                while at(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
                i = j;
            }
        }
        self.i = i;
        std::str::from_utf8(&self.s[start..i]).ok()?.trim_start_matches(',').trim().parse().ok()
    }

    /// Флаг дуги: один символ 0 или 1 (в сжатой записи они слитны с числами).
    fn flag(&mut self) -> Option<bool> {
        match self.peek()? {
            b'0' => {
                self.i += 1;
                Some(false)
            }
            b'1' => {
                self.i += 1;
                Some(true)
            }
            _ => None,
        }
    }
}

/// Преобразование из атрибута transform: translate, scale, rotate, matrix, skewX, skewY.
fn parse_transform(s: &str) -> Mat {
    let mut m = Mat::IDENT;
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        let Some(close) = rest[open..].find(')') else { break };
        let args = numbers(&rest[open + 1..open + close]);
        let t = match (name, args.as_slice()) {
            ("translate", [x]) => Mat { e: *x, ..Mat::IDENT },
            ("translate", [x, y, ..]) => Mat { e: *x, f: *y, ..Mat::IDENT },
            ("scale", [k]) => Mat { a: *k, d: *k, ..Mat::IDENT },
            ("scale", [kx, ky, ..]) => Mat { a: *kx, d: *ky, ..Mat::IDENT },
            ("rotate", [deg]) => {
                let (s, c) = deg.to_radians().sin_cos();
                Mat { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
            }
            ("rotate", [deg, cx, cy, ..]) => {
                let (s, c) = deg.to_radians().sin_cos();
                let r = Mat { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 };
                Mat { e: *cx, f: *cy, ..Mat::IDENT }.mul(r).mul(Mat { e: -cx, f: -cy, ..Mat::IDENT })
            }
            ("matrix", [a, b, c, d, e, f, ..]) => Mat { a: *a, b: *b, c: *c, d: *d, e: *e, f: *f },
            ("skewX", [deg]) => Mat { c: deg.to_radians().tan(), ..Mat::IDENT },
            ("skewY", [deg]) => Mat { b: deg.to_radians().tan(), ..Mat::IDENT },
            _ => Mat::IDENT,
        };
        m = m.mul(t);
        rest = &rest[open + close + 1..];
    }
    m
}

/// Длина с единицами: (число, миллиметров в одной единице); проценты не поддерживаются.
fn length(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let end = s.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))).unwrap_or(s.len());
    let (num, unit) = s.split_at(end);
    let value: f64 = num.parse().ok()?;
    let per = match unit.trim() {
        "" | "px" => 25.4 / 96.0,
        "mm" => 1.0,
        "cm" => 10.0,
        "in" => 25.4,
        "pt" => 25.4 / 72.0,
        "pc" => 25.4 / 6.0,
        _ => return None,
    };
    Some((value, per))
}

type Attrs = HashMap<String, String>;

fn num(attrs: &Attrs, key: &str) -> f64 {
    attrs.get(key).and_then(|v| numbers(v).first().copied()).unwrap_or(0.0)
}

/// Одна ломаная и признак замкнутости — контур до перевода в фигуру эскиза.
struct Poly {
    pts: Vec<DVec2>,
    closed: bool,
}

/// Точки дуги SVG (конечные параметры → центральные), без начальной точки.
fn arc_points(from: DVec2, to: DVec2, rx: f64, ry: f64, rot_deg: f64, large: bool, sweep: bool, out: &mut Vec<DVec2>) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if from == to {
        return;
    }
    if rx < 1e-12 || ry < 1e-12 {
        out.push(to);
        return;
    }
    let (s, c) = rot_deg.to_radians().sin_cos();
    let d = (from - to) / 2.0;
    let p = DVec2::new(c * d.x + s * d.y, -s * d.x + c * d.y);
    let lambda = p.x * p.x / (rx * rx) + p.y * p.y / (ry * ry);
    if lambda > 1.0 {
        let k = lambda.sqrt();
        rx *= k;
        ry *= k;
    }
    let num = rx * rx * ry * ry - rx * rx * p.y * p.y - ry * ry * p.x * p.x;
    let den = rx * rx * p.y * p.y + ry * ry * p.x * p.x;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cp = DVec2::new(coef * rx * p.y / ry, -coef * ry * p.x / rx);
    let center = DVec2::new(c * cp.x - s * cp.y, s * cp.x + c * cp.y) + (from + to) / 2.0;
    let angle = |u: DVec2| u.y.atan2(u.x);
    let v1 = DVec2::new((p.x - cp.x) / rx, (p.y - cp.y) / ry);
    let v2 = DVec2::new((-p.x - cp.x) / rx, (-p.y - cp.y) / ry);
    let start = angle(v1);
    let mut delta = angle(v2) - start;
    if sweep && delta < 0.0 {
        delta += std::f64::consts::TAU;
    } else if !sweep && delta > 0.0 {
        delta -= std::f64::consts::TAU;
    }
    let steps = ((delta.abs() / 0.09).ceil() as usize).clamp(2, 400);
    for i in 1..steps {
        let t = start + delta * i as f64 / steps as f64;
        let (st, ct) = t.sin_cos();
        let q = DVec2::new(rx * ct, ry * st);
        out.push(center + DVec2::new(c * q.x - s * q.y, s * q.x + c * q.y));
    }
    out.push(to);
}

fn cubic(p0: DVec2, p1: DVec2, p2: DVec2, p3: DVec2, out: &mut Vec<DVec2>) {
    let steps = 20;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let u = 1.0 - t;
        out.push(p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t));
    }
}

fn quad(p0: DVec2, p1: DVec2, p2: DVec2, out: &mut Vec<DVec2>) {
    let steps = 14;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let u = 1.0 - t;
        out.push(p0 * (u * u) + p1 * (2.0 * u * t) + p2 * (t * t));
    }
}

/// Разбор атрибута d: список ломаных (в координатах SVG).
fn parse_path(d: &str) -> Vec<Poly> {
    let mut out: Vec<Poly> = Vec::new();
    let mut r = Reader { s: d.as_bytes(), i: 0 };
    let (mut cur, mut start) = (DVec2::ZERO, DVec2::ZERO);
    let mut last_cubic: Option<DVec2> = None;
    let mut last_quad: Option<DVec2> = None;
    let mut poly: Option<Poly> = None;
    let mut cmd = b'M';
    let flush = |poly: &mut Option<Poly>, out: &mut Vec<Poly>| {
        if let Some(p) = poly.take() {
            if p.pts.len() > 1 {
                out.push(p);
            }
        }
    };
    loop {
        let Some(next) = r.peek() else { break };
        if next.is_ascii_alphabetic() {
            cmd = next;
            r.i += 1;
        }
        let rel = cmd.is_ascii_lowercase();
        let at = |p: DVec2, base: DVec2| if rel { base + p } else { p };
        let upper = cmd.to_ascii_uppercase();
        let mut ok = true;
        match upper {
            b'Z' => {
                if let Some(p) = poly.as_mut() {
                    p.closed = true;
                }
                flush(&mut poly, &mut out);
                cur = start;
                last_cubic = None;
                last_quad = None;
                // После Z без новой буквы чисел быть не должно.
                if r.peek().is_some_and(|c| !c.is_ascii_alphabetic()) {
                    break;
                }
                continue;
            }
            b'M' => {
                let (Some(x), Some(y)) = (r.number(), r.number()) else { break };
                flush(&mut poly, &mut out);
                cur = at(DVec2::new(x, y), cur);
                start = cur;
                poly = Some(Poly { pts: vec![cur], closed: false });
                // Дальше пары чисел без буквы — это L.
                cmd = if rel { b'l' } else { b'L' };
                last_cubic = None;
                last_quad = None;
            }
            b'L' | b'H' | b'V' => {
                let to = match upper {
                    b'L' => match (r.number(), r.number()) {
                        (Some(x), Some(y)) => at(DVec2::new(x, y), cur),
                        _ => break,
                    },
                    b'H' => match r.number() {
                        Some(x) => DVec2::new(if rel { cur.x + x } else { x }, cur.y),
                        None => break,
                    },
                    _ => match r.number() {
                        Some(y) => DVec2::new(cur.x, if rel { cur.y + y } else { y }),
                        None => break,
                    },
                };
                poly.get_or_insert_with(|| Poly { pts: vec![cur], closed: false }).pts.push(to);
                cur = to;
                last_cubic = None;
                last_quad = None;
            }
            b'C' | b'S' => {
                let mut v = [0.0; 6];
                let want = if upper == b'C' { 6 } else { 4 };
                for slot in v.iter_mut().take(want) {
                    match r.number() {
                        Some(x) => *slot = x,
                        None => ok = false,
                    }
                }
                if !ok {
                    break;
                }
                let (c1, c2, to) = if upper == b'C' {
                    (
                        at(DVec2::new(v[0], v[1]), cur),
                        at(DVec2::new(v[2], v[3]), cur),
                        at(DVec2::new(v[4], v[5]), cur),
                    )
                } else {
                    let c1 = last_cubic.map_or(cur, |l| cur * 2.0 - l);
                    (c1, at(DVec2::new(v[0], v[1]), cur), at(DVec2::new(v[2], v[3]), cur))
                };
                let p = poly.get_or_insert_with(|| Poly { pts: vec![cur], closed: false });
                cubic(cur, c1, c2, to, &mut p.pts);
                cur = to;
                last_cubic = Some(c2);
                last_quad = None;
            }
            b'Q' | b'T' => {
                let mut v = [0.0; 4];
                let want = if upper == b'Q' { 4 } else { 2 };
                for slot in v.iter_mut().take(want) {
                    match r.number() {
                        Some(x) => *slot = x,
                        None => ok = false,
                    }
                }
                if !ok {
                    break;
                }
                let (c1, to) = if upper == b'Q' {
                    (at(DVec2::new(v[0], v[1]), cur), at(DVec2::new(v[2], v[3]), cur))
                } else {
                    (last_quad.map_or(cur, |l| cur * 2.0 - l), at(DVec2::new(v[0], v[1]), cur))
                };
                let p = poly.get_or_insert_with(|| Poly { pts: vec![cur], closed: false });
                quad(cur, c1, to, &mut p.pts);
                cur = to;
                last_quad = Some(c1);
                last_cubic = None;
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot)) = (r.number(), r.number(), r.number()) else { break };
                let (Some(large), Some(sweep)) = (r.flag(), r.flag()) else { break };
                let (Some(x), Some(y)) = (r.number(), r.number()) else { break };
                let to = at(DVec2::new(x, y), cur);
                let p = poly.get_or_insert_with(|| Poly { pts: vec![cur], closed: false });
                arc_points(cur, to, rx, ry, rot, large, sweep, &mut p.pts);
                cur = to;
                last_cubic = None;
                last_quad = None;
            }
            _ => break,
        }
    }
    flush(&mut poly, &mut out);
    out
}

/// Допуск упрощения ломаных при импорте, мм: меньше толщины линии при печати, но выкидывает шум и лишние точки.
const SIMPLIFY_TOLERANCE: f64 = 0.015;

/// Расстояние от точки до отрезка.
fn dist_to_segment(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let d = b - a;
    let len2 = d.length_squared();
    let t = if len2 < 1e-18 { 0.0 } else { ((p - a).dot(d) / len2).clamp(0.0, 1.0) };
    (a + d * t - p).length()
}

/// Упрощение открытой ломаной (Рамер — Дуглас — Пекер): концы остаются, отклонение не больше `eps`.
fn simplify_open(pts: &[DVec2], eps: f64) -> Vec<DVec2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (mut far, mut far_d) = (0, 0.0);
    for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = dist_to_segment(*p, a, b);
        if d > far_d {
            (far, far_d) = (i, d);
        }
    }
    if far_d <= eps {
        return vec![a, b];
    }
    let mut left = simplify_open(&pts[..=far], eps);
    left.pop();
    left.extend(simplify_open(&pts[far..], eps));
    left
}

/// Упрощение контура; у замкнутого он делится на две цепочки по самой далёкой от первой точке.
fn simplify(pts: Vec<DVec2>, closed: bool, eps: f64) -> Vec<DVec2> {
    if !closed {
        return simplify_open(&pts, eps);
    }
    if pts.len() < 5 {
        return pts;
    }
    let far = (1..pts.len()).max_by(|&i, &j| (pts[i] - pts[0]).length().total_cmp(&(pts[j] - pts[0]).length())).unwrap_or(1);
    let mut first = simplify_open(&pts[..=far], eps);
    first.pop();
    let mut second: Vec<DVec2> = pts[far..].to_vec();
    second.push(pts[0]);
    let mut second = simplify_open(&second, eps);
    second.pop();
    first.extend(second);
    first
}

/// Ломаная в фигуру эскиза (после преобразования в миллиметры): без совпадающих подряд точек.
fn poly_shape(poly: &Poly, m: &Mat) -> Option<SketchShape> {
    let mut pts: Vec<DVec2> = Vec::with_capacity(poly.pts.len());
    for p in &poly.pts {
        let q = m.apply(*p);
        if pts.last().is_none_or(|l| (*l - q).length() > MIN_SIZE * 2.0) {
            pts.push(q);
        }
    }
    let mut closed = poly.closed;
    if pts.len() > 2 && (pts[0] - pts[pts.len() - 1]).length() <= MIN_SIZE * 2.0 {
        pts.pop();
        closed = true;
    }
    let pts = simplify(pts, closed, SIMPLIFY_TOLERANCE);
    if pts.len() < 2 || (closed && pts.len() < 3) {
        return None;
    }
    Some(SketchShape::Path { points: pts.into_iter().map(PathPoint::new).collect(), closed })
}

/// Фигуры одного элемента SVG.
fn element_shapes(name: &str, attrs: &Attrs, m: &Mat) -> Vec<SketchShape> {
    let poly = |pts: Vec<DVec2>, closed: bool| poly_shape(&Poly { pts, closed }, m);
    let mut out = Vec::new();
    match name {
        "path" => {
            if let Some(d) = attrs.get("d") {
                out.extend(parse_path(d).iter().filter_map(|p| poly_shape(p, m)));
            }
        }
        "rect" => {
            let (x, y, w, h) = (num(attrs, "x"), num(attrs, "y"), num(attrs, "width"), num(attrs, "height"));
            if w > 0.0 && h > 0.0 {
                let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)].map(|(a, b)| DVec2::new(a, b));
                if m.axis_aligned() {
                    let q = corners.map(|p| m.apply(p));
                    let (lo, hi) = (q[0].min(q[2]), q[0].max(q[2]));
                    if hi.x - lo.x > MIN_SIZE && hi.y - lo.y > MIN_SIZE {
                        out.push(SketchShape::Rect { x: lo.x, y: lo.y, w: hi.x - lo.x, h: hi.y - lo.y });
                    }
                } else {
                    out.extend(poly(corners.to_vec(), true));
                }
            }
        }
        "circle" => {
            let c = m.apply(DVec2::new(num(attrs, "cx"), num(attrs, "cy")));
            let r = num(attrs, "r") * m.scale_factor();
            if r > MIN_SIZE {
                out.push(SketchShape::Circle { cx: c.x, cy: c.y, r });
            }
        }
        "ellipse" => {
            let (cx, cy, rx, ry) = (num(attrs, "cx"), num(attrs, "cy"), num(attrs, "rx"), num(attrs, "ry"));
            if rx > 0.0 && ry > 0.0 {
                let pts = (0..96)
                    .map(|i| {
                        let t = std::f64::consts::TAU * i as f64 / 96.0;
                        DVec2::new(cx + rx * t.cos(), cy + ry * t.sin())
                    })
                    .collect();
                out.extend(poly(pts, true));
            }
        }
        "line" => {
            let (a, b) = (DVec2::new(num(attrs, "x1"), num(attrs, "y1")), DVec2::new(num(attrs, "x2"), num(attrs, "y2")));
            out.extend(poly(vec![a, b], false));
        }
        "polyline" | "polygon" => {
            let v = attrs.get("points").map(|s| numbers(s)).unwrap_or_default();
            let pts: Vec<DVec2> = v.chunks_exact(2).map(|c| DVec2::new(c[0], c[1])).collect();
            out.extend(poly(pts, name == "polygon"));
        }
        _ => {}
    }
    out
}

/// Теги SVG по порядку: (имя, признак закрывающего, признак самозакрытого, атрибуты).
fn tags(text: &str) -> Vec<(String, bool, bool, Attrs)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if text[i..].starts_with("<!--") {
            i = text[i..].find("-->").map_or(b.len(), |e| i + e + 3);
            continue;
        }
        if text[i..].starts_with("<?") || text[i..].starts_with("<!") {
            i = text[i..].find('>').map_or(b.len(), |e| i + e + 1);
            continue;
        }
        // Конец тега — первая «>» вне кавычек.
        let mut j = i + 1;
        let mut quote = 0u8;
        while j < b.len() {
            match (quote, b[j]) {
                (0, b'"' | b'\'') => quote = b[j],
                (q, c) if q != 0 && c == q => quote = 0,
                (0, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        let inner = &text[i + 1..j.min(b.len())];
        i = j + 1;
        let closing = inner.starts_with('/');
        let self_closing = inner.ends_with('/');
        let body = inner.trim_start_matches('/').trim_end_matches('/');
        let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        let name = body[..name_end].rsplit(':').next().unwrap_or("").to_string();
        out.push((name, closing, self_closing, attributes(&body[name_end..])));
    }
    out
}

fn attributes(s: &str) -> Attrs {
    let mut map = Attrs::new();
    let mut rest = s;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
        let Some(end) = after[1..].find(q) else { break };
        map.insert(key, after[1..1 + end].to_string());
        rest = &after[end + 2..];
    }
    map
}

/// Читает SVG и возвращает фигуры эскиза: в миллиметрах, Y вверх, центр контуров — в начале координат.
pub fn parse(text: &str) -> Result<Vec<SketchShape>, String> {
    const SKIP: [&str; 7] = ["defs", "clipPath", "mask", "symbol", "marker", "pattern", "style"];
    let mut stack: Vec<Mat> = vec![Mat::IDENT];
    let mut shapes: Vec<SketchShape> = Vec::new();
    let mut skip_depth = 0usize;
    let mut root_seen = false;
    for (name, closing, self_closing, attrs) in tags(text) {
        if closing {
            if SKIP.contains(&name.as_str()) {
                skip_depth = skip_depth.saturating_sub(1);
            } else if skip_depth == 0 && stack.len() > 1 && (name == "g" || name == "svg" || name == "a") {
                stack.pop();
            }
            continue;
        }
        if SKIP.contains(&name.as_str()) {
            if !self_closing {
                skip_depth += 1;
            }
            continue;
        }
        if skip_depth > 0 {
            continue;
        }
        let parent = *stack.last().unwrap_or(&Mat::IDENT);
        let own = attrs.get("transform").map_or(Mat::IDENT, |t| parse_transform(t));
        match name.as_str() {
            "svg" => {
                // Корень задаёт масштаб (единицы → мм) и разворот оси Y; вложенные svg просто группируют.
                let mut m = parent;
                if !root_seen {
                    root_seen = true;
                    let vb = attrs.get("viewBox").map(|v| numbers(v)).filter(|v| v.len() == 4);
                    let width = attrs.get("width").and_then(|w| length(w));
                    let height = attrs.get("height").and_then(|h| length(h));
                    let per_unit = match (&vb, width, height) {
                        (Some(v), Some((w, per)), _) if v[2] > 0.0 => w * per / v[2],
                        (Some(v), _, Some((h, per))) if v[3] > 0.0 => h * per / v[3],
                        (_, Some((_, per)), _) => per,
                        _ => 25.4 / 96.0,
                    };
                    let (ox, oy) = vb.as_ref().map_or((0.0, 0.0), |v| (v[0], v[1]));
                    // y' = -(y - oy)·k: ось Y вверх.
                    m = Mat { a: per_unit, b: 0.0, c: 0.0, d: -per_unit, e: -ox * per_unit, f: oy * per_unit };
                }
                if !self_closing {
                    stack.push(m.mul(own));
                }
            }
            "g" | "a" => {
                if !self_closing {
                    stack.push(parent.mul(own));
                }
            }
            _ => {
                if attrs.get("display").is_some_and(|d| d == "none") {
                    continue;
                }
                shapes.extend(element_shapes(&name, &attrs, &parent.mul(own)));
            }
        }
    }
    if shapes.is_empty() {
        return Err("в SVG не найдено контуров (читаются path, rect, circle, ellipse, line, polyline, polygon)".into());
    }
    // Центр всех контуров — в начало координат.
    let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
    for s in &shapes {
        let outline = s.outline();
        for p in outline {
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    let shift = -(lo + hi) / 2.0;
    for s in &mut shapes {
        s.translate(shift);
    }
    Ok(shapes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn rect_and_circle_in_millimeters() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
            <rect x="10" y="10" width="40" height="20"/><circle cx="80" cy="25" r="5"/></svg>"#;
        let shapes = parse(svg).unwrap();
        assert_eq!(shapes.len(), 2);
        // Габарит: x 10…85, y 10…30 (в SVG) — центр (47.5, 20) уходит в начало, размеры в мм сохраняются.
        let SketchShape::Rect { x, y, w, h } = shapes[0] else { panic!("{:?}", shapes[0]) };
        assert!(close(w, 40.0) && close(h, 20.0));
        assert!(close(x, 10.0 - 47.5) && close(y, -10.0), "{x} {y}");
        let SketchShape::Circle { cx, r, .. } = shapes[1] else { panic!() };
        assert!(close(r, 5.0) && close(cx, 80.0 - 47.5));
    }

    #[test]
    fn y_axis_points_up() {
        let svg = r#"<svg viewBox="0 0 10 10" width="10mm" height="10mm"><path d="M0 0 L10 0 L10 4 Z"/></svg>"#;
        let SketchShape::Path { points, closed } = &parse(svg).unwrap()[0] else { panic!() };
        assert!(*closed && points.len() == 3);
        // В SVG вершина (10, 4) ниже линии y=0; в эскизе — наоборот, выше.
        assert!(points[2].y < points[0].y);
    }

    #[test]
    fn relative_commands_arcs_and_curves() {
        let svg = r#"<svg width="20mm" height="20mm" viewBox="0 0 20 20">
            <path d="m2,2 h10 v10 h-10 z M 15 15 a 2 2 0 1 1 -4 0 a 2 2 0 1 1 4 0 z
                     M 1 18 C 3 14, 6 14, 8 18 S 12 22, 14 18 Q 16 16 18 18"/></svg>"#;
        let shapes = parse(svg).unwrap();
        assert_eq!(shapes.len(), 3);
        let SketchShape::Path { points, closed } = &shapes[0] else { panic!() };
        assert!(*closed && points.len() == 4);
        // Окружность из двух дуг получилась замкнутым контуром примерно того же диаметра.
        let SketchShape::Path { points, closed } = &shapes[1] else { panic!() };
        let (lo, hi) = points.iter().fold((DVec2::splat(1e9), DVec2::splat(-1e9)), |(l, h), p| (l.min(p.pos()), h.max(p.pos())));
        assert!(*closed && (hi.x - lo.x - 4.0).abs() < 0.05 && (hi.y - lo.y - 4.0).abs() < 0.05, "{lo} {hi}");
        // Кривая не замкнута и сглажена многими точками.
        let SketchShape::Path { points, closed } = &shapes[2] else { panic!() };
        assert!(!*closed && points.len() > 20);
    }

    #[test]
    fn transforms_and_groups() {
        let svg = r#"<svg width="100mm" height="100mm" viewBox="0 0 100 100">
            <g transform="translate(10 0) scale(2)"><rect width="5" height="5"/></g>
            <rect transform="rotate(45)" width="5" height="5"/></svg>"#;
        let shapes = parse(svg).unwrap();
        assert_eq!(shapes.len(), 2);
        let SketchShape::Rect { w, h, .. } = shapes[0] else { panic!() };
        assert!(close(w, 10.0) && close(h, 10.0));
        // Повёрнутый квадрат — уже контур из четырёх точек.
        assert!(matches!(&shapes[1], SketchShape::Path { points, closed: true } if points.len() == 4));
    }

    #[test]
    fn noisy_outline_is_simplified() {
        // Окружность из тысячи точек с мелким шумом упрощается до нескольких десятков, форма не меняется.
        let n = 1000;
        let pts: Vec<String> = (0..n)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / n as f64;
                let noise = if i % 2 == 0 { 0.002 } else { -0.002 };
                format!("{:.4},{:.4}", (5.0 + noise) * t.cos() + 10.0, (5.0 + noise) * t.sin() + 10.0)
            })
            .collect();
        let svg = format!(r#"<svg width="20mm" height="20mm" viewBox="0 0 20 20"><polygon points="{}"/></svg>"#, pts.join(" "));
        let SketchShape::Path { points, closed: true } = &parse(&svg).unwrap()[0] else { panic!() };
        assert!(points.len() < 120, "{} точек", points.len());
        let r = points.iter().map(|p| p.pos().length()).fold(0.0f64, f64::max);
        assert!((r - 5.0).abs() < 0.03, "радиус {r}");
    }

    #[test]
    fn empty_and_hidden_content() {
        assert!(parse("<svg><text>привет</text></svg>").is_err());
        let svg = r#"<svg><defs><rect width="5" height="5"/></defs><rect width="3" height="3" display="none"/></svg>"#;
        assert!(parse(svg).is_err());
    }
}
