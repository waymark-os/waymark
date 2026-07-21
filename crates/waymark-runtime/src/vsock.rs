// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io;

use crate::StoneGuest;

#[cfg(target_os = "hermit")]
mod imp {
    use std::io::{self, Read, Write};
    use std::mem::size_of;
    use std::os::fd::AsRawFd;
    use std::os::hermit::io::{FromRawFd, OwnedFd, RawFd};

    use hermit_abi::{
        accept, bind, listen, read, sa_family_t, sockaddr, sockaddr_vm, socket, socklen_t, write,
        AF_VSOCK, SOCK_STREAM, VMADDR_CID_ANY,
    };

    use crate::{
        server::{run_supervisor_task_server_stream, run_task_server_stream},
        StoneGuest, VmSupervisor,
    };

    pub fn run_vsock_task_server(guest: &mut StoneGuest, port: u32) -> io::Result<()> {
        eprintln!(
            "{}",
            serde_json::json!({
                "waymark_log": true,
                "level": "info",
                "target": "task_server_vsock",
                "message": "binding vsock task server",
                "port": port,
            })
        );
        let listener = VsockListener::bind(port)?;
        eprintln!(
            "{}",
            serde_json::json!({
                "waymark_log": true,
                "level": "info",
                "target": "task_server_vsock",
                "message": "listening for vsock task connection",
                "port": port,
            })
        );
        let mut stream = listener.accept()?;
        eprintln!(
            "{}",
            serde_json::json!({
                "waymark_log": true,
                "level": "info",
                "target": "task_server_vsock",
                "message": "accepted vsock task connection",
                "port": port,
            })
        );
        run_task_server_stream(guest, &mut stream)
    }

    pub fn run_vsock_supervisor_server(supervisor: &mut VmSupervisor, port: u32) -> io::Result<()> {
        eprintln!(
            "{}",
            serde_json::json!({
                "waymark_log": true,
                "level": "info",
                "target": "supervisor_vsock",
                "message": "binding reusable VM supervisor",
                "port": port,
            })
        );
        let listener = VsockListener::bind(port)?;
        eprintln!(
            "{}",
            serde_json::json!({
                "waymark_log": true,
                "level": "info",
                "target": "supervisor_vsock",
                "message": "listening for reusable VM lease connection",
                "port": port,
            })
        );
        let mut stream = listener.accept()?;
        eprintln!(
            "{}",
            serde_json::json!({
                "waymark_log": true,
                "level": "info",
                "target": "supervisor_vsock",
                "message": "accepted reusable VM lease connection",
                "port": port,
            })
        );
        run_supervisor_task_server_stream(supervisor, &mut stream)
    }

    fn check_i32(result: i32) -> io::Result<i32> {
        if result >= 0 {
            Ok(result)
        } else {
            Err(io::Error::from(errno_kind(-result)))
        }
    }

    fn check_isize(result: isize) -> io::Result<isize> {
        if result >= 0 {
            Ok(result)
        } else {
            let errno = (-result).try_into().unwrap_or_default();
            Err(io::Error::from(errno_kind(errno)))
        }
    }

    fn errno_kind(errno: i32) -> io::ErrorKind {
        match errno {
            hermit_abi::errno::EACCES | hermit_abi::errno::EPERM => io::ErrorKind::PermissionDenied,
            hermit_abi::errno::EADDRINUSE => io::ErrorKind::AddrInUse,
            hermit_abi::errno::EADDRNOTAVAIL => io::ErrorKind::AddrNotAvailable,
            hermit_abi::errno::EAGAIN => io::ErrorKind::WouldBlock,
            hermit_abi::errno::ECONNABORTED => io::ErrorKind::ConnectionAborted,
            hermit_abi::errno::ECONNREFUSED => io::ErrorKind::ConnectionRefused,
            hermit_abi::errno::ECONNRESET => io::ErrorKind::ConnectionReset,
            hermit_abi::errno::EINTR => io::ErrorKind::Interrupted,
            hermit_abi::errno::EINVAL => io::ErrorKind::InvalidInput,
            hermit_abi::errno::ENOENT => io::ErrorKind::NotFound,
            hermit_abi::errno::ENOTCONN => io::ErrorKind::NotConnected,
            hermit_abi::errno::EPIPE => io::ErrorKind::BrokenPipe,
            hermit_abi::errno::ETIMEDOUT => io::ErrorKind::TimedOut,
            _ => io::ErrorKind::Other,
        }
    }

