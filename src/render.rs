//! Отрисовка тела через OpenGL: мягкий студийный свет + рёбра. Рисуется только по запросу egui.

use crate::model::RenderMesh;
use eframe::glow::{self, HasContext as _};

const MESH_VS: &str = r#"#version 330 core
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_normal;
layout(location = 2) in float a_face;
layout(location = 3) in vec4 a_color;
uniform mat4 u_view_proj;
uniform mat3 u_normal_mat;
out vec3 v_normal;
flat out float v_face;
flat out vec4 v_color;
out vec3 v_pos;
void main() {
    v_pos = a_pos;
    v_normal = u_normal_mat * a_normal;
    v_face = a_face;
    v_color = a_color;
    gl_Position = u_view_proj * vec4(a_pos, 1.0);
}
"#;

const MESH_FS: &str = r#"#version 330 core
in vec3 v_normal;
flat in float v_face;
flat in vec4 v_color;
in vec3 v_pos;
uniform float u_hover;
uniform float u_selected;
uniform float u_alpha;
// -1 — прозрачное тело (всё с u_alpha); 0 — непрозрачные материалы; 1 — стекло.
uniform float u_pass;
out vec4 o_color;
float hash3(vec3 p) {
    return fract(sin(dot(p, vec3(12.9898, 78.233, 37.719))) * 43758.5453);
}
void main() {
    vec3 n = normalize(v_normal);
    if (!gl_FrontFacing) n = -n;

    vec3 base = v_color.rgb;
    int mat = int(v_color.w + 0.5);
    bool glass = mat == 2;
    if (u_pass > -0.5) {
        if (u_pass < 0.5 && glass) discard;
        if (u_pass > 0.5 && !glass) discard;
    }
    // Полусферическая подсветка: небо сверху, тёплый отсвет снизу.
    vec3 ambient = mix(vec3(0.34, 0.32, 0.32), vec3(0.52, 0.55, 0.60), n.y * 0.5 + 0.5);
    vec3 key_dir = normalize(vec3(-0.45, 0.65, 0.62));
    vec3 fill_dir = normalize(vec3(0.75, 0.10, 0.45));
    float key = max(dot(n, key_dir), 0.0);
    float fill = max(dot(n, fill_dir), 0.0);
    vec3 half_dir = normalize(key_dir + vec3(0.0, 0.0, 1.0));
    float spec = pow(max(dot(n, half_dir), 0.0), 60.0) * 0.22;
    float rim = pow(1.0 - max(n.z, 0.0), 3.0) * 0.16;

    vec3 c = base * (ambient + key * 0.52 + fill * 0.24) + spec + rim * vec3(0.65, 0.78, 1.0);
    float alpha = 1.0;
    // Условное окружение: тёмный низ, светлое небо и яркая полоса горизонта — по ним «отражается» металл.
    vec3 refl = reflect(vec3(0.0, 0.0, -1.0), n);
    vec3 env = mix(vec3(0.07, 0.08, 0.10), vec3(0.78, 0.85, 0.97), smoothstep(-0.35, 0.55, refl.y))
        + vec3(0.55) * exp(-pow((refl.y - 0.08) * 7.0, 2.0));
    float fres = pow(1.0 - max(n.z, 0.0), 3.0);
    if (mat == 1) {
        // Металл: почти нет рассеянного света, цвет — в отражении и ярком блике.
        float hl = pow(max(dot(n, half_dir), 0.0), 45.0);
        c = env * mix(base, vec3(1.0), 0.12) * 0.95 + base * (ambient + key * 0.5) * 0.07 + hl * 0.85;
    } else if (glass) {
        // Стекло: лёгкий оттенок, прозрачность растёт к краям (Френель), блик остаётся ярким.
        float hl = pow(max(dot(n, half_dir), 0.0), 90.0);
        c = mix(base, vec3(1.0), 0.5) * (ambient * 0.45 + key * 0.18) + env * (0.18 + 0.5 * fres) + hl;
        alpha = clamp(mix(0.22, 0.85, fres) + hl, 0.0, 1.0);
    } else if (mat == 3) {
        // Камень: матовый, с крапом по положению в пространстве (при вращении не «плывёт»).
        float grain = 0.76 + 0.26 * hash3(floor(v_pos * 1.6)) + 0.22 * hash3(floor(v_pos * 7.0));
        c = base * grain * (ambient + key * 0.45 + fill * 0.2);
    }
    if (abs(v_face - u_selected) < 0.5) {
        c = mix(c, vec3(0.20, 0.62, 1.0), 0.50);
    } else if (abs(v_face - u_hover) < 0.5) {
        c = mix(c, vec3(1.0, 0.72, 0.25), 0.35);
    }
    o_color = vec4(c, u_pass < -0.5 ? u_alpha : alpha);
}
"#;

