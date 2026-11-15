"""Строит верхнюю крышку к корпусу RP2040 и добавляет в корпус выемки замков и насечки на линии смыкания.

Запуск: python tools/build_rp2040_lid.py   (сам запускает MySolid3D с API на свободном порту и закрывает его).

Геометрия корпуса (из samples/КорпусRP2040.ms3d, система координат мира):
  дно y=0…2, стенки 3 мм вокруг платы 23×53.3 (x 0…23, z 0…53.3), открытая сторона — в сторону −Y;
  внешний контур 29×59.3 (x −3…26, z −3…56.3), углы R3; бортик (шип) высотой 4 мм шириной 1.5 мм
  по периметру полости, его внешний контур 26×56.3 (x −1.5…24.5, z −1.5…54.8), углы R2, верх на y=−5;
  внешняя половина стенки срезана до y=−1 (уступ шириной 1.5 мм); в торце z>53.3 вырез под USB (x 7…16).
Крышка зазора не имеет: пластина лежит на бортике (y −5…−7), юбка 1.5 мм заполняет уступ (y −5…−1).
"""

import glob
import math
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mysolid_client import CAD, CADError  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASE = glob.glob(os.path.join(ROOT, "samples", "*RP2040.ms3d"))[0]
LID = os.path.join(os.path.dirname(CASE), "КрышкаRP2040.ms3d")
BACKUP = CASE.replace(".ms3d", " (до замков).ms3d")

BULGE = math.tan(math.radians(90) / 4)  # четверть окружности

# Параметры (мм). Предположения про плату: толщина 1.0, кнопка выступает на 1.7, светодиоды 0.7 и 1.6.
PLATE = 2.0                      # толщина пластины крышки
BUMP_H = 0.4                     # высота защёлки (глубина выемки)
LEDS_SMALL = [(5.13, 50.43), (17.63, 50.43)]   # GP25 и PWR по бокам USB-разъёма (x, z)
LED_RGB = (11.69, 7.06)                        # RGB-светодиод внизу платы
BOOT = (6.44, 31.85)                           # кнопка BOOT (ближе к разъёму)
PIPE_D, HOLE_D = 3.0, 1.4        # световод: наружный диаметр столбика и канал
PIPE_SMALL, PIPE_RGB = 3.0, 2.1  # длина столбиков от пластины (низ на 0.3 мм выше светодиода)
STUB_D, STUB_LEN = 2.4, 1.8      # толкатель над кнопкой: зазор около 0.5 мм до нажатия
BTN_R = 4.0                      # радиус круглой площадки кнопки
# Углубление под колодку штекера USB-C на стороне разъёма (в пластине снизу, открыто к торцу):
# даёт колодке до 6 мм высоты вместо 4 мм выреза; начинается за световодами (z > 51.9).
PLUG_RECESS = (5.0, 52.2, 18.0, 57.0, 1.5)   # x0, z0, x1, z1 (z1 за краем — вырез выходит в торец), радиус углов
PLUG_RECESS_DEPTH = 1.0                      # остаётся 1 мм пластины


def rounded_rect(x0, y0, x1, y1, r):
    """Прямоугольник со скруглёнными углами (против часовой стрелки; дуги — число bulge у начала дуги)."""
    return {"type": "path", "closed": True, "points": [
        [x0 + r, y0], [x1 - r, y0, BULGE], [x1, y0 + r], [x1, y1 - r, BULGE],
        [x1 - r, y1], [x0 + r, y1, BULGE], [x0, y1 - r], [x0, y0 + r, BULGE]]}


def rect(x0, z0, x1, z1):
    return {"type": "rect", "x": min(x0, x1), "y": min(z0, z1), "w": abs(x1 - x0), "h": abs(z1 - z0)}


def circle(x, z, d):
    return {"type": "circle", "cx": x, "cy": z, "r": d / 2}


