"""Собирает иконки программы из SVG: app.ico, app_connected.ico (зелёная точка — телефон
подключён) и icon.svg для страниц. Запуск (нужен PyQt6 с QtSvg):

    C:\\Projects\\.venvs\\venv-gui\\Scripts\\python.exe app-rs\\assets\\make_icon.py

Фирменный знак — сетка 2×4 кнопок, одна подсвечена акцентом (см. «03 Work/UX-концепция.md»).
На 16–24 px восемь кнопок сливаются, поэтому там упрощённая сетка 2×2.
"""
import struct
from pathlib import Path

from PyQt6.QtCore import QBuffer, QByteArray, QIODevice, Qt
from PyQt6.QtGui import QGuiApplication, QImage, QPainter
from PyQt6.QtSvg import QSvgRenderer

HERE = Path(__file__).resolve().parent
SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
SMALL = 24  # до этого размера включительно — сетка 2×2

BACKGROUND, BORDER, KEY, ACCENT, CONNECTED = "#111418", "#2f3a47", "#3a4552", "#3b82f6", "#22c55e"


def keys(cols: int, rows: int, x0: float, y0: float, width: float, height: float, gap: float, radius: float) -> str:
    w = (width - gap * (cols - 1)) / cols
    h = (height - gap * (rows - 1)) / rows
    rects = []
    for row in range(rows):
        for col in range(cols):
            fill = ACCENT if (row, col) == (0, 0) else KEY
            rects.append(
                f'<rect x="{x0 + col * (w + gap):g}" y="{y0 + row * (h + gap):g}" width="{w:g}" height="{h:g}" '
                f'rx="{radius:g}" fill="{fill}"/>'
            )
    return "".join(rects)


def svg(small: bool, connected: bool) -> str:
    grid = keys(2, 2, 10, 10, 44, 44, 6, 4) if small else keys(4, 2, 9, 17, 46, 30, 4, 2.5)
    dot = (
        f'<circle cx="49" cy="49" r="12" fill="{CONNECTED}" stroke="{BACKGROUND}" stroke-width="4"/>'
        if connected else ""
    )
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">'
        f'<rect x="2" y="2" width="60" height="60" rx="14" fill="{BACKGROUND}" stroke="{BORDER}" stroke-width="2"/>'
        f"{grid}{dot}</svg>"
    )


def png(source: str, size: int) -> bytes:
    image = QImage(size, size, QImage.Format.Format_ARGB32)
    image.fill(Qt.GlobalColor.transparent)
    painter = QPainter(image)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    QSvgRenderer(QByteArray(source.encode())).render(painter)
    painter.end()
    data = QByteArray()
    buffer = QBuffer(data)
    buffer.open(QIODevice.OpenModeFlag.WriteOnly)
    image.save(buffer, "PNG")
    return bytes(data)


def ico(connected: bool) -> bytes:
    """ICO с картинками PNG — так Windows хранит иконки со времён Vista."""
    images = [png(svg(size <= SMALL, connected), size) for size in SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in zip(SIZES, images):
        side = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return header + entries + blobs


def main() -> None:
    app = QGuiApplication([])  # noqa: F841 — QPainter без приложения Qt не работает
    (HERE / "app.ico").write_bytes(ico(False))
    (HERE / "app_connected.ico").write_bytes(ico(True))
    (HERE / "icon.svg").write_text(svg(False, False), encoding="utf-8")
    print("готово:", ", ".join(p.name for p in sorted(HERE.glob("*.ico"))))


if __name__ == "__main__":
    main()
