import random, struct
from mkcmyk import pdf
random.seed(2)
w, h = 1200, 1600
def gen(comps):
    d = bytearray()
    for y in range(h):
        for x in range(w):
            for c in range(comps):
                v = ((x * (c + 3) + y * (c + 1)) * 17 + random.randrange(400)) & 0xFFFF
                d += struct.pack('>H', v)
    return bytes(d)
pdf('gray16.pdf', w, h, gen(1), cs=b'/DeviceGray', bpc=16)
pdf('rgb16.pdf', w, h, gen(3), cs=b'/DeviceRGB', bpc=16)
pdf('cmyk16.pdf', w, h, gen(4), cs=b'/DeviceCMYK', bpc=16)
