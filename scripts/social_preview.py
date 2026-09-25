#!/usr/bin/env python3
"""Generate assets/social-preview.png (1280x640) for GitHub's social card.

Composites the repo logo, the tagline, and a frame from the demo GIF into a
GitHub-dark styled card. Requires Pillow and the JetBrains Mono fonts used by
the demo recordings; falls back to DejaVu if JetBrains Mono is missing.

Usage: python3 scripts/social_preview.py [terminal-frame.png]
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFont

W, H = 1280, 640
BG = (13, 17, 23)
FG = (230, 237, 243)
MUTED = (139, 148, 158)
GREEN = (63, 185, 80)
BLUE = (114, 165, 253)
BORDER = (48, 54, 61)
GLOW = (21, 34, 56)

FONT_DIRS = [
    "/home/rayr/.local/share/fonts/JetBrainsMono",
    "/usr/share/fonts/truetype/jetbrains-mono",
]


def find_font(name: str) -> str:
    for d in FONT_DIRS:
        p = os.path.join(d, name)
        if os.path.exists(p):
            return p
    for root, _, files in os.walk("/usr/share/fonts"):
        if name in files:
            return os.path.join(root, name)
    raise SystemExit(f"font {name} not found; install JetBrains Mono or edit FONT_DIRS")


def main() -> None:
    frame = sys.argv[1] if len(sys.argv) > 1 else None
    bold = find_font("JetBrainsMono-Bold.ttf")
    xbold = find_font("JetBrainsMono-ExtraBold.ttf")
    medium = find_font("JetBrainsMono-Medium.ttf")

    img = Image.new("RGB", (W, H), BG)

    # Soft glow behind the terminal panel.
    glow = Image.new("RGB", (W, H), BG)
    gd = ImageDraw.Draw(glow)
    for r in range(430, 0, -6):
        gd.ellipse((W * 0.63 - r, H * 0.52 - r, W * 0.63 + r, H * 0.52 + r),
                   fill=GLOW)
    img = Image.blend(img, glow, 1.0)
    draw = ImageDraw.Draw(img)

    # Right panel: terminal frame with rounded corners.
    if frame and os.path.exists(frame):
        shot = Image.open(frame).convert("RGB")
        panel_w = 590
        panel_h = int(shot.height * panel_w / shot.width)
        panel = shot.resize((panel_w, panel_h), Image.LANCZOS)
    else:
        panel_w, panel_h = 590, 380
        panel = Image.new("RGB", (panel_w, panel_h), (1, 4, 9))
        ImageDraw.Draw(panel).text((30, 30), "(add a terminal frame)", fill=MUTED)
    mask = Image.new("L", panel.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, panel.width - 1, panel.height - 1), radius=18, fill=255)
    px, py = 655, (H - panel.height) // 2
    img.paste(panel, (px, py), mask)
    draw.rounded_rectangle((px - 1, py - 1, px + panel.width, py + panel.height),
                           radius=18, outline=BORDER, width=2)

    # Left column: logo, title, tagline, feature bullets.
    logo = Image.open("assets/logo.png").convert("RGBA")
    logo = logo.resize((130, 130), Image.LANCZOS)
    img.paste(logo, (70, 52), logo)

    draw.text((68, 216), "Farhand (fh)",
              font=ImageFont.truetype(xbold, 80), fill=FG)
    draw.text((70, 322), "Offload your builds.",
              font=ImageFont.truetype(bold, 30), fill=BLUE)
    draw.text((70, 362), "Keep your laptop cool.",
              font=ImageFont.truetype(bold, 30), fill=BLUE)

    feats = [
        ("Zero external system binaries", GREEN),
        ("zstd delta sync + global CAS", GREEN),
        ("APFS/ext4 CoW workspaces", GREEN),
        ("rustls TLS + mTLS", GREEN),
    ]
    y = 436
    for text, color in feats:
        draw.ellipse((72, y + 8, 82, y + 18), fill=color)
        draw.text((96, y), text, font=ImageFont.truetype(bold, 25), fill=FG)
        y += 40

    draw.text((70, 592), "github.com/Rayrsn/farhand",
              font=ImageFont.truetype(bold, 22), fill=MUTED)

    img.save("assets/social-preview.png")
    print("wrote assets/social-preview.png")


if __name__ == "__main__":
    main()