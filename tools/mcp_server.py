"""MCP-сервер (stdio) для MySolid3D: даёт нейросети две функции — выполнить команды и прочитать инструкцию.

Подключение (Claude Desktop — claude_desktop_config.json, Claude Code — .mcp.json):

    {"mcpServers": {"mysolid3d": {"command": "python", "args": ["C:/Project/MySolid3D/tools/mcp_server.py"]}}}

Программа MySolid3D должна быть запущена с ключом --api (порт по умолчанию 8765, другой — переменная
окружения MYSOLID3D_PORT). Только стандартная библиотека Python.
"""

import json
import os
import sys
import urllib.error
import urllib.request

PORT = int(os.environ.get("MYSOLID3D_PORT", "8765"))
BASE = f"http://127.0.0.1:{PORT}"

TOOLS = [
    {
        "name": "mysolid3d_api",
        "description": (
            "Выполнить команду или пачку команд в CAD-программе MySolid3D (строит параметрические детали: "
            "эскизы, бобышки, вырезы, скругления). Передайте один объект {\"cmd\": ..., ...} или список таких "
            "объектов. Сначала прочитайте инструкцию инструментом mysolid3d_docs. Всегда проверяйте поле error "
            "у созданных элементов."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "commands": {
                    "description": "Одна команда (объект с полем cmd) или список команд.",
                    "oneOf": [
                        {"type": "object", "properties": {"cmd": {"type": "string"}}, "required": ["cmd"]},
                        {"type": "array", "items": {"type": "object", "properties": {"cmd": {"type": "string"}}, "required": ["cmd"]}},
                    ],
                }
            },
            "required": ["commands"],
        },
    },
    {
        "name": "mysolid3d_docs",
        "description": "Полная инструкция по командам MySolid3D API: формат, плоскости, фигуры, рецепты, типичные ошибки.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def http(method: str, path: str, body=None) -> str:
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {"Content-Type": "application/json"} if data is not None else {}
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=600) as response:
            return response.read().decode("utf-8")
    except urllib.error.HTTPError as e:
        return e.read().decode("utf-8")
    except urllib.error.URLError as e:
        raise RuntimeError(
            f"MySolid3D недоступен на {BASE} ({e}). Запустите программу с ключом --api "
            "или включите «API для нейросетей» в меню «Файл»."
        )


def call_tool(name: str, arguments: dict) -> str:
    if name == "mysolid3d_docs":
        return http("GET", "/")
    if name == "mysolid3d_api":
        return http("POST", "/api", arguments.get("commands"))
    raise RuntimeError(f"неизвестный инструмент {name}")


def reply(message_id, result=None, error=None):
    message = {"jsonrpc": "2.0", "id": message_id}
    if error is not None:
        message["error"] = error
    else:
        message["result"] = result
    sys.stdout.write(json.dumps(message, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        method, message_id = message.get("method"), message.get("id")
        if message_id is None:
            continue  # уведомления (например, notifications/initialized) ответа не требуют
        if method == "initialize":
            reply(message_id, {
                "protocolVersion": message.get("params", {}).get("protocolVersion", "2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mysolid3d", "version": "1.0"},
            })
        elif method == "tools/list":
            reply(message_id, {"tools": TOOLS})
        elif method == "tools/call":
            params = message.get("params", {})
            try:
                text = call_tool(params.get("name", ""), params.get("arguments") or {})
                reply(message_id, {"content": [{"type": "text", "text": text}]})
            except Exception as e:  # ошибка инструмента возвращается нейросети текстом, а не обрывает связь
                reply(message_id, {"content": [{"type": "text", "text": str(e)}], "isError": True})
        elif method == "ping":
            reply(message_id, {})
        else:
            reply(message_id, error={"code": -32601, "message": f"метод {method} не поддерживается"})


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    main()