    #[derive(Debug)]
    struct VsockListener {
        fd: OwnedFd,
    }

    impl VsockListener {
        fn bind(port: u32) -> io::Result<Self> {
            unsafe {
                let saddr = sockaddr_vm {
                    svm_len: size_of::<sockaddr_vm>().try_into().unwrap(),
                    svm_reserved1: 0,
                    svm_family: AF_VSOCK as sa_family_t,
                    svm_cid: VMADDR_CID_ANY,
                    svm_port: port,
                    svm_zero: [0; 4],
                };
                let fd = socket(AF_VSOCK, SOCK_STREAM, 0);
                check_i32(fd)?;
                let fd = OwnedFd::from_raw_fd(fd);
                check_i32(bind(
                    fd.as_raw_fd(),
                    &saddr as *const _ as *const sockaddr,
                    size_of::<sockaddr_vm>().try_into().unwrap(),
                ))?;
                check_i32(listen(fd.as_raw_fd(), 128))?;

                Ok(Self { fd })
            }
        }

        fn accept(&self) -> io::Result<VsockStream> {
            let mut addr_len: socklen_t = size_of::<sockaddr_vm>().try_into().unwrap();
            let mut addr = sockaddr_vm {
                svm_len: addr_len.try_into().unwrap(),
                svm_reserved1: 0,
                svm_family: AF_VSOCK as sa_family_t,
                svm_cid: 0,
                svm_port: 0,
                svm_zero: [0; 4],
            };

            let fd = unsafe {
                check_i32(accept(
                    self.fd.as_raw_fd(),
                    &mut addr as *mut _ as *mut sockaddr,
                    &mut addr_len as *mut u32,
                ))?
            };

            Ok(VsockStream::new(fd))
        }
    }

    struct VsockStream {
        fd: OwnedFd,
    }

    impl VsockStream {
        fn new(fd: RawFd) -> Self {
            Self {
                fd: unsafe { FromRawFd::from_raw_fd(fd) },
            }
        }
    }

    impl Read for VsockStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let result =
                unsafe { check_isize(read(self.fd.as_raw_fd(), buf.as_mut_ptr(), buf.len()))? };
            Ok(result.try_into().unwrap())
        }
    }

    impl Write for VsockStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let result =
                unsafe { check_isize(write(self.fd.as_raw_fd(), buf.as_ptr(), buf.len()))? };
            Ok(result.try_into().unwrap())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(target_os = "hermit")]
pub fn run_vsock_task_server(guest: &mut StoneGuest, port: u32) -> io::Result<()> {
    imp::run_vsock_task_server(guest, port)
}

#[cfg(target_os = "hermit")]
pub fn run_vsock_supervisor_server(
    supervisor: &mut crate::VmSupervisor,
    port: u32,
) -> io::Result<()> {
    imp::run_vsock_supervisor_server(supervisor, port)
}

#[cfg(not(target_os = "hermit"))]
pub fn run_vsock_task_server(_guest: &mut StoneGuest, _port: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "vsock task server is not included in this waymark build",
    ))
}

#[cfg(not(target_os = "hermit"))]
pub fn run_vsock_supervisor_server(
    _supervisor: &mut crate::VmSupervisor,
    _port: u32,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "vsock supervisor server is not included in this waymark build",
    ))
}
