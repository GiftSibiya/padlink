//! Thin hidraw wrapper using the kernel ioctl interface (linux/hidraw.h).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;

const fn ioc(direction: u64, nr: u64, size: usize) -> u64 {
    (direction << 30) | ((size as u64) << 16) | ((b'H' as u64) << 8) | nr
}

pub const HID_MAX_DESCRIPTOR_SIZE: usize = 4096;
const HIDIOCGRDESCSIZE: u64 = ioc(IOC_READ, 0x01, 4);
const HIDIOCGRDESC: u64 = ioc(IOC_READ, 0x02, 4 + HID_MAX_DESCRIPTOR_SIZE);
const HIDIOCGRAWINFO: u64 = ioc(IOC_READ, 0x03, 8);
const fn hidiocgrawname(n: usize) -> u64 {
    ioc(IOC_READ, 0x04, n)
}
const fn hidiocgrawphys(n: usize) -> u64 {
    ioc(IOC_READ, 0x05, n)
}
const fn hidiocsfeature(n: usize) -> u64 {
    ioc(IOC_WRITE | IOC_READ, 0x06, n)
}
const fn hidiocgfeature(n: usize) -> u64 {
    ioc(IOC_WRITE | IOC_READ, 0x07, n)
}
const fn hidiocginput(n: usize) -> u64 {
    ioc(IOC_WRITE | IOC_READ, 0x0A, n)
}

/// `struct hidraw_devinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevInfo {
    pub bustype: u32,
    pub vendor: u16,
    pub product: u16,
}

/// An open hidraw node (non-blocking).
#[derive(Debug)]
pub struct HidrawDevice {
    pub path: PathBuf,
    file: File,
}

impl HidrawDevice {
    pub fn open(path: impl AsRef<Path>, write: bool) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        Ok(HidrawDevice { path, file })
    }

    fn ioctl(&self, request: u64, buf: *mut libc::c_void) -> io::Result<i32> {
        // SAFETY: every caller passes a buffer at least as large as the size encoded in `request`.
        let n = unsafe { libc::ioctl(self.file.as_raw_fd(), request as libc::c_ulong, buf) };
        if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n) }
    }

    pub fn info(&self) -> io::Result<DevInfo> {
        let mut buf = [0u8; 8];
        self.ioctl(HIDIOCGRAWINFO, buf.as_mut_ptr().cast())?;
        Ok(DevInfo {
            bustype: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            vendor: u16::from_le_bytes(buf[4..6].try_into().unwrap()),
            product: u16::from_le_bytes(buf[6..8].try_into().unwrap()),
        })
    }

    fn string(&self, request: fn(usize) -> u64) -> io::Result<String> {
        let mut buf = [0u8; 256];
        let n = self.ioctl(request(buf.len()), buf.as_mut_ptr().cast())?.max(0) as usize;
        let s = &buf[..n.min(buf.len())];
        let s = s.split(|&b| b == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(s).into_owned())
    }

    pub fn name(&self) -> io::Result<String> {
        self.string(hidiocgrawname)
    }

    pub fn phys(&self) -> io::Result<String> {
        self.string(hidiocgrawphys)
    }

    pub fn report_descriptor(&self) -> io::Result<Vec<u8>> {
        let mut size = [0u8; 4];
        self.ioctl(HIDIOCGRDESCSIZE, size.as_mut_ptr().cast())?;
        let n = (u32::from_le_bytes(size) as usize).min(HID_MAX_DESCRIPTOR_SIZE);
        let mut buf = vec![0u8; 4 + HID_MAX_DESCRIPTOR_SIZE];
        buf[..4].copy_from_slice(&(n as u32).to_le_bytes());
        self.ioctl(HIDIOCGRDESC, buf.as_mut_ptr().cast())?;
        Ok(buf[4..4 + n].to_vec())
    }

    /// Send an output report. data[0] is the report ID (0 if the device uses none).
    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.file.write(data)
    }

    /// Wait up to `timeout` (forever if None) for one input report.
    pub fn read(&mut self, timeout: Option<Duration>) -> io::Result<Option<Vec<u8>>> {
        if !poll_readable(self.file.as_raw_fd(), timeout)? {
            return Ok(None);
        }
        let mut buf = [0u8; 1024];
        match self.file.read(&mut buf) {
            Ok(n) => Ok(Some(buf[..n].to_vec())),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn get_feature(&self, report_id: u8, length: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; length.max(1)];
        buf[0] = report_id;
        let n = self.ioctl(hidiocgfeature(buf.len()), buf.as_mut_ptr().cast())? as usize;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn set_feature(&self, data: &[u8]) -> io::Result<i32> {
        let mut buf = data.to_vec();
        self.ioctl(hidiocsfeature(buf.len()), buf.as_mut_ptr().cast())
    }

    pub fn get_input(&self, report_id: u8, length: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; length.max(1)];
        buf[0] = report_id;
        let n = self.ioctl(hidiocginput(buf.len()), buf.as_mut_ptr().cast())? as usize;
        buf.truncate(n);
        Ok(buf)
    }
}

impl AsRawFd for HidrawDevice {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }
}

/// Put an fd into non-blocking mode.
pub fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: plain fcntl flag update on an fd the caller owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Wait until `fd` is readable. Ok(false) on timeout; EINTR counts as a timeout.
pub fn poll_readable(fd: std::os::fd::RawFd, timeout: Option<Duration>) -> io::Result<bool> {
    Ok(!poll_many(&[fd], timeout)?.is_empty())
}

/// Wait until any of `fds` is readable; returns the indexes that are.
pub fn poll_many(fds: &[std::os::fd::RawFd], timeout: Option<Duration>) -> io::Result<Vec<usize>> {
    let mut pfds: Vec<libc::pollfd> =
        fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
    // SAFETY: pfds is a valid array of pollfd for its length.
    let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, ms) };
    if n < 0 {
        let err = io::Error::last_os_error();
        return if err.kind() == io::ErrorKind::Interrupted { Ok(vec![]) } else { Err(err) };
    }
    Ok(pfds
        .iter()
        .enumerate()
        .filter(|(_, p)| p.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0)
        .map(|(i, _)| i)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux_hidraw_h() {
        // values from the kernel headers via `python3 -c "import padlink.core.hidraw as h; ..."`
        assert_eq!(HIDIOCGRDESCSIZE, 0x80044801);
        assert_eq!(HIDIOCGRDESC, 0x90044802);
        assert_eq!(HIDIOCGRAWINFO, 0x80084803);
        assert_eq!(hidiocgrawname(256), 0x81004804);
        assert_eq!(hidiocsfeature(64), 0xc0404806);
        assert_eq!(hidiocgfeature(64), 0xc0404807);
        assert_eq!(hidiocginput(64), 0xc040480a);
    }
}
