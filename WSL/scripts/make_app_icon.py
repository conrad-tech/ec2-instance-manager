#!/usr/bin/env python3
"""Regenerate assets/app_icon.ico from assets/app_icon.png.

    ./scripts/make_app_icon.py assets/app_icon.png assets/app_icon.ico

build.rs compiles the .ico into ec2_manager_gui.exe as its Win32 icon
resource, which is what Explorer and a pinned taskbar shortcut read; the .png
is the same picture, handed to `ViewportBuilder::with_icon` for the live
window. Change the .png and run this, or the two drift apart.

Pure stdlib on purpose — neither Pillow nor ImageMagick is installed on the
WSL build box, and an icon is not worth a dependency. Input must be a square,
8-bit, non-interlaced RGB/RGBA PNG.
"""
import struct, sys, zlib

def read_png(path):
    data = open(path, 'rb').read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', 'not a png'
    pos = 8
    idat = b''
    w = h = depth = ctype = None
    while pos < len(data):
        (ln,) = struct.unpack('>I', data[pos:pos+4])
        typ = data[pos+4:pos+8]
        body = data[pos+8:pos+8+ln]
        pos += 12 + ln
        if typ == b'IHDR':
            w, h, depth, ctype, comp, filt, inter = struct.unpack('>IIBBBBB', body)
            assert depth == 8, f'bit depth {depth} unsupported'
            assert ctype in (2, 6), f'color type {ctype} unsupported'
            assert inter == 0, 'interlaced png unsupported'
        elif typ == b'IDAT':
            idat += body
        elif typ == b'IEND':
            break
    raw = zlib.decompress(idat)
    nch = 4 if ctype == 6 else 3
    stride = w * nch
    out = bytearray(w * h * 4)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        f = raw[p]; p += 1
        line = bytearray(raw[p:p+stride]); p += stride
        for x in range(stride):
            a = line[x - nch] if x >= nch else 0
            b = prev[x]
            c = prev[x - nch] if x >= nch else 0
            if f == 1:   line[x] = (line[x] + a) & 0xFF
            elif f == 2: line[x] = (line[x] + b) & 0xFF
            elif f == 3: line[x] = (line[x] + ((a + b) >> 1)) & 0xFF
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 0xFF
        prev = line
        o = y * w * 4
        if nch == 4:
            out[o:o+w*4] = line
        else:
            for x in range(w):
                out[o+x*4:o+x*4+3] = line[x*3:x*3+3]
                out[o+x*4+3] = 255
    return w, h, bytes(out)

def resize(src, sw, sh, dw, dh):
    """Box-average downsample, premultiplying alpha so transparent pixels
    do not drag colour into the edges."""
    dst = bytearray(dw * dh * 4)
    for dy in range(dh):
        y0, y1 = dy * sh // dh, max(dy * sh // dh + 1, (dy + 1) * sh // dh)
        for dx in range(dw):
            x0, x1 = dx * sw // dw, max(dx * sw // dw + 1, (dx + 1) * sw // dw)
            r = g = b = a = n = 0
            for y in range(y0, y1):
                base = (y * sw) * 4
                for x in range(x0, x1):
                    o = base + x * 4
                    av = src[o+3]
                    r += src[o] * av; g += src[o+1] * av; b += src[o+2] * av
                    a += av; n += 1
            o = (dy * dw + dx) * 4
            if a:
                dst[o] = min(255, r // a); dst[o+1] = min(255, g // a); dst[o+2] = min(255, b // a)
            dst[o+3] = a // n
    return bytes(dst)

def write_png(w, h, rgba):
    def chunk(typ, body):
        return struct.pack('>I', len(body)) + typ + body + struct.pack('>I', zlib.crc32(typ + body) & 0xFFFFFFFF)
    raw = b''.join(b'\x00' + rgba[y*w*4:(y+1)*w*4] for y in range(h))
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw, 9))
            + chunk(b'IEND', b''))

def write_dib(w, h, rgba):
    """32-bit BGRA DIB with the bottom-up rows and the (unused but required)
    AND mask that the .ico format still expects for sub-256 entries."""
    hdr = struct.pack('<IiiHHIIiiII', 40, w, h * 2, 1, 32, 0, w * h * 4, 0, 0, 0, 0)
    rows = []
    for y in range(h - 1, -1, -1):
        row = bytearray()
        for x in range(w):
            o = (y * w + x) * 4
            row += bytes((rgba[o+2], rgba[o+1], rgba[o], rgba[o+3]))
        rows.append(bytes(row))
    mask_stride = ((w + 31) // 32) * 4
    mask = b'\x00' * (mask_stride * h)
    return hdr + b''.join(rows) + mask

def main(src, dst, sizes):
    sw, sh, rgba = read_png(src)
    assert sw == sh, 'icon must be square'
    images = []
    for s in sizes:
        px = rgba if s == sw else resize(rgba, sw, sh, s, s)
        # PNG-compressed entries are the convention (and the size limit fix)
        # for 256x256; classic DIB for everything smaller, which every
        # Windows shell version reads.
        images.append((s, write_png(s, s, px) if s >= 256 else write_dib(s, s, px)))
    out = struct.pack('<HHH', 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for s, blob in images:
        out += struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    out += b''.join(blob for _, blob in images)
    open(dst, 'wb').write(out)
    print(f'wrote {dst}: {len(images)} entries {sizes}, {len(out)} bytes')

if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2], [16, 24, 32, 48, 64, 128, 256])
