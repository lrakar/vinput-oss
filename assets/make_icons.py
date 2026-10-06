"""Regenerates the application icons from the VinPUT logo.

    python assets/make_icons.py        (needs Pillow)

Writes assets/vinput.ico, the icon of the executable, its shortcuts and the idle
tray, and assets/vinput-listening.ico, the same logo with a red dot, which the
tray shows while Vinput listens. build.rs embeds them as resources 1 and 2.
"""

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ASSETS = Path(__file__).parent
LOGO = Image.open(ASSETS / "vinput-logo.png").convert("RGBA")
# Windows scales the 256 px frame for anything above 64, so there is no 128.
# The tray asks for 48 px at most (at 300% scaling), so its icon stops there.
SIZES = {"vinput.ico": [16, 20, 24, 32, 40, 48, 64, 256],
         "vinput-listening.ico": [16, 20, 24, 32, 40, 48]}
RED = (212, 42, 42, 255)
WHITE = (255, 255, 255, 255)


def render(size, listening):
    image = LOGO.resize((size, size), Image.LANCZOS)
    if size <= 48:
        image = image.filter(ImageFilter.UnsharpMask(radius=0.6, percent=80, threshold=0))
    if listening:
        # The dot covers more of the smallest icons, so it still reads at 16 px.
        diameter = round(size * (0.56 if size <= 24 else 0.44))
        scale = 8
        dot = Image.new("RGBA", (diameter * scale, diameter * scale), (0, 0, 0, 0))
        draw = ImageDraw.Draw(dot)
        edge = diameter * scale - 1
        rim = max(1, round(diameter * 0.12)) * scale
        draw.ellipse([0, 0, edge, edge], fill=WHITE)
        draw.ellipse([rim, rim, edge - rim, edge - rim], fill=RED)
        dot = dot.resize((diameter, diameter), Image.LANCZOS)
        image.alpha_composite(dot, (size - diameter, size - diameter))
    return image


def write_ico(path, images):
    """Writes each image as its own PNG frame, exactly as rendered."""
    frames = []
    for image in images:
        buffer = io.BytesIO()
        image.save(buffer, format="PNG")
        frames.append(buffer.getvalue())
    offset = 6 + 16 * len(frames)
    entries = b""
    for image, frame in zip(images, frames):
        side = image.size[0] % 256  # 256 is stored as 0
        entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(frame), offset)
        offset += len(frame)
    path.write_bytes(struct.pack("<HHH", 0, 1, len(frames)) + entries + b"".join(frames))


for name, sizes in SIZES.items():
    write_ico(ASSETS / name, [render(size, name != "vinput.ico") for size in sizes])
    print(f"wrote {name} ({(ASSETS / name).stat().st_size} bytes)")
