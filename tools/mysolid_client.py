"""Клиент API MySolid3D на стандартной библиотеке Python (см. docs/API.md).

    from mysolid_client import CAD
    cad = CAD()                        # программа должна быть запущена с --api
    sk = cad.add_sketch("top")
    cad.sketch_add(sk, {"type": "rect", "x": 0, "y": 0, "w": 40, "h": 30})
    cad.boss(sk, depth=10)
    print(cad.state()["body"])
"""

import json
import urllib.error
import urllib.request


class CADError(Exception):
    """Программа вернула ok:false или недоступна."""


class CAD:
    def __init__(self, port: int = 8765, host: str = "127.0.0.1", timeout: float = 600.0):
        self.base = f"http://{host}:{port}"
        self.timeout = timeout

    # ----- низкий уровень -----
    def _post(self, body):
        data = json.dumps(body).encode("utf-8")
        request = urllib.request.Request(
            self.base + "/api", data=data, headers={"Content-Type": "application/json"}
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.URLError as e:
            raise CADError(f"MySolid3D недоступен ({e}). Запущена ли программа с ключом --api?") from e

    def docs(self) -> str:
        """Полная инструкция, которую отдаёт сама программа."""
        with urllib.request.urlopen(self.base + "/", timeout=10) as response:
            return response.read().decode("utf-8")

    def call(self, cmd: str, **params):
        """Выполнить одну команду; при ok:false бросает CADError."""
        answer = self._post({"cmd": cmd, **params})
        if not answer.get("ok"):
            raise CADError(answer.get("error", "неизвестная ошибка"))
        return answer

    def batch(self, commands: list):
        """Выполнить пачку команд; возвращает весь ответ (проверьте ok и results)."""
        return self._post(commands)

    # ----- удобные обёртки -----
    def state(self):
        return self.call("state")

    def new(self):
        return self.call("new")

    def add_sketch(self, plane="top", *, face=None, offset=None, name=None) -> int:
        params = {"face": face} if face is not None else {"plane": plane}
        if offset is not None:
            params["offset"] = offset
        if name:
            params["name"] = name
        return self.call("add_sketch", **params)["id"]

    def sketch_add(self, sketch: int, *shapes):
        """Добавить фигуры (словари вида {"type": "rect", ...}); возвращает номера фигур."""
        return self.call("sketch_add", sketch=sketch, shapes=list(shapes))["indexes"]

    def boss(self, sketch: int, depth: float, **kw):
        return self._feature("boss", sketch=sketch, depth=depth, **kw)

    def cut(self, sketch: int, depth: float = None, **kw):
        if depth is not None:
            kw["depth"] = depth
        return self._feature("cut", sketch=sketch, **kw)

    def fillet(self, radius: float, **edges):
        return self._feature("fillet", radius=radius, **edges)

    def chamfer(self, distance: float, **edges):
        return self._feature("chamfer", distance=distance, **edges)

    def _feature(self, cmd, **params):
        answer = self.call(cmd, **params)
        if answer.get("error"):
            raise CADError(f"элемент {answer['id']} не построился: {answer['error']}")
        return answer

    def faces(self):
        return self.call("faces")["faces"]

    def edges(self):
        return self.call("edges")["edges"]

    def export(self, path: str):
        return self.call("export", path=path)

    def save(self, path: str = None):
        return self.call("save", **({"path": path} if path else {}))