def c_slot(cx, cz, r_in, r_out, half_bridge):
    """C-образная прорезь вокруг круглой кнопки: круглая площадка радиуса r_in держится на перемычке
    шириной 2·half_bridge со стороны +Z (к разъёму). Дуги — через bulge = tan(угол/4)."""
    def arc_y(r):
        return math.sqrt(r * r - half_bridge * half_bridge)

    def sweep(r):
        return math.radians(360) - 2 * math.asin(half_bridge / r)

    yo, yi = arc_y(r_out), arc_y(r_in)
    return {"type": "path", "closed": True, "points": [
        [cx - half_bridge, cz + yo, math.tan(sweep(r_out) / 4)],   # внешняя дуга против часовой стрелки
        [cx + half_bridge, cz + yo],
        [cx + half_bridge, cz + yi, -math.tan(sweep(r_in) / 4)],   # внутренняя — по часовой
        [cx - half_bridge, cz + yi]]}


def bump(side):
    """Профиль защёлки в плоскости XY: левая сторона у x=−1.5, правая зеркальна относительно x=11.5."""
    pts = [(-1.5, -3.8), (-1.1, -3.6), (-1.1, -2.8), (-1.5, -1.8)]
    if side == "right":
        pts = [(23 - x, y) for x, y in pts]
    return {"type": "polygon", "points": [list(p) for p in pts]}


def knurl_rects():
    """Вертикальные насечки (как на ребре монеты): ширина 0.8, шаг 2, глубина 0.4, на прямых участках по периметру."""
    out = []
    z = 1.4
    while z < 51.8:
        out.append(rect(-3.2, z, -3 + BUMP_H, z + 0.8))      # левая сторона
        out.append(rect(26.2, z, 26 - BUMP_H, z + 0.8))      # правая сторона
        z += 2.0
    x = 1.6
    while x < 21.4:
        out.append(rect(x, -3.2, x + 0.8, -3 + BUMP_H))      # нижний торец
        if not (5.5 <= x <= 17.5):                           # у выреза под USB насечек нет
            out.append(rect(x, 56.3 - BUMP_H, x + 0.8, 56.5))
        x += 2.0
    return out


def must(cad, cmd, **kw):
    answer = cad.call(cmd, **kw)
    if answer.get("error"):
        raise CADError(f"{cmd}: {answer['error']}")
    return answer


def sketch(cad, plane, shapes, offset=None):
    sk = cad.add_sketch(plane, offset=offset)
    cad.sketch_add(sk, *shapes)
    return sk


def lid_bosses(cad):
    """Тело крышки без вырезов: пластина, юбка, защёлки, световодные столбики, толкатель."""
    cad.boss(sketch(cad, "front", [rounded_rect(-3, -3, 26, 56.3, 3)], offset=5), PLATE)           # y −5 → −7
    cad.boss(sketch(cad, "front", [rounded_rect(-3, -3, 26, 56.3, 3), rounded_rect(-1.5, -1.5, 24.5, 54.8, 2)], offset=1), 4)  # юбка y −1 → −5
    for z0 in (9.0, 36.0):                                                                          # защёлки: две пары, длина 8
        cad.boss(sketch(cad, "top", [bump("left"), bump("right")], offset=z0), 8)
    cad.boss(sketch(cad, "front", [circle(x, z, PIPE_D) for x, z in LEDS_SMALL], offset=5), PIPE_SMALL, flip=True)
    cad.boss(sketch(cad, "front", [circle(*LED_RGB, PIPE_D)], offset=5), PIPE_RGB, flip=True)
    cad.boss(sketch(cad, "front", [circle(*BOOT, STUB_D)], offset=5), STUB_LEN, flip=True)


