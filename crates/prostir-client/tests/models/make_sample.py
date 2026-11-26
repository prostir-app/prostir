#!/usr/bin/env python3
"""Writes sample.glb, the small model the glTF reader's tests read (src/model/glb.rs).

Made for these tests, free of anyone's rights: two nodes under a root with a
translation, one turned and scaled, one placed by a matrix; a textured square
(positions, normals and texture coordinates interleaved, 16-bit indices) with a
2 by 2 PNG; a triangle of one colour without normals (8-bit indices); a triangle
whose texture is in KTX2 (KHR_texture_basisu, its bytes a stand-in); and a node
outside the scene, which is not drawn.

    python3 make_sample.py   # writes sample.glb next to it
"""

import json
import math
import os
import struct
import zlib


def png(width, height, rows):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\x00" + bytes(sum(row, ())) for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def pad4(b, fill=b"\x00"):
    return b + fill * ((4 - len(b) % 4) % 4)


binary = b""
views = []


def view(data, stride=None):
    global binary
    binary = pad4(binary)
    v = {"buffer": 0, "byteOffset": len(binary), "byteLength": len(data)}
    if stride:
        v["byteStride"] = stride
    binary += data
    views.append(v)
    return len(views) - 1


# The square: position, normal, texture coordinates at the texels' centres.
square = [
    ((-0.5, -0.5, 0.0), (0.0, 0.0, 1.0), (0.25, 0.75)),
    ((0.5, -0.5, 0.0), (0.0, 0.0, 1.0), (0.75, 0.75)),
    ((0.5, 0.5, 0.0), (0.0, 0.0, 1.0), (0.75, 0.25)),
    ((-0.5, 0.5, 0.0), (0.0, 0.0, 1.0), (0.25, 0.25)),
]
interleaved = b"".join(struct.pack("<8f", *p, *n, *t) for p, n, t in square)
v_square = view(interleaved, stride=32)
v_square_idx = view(struct.pack("<6H", 0, 1, 2, 0, 2, 3))
triangle = [(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0)]
v_tri = view(b"".join(struct.pack("<3f", *p) for p in triangle))
v_tri_idx = view(struct.pack("<3B", 0, 1, 2))
red, green, blue, white = (255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 255), (255, 255, 255, 255)
v_png = view(png(2, 2, [[red, green], [blue, white]]))
v_ktx = view(b"\xabKTX 20\xbb\r\n\x1a\n" + b"\x00" * 8)

accessors = [
    {"bufferView": v_square, "byteOffset": 0, "componentType": 5126, "count": 4, "type": "VEC3",
     "min": [-0.5, -0.5, 0.0], "max": [0.5, 0.5, 0.0]},
    {"bufferView": v_square, "byteOffset": 12, "componentType": 5126, "count": 4, "type": "VEC3"},
    {"bufferView": v_square, "byteOffset": 24, "componentType": 5126, "count": 4, "type": "VEC2"},
    {"bufferView": v_square_idx, "componentType": 5123, "count": 6, "type": "SCALAR"},
    {"bufferView": v_tri, "componentType": 5126, "count": 3, "type": "VEC3",
     "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
    {"bufferView": v_tri_idx, "componentType": 5121, "count": 3, "type": "SCALAR"},
]

half = math.sqrt(0.5)
gltf = {
    "asset": {"version": "2.0", "generator": "make_sample.py"},
    "extensionsUsed": ["KHR_texture_basisu"],
    "scene": 0,
    "scenes": [{"nodes": [0]}],
    "nodes": [
        {"name": "root", "translation": [0.0, 1.0, 0.0], "children": [1, 2]},
        {"name": "square", "mesh": 0, "rotation": [0.0, 0.0, half, half], "scale": [2.0, 2.0, 2.0]},
        {"name": "triangles", "mesh": 1,
         "matrix": [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 3, 0, 0, 1]},
        {"name": "outside", "mesh": 1},
    ],
    "meshes": [
        {"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                         "indices": 3, "material": 0}]},
        {"primitives": [
            {"attributes": {"POSITION": 4}, "indices": 5, "material": 1},
            {"attributes": {"POSITION": 4, "TEXCOORD_0": 2}, "indices": 5, "material": 2, "mode": 4},
        ]},
    ],
    "materials": [
        {"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}},
        {"pbrMetallicRoughness": {"baseColorFactor": [0.5, 0.25, 1.0, 1.0]}},
        {"pbrMetallicRoughness": {"baseColorTexture": {"index": 1}}},
    ],
    "textures": [
        {"source": 0},
        {"extensions": {"KHR_texture_basisu": {"source": 1}}},
    ],
    "images": [
        {"bufferView": v_png, "mimeType": "image/png"},
        {"bufferView": v_ktx, "mimeType": "image/ktx2"},
    ],
    "accessors": accessors,
    "bufferViews": views,
    "buffers": [{"byteLength": len(pad4(binary))}],
}

js = pad4(json.dumps(gltf, separators=(",", ":")).encode(), b" ")
bn = pad4(binary)
out = (
    struct.pack("<III", 0x46546C67, 2, 12 + 8 + len(js) + 8 + len(bn))
    + struct.pack("<II", len(js), 0x4E4F534A) + js
    + struct.pack("<II", len(bn), 0x004E4942) + bn
)
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "sample.glb"), "wb") as f:
    f.write(out)