const EDGE_VS: &str = r#"#version 330 core
layout(location = 0) in vec3 a_pos;
uniform mat4 u_view_proj;
void main() {
    gl_Position = u_view_proj * vec4(a_pos, 1.0);
}
"#;

const EDGE_FS: &str = r#"#version 330 core
out vec4 o_color;
void main() {
    o_color = vec4(0.07, 0.08, 0.10, 1.0);
}
"#;

pub struct DrawParams {
    pub view_proj: [f32; 16],
    pub normal_mat: [f32; 9],
    pub hover: Option<usize>,
    pub selected: Option<usize>,
    pub mode: RenderMode,
}

/// Режим отрисовки тела.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum RenderMode {
    /// Закрашенное тело с рёбрами.
    #[default]
    Solid,
    /// Закрашенное тело без линий рёбер.
    SolidNoEdges,
    /// Только видимые рёбра; грани не закрашены, но скрывают то, что за ними.
    HiddenLines,
    /// Только рёбра, видны все (в том числе скрытые за деталью).
    Wireframe,
    /// Полупрозрачное тело: сквозь него видны все рёбра.
    XRay,
}

impl RenderMode {
    pub const ALL: [RenderMode; 5] =
        [RenderMode::Solid, RenderMode::SolidNoEdges, RenderMode::HiddenLines, RenderMode::Wireframe, RenderMode::XRay];

    pub fn name(self) -> &'static str {
        match self {
            RenderMode::Solid => "Тело с рёбрами",
            RenderMode::SolidNoEdges => "Тело без рёбер",
            RenderMode::HiddenLines => "Скрытые линии убраны",
            RenderMode::Wireframe => "Каркас",
            RenderMode::XRay => "Прозрачное тело",
        }
    }
}

pub struct Renderer {
    mesh_program: glow::Program,
    edge_program: glow::Program,
    mesh_vao: glow::VertexArray,
    mesh_vbo: glow::Buffer,
    mesh_ibo: glow::Buffer,
    /// Цвет каждой вершины (r, g, b) — отдельным буфером, чтобы перекрашивать без пересборки сетки.
    color_vbo: glow::Buffer,
    edge_vao: glow::VertexArray,
    edge_vbo: glow::Buffer,
    index_count: i32,
    edge_vertex_count: i32,
    /// В детали есть стеклянные грани — нужен второй (прозрачный) проход.
    has_glass: bool,
}

fn as_bytes<T: Copy>(data: &[T]) -> &[u8] {
    // SAFETY: f32/u32 — простые типы без паддинга; срез живёт столько же, сколько исходный.
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), std::mem::size_of_val(data)) }
}

