"""Generates assets/: the app icon (smiley sponge) and the folder state images.

Usage: python tools/make_icons.py
To use a different logo, replace assets/app.ico (and assets/sponge.png) and rebuild.
"""
import math, os, random
from PIL import Image, ImageDraw, ImageFilter

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets")
S = 1024  # supersampled canvas
os.makedirs(OUT, exist_ok=True)


def sponge(size=S):
    """Yellow smiley sponge face: round body, two eye holes, big grin hole."""
    im = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    k = size / 1024
    # body (slightly squashed circle) with darker rim for depth
    body = [60 * k, 90 * k, 964 * k, 960 * k]
    d.ellipse([b + (14 * k if i < 2 else 14 * k) for i, b in enumerate(body)], fill=(196, 140, 0, 255))
    d.ellipse(body, fill=(255, 205, 0, 255))
    # foam texture: little pores
    rnd = random.Random(7)
    cx, cy, rx, ry = 512 * k, 525 * k, 440 * k, 425 * k
    for _ in range(420):
        a = rnd.random() * 2 * math.pi
        r = math.sqrt(rnd.random()) * 0.93
        x, y = cx + math.cos(a) * rx * r, cy + math.sin(a) * ry * r
        pr = rnd.uniform(5, 13) * k
        d.ellipse([x - pr, y - pr, x + pr, y + pr], fill=(236, 178, 0, 255))
    # highlight
    hl = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ImageDraw.Draw(hl).ellipse([200 * k, 150 * k, 560 * k, 400 * k], fill=(255, 245, 170, 110))
    im = Image.alpha_composite(im, hl.filter(ImageFilter.GaussianBlur(40 * k)))
    d = ImageDraw.Draw(im)
    hole = (88, 52, 0, 255)
    inner = (50, 28, 0, 255)
    # eyes
    for ex in (340, 684):
        d.ellipse([(ex - 85) * k, 280 * k, (ex + 85) * k, 470 * k], fill=hole)
        d.ellipse([(ex - 65) * k, 300 * k, (ex + 65) * k, 455 * k], fill=inner)
    # grin: crescent = big ellipse minus offset ellipse
    mouth = Image.new("L", (size, size), 0)
    md = ImageDraw.Draw(mouth)
    md.ellipse([215 * k, 360 * k, 809 * k, 830 * k], fill=255)
    md.ellipse([170 * k, 250 * k, 854 * k, 640 * k], fill=0)
    im.paste(Image.new("RGBA", (size, size), hole), (0, 0), mouth)
    mouth2 = mouth.filter(ImageFilter.MinFilter(int(31 * k) | 1))
    im.paste(Image.new("RGBA", (size, size), inner), (0, 0), mouth2)
    return im


def folder(done=False, size=S):
    im = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    k = size / 1024
    back = (219, 160, 38, 255)
    front = (252, 199, 76, 255)
    # shadow
    sh = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ImageDraw.Draw(sh).rounded_rectangle([90 * k, 300 * k, 950 * k, 890 * k], 50 * k, fill=(0, 0, 0, 90))
    im = Image.alpha_composite(im, sh.filter(ImageFilter.GaussianBlur(22 * k)))
    d = ImageDraw.Draw(im)
    # back + tab
    d.rounded_rectangle([70 * k, 190 * k, 420 * k, 330 * k], 40 * k, fill=back)
    d.rounded_rectangle([70 * k, 250 * k, 954 * k, 860 * k], 50 * k, fill=back)
    # paper peeking out
    d.rounded_rectangle([130 * k, 290 * k, 894 * k, 700 * k], 20 * k, fill=(250, 250, 250, 255))
    for i in range(5):
        y = (350 + i * 55) * k
        d.rounded_rectangle([190 * k, y, (830 - (i % 2) * 140) * k, y + 18 * k], 9 * k, fill=(205, 205, 205, 255))
    # front flap
    d.rounded_rectangle([70 * k, 400 * k, 954 * k, 870 * k], 50 * k, fill=front)
    d.rounded_rectangle([70 * k, 400 * k, 954 * k, 430 * k], 15 * k, fill=(255, 220, 125, 255))
    # sponge face stamped on the front
    sp = sponge(int(330 * k))
    im.alpha_composite(sp, (int(347 * k), int(470 * k)))
    if done:
        d = ImageDraw.Draw(im)
        cx, cy, r = 800 * k, 760 * k, 200 * k
        d.ellipse([cx - r - 14 * k, cy - r - 14 * k, cx + r + 14 * k, cy + r + 14 * k], fill=(255, 255, 255, 255))
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(34, 170, 84, 255))
        d.line([(cx - 95 * k, cy + 5 * k), (cx - 25 * k, cy + 80 * k), (cx + 105 * k, cy - 75 * k)],
               fill=(255, 255, 255, 255), width=int(48 * k), joint="curve")
        for (px, py) in [(cx - 95 * k, cy + 5 * k), (cx + 105 * k, cy - 75 * k), (cx - 25 * k, cy + 80 * k)]:
            d.ellipse([px - 24 * k, py - 24 * k, px + 24 * k, py + 24 * k], fill=(255, 255, 255, 255))
    return im


def save_png(im, name, px):
    im.resize((px, px), Image.LANCZOS).save(os.path.join(OUT, name))


sp = sponge()
save_png(sp, "sponge.png", 256)
sp.resize((256, 256), Image.LANCZOS).save(
    os.path.join(OUT, "app.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
save_png(folder(False), "folder.png", 384)
save_png(folder(True), "folder_done.png", 384)
print("ok")
