"""Generates the MSI installer artwork from the app icon.

Outputs wix-banner.bmp and wix-dialog.bmp next to this script. Run after
changing the app icon or palette:

    python generate.py   # requires Pillow

Colors mirror public/logo.svg. WiX requires 24-bit BMP at these exact sizes
(they are stretched on high-DPI displays; there is no oversized-asset
escape hatch). Referenced from newt.wxs via the WixUIBannerBmp and
WixUIDialogBmp variables.
"""

from pathlib import Path

from PIL import Image

HERE = Path(__file__).parent

GOLD = (255, 204, 0)  # #ffcc00
RED = (170, 0, 0)  # #aa0000
RED_DARK = (128, 0, 0)  # #800000
WHITE = (255, 255, 255)

ICON = Image.open(HERE / ".." / ".." / "src-tauri" / "icons" / "icon.png").convert("RGBA")


def flat(size: tuple[int, int], color: tuple[int, int, int]) -> Image.Image:
    return Image.new("RGBA", size, color + (255,))


def vgradient(
    size: tuple[int, int], top: tuple[int, int, int], bottom: tuple[int, int, int]
) -> Image.Image:
    w, h = size
    col = Image.new("RGBA", (1, h))
    for y in range(h):
        t = y / (h - 1)
        col.putpixel(
            (0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,)
        )
    return col.resize((w, h))


def paste_icon(bg: Image.Image, size: int, center: tuple[int, int]) -> None:
    icon = ICON.resize((size, size), Image.LANCZOS)
    bg.alpha_composite(icon, (center[0] - size // 2, center[1] - size // 2))


def save(img: Image.Image, name: str) -> None:
    img.convert("RGB").save(HERE / name, "BMP")
    print(f"{name}: {img.size[0]}x{img.size[1]}")


# Top banner: title text is drawn on the left, so the logo sits right.
banner = flat((493, 58), WHITE)
paste_icon(banner, 48, (460, 26))
banner.paste(GOLD + (255,), (0, 55, 493, 58))
save(banner, "wix-banner.bmp")

# Welcome/exit dialog background: red panel on the left, the rest stays
# white for the dialog text.
dialog = flat((493, 312), WHITE)
panel = vgradient((164, 312), RED, RED_DARK)
paste_icon(panel, 128, (82, 100))
dialog.alpha_composite(panel, (0, 0))
save(dialog, "wix-dialog.bmp")
