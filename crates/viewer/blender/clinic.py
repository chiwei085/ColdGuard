"""Builds assets/clinic.glb with Blender 4.0.

Units are meters. Front is -Y. Node names starting with fridge, carrier, lamp,
glow, sky, display, batch, dnu and spot are read by scene.rs and batches.rs.
"""

import math
import random
import sys
from pathlib import Path

import bpy
import numpy as np

OUT = Path(__file__).resolve().parent.parent / "assets" / "clinic.glb"
SIZE = 512
rng = np.random.default_rng(7)
dice = random.Random(7)

bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene


# Textures


def noise(scale_x, scale_y=None):
    """Seamless noise in 0..1. Scales are feature sizes in pixels."""
    sx, sy = scale_x, scale_y or scale_x
    fx = np.fft.fftfreq(SIZE)[None, :]
    fy = np.fft.fftfreq(SIZE)[:, None]
    mask = np.exp(-2 * math.pi**2 * ((fx * sx) ** 2 + (fy * sy) ** 2))
    n = np.fft.ifft2(np.fft.fft2(rng.standard_normal((SIZE, SIZE))) * mask).real
    n = n / n.std() / 6 + 0.5
    return np.clip(n, 0, 1)


def image(name, rgb, data=True):
    img = bpy.data.images.new(name, rgb.shape[1], rgb.shape[0])
    if data:
        img.colorspace_settings.name = "Non-Color"
    alpha = np.ones(rgb.shape[:2] + (1,))
    img.pixels.foreach_set(np.concatenate([rgb, alpha], 2).astype(np.float32).ravel())
    img.pack()
    return img


def normal_map(name, height, strength):
    dx = (np.roll(height, -1, 1) - np.roll(height, 1, 1)) * strength
    dy = (np.roll(height, -1, 0) - np.roll(height, 1, 0)) * strength
    n = np.dstack([-dx, -dy, np.ones_like(dx)])
    n /= np.linalg.norm(n, axis=2, keepdims=True)
    return image(name, n * 0.5 + 0.5)


def rough_map(name, value, low, high, metal):
    rough = low + (high - low) * value
    return image(name, np.dstack([np.ones_like(rough), rough, np.full_like(rough, metal)]))


peel, fine, smear, brush = noise(5), noise(1.2), noise(40), noise(60, 0.6)
speck = noise(1.5)
tiles = np.ones((SIZE, SIZE))
tiles[:2, :] = tiles[:, :2] = 0.0

MAPS = {
    "enamel": (normal_map("enamel_n", peel, 0.16), rough_map("enamel_r", smear, 0.2, 0.36, 0)),
    "plastic": (normal_map("plastic_n", fine, 0.35), rough_map("plastic_r", smear, 0.42, 0.6, 0)),
    "steel": (normal_map("steel_n", brush, 0.15), rough_map("steel_r", brush, 0.2, 0.42, 1)),
    "laminate": (normal_map("laminate_n", fine, 0.12), rough_map("laminate_r", smear, 0.4, 0.55, 0)),
    "stone": (normal_map("stone_n", fine, 0.15), rough_map("stone_r", smear, 0.16, 0.34, 0)),
    "plaster": (normal_map("plaster_n", noise(3) * 0.6 + noise(9) * 0.4, 0.4), None),
    "floor": (normal_map("floor_n", tiles * 0.6 + smear * 0.2, 0.5), rough_map("floor_r", smear, 0.22, 0.42, 0)),
}


def tinted(name, base, amount, source, extra=None):
    """Color map with subtle value variation around a linear color."""
    v = 1 + (source[..., None] - 0.5) * amount
    rgb = np.array(base)[None, None, :] * v
    if extra is not None:
        rgb = rgb * extra[..., None]
    srgb = np.where(rgb < 0.0031308, rgb * 12.92, 1.055 * np.clip(rgb, 1e-6, 1) ** (1 / 2.4) - 0.055)
    return image(name, np.clip(srgb, 0, 1), data=False)


def screen_map():
    """Inventory table shown on the terminal."""
    h, w = 288, 512
    px = np.zeros((h, w, 3))
    px[:] = (0.07, 0.1, 0.13)
    px[:34] = (0.1, 0.36, 0.4)
    px[12:22, 16:130] = (0.85, 0.95, 0.95)
    px[50:270, 16:96] = (0.1, 0.14, 0.18)
    for i in range(6):
        px[64 + i * 32 : 70 + i * 32, 26 : 60 + 5 * (i % 3)] = (0.45, 0.55, 0.6)
    for i in range(9):
        y = 54 + i * 24
        px[y : y + 20, 110:496] = (0.11, 0.15, 0.19) if i % 2 else (0.13, 0.18, 0.22)
        for x, length in ((120, 60), (230, 40), (320, 70)):
            px[y + 7 : y + 12, x : x + length - 7 * (i % 3)] = (0.6, 0.68, 0.72)
        px[y + 5 : y + 14, 450:482] = (0.75, 0.55, 0.2) if i == 4 else (0.2, 0.6, 0.42)
    return image("screen", px[::-1], data=False)


# Materials


def material(name, color, maps=None, tile=1.0, rough=0.5, metal=0.0, emit=0.0, tex=None, alpha=1.0):
    m = bpy.data.materials.new(name)
    m["tile"] = tile
    m.use_nodes = True
    nodes, links = m.node_tree.nodes, m.node_tree.links
    b = nodes["Principled BSDF"]
    b.inputs["Base Color"].default_value = (*color, 1)
    b.inputs["Roughness"].default_value = rough
    b.inputs["Metallic"].default_value = metal
    if emit:
        b.inputs["Emission Color"].default_value = (*color, 1)
        b.inputs["Emission Strength"].default_value = emit
    if alpha < 1:
        b.inputs["Alpha"].default_value = alpha
        m.blend_method = "BLEND"

    def sample(img):
        n = nodes.new("ShaderNodeTexImage")
        n.image = img
        return n

    if tex:
        t = sample(tex)
        links.new(t.outputs["Color"], b.inputs["Base Color"])
        if emit:
            links.new(t.outputs["Color"], b.inputs["Emission Color"])
    if maps:
        normal, rough_img = MAPS[maps]
        nm = nodes.new("ShaderNodeNormalMap")
        links.new(sample(normal).outputs["Color"], nm.inputs["Color"])
        links.new(nm.outputs["Normal"], b.inputs["Normal"])
        if rough_img:
            sep = nodes.new("ShaderNodeSeparateColor")
            links.new(sample(rough_img).outputs["Color"], sep.inputs["Color"])
            links.new(sep.outputs["Green"], b.inputs["Roughness"])
            links.new(sep.outputs["Blue"], b.inputs["Metallic"])
    return m


