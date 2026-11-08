import zlib, sys, random
def pdf(path, w, h, data, extra=b'', bpc=8, cs=b'/DeviceCMYK'):
    comp = zlib.compress(data, 1)
    objs = []
    objs.append(b'<< /Type /Catalog /Pages 2 0 R >>')
    objs.append(b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>')
    objs.append(b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>')
    c = b'q 612 0 0 792 0 0 cm /Im0 Do Q'
    objs.append(b'<< /Length %d >>\nstream\n' % len(c) + c + b'\nendstream')
    objs.append(b'<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace %s /BitsPerComponent %d %s /Filter /FlateDecode /Length %d >>\nstream\n' % (w, h, cs, bpc, extra, len(comp)) + comp + b'\nendstream')
    out = bytearray(b'%PDF-1.5\n'); offs = []
    for i, o in enumerate(objs):
        offs.append(len(out)); out += b'%d 0 obj\n' % (i + 1) + o + b'\nendobj\n'
    x = len(out)
    out += b'xref\n0 %d\n0000000000 65535 f \n' % (len(objs) + 1)
    for o in offs: out += b'%010d 00000 n \n' % o
    out += b'trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n' % (len(objs) + 1, x)
    open(path, 'wb').write(out)
if __name__ == '__main__':
    random.seed(1)
    w, h = 2000, 2800
    # smooth gradient with noise, so samples vary
    row = bytearray()
    d = bytearray()
    for y in range(h):
        for x in range(w):
            d += bytes(((x * 255 // w + random.randrange(8)) & 255, (y * 255 // h) & 255, ((x + y) * 255 // (w + h)) & 255, (x ^ y) & 31))
    pdf(sys.argv[1], w, h, bytes(d))
