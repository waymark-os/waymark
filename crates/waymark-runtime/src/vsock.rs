// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io;

use crate::StoneGuest;

pub fn run_vsock_task_server(_guest: &mut StoneGuest, _port: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "vsock task server is not included in this waymark build",
    ))
}
