#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow"]
# ///
"""tui2c re-review scenario specs for tui2_scn.py. usage: tui2c_specs.py <outdir> <work-dir-for-image-paths>
Names: s1b_fence_<cols>, s1b_table_<cols>, s1b_list_<cols>, s1b_all_<cols> (one long answer with all three),
r5_stop (429 wait, Esc), r5_say (429 wait, typed 'stop'), s3b (PNG named .jpg and JPEG named .png typed as paths)."""
import base64, io, json, os, sys
from PIL import Image

out, imgdir = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
start = [{"wait_for": "kelir", "timeout": 20}, {"wait": 1.0}]
quit_ = [{"send": "/quit"}, {"wait": 0.3}, {"send": "\r"}]
say = lambda t: [{"send": t}, {"wait": 0.3}, {"send": "\r"}]
w = lambda n, d: json.dump(d, open(os.path.join(out, n + ".json"), "w"))
fence = "Here is the code:\n\n```rust\n" + "".join("fn f%02d() -> u32 { %d }\n" % (i, i) for i in range(1, 61)) + "```\n\nAfter the fence.\n"
table = "Table below.\n\n| Name | Description | Status |\n|---|---|---|\n" + "".join("| row%02d | a description that is long for row %d | ok |\n" % (i, i) for i in range(1, 31)) + "\nAfter the table.\n"
lst = "List below.\n\n" + "".join("%d. item %02d with some words to wrap around the pane when narrow\n    - nested a of %d\n    - nested b of %d\n" % (i, i, i, i) for i in range(1, 26)) + "\nAfter the list.\n"
for c in (80, 40):
    for nm, md, endm in (("fence", fence, "After the fence"), ("table", table, "After the table"), ("list", lst, "After the list"), ("all", fence + "\n" + table + "\n" + lst, "After the list")):
        w("s1b_%s_%d" % (nm, c), dict(turns=[[{"text": md, "chunk": 16, "delay": 0.06}]], rows=40, cols=c, timeout=240,
          steps=start + say("go") + [{"wait": 2.0}, {"snap": "mid1"}, {"wait": 2.5}, {"snap": "mid2"}, {"wait_for": endm, "timeout": 90}, {"wait": 1.5}, {"snap": "done"}] + quit_))
rl = [{"status": 429, "body": "{\"error\":{\"message\":\"slow\"}}"}]
w("r5_stop", dict(turns=[rl, rl, [{"text": "never shown\n"}]], cfg="retry_base_ms = 8000\nretry_cap_ms = 8000\n", rows=30, cols=100, timeout=120,
  steps=start + say("hi") + [{"wait": 2.5}, {"snap": "w1"}, {"send": "\u001b"}, {"wait": 1.5}, {"snap": "stopped"}] + quit_))
w("r5_say", dict(turns=[rl, [{"text": "answer after the retry\n"}]], cfg="retry_base_ms = 5000\nretry_cap_ms = 5000\n", rows=30, cols=100, timeout=120,
  steps=start + say("hi") + [{"wait": 1.5}, {"snap": "w1"}] + quit_))
png = io.BytesIO(); Image.new("RGB", (32, 32), (200, 30, 30)).save(png, "PNG")
jpg = io.BytesIO(); Image.new("RGB", (32, 32), (30, 30, 200)).save(jpg, "JPEG")
b = lambda x: base64.b64encode(x.getvalue()).decode()
P = lambda n: (imgdir + "/s3b.work/" + n).replace(chr(92), "/")
w("s3b", dict(turns=[[{"text": "I see them.\n"}]], rows=30, cols=100, timeout=120,
  binfiles={"real_png_named.jpg": b(png), "real_jpeg_named.png": b(jpg)},
  steps=start + [{"send": "look "}, {"send": P("real_png_named.jpg")}, {"wait": 0.8}, {"send": " and "}, {"send": P("real_jpeg_named.png")}, {"wait": 0.8},
                 {"snap": "typed"}, {"send": "\r"}, {"wait_for": "I see them", "timeout": 40}, {"wait": 1.0}, {"snap": "done"}] + quit_))
print("wrote specs to", out)
