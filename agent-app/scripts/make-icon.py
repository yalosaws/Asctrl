"""Render the app icon: the same flowing-gradient orb the overlay shows (see .orb-* in src/styles.css).

Usage: python scripts/make-icon.py [out.png]   then   npx tauri icon out.png
"""
import sys
import numpy as np
from PIL import Image, ImageFilter

N = 1024          # output size
SS = 3            # supersampling
S = N * SS
LOOP = ["#5b8cff", "#b36bff", "#ff7ab6", "#ffb86b", "#5be0c8", "#5b8cff"]
FROM = 108 / 360  # conic start angle: blue -> purple -> pink fill the body, orange/teal sit up by the highlight

R = 0.43 * S      # orb radius
RIM = 0.016 * S   # white rim width (half inside, half outside the body)
cx = cy = S / 2

def hexrgb(h):
    return np.array([int(h[i:i + 2], 16) for i in (1, 3, 5)], float) / 255

def srgb_to_lin(c):
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)

def lin_to_srgb(c):
    c = np.clip(c, 0, 1)
    return np.where(c <= 0.0031308, c * 12.92, 1.055 * c ** (1 / 2.4) - 0.055)

M1 = np.array([[0.4122214708, 0.5363325363, 0.0514459929], [0.2119034982, 0.6806995451, 0.1073969566], [0.0883024619, 0.2817188376, 0.6299787005]])
M2 = np.array([[0.2104542553, 0.7936177850, -0.0040720468], [1.9779984951, -2.4285922050, 0.4505937099], [0.0259040371, 0.7827717662, -0.8086757660]])

def to_oklab(rgb):
    return np.cbrt(srgb_to_lin(rgb) @ M1.T) @ M2.T

def from_oklab(lab):
    return lin_to_srgb((lab @ np.linalg.inv(M2).T) ** 3 @ np.linalg.inv(M1).T)

def to_lch(lab):
    return np.stack([lab[..., 0], np.hypot(lab[..., 1], lab[..., 2]), np.arctan2(lab[..., 2], lab[..., 1])], -1)

stops = to_lch(to_oklab(np.array([hexrgb(c) for c in LOOP])))
y, x = np.mgrid[0:S, 0:S].astype(np.float32) + 0.5
dx, dy = x - cx, y - cy
d = np.hypot(dx, dy)
D = 2 * R
u, v = (x - (cx - R)) / D, (y - (cy - R)) / D   # position inside the orb's bounding box, 0..1
LIGHT = (0.35, 0.29)                            # where the gloss sits (as .orb-gloss)

# Conic core, centred on the orb like the real one.
# Interpolated in OKLCH so neighbours (orange -> teal) pass through clean hues instead of grey-brown.
t = (np.arctan2(u - 0.5, -(v - 0.5)) / (2 * np.pi) - FROM) % 1.0
seg = t * (len(LOOP) - 1)
i = np.clip(seg.astype(int), 0, len(LOOP) - 2)
f = seg - i
a, b = stops[i], stops[i + 1]
dh = (b[..., 2] - a[..., 2] + np.pi) % (2 * np.pi) - np.pi
L = a[..., 0] + (b[..., 0] - a[..., 0]) * f
C = (a[..., 1] + (b[..., 1] - a[..., 1]) * f) * 1.2   # more saturated than the CSS swatches
H = a[..., 2] + dh * f

# Sphere shading: bright toward the light, deep toward the far edge (this is where the contrast comes from)
r = np.hypot(u - LIGHT[0], v - LIGHT[1]) / 0.92
shade = np.clip(r, 0, 1) ** 1.3
L = L * (1.08 - 0.5 * shade)
C = C * (1 + 0.25 * shade)
lab = np.stack([L, C * np.cos(H), C * np.sin(H)], -1)
rgb = from_oklab(lab)

def over(base, color, a):
    a = a[..., None]
    return base * (1 - a) + np.asarray(color) * a

# Gloss: a tight specular spot plus a soft sheen around it
g = np.hypot(u - LIGHT[0], v - LIGHT[1])
rgb = over(rgb, [1, 1, 1], np.interp(g, [0, 0.07, 0.2], [0.95, 0.55, 0.0]))
# Faint bounce light on the lower-right edge so the dark side doesn't go flat
edge = np.clip((d / R - 0.82) / 0.18, 0, 1) * np.clip((dx + dy) / (1.414 * R), 0, 1)
rgb = over(rgb, [1, 0.85, 0.95], edge ** 2 * 0.25)

# White rim around the body
inside = np.clip(R - d + 0.5 * SS, 0, SS) / SS        # antialiased disc
rim = np.clip(d - (R - RIM) + 0.5 * SS, 0, SS) / SS
rgb = over(rgb, [1, 1, 1], rim * 0.95)
outer = np.clip(R + RIM - d + 0.5 * SS, 0, SS) / SS  # rim extends slightly outside too
rim_out = outer * (1 - inside)

body = np.dstack([rgb, inside])
body_img = Image.fromarray((np.clip(body, 0, 1) * 255).astype(np.uint8), "RGBA")
rim_img = Image.fromarray((np.dstack([np.ones_like(rgb), rim_out * 0.95]) * 255).astype(np.uint8), "RGBA")

# Gentle drop shadow
shadow_a = (np.hypot(dx, dy - 0.02 * S) < R).astype(np.float32) * 0.3
shadow = Image.fromarray((np.dstack([np.zeros_like(d), np.zeros_like(d), np.full_like(d, 30), shadow_a * 255])).astype(np.uint8), "RGBA")
shadow = shadow.filter(ImageFilter.GaussianBlur(0.02 * S))

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
for layer in (shadow, body_img, rim_img):
    img = Image.alpha_composite(img, layer)
img = img.resize((N, N), Image.LANCZOS)
out = sys.argv[1] if len(sys.argv) > 1 else "icon-source.png"
img.save(out)
print("wrote", out)
