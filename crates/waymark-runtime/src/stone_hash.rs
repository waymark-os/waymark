// SPDX-License-Identifier: MIT OR Apache-2.0

use md5::Md5;
use nu_protocol::{shell_error::generic::GenericError, ShellError, Span, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub(crate) fn hash_builtin(name: &str, text: &str) -> Result<Value, ShellError> {
    let digest = match name {
        "md5" => hex_digest::<Md5>(text.as_bytes()),
        "sha1" => hex_digest::<Sha1>(text.as_bytes()),
        "sha256" => hex_digest::<Sha256>(text.as_bytes()),
        other => {
            return Err(ShellError::Generic(
                GenericError::new_internal(
                    "Stone hash error",
                    format!("unsupported hash builtin `{other}`"),
                )
                .with_code("stone_script_error"),
            ));
        }
    };
    Ok(Value::string(digest, Span::unknown()))
}

fn hex_digest<D>(bytes: &[u8]) -> String
where
    D: Digest,
{
    let digest = D::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("write to string");
    }
    output
}
