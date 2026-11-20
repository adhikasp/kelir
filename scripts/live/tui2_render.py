#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte", "pillow"]
# ///
"""Replay <prefix>.raw through pyte up to each snapshot and rasterise to PNG.
usage: tui2_render.py <prefix> <outdir> [--light] [--screen-only] [--lines N] [--only name,name]
Scrollback (rows scrolled off the top) is shown above the visible screen (blue rule) unless --screen-only.
SGR 2 (dim) is emulated (pyte ignores it) by tagging the foreground colour."""
import json, sys, os
import pyte
from PIL import Image, ImageDraw, ImageFont

prefix, outdir = sys.argv[1], sys.argv[2]
light = "--light" in sys.argv
screen_only = "--screen-only" in sys.argv
maxlines = int(sys.argv[sys.argv.index("--lines") + 1]) if "--lines" in sys.argv else 400
only = set(sys.argv[sys.argv.index("--only") + 1].split(",")) if "--only" in sys.argv else None
raw = open(prefix + ".raw", "rb").read()
meta = json.load(open(prefix + ".json"))
rows, cols = meta["init"]


class DimScreen(pyte.HistoryScreen):
    dim = False

    def select_graphic_rendition(self, *attrs):
        if not attrs or 0 in attrs or 22 in attrs:
            self.dim = False
        if 2 in attrs:
            self.dim = True
        super().select_graphic_rendition(*[a for a in attrs if a != 2])
        fg = self.cursor.attrs.fg
        if self.dim and not fg.startswith("dim:"):
            self.cursor.attrs = self.cursor.attrs._replace(fg="dim:" + fg)
        elif not self.dim and fg.startswith("dim:"):
            self.cursor.attrs = self.cursor.attrs._replace(fg=fg[4:])


DARK = {"default": (204, 204, 204), "black": (12, 12, 12), "red": (231, 72, 86), "green": (19, 161, 14),
        "brown": (193, 156, 0), "blue": (59, 120, 255), "magenta": (180, 0, 158), "cyan": (58, 150, 221),
        "white": (204, 204, 204), "brightblack": (118, 118, 118), "brightred": (231, 72, 86),
        "brightgreen": (22, 198, 12), "brightbrown": (249, 241, 165), "brightblue": (59, 120, 255),
        "brightmagenta": (180, 0, 158), "brightcyan": (97, 214, 214), "brightwhite": (242, 242, 242)}
LIGHT = {"default": (30, 30, 30), "black": (0, 0, 0), "red": (205, 49, 49), "green": (0, 128, 0),
         "brown": (128, 100, 0), "blue": (4, 81, 165), "magenta": (188, 63, 188), "cyan": (5, 152, 188),
         "white": (110, 110, 110), "brightblack": (102, 102, 102), "brightred": (205, 49, 49),
         "brightgreen": (20, 142, 20), "brightbrown": (181, 137, 0), "brightblue": (7, 102, 200),
         "brightmagenta": (188, 63, 188), "brightcyan": (17, 168, 205), "brightwhite": (60, 60, 60)}
PAL = LIGHT if light else DARK
BG = (255, 255, 255) if light else (12, 12, 12)


def col(name, is_bg=False):
    name = name or "default"
    dim = name.startswith("dim:")
    if dim:
        name = name[4:]
    if name == "default":
        c = BG if is_bg else PAL["default"]
    elif name in PAL:
        c = PAL[name]
    elif len(name) == 6:
        try:
            c = tuple(int(name[i:i + 2], 16) for i in (0, 2, 4))
        except ValueError:
            c = PAL["default"]
    else:
        c = PAL["default"]
    return c, dim


def blend(c, a=0.5):
    return tuple(int(c[i] * a + BG[i] * (1 - a)) for i in range(3))


FS = 16
F = None
for f in ("CascadiaMono.ttf", "consola.ttf"):
    try:
        F = ImageFont.truetype("C:/Windows/Fonts/" + f, FS)
        break
    except OSError:
        pass
F = F or ImageFont.load_default()
CW = max(1, round(F.getlength("M")))
CH = FS + 6


def lines_of(screen):
    out = []
    if not screen_only:
        for row in screen.history.top:
            out.append([row[x] for x in range(screen.columns)])
    for y in range(screen.lines):
        out.append([screen.buffer[y][x] for x in range(screen.columns)])
    return out


def draw(screen, path, title):
    ls = lines_of(screen)[-maxlines:]
    W = screen.columns * CW + 12
    H = len(ls) * CH + 28
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    d.text((4, 2), title, fill=(120, 120, 120), font=F)
    top = 22
    for y, row in enumerate(ls):
        for x, ch in enumerate(row):
            data = ch.data
            if data == "":
                continue
            fg, dim = col(ch.fg)
            bg, _ = col(ch.bg, True)
            if ch.reverse:
                fg, bg = (BG if ch.fg == "default" else fg) if False else bg, fg
            if dim:
                fg = blend(fg, 0.55)
            px = 4 + x * CW
            py = top + y * CH
            w = CW * (2 if pyte.screens.wcwidth(data[0]) == 2 else 1)
            if bg != BG:
                d.rectangle([px, py, px + w, py + CH], fill=bg)
            if data != " ":
                d.text((px, py + 1), data, fill=fg, font=F)
                if ch.bold:
                    d.text((px + 1, py + 1), data, fill=fg, font=F)
                if ch.italics:
                    d.line([px + w - 2, py + 2, px + w // 2, py + CH - 4], fill=fg, width=1)
            if ch.underscore:
                d.line([px, py + CH - 3, px + w, py + CH - 3], fill=fg)
            if ch.strikethrough:
                d.line([px, py + CH // 2, px + w, py + CH // 2], fill=fg)
    if not screen_only and len(ls) > screen.lines:
        sy = top + (len(ls) - screen.lines) * CH
        d.line([0, sy, W, sy], fill=(80, 80, 200))
    d.line([4 + screen.columns * CW, top, 4 + screen.columns * CW, H], fill=(150, 60, 60))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    im.save(path)


screen = DimScreen(cols, rows, history=100000, ratio=0.5)
st = pyte.ByteStream(screen)
pos = 0
marks = sorted(meta["events"], key=lambda e: e["off"])
base = os.path.join(outdir, os.path.basename(prefix))
def frame_aligned(off):
    """A snap taken inside a synchronized-update block is moved back to the block's start (the last complete frame)."""
    h = raw.rfind(b"[?2026h", 0, off)
    l = raw.rfind(b"[?2026l", 0, off)
    return h if h > l else off


for e in marks + [{"off": len(raw), "kind": "snap", "name": "final"}]:
    if e["kind"] == "snap":
        e = dict(e, off=max(pos, frame_aligned(e["off"])))
    st.feed(raw[pos:e["off"]])
    pos = e["off"]
    if e["kind"] == "resize":
        screen.resize(e["rows"], e["cols"])
    elif only is None or e["name"] in only:
        p = "%s.%s.png" % (base, e["name"])
        draw(screen, p, "%s [%dx%d]%s" % (e["name"], screen.columns, screen.lines, " light" if light else ""))
        print(p)