ENAMEL = material("enamel", (0.66, 0.67, 0.66), "enamel", 0.35)
LINER = material("liner", (0.5, 0.53, 0.55), "enamel", 0.35)
STEEL = material("steel", (0.62, 0.63, 0.64), "steel", 0.3)
WIRE = material("wire", (0.7, 0.71, 0.72), rough=0.28, metal=1.0)
BLACK = material("black_glass", (0.006, 0.007, 0.009), rough=0.06)
DARK = material("dark_plastic", (0.022, 0.024, 0.027), "plastic", 0.15)
RUBBER = material("rubber", (0.018, 0.018, 0.019), rough=0.85)
GLASS = material("glass", (0.02, 0.025, 0.025), rough=0.02, alpha=0.1)
CARTON = material("carton", (0.6, 0.6, 0.57), "laminate", 0.1)
BANDS = [
    material("band_teal", (0.03, 0.22, 0.24), rough=0.45),
    material("band_navy", (0.03, 0.07, 0.2), rough=0.45),
    material("band_amber", (0.5, 0.21, 0.03), rough=0.45),
]
BLUE = material("carrier_blue", (0.012, 0.06, 0.13), "plastic", 0.18)
LID = material("carrier_lid", (0.72, 0.73, 0.72), "plastic", 0.18)
ICE = material("ice", (0.25, 0.5, 0.65), rough=0.2)
FRONT = material("cabinet", (0.4, 0.415, 0.385), "laminate", 0.4)
KICK = material("kick", (0.09, 0.095, 0.1), "laminate", 0.4)
STONE = material("stone", (1, 1, 1), "stone", 0.5, tex=tinted("stone_c", (0.05, 0.052, 0.055), 0.8, speck))
WALL = material("wall", (0.56, 0.55, 0.51), "plaster", 0.5, rough=0.85)
PANEL = material("wainscot", (0.2, 0.265, 0.25), "plaster", 0.5, rough=0.6)
TRIM = material("trim", (0.72, 0.72, 0.7), "laminate", 0.4)
CEILING = material("ceiling", (0.75, 0.75, 0.74), "plaster", 0.5, rough=0.95)
FLOOR = material(
    "floor", (1, 1, 1), "floor", 0.9, tex=tinted("floor_c", (0.24, 0.24, 0.225), 0.16, speck, 0.9 + 0.1 * tiles)
)
SKY = material("sky", (0.75, 0.88, 1.0), emit=6.0)
LED = material("led", (1.0, 0.98, 0.94), emit=5.0)
LAMP = material("lamp", (0.0, 0.0, 0.0), rough=0.3)
SCREEN = material("screen", (1, 1, 1), tile=0, emit=1.0, rough=0.15, tex=screen_map())
PAPER = material("paper", (0.75, 0.74, 0.7), rough=0.8)


# Geometry


def group(name, pivot=(0, 0, 0), parent=None):
    o = bpy.data.objects.new(name, None)
    o.location = pivot
    o.parent = parent
    scene.collection.objects.link(o)
    return o


def add(name, verts, faces, mats, parent, bevel=0.0, segments=3, slots=None):
    """Adds a mesh given in world space. UVs use box projection in meters."""
    pivot = np.array(parent.location)
    me = bpy.data.meshes.new(name)
    me.from_pydata([tuple(np.array(v) - pivot) for v in verts], [], faces)
    mats = mats if isinstance(mats, list) else [mats]
    for m in mats:
        me.materials.append(m)
    uv = me.uv_layers.new(name="UVMap")
    for p in me.polygons:
        p.use_smooth = True
        if slots:
            p.material_index = slots[p.index]
        axis = max(range(3), key=lambda i: abs(p.normal[i]))
        a, b = {0: (1, 2), 1: (0, 2), 2: (0, 1)}[axis]
        tile = mats[p.material_index]["tile"]
        flip = -1 if p.normal[axis] * (1, -1, 1)[axis] < 0 else 1
        for loop in p.loop_indices:
            v = verts[me.loops[loop].vertex_index]
            uv.data[loop].uv = (flip * v[a] / tile, v[b] / tile) if tile else (0, 0)
    if not any(m["tile"] for m in mats):
        for p in me.polygons:
            if p.normal.y < -0.9:
                co = [verts[me.loops[i].vertex_index] for i in p.loop_indices]
                xs, zs = [c[0] for c in co], [c[2] for c in co]
                for loop, c in zip(p.loop_indices, co):
                    uv.data[loop].uv = (
                        (c[0] - min(xs)) / (max(xs) - min(xs)),
                        (c[2] - min(zs)) / (max(zs) - min(zs)),
                    )
    me.use_auto_smooth = True
    me.auto_smooth_angle = math.radians(40)
    o = bpy.data.objects.new(name, me)
    scene.collection.objects.link(o)
    o.parent = parent
    if bevel:
        mod = o.modifiers.new("bevel", "BEVEL")
        mod.width, mod.segments = bevel, segments
        mod.limit_method, mod.angle_limit = "ANGLE", math.radians(35)
        mod.harden_normals = True
    # Tangent export needs triangles.
    tri = o.modifiers.new("triangulate", "TRIANGULATE")
    tri.keep_custom_normals = True
    return o


def corners(lo, hi):
    (a, b, c), (d, e, f) = lo, hi
    return [(a, b, c), (d, b, c), (d, e, c), (a, e, c), (a, b, f), (d, b, f), (d, e, f), (a, e, f)]


CUBE = [(0, 3, 2, 1), (4, 5, 6, 7), (0, 1, 5, 4), (2, 3, 7, 6), (1, 2, 6, 5), (3, 0, 4, 7)]


def box(name, lo, hi, mat, parent, bevel=0.003, segments=3):
    return add(name, corners(lo, hi), CUBE, mat, parent, bevel, segments)


def shell(name, lo, hi, cut_lo, cut_hi, mats, parent, side, bevel):
    """Box with a cavity. Side is front or top, the cavity reaches that face."""
    v = corners(lo, hi) + corners(cut_lo, cut_hi)
    ring = [0, 1, 5, 4] if side == "front" else [4, 5, 6, 7]
    inner = {"front": CUBE[2], "top": CUBE[1]}[side]
    faces = [f for f in CUBE if f != inner]
    slots = [0] * len(faces)
    for i in range(4):
        a, b = ring[i], ring[(i + 1) % 4]
        faces.append((a, b, b + 8, a + 8))
        slots.append(0)
    for f in CUBE:
        if f != inner:
            faces.append(tuple(i + 8 for i in reversed(f)))
            slots.append(1)
    return add(name, v, faces, mats, parent, bevel, 3, slots)


