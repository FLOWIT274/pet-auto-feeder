import fcntl, struct, os, mmap, select, sys, time

def hle(nr, size): return (3 << 30) | (size << 16) | (0x56 << 8) | nr
S_FMT, REQBUFS, QUERYBUF, QBUF, DQBUF = hle(5,208), hle(8,20), hle(9,88), hle(15,88), hle(17,88)
STREAMON, STREAMOFF = 0x40045612, 0x40045613

DEV = sys.argv[1] if len(sys.argv) > 1 else "/dev/video0"
N = int(sys.argv[2]) if len(sys.argv) > 2 else 5

fd = os.open(DEV, os.O_RDWR)
pix = struct.pack("IIIIIIII", 1280, 720, 0x47504A4D, 0, 0, 0, 0, 0)
fmt = struct.pack("I", 1) + pix + b"\0" * (208 - 36)
fcntl.ioctl(fd, S_FMT, fmt)
rb = bytearray(20); struct.pack_into("III", rb, 0, 4, 1, 1)
fcntl.ioctl(fd, REQBUFS, rb)

bufs = []
for i in range(4):
    qb = bytearray(88); struct.pack_into("II", qb, 0, i, 1)
    fcntl.ioctl(fd, QUERYBUF, qb)
    off = struct.unpack_from("I", qb, 64)[0]
    ln = struct.unpack_from("I", qb, 72)[0]
    bufs.append(mmap.mmap(fd, ln, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE, offset=off))
    q = bytearray(88); struct.pack_into("II", q, 0, i, 1); struct.pack_into("I", q, 60, 1)
    fcntl.ioctl(fd, QBUF, q)
fcntl.ioctl(fd, STREAMON, struct.pack("I", 1))
print("STREAMON OK")

saved = 0
t0 = time.time()
while saved < N:
    r, _, _ = select.select([fd], [], [], 3)
    if not r:
        print("超时"); break
    dq = bytearray(88); struct.pack_into("II", dq, 0, 0, 1); struct.pack_into("I", dq, 60, 1)
    fcntl.ioctl(fd, DQBUF, dq)
    idx = struct.unpack_from("I", dq, 0)[0]
    used = struct.unpack_from("I", dq, 8)[0]
    data = bytes(bufs[idx][:used])
    with open("/tmp/cam_%d.jpg" % saved, "wb") as f:
        f.write(data)
    print("帧 %d: %d 字节 jpeg=%s" % (saved, used, data[2:4] == b"\xff\xd0"))
    saved += 1
    q = bytearray(88); struct.pack_into("II", q, 0, idx, 1); struct.pack_into("I", q, 60, 1)
    fcntl.ioctl(fd, QBUF, q)

fcntl.ioctl(fd, STREAMOFF, struct.pack("I", 1))
print("完成 %d 帧 / %.1fs" % (saved, time.time() - t0))
os.close(fd)
