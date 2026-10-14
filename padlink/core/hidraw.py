"""Thin hidraw wrapper using the kernel ioctl interface (linux/hidraw.h)."""

from __future__ import annotations

import fcntl
import os
import select
import struct

_IOC_WRITE = 1
_IOC_READ = 2


def _ioc(direction: int, nr: int, size: int) -> int:
    return (direction << 30) | (size << 16) | (ord("H") << 8) | nr


HID_MAX_DESCRIPTOR_SIZE = 4096
HIDIOCGRDESCSIZE = _ioc(_IOC_READ, 0x01, 4)
HIDIOCGRDESC = _ioc(_IOC_READ, 0x02, 4 + HID_MAX_DESCRIPTOR_SIZE)
HIDIOCGRAWINFO = _ioc(_IOC_READ, 0x03, 8)


def HIDIOCGRAWNAME(n: int) -> int:
    return _ioc(_IOC_READ, 0x04, n)


def HIDIOCGRAWPHYS(n: int) -> int:
    return _ioc(_IOC_READ, 0x05, n)


def HIDIOCSFEATURE(n: int) -> int:
    return _ioc(_IOC_WRITE | _IOC_READ, 0x06, n)


def HIDIOCGFEATURE(n: int) -> int:
    return _ioc(_IOC_WRITE | _IOC_READ, 0x07, n)


def HIDIOCGINPUT(n: int) -> int:
    return _ioc(_IOC_WRITE | _IOC_READ, 0x0A, n)


def HIDIOCSOUTPUT(n: int) -> int:
    return _ioc(_IOC_WRITE | _IOC_READ, 0x0B, n)


def parse_hex(text: str) -> bytes:
    """Accept "05 ff 00", "05ff00", "0x05,0xff,0x00" and similar."""
    cleaned = text.replace(",", " ").replace(":", " ").split()
    out = bytearray()
    for tok in cleaned:
        tok = tok.lower().removeprefix("0x")
        if len(tok) > 2 and len(tok) % 2 == 0:
            out += bytes.fromhex(tok)
        else:
            out.append(int(tok, 16))
    return bytes(out)


def hexstr(data: bytes) -> str:
    return data.hex(" ")


class HidrawDevice:
    """An open hidraw node. Use as a context manager."""

    def __init__(self, path: str, write: bool = False):
        self.path = path
        flags = (os.O_RDWR if write else os.O_RDONLY) | os.O_NONBLOCK | os.O_CLOEXEC
        self.fd = os.open(path, flags)

    def close(self) -> None:
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1

    def __enter__(self) -> "HidrawDevice":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def fileno(self) -> int:
        return self.fd

    def info(self) -> dict:
        buf = bytearray(8)
        fcntl.ioctl(self.fd, HIDIOCGRAWINFO, buf)
        bustype, vendor, product = struct.unpack("<Ihh", buf)
        return {"bustype": bustype, "vendor": vendor & 0xFFFF, "product": product & 0xFFFF}

    def name(self) -> str:
        buf = bytearray(256)
        n = fcntl.ioctl(self.fd, HIDIOCGRAWNAME(len(buf)), buf)
        return bytes(buf[: max(n, 0)]).split(b"\0", 1)[0].decode(errors="replace")

    def phys(self) -> str:
        buf = bytearray(256)
        n = fcntl.ioctl(self.fd, HIDIOCGRAWPHYS(len(buf)), buf)
        return bytes(buf[: max(n, 0)]).split(b"\0", 1)[0].decode(errors="replace")

    def report_descriptor(self) -> bytes:
        size = bytearray(4)
        fcntl.ioctl(self.fd, HIDIOCGRDESCSIZE, size)
        (n,) = struct.unpack("<I", size)
        buf = bytearray(struct.pack("<I", n) + bytes(HID_MAX_DESCRIPTOR_SIZE))
        fcntl.ioctl(self.fd, HIDIOCGRDESC, buf)
        return bytes(buf[4 : 4 + n])

    def write(self, data: bytes) -> int:
        """Send an output report. data[0] is the report ID (0 if the device uses none)."""
        return os.write(self.fd, data)

    def read(self, timeout: float | None = None, size: int = 1024) -> bytes | None:
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return None
        try:
            return os.read(self.fd, size)
        except BlockingIOError:
            return None

    def get_feature(self, report_id: int, length: int) -> bytes:
        buf = bytearray(length)
        buf[0] = report_id
        n = fcntl.ioctl(self.fd, HIDIOCGFEATURE(length), buf)
        return bytes(buf[:n])

    def set_feature(self, data: bytes) -> int:
        buf = bytearray(data)
        return fcntl.ioctl(self.fd, HIDIOCSFEATURE(len(buf)), buf)

    def get_input(self, report_id: int, length: int) -> bytes:
        buf = bytearray(length)
        buf[0] = report_id
        n = fcntl.ioctl(self.fd, HIDIOCGINPUT(length), buf)
        return bytes(buf[:n])