unsafe fn compile(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
    let program = gl.create_program()?;
    let mut shaders = Vec::new();
    for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
        let shader = gl.create_shader(kind)?;
        gl.shader_source(shader, src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            return Err(format!("ошибка шейдера: {}", gl.get_shader_info_log(shader)));
        }
        gl.attach_shader(program, shader);
        shaders.push(shader);
    }
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        return Err(format!("ошибка линковки: {}", gl.get_program_info_log(program)));
    }
    for shader in shaders {
        gl.detach_shader(program, shader);
        gl.delete_shader(shader);
    }
    Ok(program)
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let mesh_program = compile(gl, MESH_VS, MESH_FS)?;
            let edge_program = compile(gl, EDGE_VS, EDGE_FS)?;

            let mesh_vao = gl.create_vertex_array()?;
            let mesh_vbo = gl.create_buffer()?;
            let mesh_ibo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(mesh_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh_vbo));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(mesh_ibo));
            let stride = 7 * 4;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 12);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 24);
            let color_vbo = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(color_vbo));
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 4, glow::FLOAT, false, 16, 0);

            let edge_vao = gl.create_vertex_array()?;
            let edge_vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(edge_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(edge_vbo));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            Ok(Self {
                mesh_program,
                edge_program,
                mesh_vao,
                mesh_vbo,
                mesh_ibo,
                color_vbo,
                edge_vao,
                edge_vbo,
                index_count: 0,
                edge_vertex_count: 0,
                has_glass: false,
            })
        }
    }

    pub fn upload(&mut self, gl: &glow::Context, mesh: &RenderMesh) {
        unsafe {
            gl.bind_vertex_array(Some(self.mesh_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.mesh_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&mesh.vertices), glow::STATIC_DRAW);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.mesh_ibo));
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                as_bytes(&mesh.indices),
                glow::STATIC_DRAW,
            );
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.edge_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&mesh.edge_lines), glow::STATIC_DRAW);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
        self.index_count = mesh.indices.len() as i32;
        self.edge_vertex_count = (mesh.edge_lines.len() / 3) as i32;
    }

    /// Цвета вершин: по четыре числа на вершину (r, g, b, номер материала), в том же порядке, что и в сетке.
    pub fn upload_colors(&mut self, gl: &glow::Context, colors: &[f32]) {
        self.has_glass = colors.chunks_exact(4).any(|c| c[3] > 1.5 && c[3] < 2.5);
        unsafe {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.color_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(colors), glow::DYNAMIC_DRAW);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    pub fn paint(&self, gl: &glow::Context, p: &DrawParams) {
        if self.index_count == 0 {
            return;
        }
        unsafe {
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);

            // Грани чуть отодвигаем вглубь, чтобы рёбра рисовались поверх без мерцания.
            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(1.0, 1.0);
            gl.use_program(Some(self.mesh_program));
            let loc = |name| gl.get_uniform_location(self.mesh_program, name);
            gl.uniform_matrix_4_f32_slice(loc("u_view_proj").as_ref(), false, &p.view_proj);
            gl.uniform_matrix_3_f32_slice(loc("u_normal_mat").as_ref(), false, &p.normal_mat);
            let id = |v: Option<usize>| v.map_or(-10.0, |i| i as f32);
            gl.uniform_1_f32(loc("u_hover").as_ref(), id(p.hover));
            gl.uniform_1_f32(loc("u_selected").as_ref(), id(p.selected));
            let xray = p.mode == RenderMode::XRay;
            gl.uniform_1_f32(loc("u_alpha").as_ref(), if xray { 0.28 } else { 1.0 });
            let pass = loc("u_pass");
            gl.uniform_1_f32(pass.as_ref(), if xray { -1.0 } else { 0.0 });
            gl.bind_vertex_array(Some(self.mesh_vao));
            if xray {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.depth_mask(false);
            }
            // «Скрытые линии»: грани пишут только глубину — цвета не видно, но рёбра за ними пропадают.
            if p.mode == RenderMode::HiddenLines {
                gl.color_mask(false, false, false, false);
            }
            if p.mode != RenderMode::Wireframe {
                gl.draw_elements(glow::TRIANGLES, self.index_count, glow::UNSIGNED_INT, 0);
            }
            // Стекло — вторым проходом, с прозрачностью и без записи глубины.
            if self.has_glass && !xray && matches!(p.mode, RenderMode::Solid | RenderMode::SolidNoEdges) {
                gl.uniform_1_f32(pass.as_ref(), 1.0);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.depth_mask(false);
                gl.draw_elements(glow::TRIANGLES, self.index_count, glow::UNSIGNED_INT, 0);
                gl.disable(glow::BLEND);
                gl.depth_mask(true);
            }
            if p.mode == RenderMode::HiddenLines {
                gl.color_mask(true, true, true, true);
            }
            if xray {
                gl.disable(glow::BLEND);
                gl.depth_mask(true);
            }
            gl.disable(glow::POLYGON_OFFSET_FILL);
            if p.mode == RenderMode::SolidNoEdges {
                gl.bind_vertex_array(None);
                gl.use_program(None);
                gl.disable(glow::DEPTH_TEST);
                gl.enable(glow::BLEND);
                return;
            }

            gl.use_program(Some(self.edge_program));
            let loc = gl.get_uniform_location(self.edge_program, "u_view_proj");
            gl.uniform_matrix_4_f32_slice(loc.as_ref(), false, &p.view_proj);
            gl.bind_vertex_array(Some(self.edge_vao));
            gl.draw_arrays(glow::LINES, 0, self.edge_vertex_count);

            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::DEPTH_TEST);
            gl.enable(glow::BLEND);
        }
    }
}
