# Writes a 2-page sgray_8 PWG raster (A4 @150dpi). Page 1 is mostly black, page 2 has
# "text lines" in a narrow column, so cropping should enlarge it.
import struct, sys
W, H, DPI = 1240, 1754, 150

def header():
    h = bytearray(1796)
    def put(off, v): h[off:off+4] = struct.pack('>I', v)
    put(276, DPI); put(280, DPI); put(372, W); put(376, H)
    put(384, 8); put(388, 8); put(392, W); put(400, 18)
    return bytes(h)

def encode_line(px):
    out = bytearray([0])
    for i in range(0, len(px), 128):
        chunk = px[i:i+128]
        out.append(257 - len(chunk) if len(chunk) > 1 else 0)
        out += chunk
    return bytes(out)

def page(fn):
    data = bytearray(header())
    for y in range(H):
        data += encode_line(bytes(fn(x, y) for x in range(W)))
    return data

p1 = page(lambda x, y: 0 if 100 < x < 1100 and 100 < y < 1600 else 255)
def text(x, y):
    if not (300 <= x < 800 and 400 <= y < 900): return 255
    line, row = divmod(y - 400, 40)
    return 0 if row < 18 and (x // 25) % 3 != 2 else 255
p2 = page(text)
with open(sys.argv[1], 'wb') as f:
    f.write(b'RaS2' + p1 + p2)
