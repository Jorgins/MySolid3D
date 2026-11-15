"""Готовит иконки приложения: assets/icons/*.png (64x64, прозрачный фон) и assets/app_icon.png.
Запуск из корня проекта: python make_icons.py <папка для контрольного листа>"""
import os, sys
from PIL import Image, ImageDraw

OUT = 'assets/icons'
os.makedirs(OUT, exist_ok=True)
for f in os.listdir(OUT):
    os.remove(os.path.join(OUT, f))
SIZE = 64


def keyed(im, bg, lo=10, hi=70):
    """Фон -> прозрачность: чем дальше цвет пикселя от фона, тем он плотнее."""
    im = im.convert('RGB')
    out = Image.new('RGBA', im.size)
    px = []
    for (r, g, b) in im.getdata():
        d = max(abs(r - bg[0]), abs(g - bg[1]), abs(b - bg[2]))
        px.append((r, g, b, max(0, min(255, int((d - lo) * 255 / (hi - lo))))))
    out.putdata(px)
    return out


# 1. Набор in/2/icons: (файл в наборе -> имя в программе)
SET2 = {
    'new_file': 'new_file', 'open_file': 'open_file', 'save_file': 'save_file', 'undo': 'undo', 'redo': 'redo',
    'sketch': 'sketch', 'extrude_boss': 'extrude_boss', 'extrude_cut': 'extrude_cut', 'fillet': 'fillet',
    'chamfer': 'chamfer', 'view_isometric': 'view_iso', 'view_front': 'view_front', 'view_top': 'view_top',
    'view_right': 'view_right', 'fit_view': 'fit_view', 'zoom_in': 'zoom_in', 'zoom_out': 'zoom_out',
    'wireframe': 'wireframe', 'solid_view': 'solid_view', 'select': 'select', 'dimension': 'dimension',
    'delete': 'delete', 'copy': 'copy', 'rotate': 'rotate', 'scale': 'scale',
    'materials': 'materials',
}
for src, name in SET2.items():
    im = Image.open('in/2/icons/%s.png' % src).convert('RGB')
    bg = im.getpixel((2, 2))
    keyed(im, bg).resize((SIZE, SIZE), Image.LANCZOS).save('%s/%s.png' % (OUT, name))

# 2. Лист icons.jfif (4 ряда по 7): (ряд, столбец) -> имя
sheet = Image.open('icons.jfif').convert('RGB')
bg = sheet.getpixel((20, 20))
cx = [280, 520, 760, 1000, 1240, 1478, 1716]
cy = [150, 388, 628, 870]
SHEET = {'arc': (1, 3), 'mirror': (0, 3), 'offset': (0, 4), 'corner_chamfer': (1, 5), 'corner_fillet': (1, 6),
         'pattern_grid': (3, 0), 'pattern_circle': (3, 2)}
for name, (row, col) in SHEET.items():
    x, y, h = cx[col] * 1.92, cy[row] * 1.92, 104 * 1.92
    tile = sheet.crop((int(x - h), int(y - h), int(x + h), int(y + h))).resize((SIZE, SIZE), Image.LANCZOS)
    keyed(tile, bg, 6, 76).save('%s/%s.png' % (OUT, name))

# 3. Недостающие инструменты рисования — в том же стиле (светло-голубая линия, голубые точки).
LINE, DOT = (200, 220, 248, 255), (72, 190, 250, 255)
K = 8  # рисуем крупно и уменьшаем — для сглаживания


def drawn(name, paint):
    im = Image.new('RGBA', (SIZE * K, SIZE * K), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    paint(d, lambda v: int(v * K))
    im.resize((SIZE, SIZE), Image.LANCZOS).save('%s/%s.png' % (OUT, name))


def dot(d, s, x, y, r=5):
    d.ellipse((s(x - r), s(y - r), s(x + r), s(y + r)), fill=DOT)


def line_icon(d, s):
    d.line((s(13), s(50), s(51), s(14)), fill=LINE, width=s(4))
    dot(d, s, 13, 50)
    dot(d, s, 51, 14)


def rect_icon(d, s):
    d.rounded_rectangle((s(10), s(17), s(54), s(47)), radius=s(3), outline=LINE, width=s(4))
    dot(d, s, 10, 47)
    dot(d, s, 54, 17)


def circle_icon(d, s):
    d.ellipse((s(11), s(11), s(53), s(53)), outline=LINE, width=s(4))
    dot(d, s, 32, 32, 4)


drawn('line', line_icon)
drawn('rect', rect_icon)
drawn('circle', circle_icon)

# 3б. «Сохранить как»: значок «сохранить» с карандашом в правом нижнем углу.
save_as = Image.open('%s/save_file.png' % OUT).convert('RGBA')
badge = Image.new('RGBA', (SIZE * K, SIZE * K), (0, 0, 0, 0))
bd = ImageDraw.Draw(badge)
bd.ellipse((int(33 * K), int(33 * K), int(63 * K), int(63 * K)), fill=(18, 24, 36, 255), outline=DOT, width=int(2 * K))
bd.line((int(41 * K), int(55 * K), int(55 * K), int(41 * K)), fill=(255, 255, 255, 255), width=int(5 * K))
bd.polygon([(int(38 * K), int(58 * K)), (int(40 * K), int(52 * K)), (int(44 * K), int(56 * K))], fill=DOT)
save_as.alpha_composite(badge.resize((SIZE, SIZE), Image.LANCZOS))
save_as.save('%s/save_as.png' % OUT)

# 4. Иконка приложения.
Image.open('in/05_axis_cube.png').convert('RGBA').resize((256, 256), Image.LANCZOS).save('assets/app_icon.png')

# Контрольный лист.
names = sorted(f[:-4] for f in os.listdir(OUT))
print(len(names), ' '.join(names))
cols = 10
check = Image.new('RGBA', (cols * 72, ((len(names) + cols - 1) // cols) * 72), (40, 44, 52, 255))
for i, n in enumerate(names):
    check.alpha_composite(Image.open('%s/%s.png' % (OUT, n)), ((i % cols) * 72 + 4, (i // cols) * 72 + 4))
check.resize((check.width * 2, check.height * 2), Image.LANCZOS).save(os.path.join(sys.argv[1], 'check_icons.png')) if len(sys.argv) > 1 else None