def ring(name, x0, x1, z0, z1, width, y0, y1, mat, parent, bevel):
    """Rectangular frame facing front."""
    out = [(x0, z0), (x1, z0), (x1, z1), (x0, z1)]
    ins = [(x0 + width, z0 + width), (x1 - width, z0 + width), (x1 - width, z1 - width), (x0 + width, z1 - width)]
    v = [(x, y, z) for y in (y0, y1) for x, z in out + ins]
    f = []
    for i in range(4):
        j = (i + 1) % 4
        f += [(i, j, j + 4, i + 4), (j + 8, i + 8, i + 12, j + 12)]
        f += [(j, i, i + 8, j + 8), (i + 4, j + 4, j + 12, i + 12)]
    return add(name, v, f, mat, parent, bevel)


def tube(name, points, radius, mat, parent, sides=12, smooth=True, segments=None):
    """Round bar along points. Each item of segments is a separate path."""
    verts, faces = [], []
    for path in segments or [points]:
        pts = [np.array(p, float) for p in path]
        base = len(verts)
        for i, p in enumerate(pts):
            d = pts[min(i + 1, len(pts) - 1)] - pts[max(i - 1, 0)]
            d /= np.linalg.norm(d)
            u = np.cross(d, (0, 0, 1) if abs(d[2]) < 0.9 else (1, 0, 0))
            u /= np.linalg.norm(u)
            w = np.cross(d, u)
            for k in range(sides):
                a = 2 * math.pi * k / sides
                verts.append(tuple(p + radius * (math.cos(a) * u + math.sin(a) * w)))
        n = len(pts)
        for i in range(n - 1):
            for k in range(sides):
                a, b = base + i * sides + k, base + i * sides + (k + 1) % sides
                faces.append((a, b, b + sides, a + sides))
        faces.append(tuple(reversed(range(base, base + sides))))
        faces.append(tuple(range(base + (n - 1) * sides, base + n * sides)))
    return add(name, verts, faces, mat, parent)


def arc(center, radius, start, end, plane, n=8):
    """Points on an arc. Plane is the pair of axes it spans."""
    out = []
    for i in range(n + 1):
        a = math.radians(start + (end - start) * i / n)
        p = list(center)
        p[plane[0]] += radius * math.cos(a)
        p[plane[1]] += radius * math.sin(a)
        out.append(tuple(p))
    return out


def bar_handle(name, a, b, stand, radius, parent):
    """Bar between two points standing off the face behind it."""
    a, b = np.array(a, float), np.array(b, float)
    d = (b - a) / np.linalg.norm(b - a)
    off = np.array((0, -stand, 0))
    inset = d * 0.04 if np.linalg.norm(b - a) > 0.2 else d * 0.02
    paths = [[a + off, b + off], [a + inset, a + inset + off], [b - inset, b - inset + off]]
    tube(name, None, radius, STEEL, parent, 16, segments=paths)


def disc(name, center, radius, depth, mat, parent, sides=32, bevel=0.002):
    """Cylinder along Z."""
    x, y, z = center
    v = []
    for h in (0, depth):
        for k in range(sides):
            a = 2 * math.pi * k / sides
            v.append((x + radius * math.cos(a), y + radius * math.sin(a), z + h))
    f = [tuple(reversed(range(sides))), tuple(range(sides, 2 * sides))]
    f += [(k, (k + 1) % sides, (k + 1) % sides + sides, k + sides) for k in range(sides)]
    return add(name, v, f, mat, parent, bevel, 2)


def lamp(name, lo, hi, parent, owner=None):
    node = group(name, parent=owner)
    box(name + "_face", lo, hi, LAMP, node, 0.0008, 2)


def text(value, center, size, mat, parent, side="front"):
    """Raised lettering centered on a point. Side is front, left or top."""
    curve = bpy.data.curves.new("text", "FONT")
    curve.body, curve.size = value, size
    curve.align_x, curve.align_y = "CENTER", "CENTER"
    curve.resolution_u = 2
    o = bpy.data.objects.new("text", curve)
    scene.collection.objects.link(o)
    bpy.context.view_layer.update()
    me = bpy.data.meshes.new_from_object(o.evaluated_get(bpy.context.evaluated_depsgraph_get()))
    bpy.data.objects.remove(o)
    x, y, z = np.array(center) - np.array(parent.location)
    for v in me.vertices:
        a, b, _ = v.co
        v.co = {"front": (x + a, y, z + b), "left": (x, y - a, z + b), "top": (x + a, y + b, z)}[side]
    me.materials.clear()
    me.materials.append(mat)
    out = bpy.data.objects.new("text", me)
    scene.collection.objects.link(out)
    out.parent = parent
    out.modifiers.new("triangulate", "TRIANGULATE")


def spot(name, x, y, z, parent=None):
    group(name, (x, y, z), parent)


def tray(name, x0, x1, y0, y1, z, mat, parent, height=0.03):
    shell(name, (x0, y0, z), (x1, y1, z + height), (x0 + 0.008, y0 + 0.008, z + 0.006),
          (x1 - 0.008, y1 - 0.008, z + height), [mat, mat], parent, "top", 0.002)


