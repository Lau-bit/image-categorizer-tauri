"""Regenerate icon.ico and icon.rgba from icon.png. Run from this folder:

    # 1. icon.svg -> icon.png (any headless Chromium; the SVG is the source of truth)
    chrome --headless=new --disable-gpu --hide-scrollbars \
           --default-background-color=00000000 --window-size=1024,1024 \
           --screenshot=icon.png file:///<abs-path>/render.html

    # 2. icon.png -> icon.ico + icon.rgba
    python build-icons.py

Why this is not just `tauri icon`: the Windows 11 taskbar requests IN-BETWEEN sizes at scaled DPI
(20, 30, 36, 40 px), and an .ico missing those forces Windows to upscale the nearest one, which is
what a blurry taskbar icon actually is. The default generators also write the small entries as BMP.
So the container is built by hand below: every size Windows asks for, every entry PNG-compressed.

icon.rgba covers the other icon slot — a running window's taskbar icon comes from WM_SETICON, not
from the exe's .ico, so lib.rs feeds this raw 1024x1024 buffer to window.set_icon() at startup.
"""
import io
import os
import struct

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
SIZES = [16, 20, 24, 30, 32, 36, 40, 48, 64, 128, 256]

src = Image.open(os.path.join(HERE, "icon.png")).convert("RGBA")
if src.size != (1024, 1024):
    raise SystemExit(f"icon.png must be 1024x1024, got {src.size} — re-run the render step")


def png_bytes(image):
    buffer = io.BytesIO()
    image.save(buffer, format="PNG")
    return buffer.getvalue()


entries, data = [], b""
offset = 6 + 16 * len(SIZES)  # ICONDIR header + one 16-byte directory entry per image
for size in SIZES:
    payload = png_bytes(src.resize((size, size), Image.LANCZOS))
    # 256 is encoded as 0 in the directory — a byte can't hold it.
    dimension = 0 if size >= 256 else size
    entries.append(struct.pack("<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(payload), offset))
    data += payload
    offset += len(payload)

with open(os.path.join(HERE, "icon.ico"), "wb") as handle:
    handle.write(struct.pack("<HHH", 0, 1, len(SIZES)))  # reserved, type=1 (icon), count
    for entry in entries:
        handle.write(entry)
    handle.write(data)

with open(os.path.join(HERE, "icon.rgba"), "wb") as handle:
    handle.write(src.tobytes())  # raw RGBA, no header — exactly 1024*1024*4 bytes

print(f"icon.ico  {os.path.getsize(os.path.join(HERE, 'icon.ico')):>8} bytes, {len(SIZES)} PNG entries {SIZES}")
print(f"icon.rgba {os.path.getsize(os.path.join(HERE, 'icon.rgba')):>8} bytes")
