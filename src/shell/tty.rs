// src/shell/tty.rs – Raw terminal I/O (Unix via direct FFI, Windows no‑op)

use std::io::{self, Read};

#[cfg(unix)]
mod unix {
    #![allow(dead_code)]

    use super::*;
    use std::ffi::c_int;
    use std::os::unix::io::AsRawFd;

    #[repr(C)]
    struct Termios {
        c_iflag: u32,
        c_oflag: u32,
        c_cflag: u32,
        c_lflag: u32,
        c_cc: [u8; 32],
        c_ispeed: u32,
        c_ospeed: u32,
    }

    const TCSANOW: c_int = 0;
    const ICANON: u32 = 0x0002;
    const ECHO: u32 = 0x0008;

    unsafe extern "C" {
        fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
        fn tcsetattr(fd: c_int, optional_actions: c_int, termios: *const Termios) -> c_int;
    }

    pub fn enable_raw_mode() -> io::Result<()> {
        let fd = io::stdin().as_raw_fd();
        let mut termios = unsafe { std::mem::zeroed::<Termios>() };
        unsafe {
            if tcgetattr(fd, &mut termios) != 0 {
                return Err(io::Error::last_os_error());
            }
            termios.c_lflag &= !(ICANON | ECHO);
            if tcsetattr(fd, TCSANOW, &termios) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn disable_raw_mode() -> io::Result<()> {
        let fd = io::stdin().as_raw_fd();
        let mut termios = unsafe { std::mem::zeroed::<Termios>() };
        unsafe {
            if tcgetattr(fd, &mut termios) != 0 {
                return Err(io::Error::last_os_error());
            }
            termios.c_lflag |= ICANON | ECHO;
            if tcsetattr(fd, TCSANOW, &termios) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn read_key() -> io::Result<Vec<u8>> {
        let mut buf = [0; 8];
        let n = io::stdin().read(&mut buf)?;
        Ok(buf[..n].to_vec())
    }

    pub fn terminal_size() -> Option<(usize, usize)> {
        use std::process::Command;
        let output = Command::new("stty").arg("size").output().ok()?;
        let stdout = String::from_utf8(output.stdout).ok()?;
        let mut parts = stdout.split_whitespace();
        let rows = parts.next()?.parse().ok()?;
        let cols = parts.next()?.parse().ok()?;
        Some((cols, rows))
    }
}

#[cfg(not(unix))]
mod windows {
    use super::*;
    pub fn enable_raw_mode() -> io::Result<()> {
        // emit_diagnostic(
        //     &Diagnostic::warning("Raw mode not supported on Windows. Using line input.")
        //         .with_code("VX9002"),
        // );
        Ok(())
    }
    pub fn disable_raw_mode() -> io::Result<()> {
        Ok(())
    }
    pub fn read_key() -> io::Result<Vec<u8>> {
        let mut buf = [0; 1];
        io::stdin().read(&mut buf)?;
        Ok(buf[..1].to_vec())
    }
    pub fn terminal_size() -> Option<(usize, usize)> {
        Some((80, 24))
    }
}

#[cfg(unix)]
#[allow(unused_imports)]
pub use unix::*;
#[cfg(not(unix))]
pub use windows::*;
