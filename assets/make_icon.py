"""Генерирует pdfsmith.ico — лист документа с красной плашкой PDF."""
import os
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SIZE = 256
img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# Лист бумаги с загнутым уголком.
margin, fold = 44, 64
page = [
    (margin, 24),
    (SIZE - margin - fold, 24),
    (SIZE - margin, 24 + fold),
    (SIZE - margin, SIZE - 24),
    (margin, SIZE - 24),
]
d.polygon(page, fill=(245, 245, 247, 255), outline=(120, 120, 128, 255))
d.polygon(
    [(SIZE - margin - fold, 24), (SIZE - margin - fold, 24 + fold), (SIZE - margin, 24 + fold)],
    fill=(210, 210, 214, 255),
)

# Красная плашка PDF.
band_top, band_h = 150, 56
d.rectangle([margin, band_top, SIZE - margin, band_top + band_h], fill=(200, 42, 42, 255))


def load_font(size):
    """Пытается взять жирный шрифт из системных; иначе — встроенный bitmap."""
    win = os.environ.get("WINDIR", r"C:\Windows")
    candidates = [
        "arialbd.ttf",
        os.path.join(win, "Fonts", "arialbd.ttf"),
        os.path.join(win, "Fonts", "segoeuib.ttf"),
        os.path.join(win, "Fonts", "arial.ttf"),
    ]
    for c in candidates:
        try:
            return ImageFont.truetype(c, size)
        except OSError:
            continue
    return ImageFont.load_default()


font = load_font(40)
d.text((SIZE // 2, band_top + band_h // 2), "PDF", font=font, fill=(255, 255, 255, 255), anchor="mm")

out = Path(__file__).resolve().parent.parent / "crates" / "pdfsmith-app" / "assets" / "pdfsmith.ico"
out.parent.mkdir(parents=True, exist_ok=True)
img.save(out, sizes=[(16, 16), (32, 32), (48, 48), (256, 256)])
print(f"wrote {out}")