def grid(place, x, y, z, parent=None, columns=3, dx=0.145, dy=0.125):
    """Six spots, the front row first. Starts at the front left."""
    for i in range(MAX):
        spot(f"spot_{place}_{i + 1}", x + i % columns * dx, y + i // columns * dy, z, parent)


# Room

MAX = 6
BW, BD, BH = 0.13, 0.11, 0.1
RED = material("warning_red", (0.55, 0.02, 0.015), rough=0.45)
GREEN = material("exit_green", (0.02, 0.2, 0.1), rough=0.5)
WHITE = material("print_white", (0.85, 0.85, 0.83), rough=0.6)
INK = material("ink", (0.01, 0.01, 0.012), rough=0.7)
room = group("room")
X0, X1, Y0, TOP = -2.2, 6.2, -5.6, 2.8
box("floor", (X0, Y0, -0.1), (X1, 0, 0), FLOOR, room, 0)
box("wall", (X0, 0, -0.1), (X1, 0.12, TOP), WALL, room, 0)
box("wall", (X1, Y0, -0.1), (X1 + 0.12, 0.12, TOP), WALL, room, 0)
box("ceiling", (X0 - 0.2, Y0, TOP), (X1 + 0.2, 0.2, TOP + 0.1), CEILING, room, 0)
WY0, WY1, WZ0, WZ1 = -2.75, -1.05, 1.0, 2.25
for lo, hi in (
    ((Y0, -0.1), (0.12, WZ0)),
    ((Y0, WZ1), (0.12, TOP)),
    ((Y0, WZ0), (WY0, WZ1)),
    ((WY1, WZ0), (0.12, WZ1)),
):
    box("wall", (X0 - 0.16, lo[0], lo[1]), (X0, hi[0], hi[1]), WALL, room, 0)
box("wainscot", (X0, -0.008, 0), (X1, 0.01, 1.08), PANEL, room, 0)
box("wainscot", (X0 - 0.01, Y0, 0), (X0 + 0.008, 0, 0.98), PANEL, room, 0)
box("rail", (X0, -0.022, 1.08), (X1, 0, 1.125), TRIM, room, 0.006)
box("skirt", (X0, -0.016, 0), (X1, 0, 0.1), KICK, room, 0.004)
box("skirt", (X0, Y0, 0), (X0 + 0.016, 0, 0.1), KICK, room, 0.004)

# Window with blinds on the left wall.
for lo, hi in (
    ((WY0, WZ0), (WY1, WZ0 + 0.05)),
    ((WY0, WZ1 - 0.05), (WY1, WZ1)),
    ((WY0, WZ0), (WY0 + 0.05, WZ1)),
    ((WY1 - 0.05, WZ0), (WY1, WZ1)),
    (((WY0 + WY1) / 2 - 0.025, WZ0), ((WY0 + WY1) / 2 + 0.025, WZ1)),
):
    box("frame", (X0 - 0.1, lo[0], lo[1]), (X0 - 0.04, hi[0], hi[1]), TRIM, room, 0.004)
box("sill", (X0 - 0.16, WY0 - 0.04, WZ0 - 0.03), (X0 + 0.08, WY1 + 0.04, WZ0), TRIM, room, 0.006)
for i in range(9):
    z = WZ1 - 0.07 - i * 0.045
    y0, y1 = WY0 + 0.06, WY1 - 0.06
    v = [(X0 - 0.03, y0, z + 0.012), (X0 - 0.03, y1, z + 0.012), (X0 + 0.012, y1, z), (X0 + 0.012, y0, z)]
    v += [(x, y, h - 0.002) for x, y, h in v]
    add("slat", v, [(3, 2, 1, 0), (4, 5, 6, 7), (0, 1, 5, 4), (1, 2, 6, 5), (2, 3, 7, 6), (3, 0, 4, 7)], TRIM, room)
sky = group("sky")
box("sky_face", (X0 - 0.7, WY0 - 1.0, WZ0 - 0.6), (X0 - 0.68, WY1 + 1.0, WZ1 + 0.8), SKY, sky, 0)
glow = group("glow_ceiling")
for x in (-0.9, 1.9, 4.6):
    box("panel_rim", (x - 0.63, -2.33, TOP - 0.02), (x + 0.63, -1.67, TOP), TRIM, room, 0.004)
    box("panel", (x - 0.6, -2.3, TOP - 0.022), (x + 0.6, -1.7, TOP - 0.01), LED, glow if x < 3 else room, 0)

# Outlet with the fridge cord.
box("outlet", (-0.36, -0.012, 1.22), (-0.28, 0, 1.34), TRIM, room, 0.004)
box("plug", (-0.345, -0.045, 1.255), (-0.295, -0.01, 1.3), DARK, room, 0.006)
cord = [(-0.32, -0.04, 1.26), (-0.325, -0.05, 1.2), (-0.34, -0.05, 1.12), (-0.39, -0.045, 1.04), (-0.44, -0.04, 1.0), (-0.5, -0.02, 0.98)]
tube("cord", cord, 0.005, RUBBER, room)

# Fridge

F = -0.87
W, FRONT_Y, H = 0.39, -0.7, 1.98
DY0, DY1 = -0.765, -0.712
DZ0, DZ1 = 0.315, 1.755
body = group("fridge_body")
shell(
    "cabinet",
    (F - W, FRONT_Y, 0.05),
    (F + W, -0.04, H),
    (F - W + 0.055, FRONT_Y, 0.37),
    (F + W - 0.055, -0.11, 1.7),
    [ENAMEL, LINER],
    body,
    "front",
    0.008,
)
for x in (F - W + 0.06, F + W - 0.06):
    for y in (-0.62, -0.12):
        disc("foot", (x, y, 0), 0.025, 0.06, RUBBER, body)
# Header and base fascia share the door plane.
box("header", (F - W, DY0, DZ1 + 0.005), (F + W, FRONT_Y + 0.002, H), ENAMEL, body, 0.007)
box("base", (F - W, DY0 + 0.012, 0.07), (F + W, FRONT_Y + 0.002, DZ0 - 0.006), ENAMEL, body, 0.007)
ring("grille_rim", F - 0.3, F + 0.3, 0.105, 0.275, 0.012, DY0 + 0.006, DY0 + 0.02, DARK, body, 0.003)
box("grille_back", (F - 0.29, DY0 + 0.0105, 0.115), (F + 0.29, DY0 + 0.014, 0.265), RUBBER, body, 0)
for i in range(9):
    z = 0.124 + i * 0.0165
    box("grille_slat", (F - 0.289, DY0 + 0.004, z), (F + 0.289, DY0 + 0.016, z + 0.007), DARK, body, 0.0025, 2)
box("display", (F - 0.3, DY0 - 0.003, 1.815), (F + 0.3, DY0 + 0.01, 1.92), BLACK, body, 0.004)
display = group("display_fridge")
box("display_face", (F - 0.27, DY0 - 0.0045, 1.838), (F + 0.15, DY0 - 0.003, 1.913), SCREEN, display, 0)
lamp("lamp_fridge_temperature", (F - 0.26, DY0 - 0.005, 1.824), (F + 0.26, DY0 - 0.002, 1.831), body)
text("2 - 8 \u00b0C", (F, DY0 - 0.001, 1.95), 0.04, INK, body)
text("VACCINES ONLY", (F, DY0 + 0.0115, 0.295), 0.02, INK, body)
lamp("lamp_fridge_power", (F + 0.19, DY0 - 0.005, 1.861), (F + 0.202, DY0 - 0.002, 1.873), body)
lamp("lamp_fridge_network", (F + 0.23, DY0 - 0.005, 1.861), (F + 0.242, DY0 - 0.002, 1.873), body)
for z in (DZ0 + 0.0, DZ1 - 0.012):
    box("hinge", (F - W + 0.005, DY0 + 0.01, z - 0.006), (F - W + 0.07, FRONT_Y, z + 0.018), STEEL, body, 0.002)
box("seal_face", (F - W + 0.035, FRONT_Y - 0.004, 0.35), (F - W + 0.055, FRONT_Y + 0.01, 1.72), STEEL, body, 0.001)
box("seal_face", (F + W - 0.055, FRONT_Y - 0.004, 0.35), (F + W - 0.035, FRONT_Y + 0.01, 1.72), STEEL, body, 0.001)

# Interior.
IX0, IX1, IY0, IY1 = F - W + 0.06, F + W - 0.06, -0.64, -0.14
box("duct", (F - 0.2, -0.135, 0.45), (F + 0.2, -0.108, 1.66), LINER, body, 0.008)
for i in range(6):
    z = 0.62 + i * 0.16
    box("vent", (F - 0.14, -0.138, z), (F + 0.14, -0.133, z + 0.008), LINER, body, 0.001)
strip = group("glow_fridge")
box("led_strip", (F - 0.3, -0.66, 1.688), (F + 0.3, -0.635, 1.698), LED, strip, 0.002)
for level, z in enumerate((0.44, 0.75, 1.06, 1.37)):
    thick = [
        [(IX0, IY0, z), (IX1, IY0, z)],
        [(IX0, IY1, z), (IX1, IY1, z)],
        [(IX0, IY0, z + 0.03), (IX1, IY0, z + 0.03)],
        [(IX0, -0.39, z - 0.004), (IX1, -0.39, z - 0.004)],
    ]
    tube("shelf", None, 0.0035, WIRE, body, 8, segments=thick)
    n = 26
    thin = [
        [(IX0 + (IX1 - IX0) * (i + 0.5) / n, y, z + 0.004) for y in (IY0, IY1)] for i in range(n)
    ]
    tube("shelf", None, 0.0018, WIRE, body, 6, segments=thin)
    for x in (IX0, IX1 - 0.006):
        box("bracket", (x, IY0, z - 0.012), (x + 0.006, IY1, z - 0.002), LINER, body, 0.001)

for level, z in ((1, 0.754), (2, 1.064)):
    for i in range(3):
        box("rail", (F - 0.2 + i * 0.2 - 0.05, IY0 - 0.006, z - 0.006), (F - 0.2 + i * 0.2 + 0.05, IY0 - 0.002, z + 0.016), LID, body, 0.001)
        text(f"{(2 - level) * 3 + i + 1}", (F - 0.2 + i * 0.2, IY0 - 0.0065, z + 0.005), 0.018, INK, body)
for i in range(MAX):
    spot(f"spot_fridge_{i + 1}", F - 0.2 + i % 3 * 0.2, -0.55, 1.064 - i // 3 * 0.31)
# Locked batches stay cold in the basket below.
tray("basket", IX0 + 0.02, IX1 - 0.02, IY0 + 0.01, IY1 - 0.06, 0.446, RED, body, 0.05)
text("QUARANTINE   DO NOT USE", (F, IY0 + 0.0085, 0.472), 0.032, WHITE, body)
grid("quarantine", F - 0.145, -0.55, 0.453, dx=0.145, dy=0.13)
# Probe in a glycol bottle on the top shelf.
disc("bottle", (F + 0.27, -0.55, 1.376), 0.028, 0.085, ICE, body, bevel=0.006)
disc("cap", (F + 0.27, -0.55, 1.461), 0.02, 0.018, DARK, body)
tube("probe_cord", [(F + 0.27, -0.55, 1.475), (F + 0.275, -0.54, 1.5), (F + 0.3, -0.5, 1.52), (F + 0.322, -0.4, 1.54), (F + 0.325, -0.2, 1.56)], 0.0025, RUBBER, body, 8)
text("PROBE", (F + 0.27, -0.581, 1.41), 0.014, INK, body)

# The fan sits behind the grille of the top vent.
fan = group("fridge_fan", (F, -0.142, 1.6))
for i in range(5):
    a = 2 * math.pi * i / 5
    b = a + 0.75
    v = []
    for r, t, y in ((0.012, a, -0.004), (0.048, a + 0.15, -0.006), (0.048, b, 0.003), (0.012, b, 0.002)):
        v.append((F + r * math.cos(t), -0.146 + y, 1.6 + r * math.sin(t)))
    v += [(x, y + 0.0015, z) for x, y, z in v]
    add("blade", v, [(0, 1, 2, 3), (7, 6, 5, 4), (1, 0, 4, 5), (2, 1, 5, 6), (3, 2, 6, 7), (0, 3, 7, 4)], DARK, fan)
tube("hub", [(F, -0.152, 1.6), (F, -0.14, 1.6)], 0.013, DARK, fan, 20)
guard = []
for r in (0.02, 0.035, 0.052):
    guard.append(arc((F, -0.158, 1.6), r, 0, 360, (0, 2), 32))
guard += [[(F - 0.052, -0.158, 1.6), (F + 0.052, -0.158, 1.6)], [(F, -0.158, 1.548), (F, -0.158, 1.652)]]
tube("guard", None, 0.0012, WIRE, body, 6, segments=guard)

door = group("fridge_door", (F - W, DY1, (DZ0 + DZ1) / 2))
ring("door_frame", F - W, F + W, DZ0, DZ1, 0.062, DY0, DY1, ENAMEL, door, 0.007)
ring("door_trim", F - W + 0.056, F + W - 0.056, DZ0 + 0.056, DZ1 - 0.056, 0.028, DY0 + 0.004, DY0 + 0.012, BLACK, door, 0.0015)
ring("door_spacer", F - W + 0.06, F + W - 0.06, DZ0 + 0.06, DZ1 - 0.06, 0.02, DY0 + 0.012, DY1 - 0.006, STEEL, door, 0.001)
ring("gasket", F - W + 0.02, F + W - 0.02, DZ0 + 0.02, DZ1 - 0.02, 0.026, DY1 - 0.001, DY1 + 0.01, RUBBER, door, 0.004)
box("pane", (F - W + 0.058, DY0 + 0.006, DZ0 + 0.058), (F + W - 0.058, DY0 + 0.01, DZ1 - 0.058), GLASS, door, 0)
bar_handle("handle", (F + W - 0.031, DY0, 0.78), (F + W - 0.031, DY0, 1.38), 0.048, 0.0095, door)

# Counter

CX0, CX1, CY = -0.34, 2.46, -0.62
counter = group("counter")
box("carcass", (CX0, CY + 0.03, 0.1), (CX1, -0.02, 0.88), FRONT, counter, 0.002)
box("kick", (CX0 + 0.02, CY + 0.09, 0), (CX1 - 0.02, -0.02, 0.1), KICK, counter, 0)
box("top", (CX0 - 0.015, CY - 0.03, 0.88), (CX1 + 0.02, 0, 0.915), STONE, counter, 0.005, 4)
box("upstand", (CX0 - 0.015, -0.018, 0.915), (CX1 + 0.02, 0, 1.0), STONE, counter, 0.003)
BAYS = 5
bay = (CX1 - CX0) / BAYS
for i in range(BAYS):
    x0, x1 = CX0 + i * bay + 0.002, CX0 + (i + 1) * bay - 0.002
    rows = [(0.105, 0.36), (0.364, 0.62), (0.624, 0.872)] if i in (0, 3) else [(0.105, 0.872)]
    for z0, z1 in rows:
        box("front", (x0, CY + 0.008, z0), (x1, CY + 0.03, z1), FRONT, counter, 0.0025)
        mid = (x0 + x1) / 2
        if len(rows) > 1:
            bar_handle("pull", (mid - 0.09, CY + 0.008, z1 - 0.05), (mid + 0.09, CY + 0.008, z1 - 0.05), 0.026, 0.005, counter)
        else:
            x = x1 - 0.045 if i % 2 else x0 + 0.045
            bar_handle("pull", (x, CY + 0.008, 0.6), (x, CY + 0.008, 0.82), 0.026, 0.005, counter)

# Wall cabinet with binders and stock beside it.
UX0, UX1 = 1.1, 2.46
box("upper", (UX0, -0.34, 1.72), (UX1, 0, 2.36), FRONT, counter, 0.002)
for i in range(3):
    w = (UX1 - UX0) / 3
    x0 = UX0 + i * w
    box("upper_front", (x0 + 0.002, -0.362, 1.715), (x0 + w - 0.002, -0.34, 2.362), FRONT, counter, 0.0025)
    x = x0 + 0.04 if i == 1 else x0 + w - 0.04
    bar_handle("pull", (x, -0.362, 1.76), (x, -0.362, 1.94), 0.026, 0.005, counter)
box("shelf", (-0.1, -0.24, 1.62), (0.88, 0, 1.645), FRONT, counter, 0.003)
x = -0.06
for i in range(6):
    w = dice.choice([0.04, 0.055, 0.065])
    h = dice.uniform(0.27, 0.3)
    box("binder", (x, -0.22, 1.646), (x + w, -0.02, 1.646 + h), [KICK, BANDS[0], PAPER, BANDS[1], CARTON, KICK][i], counter, 0.004)
    box("spine", (x + w * 0.2, -0.2212, 1.78), (x + w * 0.8, -0.219, 1.86), PAPER, counter, 0)
    x += w + 0.004
for x, r, h in ((0.42, 0.04, 0.16), (0.53, 0.032, 0.12), (0.62, 0.032, 0.12)):
    disc("jar", (x, -0.12, 1.646), r, h, LID, counter, bevel=0.008)
    disc("jar_cap", (x, -0.12, 1.646 + h), r * 0.8, 0.025, DARK, counter, bevel=0.003)
# Battery module beside the outlet. It feeds monitoring only.
box("ups", (-0.2, -0.1, 1.18), (0.06, -0.01, 1.48), DARK, counter, 0.008)
box("ups_face", (-0.185, -0.104, 1.2), (0.045, -0.098, 1.46), BLACK, counter, 0.004)
text("MONITOR", (-0.07, -0.1045, 1.43), 0.034, WHITE, counter)
text("BATTERY", (-0.07, -0.1045, 1.395), 0.034, WHITE, counter)
for i in range(5):
    lamp(f"lamp_fridge_battery{i + 1}", (-0.15, -0.107, 1.22 + i * 0.03), (0.01, -0.104, 1.24 + i * 0.03), counter)
tube("ups_cord", [(-0.28, -0.04, 1.29), (-0.25, -0.05, 1.26), (-0.22, -0.05, 1.26), (-0.19, -0.05, 1.27)], 0.004, RUBBER, counter, 8)
tube("ups_feed", [(-0.15, -0.05, 1.19), (-0.15, -0.045, 1.1), (-0.3, -0.04, 1.03), (-0.5, -0.02, 1.0)], 0.004, RUBBER, counter, 8)

# Carrier

C, CZ = 0.36, 0.935
CW, CD = 0.25, 0.17
CB, CF = -0.15, -0.49
rig = group("carrier_rig")
carrier = group("carrier_body", parent=rig)
shell(
    "tub",
    (C - CW, CF, CZ),
    (C + CW, CB, CZ + 0.3),
    (C - CW + 0.045, CF + 0.045, CZ + 0.045),
    (C + CW - 0.045, CB - 0.045, CZ + 0.3),
    [BLUE, LID],
    carrier,
    "top",
    0.014,
)
box("rim", (C - CW - 0.008, CF - 0.008, CZ + 0.262), (C + CW + 0.008, CF + 0.02, CZ + 0.292), BLUE, carrier, 0.006)
box("rim", (C - CW - 0.008, CB - 0.02, CZ + 0.262), (C + CW + 0.008, CB + 0.008, CZ + 0.292), BLUE, carrier, 0.006)
for s in (-1, 1):
    x = C + s * CW
    box("rim", (min(x, x + s * 0.008), CF, CZ + 0.262), (max(x, x + s * 0.008), CB, CZ + 0.292), BLUE, carrier, 0.003)
    for y in (CF + 0.05, CB - 0.05):
        disc("foot", (C + s * (CW - 0.05), y, 0.915), 0.022, 0.022, RUBBER, carrier)
    for y in (-0.4, -0.32, -0.24):
        box("rib", (min(x, x + s * 0.007), y - 0.012, CZ + 0.03), (max(x, x + s * 0.007), y + 0.012, CZ + 0.24), BLUE, carrier, 0.004)
box("plate", (C - 0.17, CF - 0.006, CZ + 0.085), (C + 0.17, CF + 0.01, CZ + 0.2), DARK, carrier, 0.008)
box("plate_glass", (C - 0.155, CF - 0.008, CZ + 0.1), (C + 0.155, CF, CZ + 0.185), BLACK, carrier, 0.004)
shown = group("display_carrier", parent=rig)
box("display_face", (C - 0.147, CF - 0.0095, CZ + 0.122), (C + 0.073, CF - 0.0085, CZ + 0.18), SCREEN, shown, 0)
lamp("lamp_carrier_temperature", (C - 0.14, CF - 0.0095, CZ + 0.107), (C + 0.14, CF - 0.007, CZ + 0.113), carrier, rig)
lamp("lamp_carrier_power", (C + 0.085, CF - 0.0095, CZ + 0.138), (C + 0.095, CF - 0.007, CZ + 0.148), carrier, rig)
lamp("lamp_carrier_network", (C + 0.115, CF - 0.0095, CZ + 0.138), (C + 0.125, CF - 0.007, CZ + 0.148), carrier, rig)
for s in (-1, 1):
    box("ice_pack", (C + s * 0.18 - 0.02, CF + 0.05, CZ + 0.05), (C + s * 0.18 + 0.02, CB - 0.05, CZ + 0.24), ICE, carrier, 0.008)
tube("probe", [(C - 0.14, CF + 0.06, CZ + 0.06), (C - 0.14, CF + 0.06, CZ + 0.25)], 0.006, STEEL, carrier, 12)
tube("probe_cord", [(C - 0.14, CF + 0.06, CZ + 0.25), (C - 0.14, CF + 0.045, CZ + 0.27), (C - 0.1, CF + 0.04, CZ + 0.285)], 0.0025, RUBBER, carrier, 8)
text("BOX-1", (C, CF - 0.0005, CZ + 0.045), 0.04, WHITE, carrier)
for i in range(MAX):
    spot(f"spot_carrier_{i + 1}", C - 0.069 + i % 2 * 0.138, -0.375 + i // 2 % 2 * 0.115, CZ + 0.047 + i // 4 * 0.102, rig)
lid = group("carrier_lid", (C, CB + 0.004, CZ + 0.302), rig)
box("lid", (C - CW - 0.006, CF - 0.006, CZ + 0.302), (C + CW + 0.006, CB + 0.006, CZ + 0.362), LID, lid, 0.014, 4)
box("lid_crown", (C - CW + 0.04, CF + 0.04, CZ + 0.35), (C + CW - 0.04, CB - 0.04, CZ + 0.372), LID, lid, 0.008, 4)
box("lid_plug", (C - CW + 0.05, CF + 0.05, CZ + 0.27), (C + CW - 0.05, CB - 0.05, CZ + 0.31), LINER, lid, 0.006)
box("lid_seal", (C - CW + 0.03, CF + 0.03, CZ + 0.297), (C + CW - 0.03, CB - 0.03, CZ + 0.304), RUBBER, lid, 0.002)
for s in (-1, 1):
    x = C + s * 0.15
    box("latch", (x - 0.028, CF - 0.02, CZ + 0.235), (x + 0.028, CF - 0.006, CZ + 0.345), DARK, lid, 0.005)
    box("latch_key", (x - 0.018, CF - 0.024, CZ + 0.25), (x + 0.018, CF - 0.018, CZ + 0.3), STEEL, lid, 0.002)
grip = [(C - 0.12, -0.32, CZ + 0.365)]
grip += arc((C - 0.1, -0.32, CZ + 0.4), 0.02, 180, 90, (0, 2), 6)
grip += arc((C + 0.1, -0.32, CZ + 0.4), 0.02, 90, 0, (0, 2), 6)
grip += [(C + 0.12, -0.32, CZ + 0.365)]
tube("grip", grip, 0.009, DARK, lid, 16)

# Trolley under the box. The whole rig moves to the receiver.
cart = group("cart", parent=rig)
box("cart_top", (C - 0.31, CF - 0.06, 0.885), (C + 0.31, CB + 0.06, 0.914), STEEL, cart, 0.006)
box("cart_shelf", (C - 0.3, CF - 0.05, 0.36), (C + 0.3, CB + 0.05, 0.38), STEEL, cart, 0.004)
legs = [[(C + a * 0.29, (CF - 0.045 if b else CB + 0.045), 0.19), (C + a * 0.29, (CF - 0.045 if b else CB + 0.045), 0.89)] for a in (-1, 1) for b in (0, 1)]
tube("cart_leg", None, 0.013, STEEL, cart, 16, segments=legs)
tube("cart_handle", [(C - 0.3, CF, 0.9), (C - 0.38, CF, 0.94), (C - 0.38, CB, 0.94), (C - 0.3, CB, 0.9)], 0.011, DARK, cart, 16)
for a in (-1, 1):
    for y in (CF - 0.045, CB + 0.045):
        tube("wheel", [(C + a * 0.29 - 0.016, y, 0.16), (C + a * 0.29 + 0.016, y, 0.16)], 0.045, RUBBER, cart, 24)
# Modelled over the counter line, parked in front of it.
rig.location = (0.26, -0.72, -0.115)

# Inventory terminal

T = 1.55
term = group("terminal")
box("foot", (T - 0.11, -0.36, 0.916), (T + 0.11, -0.18, 0.926), STEEL, term, 0.004)
box("neck", (T - 0.03, -0.215, 0.92), (T + 0.03, -0.2, 1.2), STEEL, term, 0.004)
box("monitor", (T - 0.27, -0.25, 1.06), (T + 0.27, -0.215, 1.385), DARK, term, 0.006)
box("bezel", (T - 0.268, -0.2525, 1.062), (T + 0.268, -0.248, 1.383), BLACK, term, 0.002)
screen = group("lamp_inventory_screen")
box("screen_face", (T - 0.256, -0.254, 1.083), (T + 0.256, -0.2515, 1.371), SCREEN, screen, 0)
lamp("lamp_inventory_power", (T + 0.235, -0.2545, 1.068), (T + 0.25, -0.252, 1.073), term)
box("keyboard", (T - 0.22, -0.55, 0.916), (T + 0.22, -0.42, 0.93), DARK, term, 0.004)
for r in range(5):
    for c in range(15):
        x, y = T - 0.21 + c * 0.0282, -0.543 + r * 0.0238
        if r == 0 and 4 <= c <= 9:
            if c > 4:
                continue
            box("key", (x, y, 0.928), (x + 0.0282 * 6 - 0.004, y + 0.02, 0.935), KICK, term, 0.002, 2)
        else:
            box("key", (x, y, 0.928), (x + 0.0242, y + 0.02, 0.935), KICK, term, 0.002, 2)
box("mouse", (T + 0.27, -0.53, 0.916), (T + 0.33, -0.43, 0.945), DARK, term, 0.014, 4)
reader = group("reader")
box("reader_base", (T + 0.42, -0.53, 0.916), (T + 0.56, -0.36, 0.938), DARK, reader, 0.007, 4)
box("reader_pad", (T + 0.432, -0.518, 0.936), (T + 0.548, -0.372, 0.9405), BLACK, reader, 0.002)
lamp("lamp_inventory_network", (T + 0.46, -0.39, 0.9402), (T + 0.52, -0.386, 0.942), reader)
waves = [arc((T + 0.49, -0.47, 0.941), r, 30, 150, (0, 1), 12) for r in (0.018, 0.03, 0.042)]
tube("reader_mark", None, 0.0014, PAPER, reader, 6, segments=waves)
tube("reader_cord", [(T + 0.49, -0.362, 0.925), (T + 0.49, -0.3, 0.921), (T + 0.44, -0.24, 0.921), (T + 0.3, -0.2, 0.921), (T + 0.08, -0.19, 0.923)], 0.003, RUBBER, term, 8)
badge = group("badge")
box("card", (T + 0.3, -0.36, 0.9165), (T + 0.39, -0.3, 0.9195), WHITE, badge, 0.0015)
box("card_band", (T + 0.3005, -0.316, 0.9196), (T + 0.3895, -0.301, 0.92), BANDS[0], badge, 0)
text("STAFF", (T + 0.345, -0.336, 0.9203), 0.022, INK, badge, "top")
lanyard = [(T + 0.345, -0.3, 0.919), (T + 0.35, -0.26, 0.919), (T + 0.41, -0.22, 0.919), (T + 0.46, -0.25, 0.919), (T + 0.42, -0.29, 0.919), (T + 0.36, -0.3, 0.919)]
tube("lanyard", lanyard, 0.003, BANDS[0], badge, 8)

# Counter places

tray("delivery", -0.3, 0.16, -0.34, -0.06, 0.916, LID, counter)
text("DELIVERY", (-0.07, -0.341, 0.932), 0.028, INK, counter)
grid("delivery", -0.215, -0.265, 0.923)
tray("dispensed", 0.83, 1.29, -0.34, -0.06, 0.916, LID, counter)
text("TO ROOM 2", (1.06, -0.341, 0.932), 0.028, INK, counter)
grid("out", 0.915, -0.265, 0.923)
tray("discard", 2.14, 2.44, -0.34, -0.06, 0.916, RED, counter, 0.16)
text("DISCARD", (2.29, -0.341, 1.0), 0.05, WHITE, counter)
for i in range(MAX):
    spot(f"spot_discard_{i + 1}", 2.22 + i % 2 * 0.14, -0.262 + i // 2 % 2 * 0.12, 0.923 + i // 4 * 0.102)

# Receiving site behind a partition

site = group("site")
PX, DL, DR = 2.96, -1.95, -0.95
for lo, hi in (((DR, 0), (0, TOP)), ((DL, 2.15), (DR, TOP)), ((-2.5, 0), (DL, TOP))):
    box("partition", (PX, lo[0], lo[1]), (PX + 0.1, hi[0], hi[1]), WALL, site, 0)
for y in (DL, DR):
    box("jamb", (PX - 0.015, y - 0.03, 0), (PX + 0.115, y + 0.03, 2.18), TRIM, site, 0.004)
box("lintel", (PX - 0.015, DL - 0.03, 2.12), (PX + 0.115, DR + 0.03, 2.18), TRIM, site, 0.004)
box("sign", (PX - 0.03, DL + 0.1, 2.28), (PX - 0.005, DR - 0.1, 2.5), GREEN, site, 0.004)
text("TO CLINIC B", (PX - 0.0305, (DL + DR) / 2, 2.39), 0.13, WHITE, site, "left")
for x in (1.2, 1.9, 2.6):
    y = -1.45
    v = [(x, y - 0.06, 0.001), (x + 0.16, y, 0.001), (x, y + 0.06, 0.001), (x + 0.05, y, 0.001)]
    add("arrow", v, [(0, 1, 3), (3, 1, 2)], GREEN, site)
box("site_sign", (3.9, -0.03, 2.02), (5.5, -0.005, 2.3), GREEN, site, 0.004)
text("CLINIC B  RECEIVING", (4.7, -0.0305, 2.16), 0.14, WHITE, site)
box("bench", (4.35, -0.6, 0.1), (5.9, -0.03, 0.88), FRONT, site, 0.003)
box("bench_kick", (4.37, -0.55, 0), (5.88, -0.03, 0.1), KICK, site, 0)
box("bench_top", (4.33, -0.63, 0.88), (5.92, 0, 0.915), STONE, site, 0.005, 4)
for i in range(3):
    x = 4.36 + i * 0.515
    box("bench_front", (x, -0.612, 0.105), (x + 0.505, -0.6, 0.872), FRONT, site, 0.0025)
    bar_handle("pull", (x + 0.45, -0.612, 0.6), (x + 0.45, -0.612, 0.82), 0.026, 0.005, site)
tray("accepted", 4.62, 5.08, -0.42, -0.14, 0.916, BANDS[0], site)
text("ACCEPTED", (4.85, -0.421, 0.932), 0.028, WHITE, site)
grid("receiver", 4.705, -0.345, 0.923)
box("site_reader", (5.3, -0.45, 0.916), (5.46, -0.28, 0.94), DARK, site, 0.007, 4)
box("site_pad", (5.312, -0.438, 0.938), (5.448, -0.292, 0.9425), BLACK, site, 0.002)
text("BADGE", (5.38, -0.365, 0.9427), 0.03, WHITE, site, "top")
box("bay", (3.62, -1.62, 0.0005), (4.3, -1.04, 0.001), GREEN, site, 0)
box("bay_in", (3.65, -1.59, 0.0008), (4.27, -1.07, 0.0014), FLOOR, site, 0)
text("HANDOVER", (3.96, -1.5, 0.002), 0.1, GREEN, site, "top")

# Batches are built around the origin and placed by the viewer.

for i in range(MAX):
    b = group(f"batch_{i + 1}")
    w, d = BW / 2, BD / 2
    e = 0.0006
    box("carton", (-w, -d, 0), (w, d, BH), CARTON, b, 0.0015, 2)
    box("band", (-w - e, -d - e, 0.006), (w + e, d + e, 0.024), BANDS[i % 3], b, 0)
    text(f"VAX-{i + 1:02}", (-0.006, -d - 0.0008, 0.062), 0.032, INK, b)
    box("tag", (0.043, -d - e, 0.076), (0.061, -d, 0.094), STEEL, b, 0)
    text(f"VAX-{i + 1:02}", (0, 0, BH + 0.0006), 0.036, INK, b, "top")
    t = group(f"dnu_{i + 1}", parent=b)
    box("sleeve", (-w - 0.002, -d - 0.002, 0.026), (w + 0.002, d + 0.002, 0.046), RED, t, 0)
    text("DO NOT USE", (0, -d - 0.0026, 0.0355), 0.019, WHITE, t)
    box("sleeve", (-w * 0.9, -d * 0.9, BH), (w * 0.9, -d * 0.45, BH + 0.0015), RED, t, 0)
    text("DO NOT USE", (0, -d * 0.67, BH + 0.002), 0.017, WHITE, t, "top")

bpy.ops.export_scene.gltf(
    filepath=str(OUT), export_format="GLB", export_apply=True, export_tangents=True, export_yup=True
)
print("written", OUT, OUT.stat().st_size, file=sys.stderr)
