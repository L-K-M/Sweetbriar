"""Derive the app's AppIcon asset catalog from media-sources/icon.png.

Usage (from the repository root; needs uv, any OS):

    uv run --with pillow==13.0.0 python -I scripts/app-icon.py

Writes one PNG per pixel size macOS asks for (16 to 1024) and the
catalog's Contents.json into
apps/macos/Sweetbriar/Assets.xcassets/AppIcon.appiconset. The master is
square and full-bleed; macOS applies the icon mask itself. Rerun it and
commit the output whenever the master changes; never edit the derived
PNGs by hand.
"""

import json
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
MASTER = ROOT / "media-sources" / "icon.png"
CATALOG = ROOT / "apps" / "macos" / "Sweetbriar" / "Assets.xcassets"
ICONSET = CATALOG / "AppIcon.appiconset"

# macOS's icon slots: (point size, scale). Each needs point * scale pixels,
# so one 32 px file serves both 16@2x and 32@1x.
SLOTS = [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2),
         (256, 1), (256, 2), (512, 1), (512, 2)]

XCODE_INFO = {"author": "xcode", "version": 1}


def file_name(pixels: int) -> str:
    return f"icon-{pixels}.png"


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n")


def main() -> int:
    master = Image.open(MASTER).convert("RGB")
    if master.width != master.height:
        print(f"!! {MASTER} is {master.width}x{master.height}, not square", file=sys.stderr)
        return 1

    ICONSET.mkdir(parents=True, exist_ok=True)

    # Downscale from the master each time, never from a smaller derivative.
    for pixels in sorted({points * scale for points, scale in SLOTS}):
        icon = master.resize((pixels, pixels), Image.Resampling.LANCZOS)
        icon.save(ICONSET / file_name(pixels), optimize=True)

    images = [
        {
            "filename": file_name(points * scale),
            "idiom": "mac",
            "scale": f"{scale}x",
            "size": f"{points}x{points}",
        }
        for points, scale in SLOTS
    ]
    write_json(ICONSET / "Contents.json", {"images": images, "info": XCODE_INFO})
    write_json(CATALOG / "Contents.json", {"info": XCODE_INFO})
    return 0


if __name__ == "__main__":
    sys.exit(main())