def lid_cuts(cad):
    # Вырез под USB в юбке (как в корпусе: x 7…16, y −5…−1, на глубину стенки).
    cad.cut(sketch(cad, "top", [rect(7, -5, 16, -1)], offset=56.3), 3)
    # Углубление под колодку штекера: вверх от нижней плоскости пластины (y=−5), к торцу с разъёмом.
    px0, pz0, px1, pz1, pr = PLUG_RECESS
    cad.cut(sketch(cad, "front", [rounded_rect(px0, pz0, px1, pz1, pr)], offset=5), PLUG_RECESS_DEPTH, flip=True)
    # Круглая кнопка над BOOT: площадка Ø8 в C-образной прорези шириной 0.8 мм, перемычка со стороны разъёма.
    bx, bz = BOOT
    top = 5 + PLATE  # верхняя поверхность пластины: смещение 7 от плоскости спереди
    cad.cut(sketch(cad, "front", [c_slot(bx, bz, BTN_R, BTN_R + 0.8, 1.8)], offset=top), PLATE + 0.2)
    # Шарнир: канавка сверху поперёк перемычки, остаётся 1.2 мм.
    cad.cut(sketch(cad, "front", [rect(bx - 1.8, bz + BTN_R - 0.8, bx + 1.8, bz + BTN_R + 1.6)], offset=top), 0.8)
    # Каналы световодов: насквозь через пластину и столбики.
    cad.cut(sketch(cad, "front", [circle(x, z, HOLE_D) for x, z in LEDS_SMALL + [LED_RGB]], offset=top), PLATE + PIPE_SMALL + 0.2)


def knurl_cut(cad):
    """Насечки на линии смыкания (y=−1): от y=−2.5 до y=0.4 — поровну на юбку крышки и стенку корпуса."""
    cad.cut(sketch(cad, "front", knurl_rects(), offset=-0.4), 2.9, flip=True)


def case_locks(cad):
    """Выемки в бортике корпуса под защёлки крышки (по форме защёлки, зазор 0)."""
    for z0 in (9.0, 36.0):
        sk = sketch(cad, "top", [bump("left"), bump("right")], offset=z0 + 8)
        cad.cut(sk, 8)  # вырез идёт против нормали +Z: от z0+8 до z0


def report(cad, title):
    st = cad.state()
    body = st["body"]
    errors = [(f["name"], f["error"]) for f in st["features"] if f["error"]]
    print(f"{title}: размер {body.get('size')}, объём {body.get('volume')}, ошибки {errors or 'нет'}")
    return body.get("volume")


def start(port):
    exe = os.path.join(ROOT, "target", "debug", "mysolid3d.exe")
    proc = subprocess.Popen([exe, f"--api={port}"])
    cad = CAD(port=port)
    for _ in range(80):
        try:
            cad.docs()
            return proc, cad
        except Exception:
            time.sleep(0.5)
    proc.kill()
    raise SystemExit("программа не запустилась")


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    if not os.path.exists(BACKUP):
        shutil.copyfile(CASE, BACKUP)
        print("резервная копия:", BACKUP)
    proc, cad = start(8796)
    try:
        # --- корпус: выемки замков и насечки ---
        cad.call("open", path=BACKUP)
        v_case0 = report(cad, "корпус (исходный)")
        case_locks(cad)
        v_locks = report(cad, "корпус + выемки замков")
        knurl_cut(cad)
        v_case = report(cad, "корпус + насечки")
        cad.save(CASE)

        # --- крышка ---
        cad.new()
        lid_bosses(cad)
        v_lid_bosses = report(cad, "крышка (тело)")
        lid_cuts(cad)
        report(cad, "крышка (вырезы)")
        knurl_cut(cad)
        v_lid = report(cad, "крышка + насечки")
        cad.save(LID)

        # --- проверка смыкания: корпус и тело крышки в одной детали не должны пересекаться ---
        cad.call("open", path=CASE)
        lid_bosses(cad)
        v_both = report(cad, "корпус + тело крышки")
        overlap = v_case + v_lid_bosses - v_both
        print(f"пересечение (должно быть ≈ 0, допуск сетки ~1%): {overlap:.2f} мм³ из {v_lid_bosses:.1f}")
        print(f"выемок замков: {v_case0 - v_locks:.2f} мм³; насечек у корпуса: {v_locks - v_case:.2f} мм³")
    finally:
        proc.kill()


if __name__ == "__main__":
    main()
